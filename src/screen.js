import { createAudioController } from "./audio.js";
import { createGuiController } from "./gui.js";
import { createStatsTracker } from "./stats.js";
import { createVideoController } from "./video.js";

const REQUESTED_CODEC = "avc"; // "avc" or "hevc" (not implemented yet)
const STATS_WINDOW_MS = 1000;
const BACKOFF_STEPS_MS = [250, 1000, 2000, 5000];

const wsScheme = location.protocol === "https:" ? "wss" : "ws";
const endpoint = `${wsScheme}://${location.host}/ws`;

let reconnectAttempts = 0;
let reconnectTimer = null;
let ws = null;

const canvas = document.getElementById("screen");
const overlay = document.getElementById("overlay");
const overlayToggle = document.getElementById("overlay-toggle");
const logList = document.getElementById("log");
const endpointEl = document.getElementById("endpoint");
const statsBw = document.getElementById("stats-bw");
const statsFps = document.getElementById("stats-fps");
const micIconToggle = document.getElementById("mic-icon-toggle");
const micMeter = document.getElementById("mic-meter");
const micIconLevel = document.getElementById("mic-icon-level");
const audioToggle = document.getElementById("audio-toggle");
const audioStatus = document.getElementById("audio-status");
const uvMeter = document.getElementById("uv-meter");

const gui = createGuiController({
  overlay,
  overlayToggle,
  logList,
  endpointEl,
  canvas,
});
gui.setEndpoint(endpoint);
const { log } = gui;

const stats = createStatsTracker({
  windowMs: STATS_WINDOW_MS,
  statsBwEl: statsBw,
  statsFpsEl: statsFps,
});
const recordChunkSample = stats.recordChunkSample;
const recordFrameSample = stats.recordFrameSample;
const resetStats = stats.reset;
const showDisconnected = stats.showDisconnected;

const audioController = createAudioController({
  audioToggle,
  micIconToggle,
  micMeter,
  micIconLevel,
  audioStatusEl: audioStatus,
  uvMeter,
  log,
  isSocketOpen,
  sendAudioBuffer: sendBinary,
});

const videoController = canvas
  ? createVideoController({
      canvas,
      log,
      requestKeyframe,
      onFrame: recordFrameSample,
    })
  : null;

if (audioToggle) {
  audioToggle.onclick = () => audioController.handleMicToggle();
}
if (micIconToggle) {
  micIconToggle.onclick = () => audioController.handleMicToggle();
}

setConnectedState(false);
openSocket();

window.addEventListener("beforeunload", () => {
  audioController.stop("page-unload");
  videoController?.dispose();
});

function setConnectedState(isConnected, delayMs) {
  gui.setCanvasConnected(isConnected, "0.5");
  if (!isConnected) {
    showDisconnected(delayMs);
  }
}

function currentBackoffMs() {
  const idx = Math.min(reconnectAttempts, BACKOFF_STEPS_MS.length - 1);
  return BACKOFF_STEPS_MS[idx];
}

function jitteredBackoffMs() {
  const base = currentBackoffMs();
  const factor = 0.75 + Math.random() * 0.5;
  return Math.floor(base * factor);
}

function resetBackoff() {
  reconnectAttempts = 0;
}

function scheduleReconnect(_reason) {
  if (reconnectTimer) return;
  const delay = jitteredBackoffMs();
  log(`socket reconnecting in ${delay}ms`);
  showDisconnected(delay);
  reconnectTimer = setTimeout(() => {
    reconnectTimer = null;
    reconnectAttempts += 1;
    openSocket();
  }, delay);
}

function isSocketOpen(socket = ws) {
  return socket?.readyState === WebSocket.OPEN;
}

function sendBinary(buf) {
  if (isSocketOpen()) {
    ws.send(buf);
  }
}

function sendJson(message, socket = ws) {
  if (!isSocketOpen(socket)) return false;
  socket.send(JSON.stringify(message));
  return true;
}

function requestKeyframe(context = "") {
  const ok = sendJson({ type: "force-keyframe" });
  if (!ok) {
    log(
      `keyframe request skipped (socket not open${context ? `: ${context}` : ""})`,
    );
  }
}

function openSocket() {
  const socket = new WebSocket(endpoint);
  ws = socket;
  socket.binaryType = "arraybuffer";

  socket.onopen = () => {
    if (ws !== socket) return socket.close();
    log("socket opened");
    resetBackoff();
    resetStats();
    setConnectedState(true);
    audioController.onSocketOpen();
    sendJson({ type: "mode", mode: "video", codec: REQUESTED_CODEC }, socket);
    requestKeyframe("socket-open");
  };

  socket.onclose = (ev) => {
    if (ws !== socket) return;
    const reason = ev.reason ? `${ev.code} ${ev.reason}` : `${ev.code}`;
    log(`socket closed (${reason})`);
    setConnectedState(false);
    audioController.onSocketClosed();
    scheduleReconnect(reason);
  };

  socket.onerror = (err) => {
    if (ws !== socket) return;
    log(`socket error ${err.message ?? ""}`);
  };

  socket.onmessage = (ev) => {
    if (ws !== socket) return;
    if (typeof ev.data === "string") {
      if (ev.data === "heartbeat") {
        return;
      }
      try {
        const msg = JSON.parse(ev.data);
        if (msg.type === "mode-ack") {
          log(`mode-ack: ${msg.mode} codec: ${msg.codec}`);
        } else if (msg.type === "video-config") {
          videoController?.configureDecoder(msg.config);
        } else {
          log(`received: ${ev.data}`);
        }
      } catch (_) {
        log(`received: ${ev.data}`);
      }
      return;
    }
    if (audioController.isAudioBuffer(ev.data)) {
      audioController.handleIncomingAudio(ev.data);
      return;
    }
    recordChunkSample(ev.data?.byteLength ?? 0);
    videoController?.enqueueChunk(ev.data);
  };
}
