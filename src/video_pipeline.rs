use std::sync::Arc;

use anyhow::{anyhow, Result};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use openh264::encoder::EncodedBitStream;
use openh264_sys2::SFrameBSInfo;
use xcap::Frame;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoCodec {
    Avc,
    Hevc,
}

#[derive(Debug)]
pub struct VideoConfig {
    pub codec: VideoCodec,
    pub width: u32,
    pub height: u32,
    pub description_b64: String,
}

#[derive(Debug)]
pub struct EncodedChunk {
    pub data: Vec<u8>,
}

pub struct VideoPipeline {
    inner: EncoderImpl,
}

impl VideoPipeline {
    pub fn new(codec: VideoCodec) -> Result<Self> {
        let inner = EncoderImpl::new(codec)?;
        Ok(Self { inner })
    }

    pub fn config(&self) -> VideoConfig {
        self.inner.config()
    }

    pub fn encode(&mut self, frame: Arc<Frame>, force_idr: bool) -> Result<Option<EncodedChunk>> {
        self.inner.encode(frame, force_idr)
    }
}

#[cfg(feature = "openh264-encoder")]
struct EncoderImpl {
    encoder: openh264::encoder::Encoder,
    width: u32,
    height: u32,
    codec: VideoCodec,
    config_b64: String,
    pending_idr: bool,
}

#[cfg(feature = "openh264-encoder")]
impl EncoderImpl {
    fn new(codec: VideoCodec) -> Result<Self> {
        if codec == VideoCodec::Hevc {
            return Err(anyhow!("HEVC not available in openh264 encoder; choose avc"));
        }
        let width = 0;
        let height = 0;
        // placeholder encoder; will be re-created once we know dimensions.
        let cfg = openh264::encoder::EncoderConfig::new(2, 2);
        let encoder = openh264::encoder::Encoder::with_config(cfg)?;

        Ok(Self {
            encoder,
            width,
            height,
            codec,
            config_b64: String::new(),
            pending_idr: true,
        })
    }

    fn config(&self) -> VideoConfig {
        VideoConfig {
            codec: self.codec,
            width: self.width,
            height: self.height,
            description_b64: self.config_b64.clone(),
        }
    }

    fn encode(&mut self, frame: Arc<Frame>, force_idr: bool) -> Result<Option<EncodedChunk>> {
        // Ensure even dimensions for I420.
        let even_w = frame.width & !1;
        let even_h = frame.height & !1;
        if even_w == 0 || even_h == 0 {
            return Ok(None);
        }

        if self.width != even_w || self.height != even_h {
            // Recreate encoder with correct dimensions.
            // Use higher bitrate for better quality (aim for ~15Mbps for 1080p)
            let bitrate = (even_w * even_h * 8).clamp(500_000, 15_000_000);
            let cfg = openh264::encoder::EncoderConfig::new(even_w, even_h)
                .set_bitrate_bps(bitrate)
                .max_frame_rate(60.0)  // Target 60 FPS
                .rate_control_mode(openh264::encoder::RateControlMode::Bitrate);
            self.encoder = openh264::encoder::Encoder::with_config(cfg)?;
            self.width = even_w;
            self.height = even_h;
            self.config_b64.clear();
            self.pending_idr = true;
        }

        let yuv = rgba_to_yuv420(&frame.raw, even_w as usize, even_h as usize);

        // Request an IDR on the first frame or when caller asks for it.
        if self.pending_idr || force_idr {
            unsafe { self.encoder.raw_api().force_intra_frame(true) };
            self.pending_idr = false;
        }

        let bitstream = self.encoder.encode(&yuv)?;
        let nals = collect_nals(&bitstream);

        // println!("self.config_b64.is_empty(): {}", self.config_b64.is_empty());
        if self.config_b64.is_empty() {
            // println!("building avcc from nals: {:?}", nals.iter().map(|nal| nal.len()).collect::<Vec<_>>());
            match build_avcc_from_nals(&nals) {
                Ok(option_cfg) => {
                    if let Some(cfg) = option_cfg {
                        // println!("built avcc from nals: {:?}", cfg);
                        self.config_b64 = B64.encode(cfg);
                        // println!("encoded config_b64: {}", self.config_b64);
                    } else {
                        // println!("no avcc built from nals");
                        // Fall back to explicitly requesting SPS/PPS from the encoder.
                        if let Some(cfg) = self.build_config_from_parameter_sets()? {
                            // println!("built avcc from encoder parameter sets");
                            self.config_b64 = B64.encode(cfg);
                        }
                    }
                }
                Err(err) => {
                    println!("error building avcc from nals: {:?}", err);
                }
            }
        }

        // Skip frames with no NAL units (encoder skipped output)
        if nals.is_empty() {
            return Ok(None);
        }

        let avcc = nals_to_avcc(&nals);
        Ok(Some(EncodedChunk { data: avcc }))
    }
}

#[cfg(feature = "openh264-encoder")]
fn rgba_to_yuv420(src: &[u8], width: usize, height: usize) -> openh264::formats::YUVBuffer {
    // Drop alpha, keep RGB.
    let mut rgb = Vec::with_capacity(width * height * 3);
    for i in 0..(width * height) {
        let base = i * 4;
        rgb.push(src[base]);     // R
        rgb.push(src[base + 1]); // G
        rgb.push(src[base + 2]); // B
    }
    openh264::formats::YUVBuffer::with_rgb(width, height, &rgb)
}

