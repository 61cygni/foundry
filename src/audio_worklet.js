// Capture: resample to target sample rate, emit Int16 PCM chunks with timestamps.
class PcmCaptureProcessor extends AudioWorkletProcessor {
  constructor(options) {
    super(options);
    const {
      targetSampleRate = 32000,
      chunkMs = 100,
      startMs = Date.now(),
    } = options?.processorOptions ?? {};
    this.targetSr = targetSampleRate;
    this.chunkSamples = Math.round((this.targetSr * chunkMs) / 1000);
    this.buffer = [];
    this.pos = 0;
    this.chunkIndex = 0;
    this.startMs = startMs;
    this.inSr = sampleRate; // input sample rate provided by the audio context
  }

  process(inputs) {
    const input = inputs[0];
    if (!input || input.length === 0 || input[0].length === 0) {
      return true;
    }

    const channel = input[0];
    const inLen = channel.length;
    const ratio = this.inSr / this.targetSr;
    let pos = this.pos;

    while (pos < inLen) {
      const idx = Math.floor(pos);
      const frac = pos - idx;
      const s0 = channel[idx] ?? 0;
      const s1 = channel[idx + 1] ?? s0;
      const sample = s0 + (s1 - s0) * frac;
      this.buffer.push(sample);
      pos += ratio;

      if (this.buffer.length >= this.chunkSamples) {
        const chunk = this.buffer.splice(0, this.chunkSamples);
        const int16 = new Int16Array(chunk.length);
        for (let i = 0; i < chunk.length; i++) {
          const v = Math.max(-1, Math.min(1, chunk[i]));
          int16[i] = v < 0 ? v * 0x8000 : v * 0x7fff;
        }
        const chunkStartMs =
          this.startMs +
          (this.chunkIndex * this.chunkSamples * 1000) / this.targetSr;
        this.port.postMessage(
          {
            type: "chunk",
            startMs: chunkStartMs,
            sampleRate: this.targetSr,
            channels: 1,
            samples: int16,
          },
          [int16.buffer],
        );
        this.chunkIndex += 1;
      }
    }

    this.pos = pos - inLen;
    return true;
  }
}

registerProcessor("pcm-capture-processor", PcmCaptureProcessor);
