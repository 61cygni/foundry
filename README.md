# Foundry

- [Overview](#overview)
- [Runtime-shape](#runtime-shape)
- [Routes](#routes)
- [Frontend-bits](#frontend-bits)
- [Media-pipeline](#media-pipeline)
- [Development-notes](#development-notes)

## Overview
Foundry is a small axum-powered demo that serves a browser UI and streams screen/audio over websockets. Everything lives in `src/`, with helpers in Rust for recording, mixing, and pushing frames.

## Runtime-shape
- Server listens on [0.0.0.0:23646](http://localhost:23646/).
- State: `Recorder` + `AudioMixer`, shared via `AppState`.
- Heartbeat every 10s over the websocket keeps idle links alive.
- Minimal routing; most logic is in the websocket session handler.

## Routes
- `/` → inlined [`root.html`](src/root.html) with [`root.js`](src/root.js) spliced in.
- `/ws` → websocket upgrade; session orchestration lives here.
- `/{file}` for each JS helper in `serve_files` (e.g. [`video_worker.js`](src/video_worker.js), [`audio_worklet.js`](src/audio_worklet.js), [`audio.js`](src/audio.js), [`stats.js`](src/stats.js), [`video.js`](src/video.js), [`gui.js`](src/gui.js)), all served from `src/`.

## Frontend-bits
- [`root.js`](src/root.js) bootstraps the UI and wires controllers.
- [`gui.js`](src/gui.js) draws overlay controls; [`stats.js`](src/stats.js) tracks metrics.
- [`video.js`](src/video.js) spins up `/video-worker.js` to push frames.
- [`audio.js`](src/audio.js) plugs into an `AudioWorklet` from `/audio_worklet.js`.

## Media-pipeline
- Video: captured, encoded via [`video_pipeline`](src/video_pipeline.rs) (openh264 optional), shipped over WS.
- Audio: mixed in [`audio_mixer`](src/audio_mixer.rs), sent alongside video; levels fed back to the UI.
- Recorder: coordinates ingest/output and drives the session loop in [`recording.rs`](src/recording.rs).

## Development-notes
- Everything static is under [`src/`](src/); add a filename to `serve_files` to expose it.
- Content-Type for these helpers defaults to `text/javascript`.
- The server is single-binary; no external assets or build step required.

