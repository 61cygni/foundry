//! MP4 demuxer: extracts H.264 video and decodes AAC audio to PCM

use anyhow::{anyhow, Result};
use base64::Engine;
use mp4::{Mp4Reader, TrackType};
use std::{
    fs::File,
    io::BufReader,
    path::Path,
};

/// Video configuration for WebCodecs
pub struct VideoConfig {
    pub codec_string: String,
    pub description_b64: String,
    pub width: u32,
    pub height: u32,
}

/// A frame of media (video or audio)
pub struct TimestampedFrame {
    pub timestamp_secs: f64,
    pub media: MediaFrame,
}

pub enum MediaFrame {
    Video { data: Vec<u8>, is_keyframe: bool },
}

/// MP4 demuxer with H.264 passthrough
pub struct Mp4Demuxer {
    path: std::path::PathBuf,
    video_track_id: u32,
    has_audio: bool,
    video_width: u32,
    video_height: u32,
    frame_rate: f64,
    frame_count: u32,
    avcc_data: Vec<u8>,
    /// SPS/PPS NALs in AVCC format (4-byte length prefix) for prepending to keyframes
    sps_pps_avcc: Vec<u8>,
}

impl Mp4Demuxer {
    pub fn open(path: &Path) -> Result<Self> {
        let file = File::open(path)?;
        let size = file.metadata()?.len();
        let reader = BufReader::new(file);
        let mp4 = Mp4Reader::read_header(reader, size)?;

        // Find video track
        let video_track = mp4
            .tracks()
            .values()
            .find(|t| matches!(t.track_type(), Ok(TrackType::Video)))
            .ok_or_else(|| anyhow!("No video track found"))?;

        let video_track_id = video_track.track_id();
        let video_width = video_track.width() as u32;
        let video_height = video_track.height() as u32;
        let frame_count = video_track.sample_count();

        // Calculate frame rate from duration and sample count
        let duration_secs = video_track.duration().as_secs_f64();
        let frame_rate = if duration_secs > 0.0 {
            frame_count as f64 / duration_secs
        } else {
            30.0 // fallback
        };

        // Get AVCC data (SPS/PPS) from video track
        let (avcc_data, sps_pps_avcc) = extract_avcc(&video_track)?;

        // Check for audio track
        let has_audio = mp4
            .tracks()
            .values()
            .any(|t| matches!(t.track_type(), Ok(TrackType::Audio)));

        Ok(Self {
            path: path.to_path_buf(),
            video_track_id,
            has_audio,
            video_width,
            video_height,
            frame_rate,
            frame_count,
            avcc_data,
            sps_pps_avcc,
        })
    }

    pub fn video_width(&self) -> u32 {
        self.video_width
    }

    pub fn video_height(&self) -> u32 {
        self.video_height
    }

    pub fn frame_rate(&self) -> f64 {
        self.frame_rate
    }

    pub fn frame_count(&self) -> u32 {
        self.frame_count
    }

    pub fn has_audio(&self) -> bool {
        self.has_audio
    }

    pub fn video_config(&self) -> Result<VideoConfig> {
        // Build codec string from AVCC
        let codec_string = if self.avcc_data.len() >= 4 {
            format!(
                "avc1.{:02X}{:02X}{:02X}",
                self.avcc_data[1], // profile
                self.avcc_data[2], // constraints
                self.avcc_data[3], // level
            )
        } else {
            "avc1.42E01E".to_string() // fallback baseline
        };

        let description_b64 = base64::engine::general_purpose::STANDARD.encode(&self.avcc_data);

        Ok(VideoConfig {
            codec_string,
            description_b64,
            width: self.video_width,
            height: self.video_height,
        })
    }

    /// Returns an iterator over video frames in the file
    pub fn frames(&self) -> Result<FrameIterator> {
        let file = File::open(&self.path)?;
        let size = file.metadata()?.len();
        let reader = BufReader::new(file);
        let mp4 = Mp4Reader::read_header(reader, size)?;

        Ok(FrameIterator {
            mp4,
            video_track_id: self.video_track_id,
            video_sample_idx: 1,
            frame_rate: self.frame_rate,
            sps_pps_avcc: self.sps_pps_avcc.clone(),
        })
    }
}

