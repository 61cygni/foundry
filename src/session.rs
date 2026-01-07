use std::sync::Arc;

use axum::{body::Bytes, extract::ws::{Message, Utf8Bytes, WebSocket}};
use futures_util::{stream::SplitStream, StreamExt};
use serde::Deserialize;
use serde_json::Value;
use tokio::sync::mpsc;
use xcap::Frame;

use crate::{
    AppState,
    audio_mixer::{MixerInput, MixedChunk},
    audio_capture::AudioChunk,
    video_pipeline::{VideoCodec, VideoPipeline},
};

// Keep resolution manageable for software encoding (~1080p equivalent)
const MAX_PIXELS: usize = 1_920 * 1_080;

#[derive(Debug, Deserialize)]
struct ModeRequest {
    #[serde(rename = "type")]
    msg_type: String,
    codec: Option<String>,
}

#[derive(Debug, Clone)]
struct DownsampledFrame {
    frame: Arc<Frame>,
    #[allow(dead_code)]
    scale: u32,
}

#[derive(Default)]
struct Downsampler {
    buffer: Vec<u8>,
}

impl Downsampler {
    fn new() -> Self {
        Self { buffer: Vec::new() }
    }

    fn downsample(&mut self, frame: Arc<Frame>) -> DownsampledFrame {
        let src_w = frame.width as usize;
        let src_h = frame.height as usize;
        let pixels = src_w.saturating_mul(src_h);

        // Choose integer scale >=1 such that the downsampled pixel count fits the target.
        let mut scale: usize = 1;
        if pixels > MAX_PIXELS {
            let ratio = (pixels + MAX_PIXELS - 1) / MAX_PIXELS; // ceil division
            let approx = (ratio as f64).sqrt().ceil() as usize;
            scale = approx.max(2);
            while scale < 16
                && (src_w / scale).saturating_mul(src_h / scale) > MAX_PIXELS
            {
                scale += 1;
            }
        }

        if scale <= 1 {
            return DownsampledFrame { frame, scale: 1 };
        }

        let dst_w = src_w / scale;
        let dst_h = src_h / scale;
        if dst_w == 0 || dst_h == 0 {
            return DownsampledFrame { frame, scale: 1 };
        }

        let needed = dst_w * dst_h * 4;
        if self.buffer.len() < needed {
            self.buffer.resize(needed, 0);
        }

        let src = &frame.raw;
        let dst = &mut self.buffer[..needed];
        let block = scale as usize;
        let block_area = (block * block) as u32;

        for y in 0..dst_h {
            let sy0 = y * block;
            for x in 0..dst_w {
                let sx0 = x * block;
                let mut acc = [0u32; 4];
                for ky in 0..block {
                    let row_base = (sy0 + ky) * src_w * 4;
                    let start = row_base + sx0 * 4;
                    for kx in 0..block {
                        let idx = start + kx * 4;
                        acc[0] += src[idx] as u32;
                        acc[1] += src[idx + 1] as u32;
                        acc[2] += src[idx + 2] as u32;
                        acc[3] += src[idx + 3] as u32;
                    }
                }
                let out_idx = (y * dst_w + x) * 4;
                dst[out_idx] = (acc[0] / block_area) as u8;
                dst[out_idx + 1] = (acc[1] / block_area) as u8;
                dst[out_idx + 2] = (acc[2] / block_area) as u8;
                dst[out_idx + 3] = (acc[3] / block_area) as u8;
            }
        }

        let down_frame = Frame {
            width: dst_w as u32,
            height: dst_h as u32,
            raw: dst[..needed].to_vec(),
        };

        DownsampledFrame {
            frame: Arc::new(down_frame),
            scale: scale as u32,
        }
    }
}

fn is_audio_magic(buf: &[u8]) -> bool {
    buf.len() >= 4 && &buf[..4] == b"AUD0"
}

