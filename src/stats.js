export function createStatsTracker({
  windowMs = 1000,
  statsBwEl,
  statsFpsEl,
} = {}) {
  const chunkSamples = [];
  const frameSamples = [];

  updateStats();

  function recordChunkSample(sizeBytes) {
    const now = performance.now();
    chunkSamples.push({ t: now, size: sizeBytes });
    pruneWindow(chunkSamples, now);
    updateStats();
  }

  function recordFrameSample() {
    const now = performance.now();
    frameSamples.push(now);
    pruneWindow(frameSamples, now);
    updateStats();
  }

  function reset() {
    chunkSamples.length = 0;
    frameSamples.length = 0;
    updateStats();
  }

  function showDisconnected(delayMs) {
    if (statsBwEl) {
      statsBwEl.textContent = "";
    }
    if (statsFpsEl) {
      const retry =
        typeof delayMs === "number" ? ` (reconnect in ${delayMs}ms)` : "";
      statsFpsEl.textContent = `Disconnected${retry}`;
    }
  }

  function updateStats() {
    if (statsBwEl) {
      statsBwEl.textContent = `Mbps: ${computeMbps(chunkSamples) ?? "--.-"}`;
    }
    if (statsFpsEl) {
      statsFpsEl.textContent = `FPS: ${computeFps(frameSamples) ?? "--.-"}`;
    }
  }

  function computeMbps(samples) {
    if (samples.length < 2) return null;
    const first = samples[0];
    const last = samples[samples.length - 1];
    const dtMs = last.t - first.t;
    if (dtMs <= 0) return null;
    const totalBytes = samples.reduce((acc, s) => acc + s.size, 0);
    const mbps = (totalBytes * 8) / (dtMs / 1000) / 1_000_000;
    return mbps.toFixed(2);
  }

  function computeFps(samples) {
    if (samples.length < 2) return null;
    const first = samples[0];
    const last = samples[samples.length - 1];
    const dtMs = last - first;
    if (dtMs <= 0) return null;
    const fps = (samples.length - 1) / (dtMs / 1000);
    return fps.toFixed(1);
  }

  function pruneWindow(samples, nowMs) {
    const cutoff = nowMs - windowMs;
    while (samples.length && (samples[0].t ?? samples[0]) < cutoff) {
      samples.shift();
    }
  }

  return {
    recordChunkSample,
    recordFrameSample,
    reset,
    showDisconnected,
  };
}
