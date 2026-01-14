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
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use tokio::{
    fs,
    sync::{broadcast, mpsc, watch},
    time::{interval, MissedTickBehavior},
};

mod audio_decoder;
mod demuxer;
mod http_reader;

use audio_decoder::DecodedAudio;
use demuxer::{MediaFrame, Mp4Demuxer};

const OUTBOUND_BUFFER: usize = 256;
const BROADCAST_BUFFER: usize = 512;  // ~20 seconds at 24fps - prevents keyframe drops

#[derive(Parser)]
#[command(name = "foundry-player")]
#[command(about = "Stream MP4 files over WebSocket")]
struct Cli {
    /// Path to the MP4 file to stream (use --url for HTTP streaming)
    #[arg(required_unless_present = "url")]
    file: Option<PathBuf>,

    /// HTTP URL to stream video from (requires --audio-url for audio)
    #[arg(long)]
    url: Option<String>,

    /// HTTP URL for audio file (required with --url, downloaded on startup)
    #[arg(long)]
    audio_url: Option<String>,

    /// Port to listen on
    #[arg(long, default_value = "23646")]
    port: u16,

    /// Loop playback
    #[arg(long)]
    loop_playback: bool,

    /// Start time in seconds (seek into the video)
    #[arg(long, default_value = "0")]
    start: f64,

    /// Shared mode: single playback synced across all viewers
    #[arg(long)]
    shared: bool,
}

#[derive(Clone)]
struct AppState {
    demuxer: Arc<Mp4Demuxer>,
    audio: Option<Arc<DecodedAudio>>,
    loop_playback: bool,
    start_time: f64,
    // Shared mode state (None if not in shared mode)
    shared: Option<SharedState>,
}

/// State for shared/broadcast mode
#[derive(Clone)]
struct SharedState {
    /// Broadcast channel for frames (video and audio)
    frame_tx: broadcast::Sender<BroadcastFrame>,
    /// Global pause state - any client can pause/resume
    pause_tx: Arc<watch::Sender<bool>>,
    /// Restart signal - increments each time restart is requested
    restart_tx: Arc<watch::Sender<u64>>,
    /// Video config JSON to send to new clients
    video_config_json: Arc<String>,
    /// Number of connected viewers
    viewer_count: Arc<AtomicUsize>,
}

/// Frame types sent over broadcast channel
#[derive(Clone)]
enum BroadcastFrame {
    Video(Vec<u8>),
    Audio(Vec<u8>),
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Determine source: URL or local file
    let (demuxer, audio) = if let Some(url) = &cli.url {
        // HTTP URL mode
        println!("Loading video from URL...");
        let demuxer = Mp4Demuxer::open_url(url)?;

        println!(
            "Video: {}x{} @ {:.2} fps, {} frames",
            demuxer.video_width(),
            demuxer.video_height(),
            demuxer.frame_rate(),
            demuxer.frame_count()
        );

        // For URL mode, we need a separate audio file
        let audio = if let Some(audio_url) = &cli.audio_url {
            println!("Downloading audio from URL...");
            match http_reader::download_file(audio_url) {
                Ok(audio_data) => {
                    // Decode the downloaded audio
                    match audio_decoder::decode_audio_from_bytes(&audio_data) {
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
                            println!("Audio: no audio data found in file");
                            None
                        }
                        Err(e) => {
                            eprintln!("Audio decode failed: {}", e);
                            None
                        }
                    }
                }
                Err(e) => {
                    eprintln!("Failed to download audio: {}", e);
                    None
                }
            }
        } else {
            println!("Audio: none (use --audio-url to provide audio)");
            None
        };

