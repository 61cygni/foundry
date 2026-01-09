//! foundry-player: Stream MP4 files over WebSocket
//!
//! Usage: foundry-player movie.mp4

use anyhow::{anyhow, Result};
use axum::{
    body::Body,
    extract::{
        ws::{Message, Utf8Bytes, WebSocket, WebSocketUpgrade},
        State,
    },
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use clap::Parser;
use futures_util::{SinkExt, StreamExt};
use std::{
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{
    fs,
    sync::mpsc,
    time::{interval, MissedTickBehavior},
};

mod audio_decoder;
mod demuxer;

use audio_decoder::DecodedAudio;
use demuxer::{MediaFrame, Mp4Demuxer};

const OUTBOUND_BUFFER: usize = 256;

#[derive(Parser)]
#[command(name = "foundry-player")]
#[command(about = "Stream MP4 files over WebSocket")]
struct Cli {
    /// Path to the MP4 file to stream
    file: PathBuf,

    /// Port to listen on
    #[arg(long, default_value = "23646")]
    port: u16,

    /// Loop playback
    #[arg(long)]
    loop_playback: bool,

    /// Start time in seconds (seek into the video)
    #[arg(long, default_value = "0")]
    start: f64,
}

#[derive(Clone)]
struct AppState {
    demuxer: Arc<Mp4Demuxer>,
    audio: Option<Arc<DecodedAudio>>,
    loop_playback: bool,
    start_time: f64,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    if !cli.file.exists() {
        return Err(anyhow!("File not found: {:?}", cli.file));
    }

    println!("Loading {:?}...", cli.file);
    let demuxer = Mp4Demuxer::open(&cli.file)?;

    println!(
        "Video: {}x{} @ {:.2} fps, {} frames",
        demuxer.video_width(),
        demuxer.video_height(),
        demuxer.frame_rate(),
        demuxer.frame_count()
    );

    // Decode audio
    let audio = if demuxer.has_audio() {
        println!("Decoding audio...");
        match audio_decoder::decode_audio(&cli.file) {
            Ok(Some(decoded)) => {
                let duration_secs = decoded.samples.len() as f64 
                    / decoded.sample_rate as f64 
                    / decoded.channels as f64;
                println!(
                    "Audio: {} Hz, {} channels, {:.1}s decoded",
                    decoded.sample_rate,
                    decoded.channels,
                    duration_secs
                );
                Some(Arc::new(decoded))
            }
            Ok(None) => {
                println!("Audio: no audio data found");
                None
            }
            Err(e) => {
                eprintln!("Audio decode failed: {}", e);
                None
            }
        }
    } else {
        println!("Audio: none");
        None
    };

    let state = AppState {
        demuxer: Arc::new(demuxer),
        audio,
        loop_playback: cli.loop_playback,
        start_time: cli.start,
    };

    let app = Router::new()
        .route("/", get(serve_html))
        .route("/ws", get(get_ws))
        .route("/video.js", get(|| serve_static("video.js")))
        .route("/video_worker.js", get(|| serve_static("video_worker.js")))
        .route("/audio.js", get(|| serve_static("audio.js")))
        .route("/audio_worklet.js", get(|| serve_static("audio_worklet.js")))
        .route("/gui.js", get(|| serve_static("gui.js")))
        .route("/stats.js", get(|| serve_static("stats.js")))
        .with_state(state);

    let addr = format!("0.0.0.0:{}", cli.port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    println!("Open http://localhost:{}/", cli.port);
    axum::serve(listener, app).await?;

    Ok(())
}

async fn serve_html() -> Response {
    // Serve a minimal player HTML
    let html = include_str!("player.html");
    Response::builder()
        .header("Content-Type", "text/html")
        .body(Body::from(html))
        .unwrap()
}

async fn serve_static(file: &'static str) -> Response {
    // Serve JS files from foundry's src directory
    let path = format!(
        "{}/src/{}",
        env!("CARGO_MANIFEST_DIR").replace("/foundry-player", ""),
        file
    );

    match fs::read(&path).await {
        Ok(bytes) => Response::builder()
            .header("Content-Type", "text/javascript")
            .body(Body::from(bytes))
            .unwrap(),
        Err(err) => {
            eprintln!("Failed to read {}: {}", path, err);
            Response::builder()
                .status(404)
                .body(Body::from("not found"))
                .unwrap()
        }
    }
}

async fn get_ws(State(state): State<AppState>, ws: WebSocketUpgrade) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_ws(socket, state))
}

async fn handle_ws(stream: WebSocket, state: AppState) {
    let (mut sender, mut receiver) = stream.split();
    let (tx, mut rx) = mpsc::channel::<Message>(OUTBOUND_BUFFER);

    // Outbound task: send messages to client
    let outbound = tokio::spawn(async move {
        let mut ticker = interval(Duration::from_secs(10));
        ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                Some(msg) = rx.recv() => {
                    if sender.send(msg).await.is_err() {
                        break;
                    }
                }
                _ = ticker.tick() => {
                    if sender.send(Message::Text(Utf8Bytes::from("heartbeat"))).await.is_err() {
                        break;
                    }
                }
            }
        }
    });

    // Playback task
    let tx_clone = tx.clone();
    let playback = tokio::spawn(async move {
        if let Err(e) = run_playback(tx_clone, state).await {
            eprintln!("Playback error: {}", e);
        }
    });

    // Inbound task: handle client messages
    let inbound = tokio::spawn(async move {
        while let Some(Ok(msg)) = receiver.next().await {
            match msg {
                Message::Text(text) => {
                    // Handle commands like seek, pause, etc. (future)
                    println!("Received: {}", text);
                }
                Message::Close(_) => break,
                _ => {}
            }
        }
    });

    let _ = tokio::try_join!(outbound, playback, inbound);
    println!("Session ended");
}

