// WebCodecs-based decoder worker for H.264/HEVC.

let decoder = null;
let configured = false;
let configuring = false;  // Track if configuration is in progress
let waitingForKey = true;
let droppedSinceConfig = 0;
let lastConfig = null;  // Store config for recovery
let useSoftwareDecoder = false;  // Set via message from main thread

self.onmessage = async (event) => {
  const { type, config, chunk, software } = event.data;
  try {
    switch (type) {
      case "set-software-decoder":
        useSoftwareDecoder = !!software;
        postMessage({ type: "log", message: `Software decoder: ${useSoftwareDecoder ? "enabled" : "disabled"}` });
        break;
      case "config":
        await configure(config);
        break;
      case "chunk":
        // Skip if not configured or currently configuring
        if (!configured || configuring) {
          return;
        }
        // Check if decoder is in error state and needs recovery
        if (decoder && decoder.state === "closed") {
          postMessage({ type: "log", message: "decoder closed, attempting recovery..." });
          if (lastConfig) {
            await configure(lastConfig);
          }
          // After recovery, try to decode this chunk (might be a keyframe!)
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
  
  // Mark as configuring to prevent decode attempts during setup
  configuring = true;
  configured = false;
  
  // Store config for potential recovery
  lastConfig = config;
  
  // Close existing decoder if any
  if (decoder && decoder.state !== "closed") {
    try {
      decoder.close();
    } catch (e) {
      // Ignore close errors
    }
  }

  decoder = new VideoDecoder({
    output: handleFrame,
    error: (e) => {
      postMessage({ type: "log", message: `VideoDecoder error ${e}` });
      // Reset state to allow recovery on next keyframe
      waitingForKey = true;
      droppedSinceConfig = 0;
    },
  });

  try {
    const hwAccel = useSoftwareDecoder ? "prefer-software" : "prefer-hardware";
    
    const support = await VideoDecoder.isConfigSupported({
      codec: config.codec,
      description: base64ToBuffer(config.description),
      hardwareAcceleration: hwAccel,
    });
    if (!support.supported) {
      postMessage({
        type: "log",
        message: `codec not supported: ${config.codec} (${hwAccel})`,
      });
      configuring = false;
      return;
    }

    decoder.configure({
      codec: config.codec,
      description: base64ToBuffer(config.description),
      hardwareAcceleration: hwAccel,
    });
    
    configured = true;
    waitingForKey = true;
    droppedSinceConfig = 0;
    postMessage({ type: "log", message: `configured ${config.codec}` });
  } finally {
    configuring = false;
  }
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
  let hasSps = false;
  let firstNalType = null;
  const nalTypes = [];
  
  while (cursor + 4 <= view.byteLength) {
    const nalLen = view.getUint32(cursor);
    cursor += 4;
    if (nalLen === 0 || cursor + nalLen > view.byteLength) break;
    const nalType = data[cursor] & 0x1f;
    nalTypes.push(nalType);
    if (firstNalType === null) firstNalType = nalType;
    if (nalType === 7) hasSps = true;  // SPS
    if (nalType === 5) hasIdr = true;  // IDR
    cursor += nalLen;
  }
  
  // A proper keyframe should have SPS+PPS+IDR, but we'll accept just IDR
  const chunkType = hasIdr ? "key" : "delta";
  
  if (waitingForKey && chunkType !== "key") {
    droppedSinceConfig += 1;
    if (droppedSinceConfig % 10 === 1) {
      postMessage({
        type: "log",
        message: `dropping delta until first keyframe arrives (NAL types: [${nalTypes.join(',')}], dropped=${droppedSinceConfig})`,
      });
    }
    return;
  }
  
  if (chunkType === "key") {
    postMessage({ type: "log", message: `keyframe received (NAL types: [${nalTypes.join(',')}], size=${data.byteLength})` });
  }
  
  waitingForKey = false;
  
  try {
    // Check queue size before decoding large frames
    const queueSize = decoder.decodeQueueSize;
    if (chunkType === "key" && (queueSize > 2 || data.byteLength > 80000)) {
      postMessage({ type: "log", message: `decoding keyframe: queue=${queueSize}, size=${data.byteLength}` });
    }
    
    const chunk = new EncodedVideoChunk({
      timestamp: performance.now() * 1000, // microseconds
      type: chunkType,
      data,
    });
    decoder.decode(chunk);
  } catch (e) {
    postMessage({ type: "log", message: `decode() threw: ${e}` });
    waitingForKey = true;
  }
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
