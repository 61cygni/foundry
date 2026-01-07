use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use tokio::sync::broadcast;

/// Raw audio chunk for direct streaming (bypasses mixer for low latency)
#[derive(Debug, Clone)]
pub struct AudioChunk {
    pub sample_rate: u32,
    pub channels: u32,
    pub samples: Vec<i16>,
}

/// Handle to subscribe to audio - this is Send+Sync safe
#[derive(Clone)]
pub struct AudioBroadcast {
    sender: broadcast::Sender<AudioChunk>,
}

impl AudioBroadcast {
    pub fn subscribe(&self) -> broadcast::Receiver<AudioChunk> {
        self.sender.subscribe()
    }
}

/// Audio capture (not Send/Sync - keep on main thread)
pub struct AudioCapture {
    _stream: cpal::Stream,
}

/// Start audio capture and return a broadcast handle that can be shared across threads.
/// The AudioCapture must be kept alive (not dropped) for capture to continue.
pub fn start_audio_capture() -> anyhow::Result<(AudioCapture, AudioBroadcast)> {
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
    
    // Broadcast channel for sending to all connected clients
    let (sender, _) = broadcast::channel::<AudioChunk>(64);
    let sender_clone = sender.clone();

    // Build the appropriate stream based on sample format
    let stream = match config.sample_format() {
        cpal::SampleFormat::F32 => build_stream::<f32>(
            &device, 
            &config.into(), 
            sender_clone, 
            sample_rate, 
            channels,
        )?,
        cpal::SampleFormat::I16 => build_stream::<i16>(
            &device, 
            &config.into(), 
            sender_clone, 
            sample_rate, 
            channels,
        )?,
        cpal::SampleFormat::U16 => build_stream::<u16>(
            &device, 
            &config.into(), 
            sender_clone, 
            sample_rate, 
            channels,
        )?,
        _ => return Err(anyhow::anyhow!("Unsupported sample format")),
    };

    stream.play()?;
    println!("[Audio] Capture started (low-latency direct mode)");

    let capture = AudioCapture { _stream: stream };
    let broadcast = AudioBroadcast { sender };
    
    Ok((capture, broadcast))
}

fn build_stream<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    sender: broadcast::Sender<AudioChunk>,
    sample_rate: u32,
    channels: u32,
) -> anyhow::Result<cpal::Stream>
where
    T: cpal::Sample<Float = f32> + cpal::SizedSample + Send + 'static,
{
    let err_fn = |err| eprintln!("[Audio] Stream error: {}", err);

    let stream = device.build_input_stream(
        config,
        move |data: &[T], _: &cpal::InputCallbackInfo| {
            // Keep stereo, convert to i16
            let samples: Vec<i16> = data.iter()
                .map(|s| sample_to_i16(*s))
                .collect();

            if samples.is_empty() {
                return;
            }

            let chunk = AudioChunk {
                sample_rate,
                channels,
                samples,
            };

            // Non-blocking send - if no receivers or buffer full, drop
            let _ = sender.send(chunk);
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