async fn run_playback(tx: mpsc::Sender<Message>, state: AppState) -> Result<()> {
    let start_time = state.start_time;
    println!("Starting playback at {:.1}s...", start_time);

    // Send video config first
    let config = state.demuxer.video_config()?;
    let config_json = serde_json::json!({
        "type": "video-config",
        "config": {
            "codec": config.codec_string,
            "description": config.description_b64,
            "width": config.width,
            "height": config.height,
        }
    });
    tx.send(Message::Text(Utf8Bytes::from(config_json.to_string())))
        .await?;

    // Send mode ack
    tx.send(Message::Text(Utf8Bytes::from(
        r#"{"type":"mode-ack","mode":"video","codec":"avc"}"#,
    )))
    .await?;

    // Audio state
    let audio_sample_rate = state.audio.as_ref().map(|a| a.sample_rate).unwrap_or(48000);
    let audio_channels = state.audio.as_ref().map(|a| a.channels).unwrap_or(2);
    let audio_samples = state.audio.as_ref().map(|a| &a.samples[..]);
    
    // Audio chunk size: ~40ms worth of samples (balance between latency and overhead)
    let audio_chunk_duration = 0.04; // 40ms
    let audio_chunk_samples = (audio_sample_rate as f64 * audio_channels as f64 * audio_chunk_duration) as usize;

    loop {
        let playback_start = Instant::now();
        let mut last_audio_time: f64 = start_time;
        let mut found_keyframe = false;
        
        // Create a fresh iterator for each playback loop
        let frames = state.demuxer.frames()?;

        for frame in frames {
            let frame = frame?;
            
            // Skip frames before start time
            if frame.timestamp_secs < start_time {
                continue;
            }
            
            // For first frame after start_time, we need a keyframe
            let MediaFrame::Video { is_keyframe, .. } = &frame.media;
            if !found_keyframe {
                if !is_keyframe {
                    continue; // Skip until we get a keyframe
                }
                found_keyframe = true;
            }

            // Calculate when this frame should be presented (relative to start_time)
            let relative_time = frame.timestamp_secs - start_time;
            let target_time = Duration::from_secs_f64(relative_time);
            let elapsed = playback_start.elapsed();

            // Wait until it's time to send this frame
            if target_time > elapsed {
                tokio::time::sleep(target_time - elapsed).await;
            }

            // Send audio for this time window (send audio just before video for sync)
            if let Some(samples) = audio_samples {
                let audio_start_sample = (last_audio_time * audio_sample_rate as f64 * audio_channels as f64) as usize;
                let audio_end_sample = (frame.timestamp_secs * audio_sample_rate as f64 * audio_channels as f64) as usize;
                
                // Send audio in chunks
                let mut pos = audio_start_sample;
                while pos < audio_end_sample && pos < samples.len() {
                    let chunk_end = (pos + audio_chunk_samples).min(audio_end_sample).min(samples.len());
                    let chunk = &samples[pos..chunk_end];
                    
                    if !chunk.is_empty() {
                        let audio_msg = build_audio_chunk(chunk, audio_sample_rate);
                        if tx.send(Message::Binary(audio_msg.into())).await.is_err() {
                            return Ok(());
                        }
                    }
                    pos = chunk_end;
                }
                last_audio_time = frame.timestamp_secs;
            }

            // Send video frame
            let MediaFrame::Video { data, .. } = frame.media;
            if tx.send(Message::Binary(data.into())).await.is_err() {
                return Ok(());
            }
        }

        if !state.loop_playback {
            println!("Playback complete");
            break;
        }

        println!("Looping playback...");
    }

    Ok(())
}

/// Build audio chunk in Foundry's format
fn build_audio_chunk(samples: &[i16], sample_rate: u32) -> Vec<u8> {
    let channels = 2u32; // Stereo
    let sample_count = samples.len() as u32;

    let mut out = Vec::with_capacity(24 + samples.len() * 2);
    out.extend_from_slice(b"AUD0");
    out.extend_from_slice(&0.0f64.to_le_bytes()); // start_ms (not used)
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&sample_count.to_le_bytes());
    for s in samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}
