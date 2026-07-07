// =============================================================================
// File: lib/mic-capture.js
// Purpose: Shared microphone-capture primitives reused by the voice chat
// pipeline (modules/chat-audio.js) and the SDK-runtime AudioCapture renderer
// (sdk-runtime/specialized-audio-capture-renderer.js). Owns the AudioWorklet
// pcm-collector source, 16 kHz WAV encoding and RMS/VAD helpers so the two
// consumers never duplicate that DSP code.
// =============================================================================

// AudioWorklet processor — emits Float32 frames from the mic. Inlined as a Blob
// so the module is self-contained (no extra file to serve).
export const PCM_WORKLET_SRC = `
class PcmCollectorProcessor extends AudioWorkletProcessor {
  process(inputs) {
    const input = inputs[0];
    if (input && input[0]) {
      // Copy — the process() buffer is reused; postMessage structured-clones,
      // so slice() avoids a race with the next frame.
      this.port.postMessage(input[0].slice(0));
    }
    return true;
  }
}
registerProcessor('pcm-collector', PcmCollectorProcessor);
`;

// Default VAD / capture tuning shared by both consumers. Comments explain WHY.
// - rmsThreshold 0.012: RMS of a quiet room on a laptop mic; higher = false
//   negatives, lower = false positives on hum.
// - holdSilenceMs 800: shorter (300 ms) clips speech during word pauses;
//   longer (1500 ms) delays the end-of-utterance unpleasantly.
// - minSpeechMs 200: shorter bursts are clicks / rustle.
// - prePadMs 200: keep 200 ms before detection so STT hears the word onset.
export const MIC_DEFAULTS = Object.freeze({
  sampleRate: 16000,
  frameMs: 50,
  rmsThreshold: 0.012,
  holdSilenceMs: 800,
  minSpeechMs: 200,
  minSilenceBeforeSpeechMs: 200,
  prePadMs: 200,
  maxRecordSec: 30,
  tailKeepSec: 2,
});

function writeStr(view, offset, str) {
  for (let i = 0; i < str.length; i++) view.setUint8(offset + i, str.charCodeAt(i));
}

/// Float32 [-1..1] → 16-bit PCM little-endian + WAV header. Result is a
/// Uint8Array ready to upload as 'audio/wav'. Linear-interpolation resampling
/// from the AudioContext rate (typically 48 kHz) to 16 kHz — adequate for
/// speech (STT is tolerant; not HQ audio) and avoids Safari's rate-mix errors.
export function floatToWav(float32, srcSampleRate, dstSampleRate = 16000) {
  const ratio = srcSampleRate / dstSampleRate;
  const dstLen = Math.floor(float32.length / ratio);
  const dst = new Int16Array(dstLen);
  for (let i = 0; i < dstLen; i++) {
    const srcIdx = i * ratio;
    const i0 = Math.floor(srcIdx);
    const i1 = Math.min(i0 + 1, float32.length - 1);
    const frac = srcIdx - i0;
    const sample = float32[i0] * (1 - frac) + float32[i1] * frac;
    const clamped = Math.max(-1, Math.min(1, sample));
    dst[i] = clamped < 0 ? clamped * 0x8000 : clamped * 0x7fff;
  }

  const dataBytes = dst.length * 2;
  const buf = new ArrayBuffer(44 + dataBytes);
  const view = new DataView(buf);
  // RIFF header
  writeStr(view, 0, 'RIFF');
  view.setUint32(4, 36 + dataBytes, true);
  writeStr(view, 8, 'WAVE');
  // fmt chunk
  writeStr(view, 12, 'fmt ');
  view.setUint32(16, 16, true); // chunk size
  view.setUint16(20, 1, true); // PCM
  view.setUint16(22, 1, true); // mono
  view.setUint32(24, dstSampleRate, true);
  view.setUint32(28, dstSampleRate * 2, true); // byte rate
  view.setUint16(32, 2, true); // block align
  view.setUint16(34, 16, true); // bits per sample
  // data chunk
  writeStr(view, 36, 'data');
  view.setUint32(40, dataBytes, true);
  // PCM data
  let offset = 44;
  for (let i = 0; i < dst.length; i++, offset += 2) {
    view.setInt16(offset, dst[i], true);
  }
  return new Uint8Array(buf);
}

/// RMS of a Float32 [-1..1] time-domain frame.
export function rmsOf(frame) {
  let sum = 0;
  for (let i = 0; i < frame.length; i++) sum += frame[i] * frame[i];
  return Math.sqrt(sum / Math.max(1, frame.length));
}

/// Opens the mic and wires an AudioWorklet pcm-collector, returning a handle
/// with the AudioContext, the analyser (time-domain RMS source) and the frame
/// sample rate. `onFrame(frame)` is called for every Float32 worklet frame.
/// Callers own the VAD / recording buffer on top of this raw source.
///
/// `getUserMedia` is the browser permission prompt; a rejection rejects here.
export async function openMicSource({ onFrame } = {}) {
  if (typeof navigator === 'undefined' || !navigator.mediaDevices) {
    throw new Error('mic capture unsupported: no navigator.mediaDevices');
  }
  const mediaStream = await navigator.mediaDevices.getUserMedia({
    audio: {
      echoCancellation: true,
      noiseSuppression: true,
      autoGainControl: true,
      channelCount: 1,
    },
  });

  const Ctx = window.AudioContext || window.webkitAudioContext;
  if (!Ctx) {
    for (const t of mediaStream.getTracks()) { try { t.stop(); } catch { /* ignore */ } }
    throw new Error('AudioContext not supported');
  }
  const audioCtx = new Ctx();
  const frameSampleRate = audioCtx.sampleRate;

  if (!audioCtx.audioWorklet) {
    try { audioCtx.close(); } catch { /* ignore */ }
    for (const t of mediaStream.getTracks()) { try { t.stop(); } catch { /* ignore */ } }
    throw new Error('AudioWorklet not supported');
  }

  const blob = new Blob([PCM_WORKLET_SRC], { type: 'application/javascript' });
  const url = URL.createObjectURL(blob);
  try {
    await audioCtx.audioWorklet.addModule(url);
  } finally {
    URL.revokeObjectURL(url);
  }

  const sourceNode = audioCtx.createMediaStreamSource(mediaStream);
  const workletNode = new AudioWorkletNode(audioCtx, 'pcm-collector');
  if (typeof onFrame === 'function') {
    workletNode.port.onmessage = (e) => onFrame(e.data);
  }

  const analyser = audioCtx.createAnalyser();
  analyser.fftSize = 1024;
  analyser.smoothingTimeConstant = 0.6;

  sourceNode.connect(workletNode);
  sourceNode.connect(analyser);
  // A leaf node with no output is not ticked in some browsers — route the
  // worklet through a gain=0 sink so process() keeps firing without feedback.
  const muteGain = audioCtx.createGain();
  muteGain.gain.value = 0;
  workletNode.connect(muteGain).connect(audioCtx.destination);

  const close = () => {
    try { workletNode.port.onmessage = null; workletNode.disconnect(); } catch { /* ignore */ }
    try { sourceNode.disconnect(); } catch { /* ignore */ }
    try { analyser.disconnect(); } catch { /* ignore */ }
    try { muteGain.disconnect(); } catch { /* ignore */ }
    try { audioCtx.close(); } catch { /* ignore */ }
    for (const t of mediaStream.getTracks()) { try { t.stop(); } catch { /* ignore */ } }
  };

  return { mediaStream, audioCtx, analyser, frameSampleRate, close };
}