fn parse_audio_chunk(buf: &[u8]) -> Option<MixerInput> {
    if !is_audio_magic(buf) || buf.len() < 24 {
        return None;
    }
    let mut offset = 4;
    let start_ms = f64::from_le_bytes(buf[offset..offset + 8].try_into().ok()?);
    offset += 8;
    let sample_rate = u32::from_le_bytes(buf[offset..offset + 4].try_into().ok()?);
    offset += 4;
    let channels = u32::from_le_bytes(buf[offset..offset + 4].try_into().ok()?);
    offset += 4;
    let sample_count = u32::from_le_bytes(buf[offset..offset + 4].try_into().ok()?);
    offset += 4;
    let needed = offset + (sample_count as usize) * 2;
    if buf.len() < needed {
        return None;
    }
    let mut samples = Vec::with_capacity(sample_count as usize);
    for chunk in buf[offset..needed].chunks_exact(2) {
        let s = i16::from_le_bytes([chunk[0], chunk[1]]);
        samples.push(s);
    }
    Some(MixerInput {
        start_ms,
        sample_rate,
        channels,
        samples,
    })
}

fn build_audio_chunk(chunk: &MixedChunk) -> Bytes {
    let sample_count = chunk.samples.len() as u32;
    let mut out = Vec::with_capacity(24 + chunk.samples.len() * 2);
    out.extend_from_slice(b"AUD0");
    out.extend_from_slice(&chunk.start_ms.to_le_bytes());
    out.extend_from_slice(&chunk.sample_rate.to_le_bytes());
    out.extend_from_slice(&chunk.channels.to_le_bytes());
    out.extend_from_slice(&sample_count.to_le_bytes());
    for s in &chunk.samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    Bytes::from(out)
}

fn build_direct_audio_chunk(chunk: &AudioChunk) -> Bytes {
    let sample_count = chunk.samples.len() as u32;
    let mut out = Vec::with_capacity(24 + chunk.samples.len() * 2);
    out.extend_from_slice(b"AUD0");
    out.extend_from_slice(&0.0f64.to_le_bytes()); // start_ms not used for direct
    out.extend_from_slice(&chunk.sample_rate.to_le_bytes());
    out.extend_from_slice(&chunk.channels.to_le_bytes());
    out.extend_from_slice(&sample_count.to_le_bytes());
    for s in &chunk.samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    Bytes::from(out)
}

pub async fn start(
    mut receiver: SplitStream<WebSocket>,
    tx: mpsc::Sender<Message>,
    state: AppState,
) {
    println!("session started");

    let codec = negotiate_mode(&mut receiver, &tx).await;

    match VideoPipeline::new(codec) {
        Ok(pipeline) => {
            if let Err(err) = run_video(receiver, tx, state, codec, pipeline).await {
                eprintln!("video pipeline error: {err}");
            }
        }
        Err(err) => {
            eprintln!("video pipeline not available: {err}");
            let _ = tx.send(Message::Text(Utf8Bytes::from("{\"type\":\"mode-ack\",\"mode\":\"video\",\"reason\":\"video-unavailable\"}"))).await;
        }
    }
}

async fn negotiate_mode(
    receiver: &mut SplitStream<WebSocket>,
    tx: &mpsc::Sender<Message>,
) -> VideoCodec {
    use tokio::time::{timeout, Duration};

    if let Ok(Some(Ok(Message::Text(text)))) =
        timeout(Duration::from_millis(500), receiver.next()).await
    {
        if let Ok(req) = serde_json::from_str::<ModeRequest>(&text) {
            if req.msg_type == "mode" {
                let codec = match req.codec.as_deref() {
                    Some("hevc") => VideoCodec::Hevc,
                    _ => VideoCodec::Avc,
                };
                let _ = tx
                    .send(Message::Text(Utf8Bytes::from(format!(
                        "{{\"type\":\"mode-ack\",\"mode\":\"video\",\"codec\":\"{}\"}}",
                        match codec {
                            VideoCodec::Avc => "avc",
                            VideoCodec::Hevc => "hevc",
                        }
                    ))))
                    .await;
                return codec;
            }
        }
    }

    // Default to AVC if no mode message received quickly.
    let _ = tx
        .send(Message::Text(Utf8Bytes::from(
            "{\"type\":\"mode-ack\",\"mode\":\"video\",\"codec\":\"avc\"}",
        )))
        .await;
    VideoCodec::Avc
}

