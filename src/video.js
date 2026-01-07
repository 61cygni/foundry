export function createVideoController({
  canvas,
  renderTarget,
  log = () => {},
  requestKeyframe = () => {},
  onFrame = () => {},
  onFrameBitmap,
  onFrameSizeChanged,
  autoCloseBitmap = true,
} = {}) {
  const target = renderTarget ?? (canvas ? "canvas" : "bitmap");

  if (target === "canvas" && !canvas) {
    throw new Error("Canvas element is required for video rendering");
  }
  if (target !== "canvas" && typeof onFrameBitmap !== "function") {
    throw new Error(
      "onFrameBitmap callback is required when renderTarget is not canvas",
    );
  }

  const ctx = target === "canvas" ? canvas.getContext("2d") : null;
  if (target === "canvas" && !ctx) {
    throw new Error("Unable to acquire 2d context");
  }

  let lastFrameWidth = 0;
  let lastFrameHeight = 0;

  const resizeCanvas = () => {
    if (target !== "canvas") return;
    const dpr = window.devicePixelRatio || 1;
    const displayWidth = Math.floor(window.innerWidth);
    const displayHeight = Math.floor(window.innerHeight);
    canvas.width = Math.floor(displayWidth * dpr);
    canvas.height = Math.floor(displayHeight * dpr);
    canvas.style.width = `${displayWidth}px`;
    canvas.style.height = `${displayHeight}px`;
  };

  if (target === "canvas") {
    window.addEventListener("resize", resizeCanvas);
    resizeCanvas();
  }

  const videoWorker = new Worker("/video_worker.js", { type: "module" });
  videoWorker.onmessage = (event) => {
    const { type, bitmap, width: fw, height: fh, error, message } = event.data;
    if (error) {
      log(`video worker error: ${error}`);
      return;
    }
    switch (type) {
      case "frame":
        handleVideoFrame(bitmap, fw, fh);
        break;
      case "log":
        if (message) {
          log(message);
        }
        break;
      case "request-keyframe":
        requestKeyframe("decoder-request");
        break;
      default:
        break;
    }
  };

  function handleVideoFrame(bitmap, fw, fh) {
    onFrame();

    const sizeChanged = fw !== lastFrameWidth || fh !== lastFrameHeight;
    if (sizeChanged) {
      lastFrameWidth = fw;
      lastFrameHeight = fh;
      onFrameSizeChanged?.(fw, fh);
    }

    if (target === "canvas" && ctx && canvas) {
      ctx.save();
      ctx.clearRect(0, 0, canvas.width, canvas.height);
      ctx.imageSmoothingEnabled = true;

      const scale = Math.min(canvas.width / fw, canvas.height / fh);
      const drawWidth = Math.floor(fw * scale);
      const drawHeight = Math.floor(fh * scale);
      const dx = Math.floor((canvas.width - drawWidth) / 2);
      const dy = Math.floor((canvas.height - drawHeight) / 2);

      ctx.drawImage(bitmap, dx, dy, drawWidth, drawHeight);
      ctx.restore();
      bitmap.close?.();
      return;
    }

    try {
      onFrameBitmap?.(bitmap, fw, fh, sizeChanged);
    } finally {
      if (autoCloseBitmap) {
        bitmap.close?.();
      }
    }
  }

  function enqueueChunk(chunk) {
    videoWorker.postMessage({ type: "chunk", chunk }, [chunk]);
  }

  function configureDecoder(config) {
    videoWorker.postMessage({ type: "config", config });
  }

  function dispose() {
    if (target === "canvas") {
      window.removeEventListener("resize", resizeCanvas);
    }
    videoWorker.terminate();
  }

  return {
    enqueueChunk,
    configureDecoder,
    dispose,
  };
}
