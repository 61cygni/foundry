const AUDIO_SAMPLE_RATE = 24000;
const AUDIO_CHUNK_MS = 100;
const AUDIO_MAGIC_BYTES = [0x41, 0x55, 0x44, 0x30]; // "AUD0"
const MIC_LABELS = {
  start: "Start mic",
  stop: "Stop mic",
};

export function createAudioController({
  audioToggle,
  micIconToggle,
  micMeter,
  micIconLevel,
  audioStatusEl,
  uvMeter,
  log = () => {},
  isSocketOpen = () => false,
  sendAudioBuffer = () => {},
} = {}) {
  let audioCtx = null;
  let audioWorkletLoaded = false;
  let audioCaptureNode = null;
  let audioSilenceSink = null;
  let audioStream = null;
  let nextPlaybackTime = null;

  syncMicUi();
  setMicLevel(0);
  setRemoteLevel(0);
  setAudioStatus("idle");

  function ensureAudioContext() {
    if (!audioCtx) {
      audioCtx = new (window.AudioContext || window.webkitAudioContext)();
      nextPlaybackTime = audioCtx.currentTime + 0.1;
    }
    if (audioCtx.state === "suspended") {
      audioCtx.resume();
    }
    return audioCtx;
  }

  async function ensureAudioWorklet() {
    ensureAudioContext();
    if (!audioWorkletLoaded) {
      await audioCtx.audioWorklet.addModule("/audio_worklet.js");
      audioWorkletLoaded = true;
    }
  }

  function setAudioStatus(text) {
    if (audioStatusEl) {
      audioStatusEl.textContent = text;
    }
  }

  function syncMicUi() {
    const active = Boolean(audioCaptureNode);
    if (audioToggle) {
      audioToggle.textContent = active ? MIC_LABELS.stop : MIC_LABELS.start;
      audioToggle.setAttribute("aria-pressed", active ? "true" : "false");
    }
    if (micIconToggle) {
      micIconToggle.classList.toggle("active", active);
      micIconToggle.setAttribute("aria-pressed", active ? "true" : "false");
      micIconToggle.setAttribute(
        "aria-label",
        active ? MIC_LABELS.stop : MIC_LABELS.start,
      );
      micIconToggle.title = active ? MIC_LABELS.stop : MIC_LABELS.start;
    }
  }

  function setMicLevel(pct) {
    if (micMeter) {
      micMeter.style.width = `${pct}%`;
    }
    if (micIconLevel) {
      micIconLevel.style.transform = `scaleY(${pct / 100})`;
    }
  }

  function setRemoteLevel(pct) {
    if (uvMeter) {
      uvMeter.style.width = `${pct}%`;
    }
  }

  function computeLevel(samples) {
    if (!samples || !samples.length) return 0;
    let sumSq = 0;
    const len = samples.length;
    for (let i = 0; i < len; i++) {
      const s = samples[i] / 32768;
      sumSq += s * s;
    }
    const rms = Math.sqrt(sumSq / len);
    const db = 20 * Math.log10(Math.max(rms, 1e-5));
    const norm = (db + 60) / 60;
    return Math.min(100, Math.max(0, Math.round(norm * 100)));
  }

  function updateMicMeters(samples) {
    setMicLevel(computeLevel(samples));
  }

  function updateRemoteMeter(samples) {
    setRemoteLevel(computeLevel(samples));
  }

  function packAudioChunk({ startMs, sampleRate, channels, samples }) {
    const count = samples.length;
    const buf = new ArrayBuffer(24 + count * 2);
    const view = new DataView(buf);
    AUDIO_MAGIC_BYTES.forEach((code, idx) => view.setUint8(idx, code));
    view.setFloat64(4, startMs, true);
    view.setUint32(12, sampleRate, true);
    view.setUint32(16, channels, true);
    view.setUint32(20, count, true);
    new Int16Array(buf, 24).set(samples);
    return buf;
  }

  function parseIncomingAudio(buffer) {
    const view = new DataView(buffer);
    const startMs = view.getFloat64(4, true);
    const sampleRate = view.getUint32(12, true);
    const channels = view.getUint32(16, true);
    const count = view.getUint32(20, true);
    const data = new Int16Array(buffer, 24, count);
    return { startMs, sampleRate, channels, samples: data };
  }

  function schedulePlayback(chunk) {
    ensureAudioContext();
    if (chunk.channels !== 1) return;
    const { samples, sampleRate } = chunk;
    const floatBuf = new Float32Array(samples.length);
    for (let i = 0; i < samples.length; i++) {
      floatBuf[i] = samples[i] / 32768;
    }
    const audioBuffer = audioCtx.createBuffer(1, floatBuf.length, sampleRate);
    audioBuffer.copyToChannel(floatBuf, 0);

    const src = audioCtx.createBufferSource();
    src.buffer = audioBuffer;
    src.connect(audioCtx.destination);

    const now = audioCtx.currentTime;
    const duration = floatBuf.length / sampleRate;
    if (nextPlaybackTime === null) {
      nextPlaybackTime = now + 0.1;
    }
    const startAt = Math.max(now + 0.05, nextPlaybackTime ?? now);
    src.start(startAt);
    nextPlaybackTime = startAt + duration;
  }

  function isAudioBuffer(data) {
    if (!(data instanceof ArrayBuffer)) return false;
    const view = new Uint8Array(data);
    if (view.length < AUDIO_MAGIC_BYTES.length) return false;
    return AUDIO_MAGIC_BYTES.every((code, idx) => view[idx] === code);
  }

  function handleIncomingAudio(buffer) {
    try {
      const chunk = parseIncomingAudio(buffer);
      updateRemoteMeter(chunk.samples);
      schedulePlayback(chunk);
    } catch (err) {
      log(`audio parse error: ${err?.message ?? err}`);
    }
  }

  async function startAudio() {
    if (audioCaptureNode) return;
    if (!navigator.mediaDevices?.getUserMedia) {
      throw new Error("getUserMedia not supported");
    }
    if (!isSocketOpen()) {
      throw new Error("socket not open");
    }

    await ensureAudioWorklet();
    setAudioStatus("requesting");

    audioStream = await navigator.mediaDevices.getUserMedia({
      audio: {
        channelCount: 1,
        sampleRate: AUDIO_SAMPLE_RATE,
        echoCancellation: true,
        noiseSuppression: true,
        autoGainControl: true,
      },
    });

    ensureAudioContext();
    nextPlaybackTime = audioCtx.currentTime + 0.1;

    const source = audioCtx.createMediaStreamSource(audioStream);
    audioCaptureNode = new AudioWorkletNode(audioCtx, "pcm-capture-processor", {
      processorOptions: {
        targetSampleRate: AUDIO_SAMPLE_RATE,
        chunkMs: AUDIO_CHUNK_MS,
        startMs: Date.now(),
      },
    });
    audioCaptureNode.port.onmessage = (ev) => {
      const {
        type,
        samples,
        sampleRate,
        channels,
        startMs: chunkStartMs,
      } = ev.data;
      if (type !== "chunk" || !samples) return;
      const pcmSamples = new Int16Array(samples);
      updateMicMeters(pcmSamples);
      const buf = packAudioChunk({
        startMs: chunkStartMs,
        sampleRate,
        channels,
        samples: pcmSamples,
      });
      if (isSocketOpen()) {
        sendAudioBuffer(buf);
      }
    };

    audioSilenceSink = audioCtx.createGain();
    audioSilenceSink.gain.value = 0;
    source
      .connect(audioCaptureNode)
      .connect(audioSilenceSink)
      .connect(audioCtx.destination);

    setAudioStatus("recording");
    syncMicUi();
    log(
      `mic started PCM ${AUDIO_SAMPLE_RATE}Hz mono, chunk=${AUDIO_CHUNK_MS}ms, ` +
        `input sampleRate=${source.context.sampleRate}`,
    );
  }

  function stopAudio(reason = "stop") {
    if (audioCaptureNode) {
      try {
        audioCaptureNode.disconnect();
      } catch (_) {}
    }
    if (audioSilenceSink) {
      try {
        audioSilenceSink.disconnect();
      } catch (_) {}
    }
    audioCaptureNode = null;
    audioSilenceSink = null;

    if (audioStream) {
      for (const t of audioStream.getTracks()) {
        t.stop();
      }
      audioStream = null;
    }

    setAudioStatus(reason === "socket-closed" ? "socket closed" : "idle");
    setMicLevel(0);
    syncMicUi();
  }

  function handleMicToggle() {
    if (audioCaptureNode) {
      stopAudio("user-stop");
      return;
    }
    startAudio().catch((err) => {
      log(`mic start failed: ${err?.message ?? err}`);
      setAudioStatus("error");
      syncMicUi();
    });
  }

  function onSocketOpen() {
    if (!audioCaptureNode) {
      setAudioStatus("connected");
    }
  }

  function onSocketClosed() {
    stopAudio("socket-closed");
    setRemoteLevel(0);
  }

  return {
    handleMicToggle,
    handleIncomingAudio,
    isAudioBuffer,
    stop: stopAudio,
    onSocketOpen,
    onSocketClosed,
  };
}
