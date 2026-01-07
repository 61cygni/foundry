use std::time::Instant;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use tokio::sync::mpsc;

use crate::audio_mixer::MixerInput;

pub struct AudioCapture {
    _stream: cpal::Stream,
}

impl AudioCapture {
    /// Create audio capture from the default input device.
    /// For system audio on macOS, use BlackHole as input device.
    pub fn new(audio_tx: mpsc::Sender<MixerInput>) -> anyhow::Result<Self> {
        let host = cpal::default_host();
        
        // Try to find BlackHole device first for system audio capture
        let device = host
            .input_devices()?
            .find(|d| {
                d.name()
                    .map(|n| n.to_lowercase().contains("blackhole"))
                    .unwrap_or(false)
            })
            .or_else(|| {
                println!("[Audio] BlackHole not found, using default input device");
                println!("[Audio] For system audio capture, install: brew install blackhole-2ch");
                host.default_input_device()
            })
            .ok_or_else(|| anyhow::anyhow!("No audio input device found"))?;

        let device_name = device.name().unwrap_or_else(|_| "Unknown".to_string());
        println!("[Audio] Using input device: {}", device_name);

        let config = device.default_input_config()?;
        println!("[Audio] Sample rate: {}, Channels: {}", 
            config.sample_rate().0, config.channels());

        let sample_rate = config.sample_rate().0;
        let channels = config.channels() as u32;
        let start_time = Instant::now();

        // Build the appropriate stream based on sample format
        let stream = match config.sample_format() {
            cpal::SampleFormat::F32 => build_stream::<f32>(
                &device, 
                &config.into(), 
                audio_tx, 
                sample_rate, 
                channels,
                start_time,
            )?,
            cpal::SampleFormat::I16 => build_stream::<i16>(
                &device, 
                &config.into(), 
                audio_tx, 
                sample_rate, 
                channels,
                start_time,
            )?,
            cpal::SampleFormat::U16 => build_stream::<u16>(
                &device, 
                &config.into(), 
                audio_tx, 
                sample_rate, 
                channels,
                start_time,
            )?,
            _ => return Err(anyhow::anyhow!("Unsupported sample format")),
        };

        stream.play()?;
        println!("[Audio] Capture started");

        Ok(Self { _stream: stream })
    }
}

fn build_stream<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    audio_tx: mpsc::Sender<MixerInput>,
    sample_rate: u32,
    channels: u32,
    start_time: Instant,
) -> anyhow::Result<cpal::Stream>
where
    T: cpal::Sample<Float = f32> + cpal::SizedSample + Send + 'static,
{
    let err_fn = |err| eprintln!("[Audio] Stream error: {}", err);

    let stream = device.build_input_stream(
        config,
        move |data: &[T], _: &cpal::InputCallbackInfo| {
            let start_ms = start_time.elapsed().as_secs_f64() * 1000.0;
            
            // Convert to mono i16 samples
            let samples: Vec<i16> = if channels == 1 {
                data.iter()
                    .map(|s| sample_to_i16(*s))
                    .collect()
            } else {
                // Mix down to mono by averaging channels
                data.chunks(channels as usize)
                    .map(|chunk| {
                        let sum: i32 = chunk.iter().map(|s| sample_to_i16(*s) as i32).sum();
                        (sum / channels as i32) as i16
                    })
                    .collect()
            };

            if samples.is_empty() {
                return;
            }

            let input = MixerInput {
                start_ms,
                sample_rate,
                channels: 1, // We mix to mono
                samples,
            };

            // Non-blocking send - drop if buffer full
            let _ = audio_tx.try_send(input);
        },
        err_fn,
        None,
    )?;

    Ok(stream)
}

fn sample_to_i16<T: cpal::Sample<Float = f32>>(sample: T) -> i16 {
    let float_sample: f32 = sample.to_float_sample();
    // Convert f32 [-1.0, 1.0] to i16 [-32768, 32767]
    (float_sample * 32767.0).clamp(-32768.0, 32767.0) as i16
}