async fn run_video(
    mut receiver: SplitStream<WebSocket>,
    tx: mpsc::Sender<Message>,
    state: AppState,
    _codec: VideoCodec,
    mut pipeline: VideoPipeline,
) -> anyhow::Result<()> {
    let mut listen_frames = state.recorder.new_listener();
    let mut pending_config_sent = false;
    let mut force_idr_next = false;
    let mut downsampler = Downsampler::new();
    
    // Use direct audio capture if available, otherwise fall back to mixer
    let mut direct_audio_rx = state.audio_broadcast.as_ref().map(|c| c.subscribe());
    let mut mixer_audio_rx = if direct_audio_rx.is_none() { Some(state.mixer.subscribe()) } else { None };
    let audio_tx = state.mixer.input_sender();

    println!("video pipeline started (audio: {})", 
        if direct_audio_rx.is_some() { "direct capture" } else { "mixer" });

    loop {
        tokio::select! {
            ws_msg = receiver.next() => {
                match ws_msg {
                    Some(Ok(msg)) => match msg {
                        Message::Text(text) => {
                            if let Ok(val) = serde_json::from_str::<Value>(&text) {
                                if let Some(msg_type) = val.get("type").and_then(|v| v.as_str()) {
                                    match msg_type {
                                        "force-keyframe" => {
                                            force_idr_next = true;
                                        }
                                        _ => {}
                                    }
                                }
                            }
                        }
                        Message::Binary(data) => {
                            if let Some(input) = parse_audio_chunk(&data) {
                                if let Err(err) = audio_tx.send(input).await {
                                    eprintln!("failed to forward audio chunk: {err}");
                                }
                            }
                        }
                        Message::Ping(payload) => {
                            if tx.send(Message::Pong(payload)).await.is_err() {
                                break;
                            }
                        }
                        Message::Close(frame) => {
                            let _ = tx.send(Message::Close(frame)).await;
                            break;
                        }
                        _ => {}
                    },
                    Some(Err(err)) => {
                        eprintln!("websocket error: {err}");
                        break;
                    }
                    None => break,
                }
            }
            // Direct audio capture (low latency, stereo)
            Some(Ok(chunk)) = async { 
                match &mut direct_audio_rx {
                    Some(rx) => Some(rx.recv().await),
                    None => None,
                }
            } => {
                if tx.send(Message::Binary(build_direct_audio_chunk(&chunk))).await.is_err() {
                    break;
                }
            }
            // Mixer audio (fallback, higher latency)
            Some(Ok(chunk)) = async {
                match &mut mixer_audio_rx {
                    Some(rx) => Some(rx.recv().await),
                    None => None,
                }
            } => {
                if tx.send(Message::Binary(build_audio_chunk(&chunk))).await.is_err() {
                    break;
                }
            }
            frame = listen_frames.recv() => {
                match frame {
                    Some(frame) => {
                        let DownsampledFrame { frame, scale: _ } = downsampler.downsample(frame);
                        // if scale > 1 {
                        //     println!("downsampled frame by {scale}x -> {}x{}", frame.width, frame.height);
                        // }
                        let force = force_idr_next;
                        force_idr_next = false;
                        let maybe_chunk = pipeline.encode(frame, force)?;
                        if let Some(chunk) = maybe_chunk {
                            // println!("sending encoded video chunk: {} bytes", chunk.data.len());

                            if !pending_config_sent {
                                let config = pipeline.config();
                                println!("video config: {:?}", config);
                                if !config.description_b64.is_empty() && config.width > 0 && config.height > 0 {
                                    let config_json = serde_json::json!({
                                        "type": "video-config",
                                        "config": {
                                            "codec": match config.codec {
                                                VideoCodec::Avc => "avc1.42E01E",
                                                VideoCodec::Hevc => "hev1.1.6.L93.B0",
                                            },
                                            "description": config.description_b64,
                                            "width": config.width,
                                            "height": config.height,
                                        }
                                    });
                                    println!("sending video config: {}", config_json.to_string());
                                    let _ = tx.send(Message::Text(Utf8Bytes::from(config_json.to_string()))).await;
                                    pending_config_sent = true;
                                }
                            }

                            if !pending_config_sent {
                                // Wait until config is available.
                                continue;
                            }

                            if tx.send(Message::Binary(Bytes::from(chunk.data.clone()))).await.is_err() {
                                break;
                            }
                        }
                    }
                    None => break,
                }
            }
        }
    }

    println!("video pipeline ended");
    Ok(())
}

