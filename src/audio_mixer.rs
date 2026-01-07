use std::collections::HashMap;
use std::time::Instant;

use tokio::sync::{broadcast, mpsc};

const CHUNK_MS: u64 = 100;
const MAX_BUCKET_AGE_MS: u64 = 2_000;

#[derive(Debug)]
pub struct MixerInput {
    pub start_ms: f64,
    pub sample_rate: u32,
    pub channels: u32,
    pub samples: Vec<i16>,
}

#[derive(Debug, Clone)]
pub struct MixedChunk {
    pub start_ms: f64,
    pub sample_rate: u32,
    pub channels: u32,
    pub samples: Vec<i16>,
}

struct MixBucket {
    start_ms: f64,
    sample_rate: u32,
    channels: u32,
    sum: Vec<i32>,
    max_len: usize,
    last_update: Instant,
}

pub struct AudioMixer {
    tx: mpsc::Sender<MixerInput>,
    bcast: broadcast::Sender<MixedChunk>,
}

impl AudioMixer {
    pub fn new() -> Self {
        let (tx, mut rx) = mpsc::channel::<MixerInput>(256);
        let (bcast, _rx) = broadcast::channel::<MixedChunk>(128);

        let bcast_tx = bcast.clone();
        tokio::spawn(async move {
            let mut buckets: HashMap<u64, MixBucket> = HashMap::new();
            let mut last_prune = Instant::now();
            while let Some(input) = rx.recv().await {
                if input.channels != 1 {
                    // Only mix mono inputs.
                    continue;
                }
                let key = (input.start_ms / CHUNK_MS as f64).floor() as u64;
                let bucket_start = key as f64 * CHUNK_MS as f64;
                let bucket = buckets.entry(key).or_insert_with(|| MixBucket {
                    start_ms: bucket_start,
                    sample_rate: input.sample_rate,
                    channels: input.channels,
                    sum: Vec::new(),
                    max_len: 0,
                    last_update: Instant::now(),
                });

                if bucket.sample_rate != input.sample_rate || bucket.channels != input.channels {
                    // Skip mismatched sample rate/channel contributions.
                    continue;
                }

                if bucket.sum.len() < input.samples.len() {
                    bucket.sum.resize(input.samples.len(), 0);
                }
                if bucket.max_len < input.samples.len() {
                    bucket.max_len = input.samples.len();
                }

                for (idx, sample) in input.samples.iter().enumerate() {
                    bucket.sum[idx] = bucket.sum[idx].saturating_add(*sample as i32);
                }
                bucket.last_update = Instant::now();

                // Emit the current mixed chunk.
                let mut mixed = Vec::with_capacity(bucket.max_len);
                for v in bucket.sum.iter().take(bucket.max_len) {
                    let val = *v;
                    let clamped = val
                        .max(i16::MIN as i32)
                        .min(i16::MAX as i32) as i16;
                    mixed.push(clamped);
                }
                let _ = bcast_tx.send(MixedChunk {
                    start_ms: bucket.start_ms,
                    sample_rate: bucket.sample_rate,
                    channels: bucket.channels,
                    samples: mixed,
                });

                // Prune old buckets occasionally.
                if last_prune.elapsed().as_millis() as u64 > CHUNK_MS {
                    let now = Instant::now();
                    buckets.retain(|_, b| now.duration_since(b.last_update).as_millis() as u64 <= MAX_BUCKET_AGE_MS);
                    last_prune = now;
                }
            }
        });

        Self { tx, bcast }
    }

    pub fn input_sender(&self) -> mpsc::Sender<MixerInput> {
        self.tx.clone()
    }

    pub fn subscribe(&self) -> broadcast::Receiver<MixedChunk> {
        self.bcast.subscribe()
    }
}

