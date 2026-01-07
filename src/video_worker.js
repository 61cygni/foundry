// WebCodecs-based decoder worker for H.264/HEVC.

let decoder = null;
let configured = false;
let waitingForKey = true;
let droppedSinceConfig = 0;

self.onmessage = async (event) => {
  // console.log("videoWorker.onmessage <=", event.data);

  const { type, config, chunk } = event.data;
  try {
    switch (type) {
      case "config":
        await configure(config);
        break;
      case "chunk":
        if (!configured) {
          // postMessage({ type: "log", message: "video not configured yet" });
          return;
        }
        decodeChunk(chunk);
        break;
      default:
        break;
    }
  } catch (error) {
    postMessage({ type: "log", message: `decoder error: ${error}` });
  }
};

async function configure(config) {
  if (!config || !config.codec || !config.description) {
    postMessage({ type: "log", message: "missing video config" });
    return;
  }
  decoder?.close?.();

  decoder = new VideoDecoder({
    output: handleFrame,
    error: (e) =>
      postMessage({ type: "log", message: `VideoDecoder error ${e}` }),
  });
  postMessage({ type: "log", message: "VideoDecoder created" });

  const support = await VideoDecoder.isConfigSupported({
    codec: config.codec,
    description: base64ToBuffer(config.description),
    hardwareAcceleration: "prefer-hardware",
  });
  if (!support.supported) {
    postMessage({
      type: "log",
      message: `codec not supported: ${config.codec}`,
    });
    return;
  }
  postMessage({ type: "log", message: `codec supported: ${config.codec}` });

  decoder.configure({
    codec: config.codec,
    description: base64ToBuffer(config.description),
    hardwareAcceleration: "prefer-hardware",
  });
  configured = true;
  waitingForKey = true;
  droppedSinceConfig = 0;
  postMessage({ type: "log", message: `configured ${config.codec}` });
}

function decodeChunk(buffer) {
  if (!decoder || decoder.state === "closed") return;

  const data = buffer instanceof ArrayBuffer ? new Uint8Array(buffer) : buffer;
  if (!data.byteLength) {
    postMessage({ type: "log", message: "empty video chunk" });
    return;
  }
  // Expect AVCC (length-prefixed NALs) from server. Scan NALs to see if this chunk has an IDR.
  const view = new DataView(data.buffer, data.byteOffset, data.byteLength);
  let cursor = 0;
  let hasIdr = false;
  let firstNalType = null;
  while (cursor + 4 <= view.byteLength) {
    const nalLen = view.getUint32(cursor);
    cursor += 4;
    if (nalLen === 0 || cursor + nalLen > view.byteLength) break;
    const nalType = data[cursor] & 0x1f;
    if (firstNalType === null) firstNalType = nalType;
    if (nalType === 5) {
      // IDR
      hasIdr = true;
      break;
    }
    cursor += nalLen;
  }
  const chunkType = hasIdr ? "key" : "delta";
  if (waitingForKey && chunkType !== "key") {
    droppedSinceConfig += 1;
    if (droppedSinceConfig % 10 === 1) {
      postMessage({
        type: "log",
        message: `dropping delta until first keyframe arrives (first NAL type ${firstNalType ?? "unknown"}, dropped=${droppedSinceConfig})`,
      });
    }
    // Ask main thread to request a keyframe from the sender after repeated drops.
    if (droppedSinceConfig % 30 === 0) {
      postMessage({ type: "request-keyframe" });
    }
    return;
  }
  waitingForKey = false;
  const chunk = new EncodedVideoChunk({
    timestamp: performance.now() * 1000, // microseconds
    type: chunkType,
    data,
  });
  decoder.decode(chunk);
}

async function handleFrame(frame) {
  try {
    const bitmap = await createImageBitmap(frame);
    postMessage(
      {
        type: "frame",
        bitmap,
        width: frame.displayWidth,
        height: frame.displayHeight,
      },
      [bitmap],
    );
  } catch (error) {
    postMessage({ type: "log", message: `frame error: ${error}` });
  } finally {
    frame.close();
  }
}

function base64ToBuffer(b64) {
  const binary = atob(b64);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) {
    bytes[i] = binary.charCodeAt(i);
  }
  return bytes.buffer;
}
