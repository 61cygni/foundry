//! Audio decoder with symphonia + ffmpeg fallback

use anyhow::{anyhow, Result};
use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use symphonia::core::audio::{AudioBufferRef, Signal};
use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

/// Decoded audio data
pub struct DecodedAudio {
    pub samples: Vec<i16>,
    pub sample_rate: u32,
    pub channels: u32,
}

/// Decode all audio from an MP4 file
/// Tries symphonia first, falls back to ffmpeg if that fails
pub fn decode_audio(path: &Path) -> Result<Option<DecodedAudio>> {
    // Try symphonia first (fast, no external dependencies)
    match decode_audio_symphonia(path) {
        Ok(Some(audio)) => return Ok(Some(audio)),
        Ok(None) => return Ok(None),
        Err(e) => {
            eprintln!("Symphonia decode failed: {}", e);
            eprintln!("Trying ffmpeg fallback...");
        }
    }

    // Fall back to ffmpeg
    match decode_audio_ffmpeg(path) {
        Ok(Some(audio)) => {
            println!("Audio decoded via ffmpeg");
            Ok(Some(audio))
        }
        Ok(None) => Ok(None),
        Err(e) => Err(anyhow!("Both decoders failed. ffmpeg error: {}", e)),
    }
}

/// Decode audio using symphonia (built-in, supports AAC-LC)
fn decode_audio_symphonia(path: &Path) -> Result<Option<DecodedAudio>> {
    let file = File::open(path)?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());

    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }

    let format_opts = FormatOptions::default();
    let metadata_opts = MetadataOptions::default();

    let probed = symphonia::default::get_probe()
        .format(&hint, mss, &format_opts, &metadata_opts)
        .map_err(|e| anyhow!("Failed to probe audio: {}", e))?;

    let mut format = probed.format;

    // Find audio track
    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or_else(|| anyhow!("No audio track found"))?;

    let track_id = track.id;
    let sample_rate = track.codec_params.sample_rate.unwrap_or(48000);
    let channels = track.codec_params.channels.map(|c| c.count() as u32).unwrap_or(2);

    let decoder_opts = DecoderOptions::default();
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &decoder_opts)
        .map_err(|e| anyhow!("Failed to create decoder: {}", e))?;

    let mut all_samples: Vec<i16> = Vec::new();

    // Decode all packets
    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(symphonia::core::errors::Error::IoError(e))
                if e.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break;
            }
            Err(e) => {
                eprintln!("Audio decode warning: {}", e);
                break;
            }
        };

        // Skip packets from other tracks
        if packet.track_id() != track_id {
            continue;
        }

        match decoder.decode(&packet) {
            Ok(decoded) => {
                let samples = convert_to_i16(&decoded, channels);
                all_samples.extend(samples);
            }
            Err(e) => {
                eprintln!("Audio decode warning: {}", e);
            }
        }
    }

    if all_samples.is_empty() {
        return Ok(None);
    }

    Ok(Some(DecodedAudio {
        samples: all_samples,
        sample_rate,
        channels,
    }))
}

/// Decode audio using ffmpeg (external, supports all formats)
fn decode_audio_ffmpeg(path: &Path) -> Result<Option<DecodedAudio>> {
    // Check if ffmpeg is available
    if Command::new("ffmpeg").arg("-version").output().is_err() {
        return Err(anyhow!("ffmpeg not found. Install with: brew install ffmpeg"));
    }

    let path_str = path.to_string_lossy();

    // First, probe the file to get audio info
    let probe = Command::new("ffprobe")
        .args([
            "-v", "quiet",
            "-select_streams", "a:0",
            "-show_entries", "stream=sample_rate,channels",
            "-of", "csv=p=0",
            &path_str,
        ])
        .output()?;

    if !probe.status.success() {
        return Err(anyhow!("ffprobe failed"));
    }

    let probe_output = String::from_utf8_lossy(&probe.stdout);
    let parts: Vec<&str> = probe_output.trim().split(',').collect();
    
    if parts.len() < 2 {
        return Ok(None); // No audio stream
    }

    let sample_rate: u32 = parts[0].parse().unwrap_or(48000);
    let channels: u32 = parts[1].parse().unwrap_or(2);

    // Decode audio to raw PCM (signed 16-bit little-endian)
    let mut child = Command::new("ffmpeg")
        .args([
            "-i", &path_str,
            "-vn",                      // No video
            "-acodec", "pcm_s16le",     // Output format: signed 16-bit LE
            "-ar", &sample_rate.to_string(),
            "-ac", &channels.to_string(),
            "-f", "s16le",              // Raw PCM output
            "-",                        // Output to stdout
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;

    let mut stdout = child.stdout.take().ok_or_else(|| anyhow!("No stdout"))?;
    
    // Read all PCM data
    let mut pcm_data = Vec::new();
    stdout.read_to_end(&mut pcm_data)?;

    let status = child.wait()?;
    if !status.success() {
        return Err(anyhow!("ffmpeg decoding failed"));
    }

    if pcm_data.is_empty() {
        return Ok(None);
    }

    // Convert bytes to i16 samples (little-endian)
    let samples: Vec<i16> = pcm_data
        .chunks_exact(2)
        .map(|chunk| i16::from_le_bytes([chunk[0], chunk[1]]))
        .collect();

    Ok(Some(DecodedAudio {
        samples,
        sample_rate,
        channels,
    }))
}

/// Convert audio buffer to interleaved i16 samples
fn convert_to_i16(buffer: &AudioBufferRef, target_channels: u32) -> Vec<i16> {
    match buffer {
        AudioBufferRef::F32(buf) => {
            let frames = buf.frames();
            let channels = buf.spec().channels.count();
            let mut samples = Vec::with_capacity(frames * target_channels as usize);

            for frame in 0..frames {
                for ch in 0..target_channels as usize {
                    let sample = if ch < channels {
                        buf.chan(ch)[frame]
                    } else {
                        buf.chan(channels - 1)[frame]
                    };
                    let clamped = sample.clamp(-1.0, 1.0);
                    samples.push((clamped * 32767.0) as i16);
                }
            }
            samples
        }
        AudioBufferRef::S16(buf) => {
            let frames = buf.frames();
            let channels = buf.spec().channels.count();
            let mut samples = Vec::with_capacity(frames * target_channels as usize);

            for frame in 0..frames {
                for ch in 0..target_channels as usize {
                    let sample = if ch < channels {
                        buf.chan(ch)[frame]
                    } else {
                        buf.chan(channels - 1)[frame]
                    };
                    samples.push(sample);
                }
            }
            samples
        }
        AudioBufferRef::S32(buf) => {
            let frames = buf.frames();
            let channels = buf.spec().channels.count();
            let mut samples = Vec::with_capacity(frames * target_channels as usize);

            for frame in 0..frames {
                for ch in 0..target_channels as usize {
                    let sample = if ch < channels {
                        buf.chan(ch)[frame]
                    } else {
                        buf.chan(channels - 1)[frame]
                    };
                    samples.push((sample >> 16) as i16);
                }
            }
            samples
        }
        _ => {
            eprintln!("Unsupported audio format");
            Vec::new()
        }
    }
}
