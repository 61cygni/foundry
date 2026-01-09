//! AAC audio decoder using symphonia

use anyhow::{anyhow, Result};
use std::fs::File;
use std::path::Path;
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
pub fn decode_audio(path: &Path) -> Result<Option<DecodedAudio>> {
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
                // Convert to i16 samples
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
                        // Duplicate last channel if fewer channels than target
                        buf.chan(channels - 1)[frame]
                    };
                    // Convert f32 [-1.0, 1.0] to i16
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
                    // Convert i32 to i16 (shift down)
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