pub struct FrameIterator {
    mp4: Mp4Reader<BufReader<File>>,
    video_track_id: u32,
    video_sample_idx: u32,
    frame_rate: f64,
    /// SPS/PPS NALs to prepend to keyframes
    sps_pps_avcc: Vec<u8>,
}

impl Iterator for FrameIterator {
    type Item = Result<TimestampedFrame>;

    fn next(&mut self) -> Option<Self::Item> {
        let video_track = self.mp4.tracks().get(&self.video_track_id)?;
        let video_count = video_track.sample_count();

        if self.video_sample_idx > video_count {
            return None;
        }

        // Read video sample
        match self.mp4.read_sample(self.video_track_id, self.video_sample_idx) {
            Ok(Some(sample)) => {
                // Calculate timestamp from sample index and frame rate
                // (sample indices are 1-based in mp4 crate)
                let timestamp_secs = (self.video_sample_idx - 1) as f64 / self.frame_rate;
                let is_keyframe = sample.is_sync;
                self.video_sample_idx += 1;
                
                // The sample bytes are already in AVCC format (4-byte length prefix)
                // For keyframes, prepend SPS/PPS so decoder can recognize them
                let data = if is_keyframe && !self.sps_pps_avcc.is_empty() {
                    let mut full_data = self.sps_pps_avcc.clone();
                    full_data.extend_from_slice(&sample.bytes);
                    full_data
                } else {
                    sample.bytes.to_vec()
                };
                
                Some(Ok(TimestampedFrame {
                    timestamp_secs,
                    media: MediaFrame::Video { data, is_keyframe },
                }))
            }
            Ok(None) => {
                self.video_sample_idx += 1;
                self.next()
            }
            Err(e) => Some(Err(anyhow!("Failed to read video sample: {}", e))),
        }
    }
}

/// Extract AVCC configuration from video track
/// Returns (avcc_config, sps_pps_avcc) where sps_pps_avcc has 4-byte length prefixes
fn extract_avcc(track: &mp4::Mp4Track) -> Result<(Vec<u8>, Vec<u8>)> {
    // Get the AVCC box data
    if let Some(avc1) = &track.trak.mdia.minf.stbl.stsd.avc1 {
        let avcc = &avc1.avcc;
        
        // Build AVCC configuration record (for WebCodecs config)
        let mut config = Vec::new();
        config.push(avcc.configuration_version);
        config.push(avcc.avc_profile_indication);
        config.push(avcc.profile_compatibility);
        config.push(avcc.avc_level_indication);
        config.push(0xFF); // 4-byte NALU length (0xFF = 0b11111111, lower 2 bits = length - 1)
        
        // SPS for config
        config.push(0xE0 | (avcc.sequence_parameter_sets.len() as u8));
        for sps in &avcc.sequence_parameter_sets {
            config.extend_from_slice(&(sps.bytes.len() as u16).to_be_bytes());
            config.extend_from_slice(&sps.bytes);
        }
        
        // PPS for config
        config.push(avcc.picture_parameter_sets.len() as u8);
        for pps in &avcc.picture_parameter_sets {
            config.extend_from_slice(&(pps.bytes.len() as u16).to_be_bytes());
            config.extend_from_slice(&pps.bytes);
        }
        
        // Build SPS/PPS with 4-byte length prefix (for prepending to keyframes)
        let mut sps_pps = Vec::new();
        for sps in &avcc.sequence_parameter_sets {
            let len = sps.bytes.len() as u32;
            sps_pps.extend_from_slice(&len.to_be_bytes());
            sps_pps.extend_from_slice(&sps.bytes);
        }
        for pps in &avcc.picture_parameter_sets {
            let len = pps.bytes.len() as u32;
            sps_pps.extend_from_slice(&len.to_be_bytes());
            sps_pps.extend_from_slice(&pps.bytes);
        }
        
        Ok((config, sps_pps))
    } else {
        Err(anyhow!("No AVC configuration found in video track"))
    }
}

