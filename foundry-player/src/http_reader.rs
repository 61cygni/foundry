//! HTTP Range request reader for streaming MP4 files from URLs
//!
//! Implements Read + Seek over HTTP using Range requests,
//! allowing the mp4 crate to read from remote URLs.

use anyhow::{anyhow, Result};
use reqwest::blocking::Client;
use reqwest::header::{ACCEPT_RANGES, CONTENT_LENGTH, RANGE};
use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::time::Duration;

/// Buffer size for reads (64KB)
const BUFFER_SIZE: usize = 64 * 1024;

/// HTTP reader that supports seeking via Range requests
pub struct HttpReader {
    client: Client,
    url: String,
    position: u64,
    /// File size in bytes
    pub size: u64,
    // Small read buffer to reduce HTTP requests
    buffer: Vec<u8>,
    buffer_start: u64,
}

impl HttpReader {
    /// Create a new HTTP reader for the given URL
    pub fn new(url: &str) -> Result<Self> {
        let client = Client::builder()
            .timeout(Duration::from_secs(30))
            .build()?;

        // HEAD request to get file size and check Range support
        let response = client.head(url).send()?;

        if !response.status().is_success() {
            return Err(anyhow!(
                "HTTP HEAD failed: {} {}",
                response.status(),
                url
            ));
        }

        // Check if server supports Range requests
        let accepts_ranges = response
            .headers()
            .get(ACCEPT_RANGES)
            .and_then(|v| v.to_str().ok())
            .map(|v| v != "none")
            .unwrap_or(false);

        if !accepts_ranges {
            // Try anyway - many servers support Range but don't advertise it
            println!("Warning: Server may not support Range requests");
        }

        // Get content length
        let size = response
            .headers()
            .get(CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse().ok())
            .ok_or_else(|| anyhow!("Could not determine file size from {}", url))?;

        println!("HTTP source: {} ({:.1} MB)", url, size as f64 / 1_000_000.0);

        Ok(Self {
            client,
            url: url.to_string(),
            position: 0,
            size,
            buffer: Vec::new(),
            buffer_start: 0,
        })
    }

    /// Read a range of bytes from the URL
    fn read_range(&self, start: u64, len: usize) -> Result<Vec<u8>> {
        let end = (start + len as u64 - 1).min(self.size - 1);
        let range = format!("bytes={}-{}", start, end);

        let response = self
            .client
            .get(&self.url)
            .header(RANGE, &range)
            .send()?;

        if response.status() == 206 {
            // Partial content - expected
            Ok(response.bytes()?.to_vec())
        } else if response.status().is_success() {
            // Some servers return 200 with full content
            // Take just what we need
            let bytes = response.bytes()?;
            let actual_start = start as usize;
            let actual_end = (end as usize + 1).min(bytes.len());
            if actual_start < bytes.len() {
                Ok(bytes[actual_start..actual_end].to_vec())
            } else {
                Ok(Vec::new())
            }
        } else {
            Err(anyhow!(
                "HTTP Range request failed: {} for range {}",
                response.status(),
                range
            ))
        }
    }

    /// Ensure buffer contains data at current position
    fn fill_buffer(&mut self) -> io::Result<()> {
        // Check if current position is within buffer
        if self.position >= self.buffer_start
            && self.position < self.buffer_start + self.buffer.len() as u64
        {
            return Ok(());
        }

        // Need to fetch new data
        let fetch_size = BUFFER_SIZE.min((self.size - self.position) as usize);
        if fetch_size == 0 {
            self.buffer.clear();
            return Ok(());
        }

        match self.read_range(self.position, fetch_size) {
            Ok(data) => {
                self.buffer_start = self.position;
                self.buffer = data;
                Ok(())
            }
            Err(e) => Err(io::Error::new(io::ErrorKind::Other, e.to_string())),
        }
    }
}

impl Read for HttpReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.position >= self.size {
            return Ok(0); // EOF
        }

        self.fill_buffer()?;

        // Calculate how much we can read from buffer
        let buffer_offset = (self.position - self.buffer_start) as usize;
        let available = self.buffer.len().saturating_sub(buffer_offset);
        let to_read = buf.len().min(available);

        if to_read == 0 {
            return Ok(0);
        }

        buf[..to_read].copy_from_slice(&self.buffer[buffer_offset..buffer_offset + to_read]);
        self.position += to_read as u64;

        Ok(to_read)
    }
}

impl Seek for HttpReader {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let new_pos = match pos {
            SeekFrom::Start(p) => p as i64,
            SeekFrom::End(p) => self.size as i64 + p,
            SeekFrom::Current(p) => self.position as i64 + p,
        };

        if new_pos < 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Seek to negative position",
            ));
        }

        self.position = new_pos as u64;
        Ok(self.position)
    }
}

/// Get the cache directory for foundry-player downloads
fn get_cache_dir() -> Result<PathBuf> {
    let cache_dir = std::env::temp_dir().join("foundry-player-cache");
    if !cache_dir.exists() {
        fs::create_dir_all(&cache_dir)?;
    }
    Ok(cache_dir)
}

/// Generate a cache filename from a URL
fn url_to_cache_filename(url: &str) -> String {
    // Hash the URL for a unique but safe filename
    let mut hasher = DefaultHasher::new();
    url.hash(&mut hasher);
    let hash = hasher.finish();

    // Extract extension from URL if possible (e.g., .m4a, .mp4)
    let extension = url
        .split('?')
        .next() // Remove query params
        .and_then(|path| path.rsplit('.').next())
        .filter(|ext| ext.len() <= 5 && ext.chars().all(|c| c.is_alphanumeric()))
        .unwrap_or("bin");

    format!("{:016x}.{}", hash, extension)
}

/// Download a file completely into memory, with caching
pub fn download_file(url: &str) -> Result<Vec<u8>> {
    // Check cache first
    let cache_dir = get_cache_dir()?;
    let cache_filename = url_to_cache_filename(url);
    let cache_path = cache_dir.join(&cache_filename);

    if cache_path.exists() {
        println!("Loading from cache: {:?}", cache_path);
        let bytes = fs::read(&cache_path)?;
        println!("  Cached: {:.1} MB", bytes.len() as f64 / 1_000_000.0);
        return Ok(bytes);
    }

    // Not in cache, download it
    println!("Downloading: {}", url);

    let client = Client::builder()
        .timeout(Duration::from_secs(300)) // 5 min timeout for large files
        .build()?;

    let response = client.get(url).send()?;

    if !response.status().is_success() {
        return Err(anyhow!("HTTP GET failed: {} {}", response.status(), url));
    }

    let size = response
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());

    if let Some(s) = size {
        println!("  Size: {:.1} MB", s as f64 / 1_000_000.0);
    }

    let bytes = response.bytes()?.to_vec();
    println!("  Downloaded: {:.1} MB", bytes.len() as f64 / 1_000_000.0);

    // Save to cache
    if let Err(e) = fs::write(&cache_path, &bytes) {
        eprintln!("Warning: Failed to cache audio: {}", e);
    } else {
        println!("  Cached to: {:?}", cache_path);
    }

    Ok(bytes)
}
