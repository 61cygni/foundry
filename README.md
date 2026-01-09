# Foundry

A low-latency screen and audio streaming server for macOS, written in Rust. Includes a companion MP4 player for streaming video files.

## Tools

| Tool | Purpose |
|------|---------|
| `foundry` | Stream your screen or a specific window |
| `foundry-player` | Stream an MP4 file with audio |
| `window-pick` | CLI tool to select a window by clicking |

## Quick Start

```bash
# Build all tools
cargo build --release

# Stream entire screen
./target/release/foundry

# Stream a specific window (click to select)
./target/release/foundry --window $(./target/release/window-pick --format=id)

# Stream an MP4 file
./target/release/foundry-player /path/to/movie.mp4
```

Open http://localhost:23646/ to view the stream.

---

## Foundry (Screen Streaming)

Stream your screen or a specific window with system audio.

### Window Streaming

```bash
# Interactive: click on a window to stream it
./target/release/foundry --window $(./target/release/window-pick --format=id)

# Or get the window ID first
./target/release/window-pick --list --format=pretty  # see all windows
./target/release/window-pick --format=id             # click to select, outputs ID
./target/release/foundry --window 12345              # stream that window
```

### window-pick options

| Flag | Description |
|------|-------------|
| `--format=json` | Full window info as JSON (default) |
| `--format=id` | Just the window ID |
| `--format=pretty` | Human-readable output |
| `--list` | List all windows instead of click-to-select |

### System Audio

To stream system audio (YouTube, Spotify, etc.):

```bash
# Install BlackHole virtual audio driver
brew install blackhole-2ch
```

Then in **Audio MIDI Setup**:
1. Create a **Multi-Output Device**
2. Enable both your speakers AND BlackHole 2ch
3. Set Multi-Output Device as system output

Foundry automatically captures from BlackHole.

---

## Foundry Player (MP4 Streaming)

Stream MP4 video files with synchronized audio.

```bash
# Basic playback
./target/release/foundry-player movie.mp4

# Start 30 seconds into the video
./target/release/foundry-player movie.mp4 --start 30

# Loop playback
./target/release/foundry-player movie.mp4 --loop-playback

# Custom port
./target/release/foundry-player movie.mp4 --port 8080
```

### Supported Formats

- **Video**: H.264 (AVC) - passed through directly
- **Audio**: AAC - decoded to PCM on server

### How it works

1. MP4 is demuxed on the server
2. H.264 video NALs are sent directly (no re-encoding)
3. AAC audio is decoded to PCM and streamed
4. Browser uses WebCodecs for video, Web Audio API for audio
5. Frames are paced by server timestamps for A/V sync

---

## Permissions

On first run, macOS will request **Screen Recording** permission. Grant it in:
**System Settings → Privacy & Security → Screen Recording**

---

## Architecture

### Server Components

| File | Purpose |
|------|---------|
| `src/main.rs` | Axum web server, routing, WebSocket handling |
| `src/recording.rs` | Screen/window capture using `xcap` crate |
| `src/video_pipeline.rs` | H.264 encoding with OpenH264 |
| `src/audio_capture.rs` | System audio capture via `cpal` + BlackHole |
| `src/session.rs` | WebSocket session management |

### Foundry Player Components

| File | Purpose |
|------|---------|
| `foundry-player/src/main.rs` | WebSocket server, playback pacing |
| `foundry-player/src/demuxer.rs` | MP4 parsing, H.264 extraction |
| `foundry-player/src/audio_decoder.rs` | AAC decoding via symphonia |
| `foundry-player/src/player.html` | Browser UI with WebCodecs |

### Frontend Components

| File | Purpose |
|------|---------|
| `src/root.html` / `src/root.js` | Main browser UI |
| `src/video.js` / `src/video_worker.js` | WebCodecs H.264 decoding |
| `src/audio.js` / `src/audio_worklet.js` | Web Audio API playback |
| `src/stats.js` | Performance metrics |

### Routes

| Route | Purpose |
|-------|---------|
| `/` | Browser UI |
| `/ws` | WebSocket endpoint for video/audio streaming |

---

## Performance

- **Video**: H.264 Baseline, 5-15 Mbps, up to 60 FPS
- **Audio**: PCM 48kHz stereo
- **Max Resolution**: Downsampled to 1080p if larger
- **Latency**: ~60-100ms end-to-end (screen streaming)

---

## Development

```bash
# Debug build
cargo build

# Release build (recommended)
cargo build --release

# Run with logging
RUST_LOG=debug ./target/release/foundry
```

---

## Integration with Protoverse

Both `foundry` and `foundry-player` work with Protoverse VR. Add to your `world.json`:

```json
{
  "foundryDisplays": [
    {
      "name": "Cinema",
      "wsUrl": "ws://localhost:23646/ws",
      "position": [0, 2, -3],
      "rotation": [0, 0, 0, 1],
      "width": 3.5,
      "aspectRatio": 1.777
    }
  ]
}
```

The protocol is identical - switch between screen sharing and movie playback by running different servers on the same port.