#[cfg(feature = "openh264-encoder")]
fn collect_nals(bitstream: &EncodedBitStream) -> Vec<Vec<u8>> {
    let mut nals = Vec::new();
    for l in 0..bitstream.num_layers() {
        if let Some(layer) = bitstream.layer(l) {
            for n in 0..layer.nal_count() {
                if let Some(nal) = layer.nal_unit(n) {
                    if let Some(clean) = normalize_nal(nal) {
                        nals.push(clean.to_vec());
                    }
                }
            }
        }
    }
    nals
}

#[cfg(feature = "openh264-encoder")]
fn nals_to_avcc(nals: &[Vec<u8>]) -> Vec<u8> {
    let mut out = Vec::new();
    for nal in nals {
        let len = nal.len() as u32;
        out.extend_from_slice(&len.to_be_bytes());
        out.extend_from_slice(nal);
    }
    out
}

#[cfg(feature = "openh264-encoder")]
fn normalize_nal(nal: &[u8]) -> Option<&[u8]> {
    if nal.is_empty() {
        return None;
    }

    // Strip a 4- or 3-byte Annex B start code if present.
    let mut offset = if nal.len() >= 4 && &nal[..4] == [0, 0, 0, 1] {
        4
    } else if nal.len() >= 3 && &nal[..3] == [0, 0, 1] {
        3
    } else {
        0
    };

    // If the buffer looks like length-prefixed (AVCC) data, drop the length prefix.
    if offset == 0 && nal.len() >= 4 {
        let declared = u32::from_be_bytes([nal[0], nal[1], nal[2], nal[3]]) as usize;
        if declared > 0 && declared + 4 <= nal.len() {
            offset = 4;
        }
    }

    if offset >= nal.len() {
        None
    } else {
        Some(&nal[offset..])
    }
}

#[cfg(feature = "openh264-encoder")]
unsafe fn collect_nals_from_info(info: &SFrameBSInfo) -> Vec<Vec<u8>> {
    let mut nals = Vec::new();
    for l in 0..(info.iLayerNum as usize) {
        let layer = &info.sLayerInfo[l];
        if layer.pBsBuf.is_null() || layer.pNalLengthInByte.is_null() {
            continue;
        }
        let count = layer.iNalCount as usize;
        let mut offset = 0usize;
        for n in 0..count {
            let size = *layer.pNalLengthInByte.add(n) as usize;
            if size == 0 {
                continue;
            }
            let slice = std::slice::from_raw_parts(layer.pBsBuf.add(offset), size);
            if let Some(clean) = normalize_nal(slice) {
                nals.push(clean.to_vec());
            }
            offset += size;
        }
    }
    nals
}

#[cfg(feature = "openh264-encoder")]
impl EncoderImpl {
    fn build_config_from_parameter_sets(&mut self) -> Result<Option<Vec<u8>>> {
        let mut info = SFrameBSInfo::default();
        let rc = unsafe { self.encoder.raw_api().encode_parameter_sets(&mut info) };
        if rc != 0 {
            return Err(anyhow!("encode_parameter_sets failed with code {}", rc));
        }
        let nals = unsafe { collect_nals_from_info(&info) };
        build_avcc_from_nals(&nals)
    }
}

#[cfg(feature = "openh264-encoder")]
fn build_avcc_from_nals(nals: &[Vec<u8>]) -> Result<Option<Vec<u8>>> {
    let mut sps: Option<&[u8]> = None;
    let mut pps: Option<&[u8]> = None;

    for nal in nals {
        if nal.is_empty() {
            continue;
        }
        let nal_type = nal[0] & 0x1F;
        match nal_type {
            7 => sps = Some(nal),
            8 => pps = Some(nal),
            _ => {}
        }
    }

    let (sps, pps) = match (sps, pps) {
        (Some(s), Some(p)) => (s, p),
        _ => return Ok(None),
    };

    if sps.len() < 4 {
        return Ok(None);
    }

    let mut avcc = Vec::with_capacity(11 + sps.len() + pps.len());
    avcc.push(1); // version
    avcc.push(sps[1]); // profile_idc
    avcc.push(sps[2]); // profile_compat
    avcc.push(sps[3]); // level_idc
    avcc.push(0xFF); // 4-byte NALU lengths
    avcc.push(0xE1); // num SPS
    avcc.extend_from_slice(&(sps.len() as u16).to_be_bytes());
    avcc.extend_from_slice(sps);
    avcc.push(1); // num PPS
    avcc.extend_from_slice(&(pps.len() as u16).to_be_bytes());
    avcc.extend_from_slice(pps);

    Ok(Some(avcc))
}

#[cfg(not(feature = "openh264-encoder"))]
struct EncoderImpl;

#[cfg(not(feature = "openh264-encoder"))]
impl EncoderImpl {
    fn new(_codec: VideoCodec) -> Result<Self> {
        Err(anyhow!("openh264 encoder feature not enabled"))
    }

    fn config(&self) -> VideoConfig {
        VideoConfig {
            codec: VideoCodec::Avc,
            width: 0,
            height: 0,
            description_b64: String::new(),
        }
    }

    fn encode(&mut self, _frame: Arc<Frame>, _force_idr: bool) -> Result<Option<EncodedChunk>> {
        Ok(None)
    }
}
