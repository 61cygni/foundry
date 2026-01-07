// import { GLTFLoader } from "three/addons/loaders/GLTFLoader.js";
import {
  ExtSplats,
  NewSparkRenderer,
  PackedSplats,
  SparkControls,
  SparkXr,
  SplatEdit,
  SplatEditRgbaBlendMode,
  SplatEditSdf,
  SplatEditSdfType,
  SplatMesh,
  SplatSkinning,
  dyno,
  isMobile,
  textSplats,
} from "@sparkjsdev/spark";
// import { getAssetFileURL } from "/examples/js/get-asset-url.js";
import GUI from "lil-gui";
import * as THREE from "three";

import { createAudioController } from "./audio.js";
import { createGuiController } from "./gui.js";
import { createStatsTracker } from "./stats.js";
import { createVideoController } from "./video.js";

const USE_LAYERS = false;
const XR_FB_SCALE = USE_LAYERS ? 0.5 : 1.0;
const XR_LAYER_DISTANCE = 2.0;
const VIDEO_GENERATE_MIPS = true;

const URL_BASE =
  "https://storage.googleapis.com/forge-dev-public/asundqui/hobbitverse";
const splatUrl = `${URL_BASE}/Hobbiton5-lod-0.spz`;

const REQUESTED_CODEC = "avc"; // "avc" or "hevc" (not implemented yet)
const STATS_WINDOW_MS = 1000;
const BACKOFF_STEPS_MS = [250, 1000, 2000, 5000];

const wsScheme = location.protocol === "https:" ? "wss" : "ws";
const endpoint = `${wsScheme}://${location.host}/ws`;

let reconnectAttempts = 0;
let reconnectTimer = null;
let ws = null;

const canvas = document.getElementById("canvas");
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

const statsjs = new Stats();
document.body.appendChild(statsjs.dom);

const lil = new GUI({ title: "Settings" });

const videoFrameCanvas = document.createElement("canvas");
const videoFrameCtx = videoFrameCanvas.getContext("2d");
let videoTexture = null;
let videoFrameSize = { w: 0, h: 0 };
let videoMaterial = null;
let videoWall = null;
let xrBinding = null;
let xrQuadLayer = null;
let xrQuadLayerSize = { w: 0, h: 0 };
let xrLayersSupported = false;