        (demuxer, audio)
    } else if let Some(file) = &cli.file {
        // Local file mode (original behavior)
        if !file.exists() {
            return Err(anyhow!("File not found: {:?}", file));
        }

        println!("Loading {:?}...", file);
        let demuxer = Mp4Demuxer::open(file)?;

        println!(
            "Video: {}x{} @ {:.2} fps, {} frames",
            demuxer.video_width(),
            demuxer.video_height(),
            demuxer.frame_rate(),
            demuxer.frame_count()
        );

        // Decode audio from local file
        let audio = if demuxer.has_audio() {
            println!("Decoding audio...");
            match audio_decoder::decode_audio(file) {
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

        (demuxer, audio)
    } else {
        return Err(anyhow!("Either a file path or --url must be provided"));
    };

    let demuxer = Arc::new(demuxer);

    // Setup shared mode if requested
    let shared = if cli.shared {
        println!("Shared mode enabled - all viewers will be synced");
        
        let (frame_tx, _) = broadcast::channel(BROADCAST_BUFFER);
        let (pause_tx, pause_rx) = watch::channel(false);
        let (restart_tx, restart_rx) = watch::channel(0u64);
        
        // Build video config JSON
        let config = demuxer.video_config()?;
        let config_json = serde_json::json!({
            "type": "video-config",
            "config": {
                "codec": config.codec_string,
                "description": config.description_b64,
                "width": config.width,
                "height": config.height,
            }
        });
        
        let shared_state = SharedState {
            frame_tx: frame_tx.clone(),
            pause_tx: Arc::new(pause_tx),
            restart_tx: Arc::new(restart_tx),
            video_config_json: Arc::new(config_json.to_string()),
            viewer_count: Arc::new(AtomicUsize::new(0)),
        };
        
        // Spawn the shared playback loop
        let playback_state = AppState {
            demuxer: demuxer.clone(),
            audio: audio.clone(),
            loop_playback: cli.loop_playback,
            start_time: cli.start,
            shared: None, // Not needed for playback loop
        };
        
        tokio::spawn(run_shared_playback(
            frame_tx,
            pause_rx,
            restart_rx,
            playback_state,
            shared_state.viewer_count.clone(),
        ));
        
        Some(shared_state)
    } else {
        None
    };

    let state = AppState {
        demuxer,
        audio,
        loop_playback: cli.loop_playback,
        start_time: cli.start,
        shared,
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
    let mode = if cli.shared { " (shared mode)" } else { "" };
    println!("Open http://localhost:{}/{}", cli.port, mode);
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
    if let Some(shared) = &state.shared {
        handle_ws_shared(stream, shared.clone()).await;
    } else {
        handle_ws_individual(stream, state).await;
    }
}

/// Handle WebSocket in shared/broadcast mode
async fn handle_ws_shared(stream: WebSocket, shared: SharedState) {
    let (mut sender, mut receiver) = stream.split();
    
    // Track this viewer
    let count = shared.viewer_count.fetch_add(1, Ordering::SeqCst) + 1;
    println!("Viewer joined ({} connected)", count);
    
    // Subscribe to broadcast
    let mut frame_rx = shared.frame_tx.subscribe();
    
    // Send video config first
    if sender
        .send(Message::Text(Utf8Bytes::from(shared.video_config_json.as_str())))
        .await
        .is_err()
    {
        shared.viewer_count.fetch_sub(1, Ordering::SeqCst);
        return;
    }
    
    // Send mode ack
    if sender
        .send(Message::Text(Utf8Bytes::from(
            r#"{"type":"mode-ack","mode":"video","codec":"avc"}"#,
        )))
        .await
        .is_err()
    {
        shared.viewer_count.fetch_sub(1, Ordering::SeqCst);
        return;
    }

    let pause_tx = shared.pause_tx.clone();
    let restart_tx = shared.restart_tx.clone();
    let viewer_count = shared.viewer_count.clone();
    
    // Outbound task: forward broadcast frames to this client
    let outbound = tokio::spawn(async move {
        let mut ticker = interval(Duration::from_secs(10));
        ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
        
        loop {
            tokio::select! {
                result = frame_rx.recv() => {
                    match result {
                        Ok(frame) => {
                            let msg = match frame {
                                BroadcastFrame::Video(data) => Message::Binary(data.into()),
                                BroadcastFrame::Audio(data) => Message::Binary(data.into()),
                            };
                            if sender.send(msg).await.is_err() {
                                break;
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            eprintln!("Viewer lagged, skipped {} frames", n);
                            // Continue receiving
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
                _ = ticker.tick() => {
                    if sender.send(Message::Text(Utf8Bytes::from("heartbeat"))).await.is_err() {
                        break;
                    }
                }
            }
        }
        viewer_count.fetch_sub(1, Ordering::SeqCst);
    });

    // Inbound task: handle client messages (pause/resume/restart affects everyone)
    let inbound = tokio::spawn(async move {
        while let Some(Ok(msg)) = receiver.next().await {
            match msg {
                Message::Text(text) => {
                    if let Ok(cmd) = serde_json::from_str::<serde_json::Value>(&text) {
                        match cmd.get("type").and_then(|t| t.as_str()) {
                            Some("pause") => {
                                println!("Paused (by viewer)");
                                let _ = pause_tx.send(true);
                            }
                            Some("resume") => {
                                println!("Resumed (by viewer)");
                                let _ = pause_tx.send(false);
                            }
                            Some("restart") => {
                                println!("Restart requested (by viewer)");
                                // Increment restart counter to signal playback loop
                                let current = *restart_tx.borrow();
                                let _ = restart_tx.send(current + 1);
                            }
                            _ => {}
                        }
                    }
                }
                Message::Close(_) => break,
                _ => {}
            }
        }
    });

    let _ = tokio::try_join!(outbound, inbound);
    
    let remaining = shared.viewer_count.load(Ordering::SeqCst);
    println!("Viewer left ({} connected)", remaining);
}

/// Handle WebSocket in individual mode (original behavior)
async fn handle_ws_individual(stream: WebSocket, state: AppState) {
    let (mut sender, mut receiver) = stream.split();
    let (tx, mut rx) = mpsc::channel::<Message>(OUTBOUND_BUFFER);
    
    // Pause state: false = playing, true = paused
    let (pause_tx, pause_rx) = watch::channel(false);

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
        if let Err(e) = run_playback(tx_clone, state, pause_rx).await {
            eprintln!("Playback error: {}", e);
        }
    });

    // Inbound task: handle client messages
    let inbound = tokio::spawn(async move {
        while let Some(Ok(msg)) = receiver.next().await {
            match msg {
                Message::Text(text) => {
                    // Parse JSON commands
                    if let Ok(cmd) = serde_json::from_str::<serde_json::Value>(&text) {
                        match cmd.get("type").and_then(|t| t.as_str()) {
                            Some("pause") => {
                                println!("Paused");
                                let _ = pause_tx.send(true);
                            }
                            Some("resume") => {
                                println!("Resumed");
                                let _ = pause_tx.send(false);
                            }
                            _ => {}
                        }
                    }
                }
                Message::Close(_) => break,
                _ => {}
            }
        }
    });

    let _ = tokio::try_join!(outbound, playback, inbound);
    println!("Session ended");
}

/// Shared playback loop - broadcasts to all connected viewers
async fn run_shared_playback(
    frame_tx: broadcast::Sender<BroadcastFrame>,
    mut pause_rx: watch::Receiver<bool>,
    mut restart_rx: watch::Receiver<u64>,
    state: AppState,
    viewer_count: Arc<AtomicUsize>,
) {
    let start_time = state.start_time;
    println!("Shared playback ready (starting at {:.1}s)", start_time);

    // Audio state
    let audio_sample_rate = state.audio.as_ref().map(|a| a.sample_rate).unwrap_or(48000);
    let audio_channels = state.audio.as_ref().map(|a| a.channels).unwrap_or(2);
    let audio_samples = state.audio.as_ref().map(|a| &a.samples[..]);
    
    let audio_chunk_duration = 0.04; // 40ms
    let audio_chunk_samples = (audio_sample_rate as f64 * audio_channels as f64 * audio_chunk_duration) as usize;

    // Track restart counter to detect new restart requests
    let mut last_restart_count = *restart_rx.borrow();

    loop {
        // Wait for at least one viewer before starting playback
        while viewer_count.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        
        println!("Starting shared playback...");
        let playback_start = Instant::now();
        let mut pause_offset = Duration::ZERO;
        let mut last_audio_time: f64 = start_time;
        let mut found_keyframe = false;
        let mut restart_requested = false;
        
        let frames = match state.demuxer.frames() {
            Ok(f) => f,
            Err(e) => {
                eprintln!("Failed to get frames: {}", e);
                return;
            }
        };

        for frame in frames {
            // Check for restart request
            let current_restart = *restart_rx.borrow();
            if current_restart != last_restart_count {
                last_restart_count = current_restart;
                restart_requested = true;
                println!("Restarting playback...");
                break;
            }
            
            // Check if paused
            while *pause_rx.borrow() {
                let pause_start = Instant::now();
                
                // Also check for restart while paused
                tokio::select! {
                    result = pause_rx.changed() => {
                        if result.is_err() {
                            return;
                        }
                    }
                    _ = restart_rx.changed() => {
                        let current = *restart_rx.borrow();
                        if current != last_restart_count {
                            last_restart_count = current;
                            restart_requested = true;
                            println!("Restarting playback (from pause)...");
                            // Also unpause
                            break;
                        }
                    }
                }
                
                pause_offset += pause_start.elapsed();
                
                if restart_requested {
                    break;
                }
            }
            
            if restart_requested {
                break;
            }
            
            let frame = match frame {
                Ok(f) => f,
                Err(e) => {
                    eprintln!("Frame error: {}", e);
                    continue;
                }
            };
            
            if frame.timestamp_secs < start_time {
                continue;
            }
            
            let MediaFrame::Video { is_keyframe, .. } = &frame.media;
            if !found_keyframe {
                if !is_keyframe {
                    continue;
                }
                found_keyframe = true;
            }

            let relative_time = frame.timestamp_secs - start_time;
            let target_time = Duration::from_secs_f64(relative_time);
            let elapsed = playback_start.elapsed() - pause_offset;

            if target_time > elapsed {
                tokio::time::sleep(target_time - elapsed).await;
            }

            // Only send if there are viewers
            if viewer_count.load(Ordering::SeqCst) == 0 {
                // No viewers, but continue playback timing
                last_audio_time = frame.timestamp_secs;
                continue;
            }

            // Send audio
            if let Some(samples) = audio_samples {
                let audio_start_sample = (last_audio_time * audio_sample_rate as f64 * audio_channels as f64) as usize;
                let audio_end_sample = (frame.timestamp_secs * audio_sample_rate as f64 * audio_channels as f64) as usize;
                
                let mut pos = audio_start_sample;
                while pos < audio_end_sample && pos < samples.len() {
                    let chunk_end = (pos + audio_chunk_samples).min(audio_end_sample).min(samples.len());
                    let chunk = &samples[pos..chunk_end];
                    
                    if !chunk.is_empty() {
                        let audio_msg = build_audio_chunk(chunk, audio_sample_rate);
                        let _ = frame_tx.send(BroadcastFrame::Audio(audio_msg));
                    }
                    pos = chunk_end;
                }
                last_audio_time = frame.timestamp_secs;
            }

            // Send video frame
            let MediaFrame::Video { data, is_keyframe } = frame.media;
            if is_keyframe {
                println!("[SERVER] Sending keyframe, size={}", data.len());
            }
            let _ = frame_tx.send(BroadcastFrame::Video(data));
        }

        // If restart was requested, loop immediately
        if restart_requested {
            continue;
        }

        if !state.loop_playback {
            println!("Shared playback complete");
            break;
        }

        println!("Looping shared playback...");
    }
}

/// Individual playback loop - one per connection (original behavior)
async fn run_playback(
    tx: mpsc::Sender<Message>,
    state: AppState,
    mut pause_rx: watch::Receiver<bool>,
) -> Result<()> {
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
        let mut pause_offset = Duration::ZERO; // Total time spent paused
        let mut last_audio_time: f64 = start_time;
        let mut found_keyframe = false;
        
        // Create a fresh iterator for each playback loop
        let frames = state.demuxer.frames()?;

        for frame in frames {
            // Check if paused - wait until resumed
            while *pause_rx.borrow() {
                let pause_start = Instant::now();
                // Wait for state change
                if pause_rx.changed().await.is_err() {
                    return Ok(()); // Channel closed
                }
                // Accumulate pause duration
                pause_offset += pause_start.elapsed();
            }
            
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
            // Account for time spent paused
            let relative_time = frame.timestamp_secs - start_time;
            let target_time = Duration::from_secs_f64(relative_time);
            let elapsed = playback_start.elapsed() - pause_offset;

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