const gui = createGuiController({
  overlay,
  overlayToggle,
  logList,
  endpointEl,
  canvas: videoFrameCanvas,
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

const videoController = createVideoController({
  renderTarget: "bitmap",
  log,
  requestKeyframe,
  onFrame: recordFrameSample,
  onFrameBitmap: handleVideoBitmapFrame,
  autoCloseBitmap: true,
});

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
  gui.setCanvasConnected(isConnected, "1.0");
  if (!isConnected) {
    showDisconnected(delayMs);
  }

  if (videoMaterial) {
    videoMaterial.color.set(
      isConnected && videoMaterial.map ? 0xffffff : 0x555555,
    );
    videoMaterial.needsUpdate = true;
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

function handleVideoBitmapFrame(bitmap, fw, fh, sizeChanged) {
  if (!videoFrameCtx) return;
  if (sizeChanged) {
    videoFrameSize = { w: fw, h: fh };
    videoFrameCanvas.width = fw;
    videoFrameCanvas.height = fh;
    videoTexture?.dispose();
    videoTexture = new THREE.CanvasTexture(videoFrameCanvas);
    videoTexture.colorSpace = THREE.SRGBColorSpace;
    videoTexture.magFilter = THREE.LinearFilter;
    videoTexture.minFilter = VIDEO_GENERATE_MIPS
      ? THREE.LinearMipmapLinearFilter
      : THREE.LinearFilter;
    videoTexture.generateMipmaps = VIDEO_GENERATE_MIPS;
    videoTexture.anisotropy = VIDEO_GENERATE_MIPS
      ? renderer.capabilities.getMaxAnisotropy?.() || 1
      : 1;
    if (videoMaterial) {
      videoMaterial.color.set(0xffffff);
      videoMaterial.map = videoTexture;
      videoMaterial.needsUpdate = true;
    }
    if (videoWall) {
      const aspect = fw / fh;
      if (aspect >= 1) {
        videoWall.scale.set(aspect, 1, 1);
      } else {
        videoWall.scale.set(1, 1 / aspect, 1);
      }
    }
    if (USE_LAYERS) {
      trySetupXrLayers({ force: true });
    }
  }

  videoFrameCtx.clearRect(0, 0, fw, fh);
  videoFrameCtx.drawImage(bitmap, 0, 0, fw, fh);

  if (videoTexture) {
    videoTexture.needsUpdate = true;
    if (videoMaterial && !videoMaterial.map) {
      videoMaterial.map = videoTexture;
      videoMaterial.needsUpdate = true;
    }
  }
}

const scene = new THREE.Scene();
// scene.background = new THREE.Color(1, 0, 0);
const renderer = new THREE.WebGLRenderer({ canvas });
renderer.outputColorSpace = THREE.SRGBColorSpace;
renderer.toneMapping = THREE.NoToneMapping;
renderer.setSize(window.innerWidth, window.innerHeight);

const spark = new NewSparkRenderer({
  renderer,
  maxStdDev: Math.sqrt(4),
  maxPagedSplats: 65536 * 256,
  lodSplatScale: USE_LAYERS ? 1.0 : 0.5,
});
scene.add(spark);

const localFrame = new THREE.Group();
const camera = new THREE.PerspectiveCamera(
  70,
  window.innerWidth / window.innerHeight,
  0.01,
  1000,
);
localFrame.add(camera);
scene.add(localFrame);

Object.assign(window, {
  THREE,
  scene,
  renderer,
  spark,
  localFrame,
  camera,
});

let renderEnabled = true;

const toggleRender = lil
  .add(
    {
      click: () => {
        renderEnabled = !renderEnabled;
        toggleRender.name(renderEnabled ? "Disable render" : "Enable render");
      },
    },
    "click",
  )
  .name("Disable render");

const splatMesh = new SplatMesh({ url: splatUrl, paged: true });
splatMesh.quaternion.set(1, 0, 0, 0);
scene.add(splatMesh);

videoMaterial = new THREE.MeshBasicMaterial({
  color: 0x555555,
  side: THREE.DoubleSide,
});
videoMaterial.needsUpdate = true;
videoWall = new THREE.Mesh(new THREE.PlaneGeometry(1, 1), videoMaterial);
videoWall.position.set(0, 0, -5);
scene.add(videoWall);

const controls = new SparkControls({ canvas });
controls.pointerControls.reverseRotate = isMobile();
lil
  .add(controls.pointerControls, "reverseRotate")
  .name("Reverse look")
  .listen();

const xr = new SparkXr({
  renderer,
  frameBufferScaleFactor: XR_FB_SCALE,
  onMouseLeaveOpacity: 0.5,
  sessionInit: USE_LAYERS ? { optionalFeatures: ["layers"] } : undefined,
  onReady: async (supported) => {
    console.log(`SparkXr ${supported ? "supported" : "not supported"}`);
    // if (USE_LAYERS && !supported) {
    //   alert("WebXR not supported; layers path unavailable");
    // }
  },
  onEnterXr: () => {
    console.log("Enter XR");
    if (USE_LAYERS) {
      requestAnimationFrame(() => {
        trySetupXrLayers({ force: true, showAlert: true });
      });
    }
  },
  onExitXr: () => {
    console.log("Exit XR");
    xrBinding = null;
    xrQuadLayer = null;
    xrLayersSupported = false;
    xrQuadLayerSize = { w: 0, h: 0 };
    if (videoWall) {
      videoWall.visible = true;
    }
  },
  controllers: {
    // moveHeading: true,
  },
});

let lastTime = 0;

renderer.setAnimationLoop(function animate(time, xrFrame) {
  statsjs.begin();

  const deltaTime = time - (lastTime || time);
  lastTime = time;

  xr?.updateControllers(camera);
  controls.update(localFrame, camera);

  if (
    USE_LAYERS &&
    xrFrame &&
    xrBinding &&
    xrQuadLayer &&
    videoFrameSize.w > 0 &&
    videoFrameSize.h > 0
  ) {
    updateXrQuadLayerTexture(xrFrame);
  }

  if (renderEnabled) {
    renderer.render(scene, camera);
  }

  statsjs.end();
});

function trySetupXrLayers({ force = false, showAlert = false } = {}) {
  if (!USE_LAYERS) return;
  const session = renderer.xr.getSession?.();
  if (!session) return;

  const binding = renderer.xr.getBinding?.();
  if (!binding || typeof binding.createQuadLayer !== "function") {
    if (showAlert) {
      alert(
        "WebXR layers not available in this browser/runtime; using scene quad instead",
      );
    }
    xrLayersSupported = false;
    xrQuadLayer = null;
    if (videoWall) {
      videoWall.visible = true;
    }
    return;
  }

  xrLayersSupported = true;
  xrBinding = binding;

  const refSpace = renderer.xr.getReferenceSpace?.();
  if (!refSpace) return;

  const baseLayer = renderer.xr.getBaseLayer?.();

  const size =
    videoFrameSize.w > 0 && videoFrameSize.h > 0
      ? videoFrameSize
      : { w: 1024, h: 1024 };

  const aspect = size.w / size.h || 1;
  if (
    !force &&
    xrQuadLayer &&
    xrQuadLayerSize.w === size.w &&
    xrQuadLayerSize.h === size.h
  ) {
    return;
  }

  xrQuadLayerSize = { ...size };

  try {
    xrQuadLayer = binding.createQuadLayer({
      space: refSpace,
      viewPixelWidth: size.w,
      viewPixelHeight: size.h,
      width: aspect >= 1 ? 1.5 * aspect : 1.5,
      height: aspect >= 1 ? 1.5 : 1.5 / aspect,
      transform: new XRRigidTransform({ z: -XR_LAYER_DISTANCE }),
      layout: "mono",
    });
    const layers = baseLayer ? [baseLayer, xrQuadLayer] : [xrQuadLayer];
    session.updateRenderState({ layers });
    if (videoWall) {
      videoWall.visible = false;
    }
  } catch (err) {
    console.warn("Failed to create XR quad layer", err);
    xrLayersSupported = false;
    xrQuadLayer = null;
    if (videoWall) {
      videoWall.visible = true;
    }
    if (showAlert) {
      alert("WebXR layers failed to init; falling back to scene quad");
    }
  }
}

function updateXrQuadLayerTexture(xrFrame) {
  if (!xrBinding || !xrQuadLayer) return;
  const gl = renderer.getContext();
  const subImage = xrBinding.getSubImage?.(xrQuadLayer, xrFrame);
  if (!subImage?.colorTexture) return;
  if (videoFrameCanvas.width === 0 || videoFrameCanvas.height === 0) return;

  const prevTex = gl.getParameter(gl.TEXTURE_BINDING_2D);
  const prevFlip = gl.getParameter(gl.UNPACK_FLIP_Y_WEBGL);

  gl.pixelStorei(gl.UNPACK_FLIP_Y_WEBGL, false);
  gl.bindTexture(gl.TEXTURE_2D, subImage.colorTexture);
  gl.texSubImage2D(
    gl.TEXTURE_2D,
    0,
    0,
    0,
    gl.RGBA,
    gl.UNSIGNED_BYTE,
    videoFrameCanvas,
  );
  gl.bindTexture(gl.TEXTURE_2D, prevTex);
  gl.pixelStorei(gl.UNPACK_FLIP_Y_WEBGL, prevFlip);
}
