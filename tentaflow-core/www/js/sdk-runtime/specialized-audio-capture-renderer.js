// =============================================================================
// File: sdk-runtime/specialized-audio-capture-renderer.js
// Description: Renderer for AudioCapture (0x0612) — microphone capture control
// for addon panels. Records the user's voice (push-to-talk or VAD), uploads the
// finished WAV utterance through the addon document-upload channel and emits the
// component's `action_id` with a doc-ref (never the audio bytes). Reuses the
// shared mic primitives from lib/mic-capture.js (same DSP as chat-audio.js).
//
// Spec ref: tentaflow-sdk-spec/src/protocol/ui/specialized/media.rs (AudioCapture).
// =============================================================================

import {
  registerComponentRenderer,
  lookupComponentRenderer,
} from './component-renderer.js';
import { resolveBindRef, subscribeBindRef } from './bind-resolver.js';
import { ApiBinary } from '../protocol/api-binary-shim.js';
import {
  MIC_DEFAULTS, floatToWav, rmsOf, openMicSource,
} from '../lib/mic-capture.js';

export const AUDIO_CAPTURE_TAG = 0x0612;
const AC_FIELD_KEYS = new Set([0, 1, 2, 3, 4, 5, 6, 7, 8]);
const AC_MODES = new Set(['push_to_talk', 'vad']);
const AC_VARIANTS = new Set(['standalone', 'docked']);

// 256 KiB upload chunk — same as the FileInput host uploader (fits one WS frame
// with room to spare).
const UPLOAD_CHUNK_SIZE = 256 * 1024;

function requireString(v, ctx) {
  if (typeof v !== 'string') throw new TypeError(`${ctx}: expected string`);
  return v;
}
function requireEnum(v, set, ctx) {
  if (typeof v !== 'string' || !set.has(v)) {
    throw new TypeError(`${ctx}: expected one of ${[...set].join('/')}, got ${JSON.stringify(v)}`);
  }
  return v;
}
function requireU16(v, ctx) {
  if (typeof v === 'bigint') {
    if (v < 0n || v > 0xFFFFn) throw new TypeError(`${ctx}: expected u16, got ${v}`);
    return Number(v);
  }
  if (!Number.isInteger(v) || v < 0 || v > 0xFFFF) throw new TypeError(`${ctx}: expected u16, got ${v}`);
  return v;
}
function requirePath(v, ctx) {
  if (!Array.isArray(v)) throw new TypeError(`${ctx}: expected StatePath`);
  return v;
}
function assertOnlyKnownFields(fields, allowedKeys, name) {
  for (const [k] of fields) {
    if (!allowedKeys.has(k)) {
      throw new TypeError(`${name}: unknown field key ${k} (allowed: ${[...allowedKeys].join(',')})`);
    }
  }
}

/// Chunked upload of one WAV utterance to the addon document store. Bytes never
/// enter an event detail — the addon receives only the returned `doc_ref`. The
/// host gates the upload on the addon's `audio.capture` permission and rejects
/// with `PolicyDenied` when absent.
async function uploadUtterance(addonId, wavBytes) {
  const total = wavBytes.length;
  const uploadId = (globalThis.crypto && globalThis.crypto.randomUUID
    && globalThis.crypto.randomUUID())
    || `mic-${Date.now()}-${Math.floor(Math.random() * 1e9)}`;
  const totalChunks = Math.max(1, Math.ceil(total / UPLOAD_CHUNK_SIZE));
  let docRef = null;
  for (let seq = 0; seq < totalChunks; seq += 1) {
    const start = seq * UPLOAD_CHUNK_SIZE;
    const end = Math.min(start + UPLOAD_CHUNK_SIZE, total);
    const resp = await ApiBinary.one('addonDocumentUploadChunkRequest', {
      addonId,
      uploadId,
      filename: 'utterance.wav',
      mime: 'audio/wav',
      seq,
      totalChunks,
      // Trusted source marker — set ONLY by this AudioCapture renderer, never by
      // guest addon code. The host gates `audio_capture` uploads on the addon's
      // `audio.capture` permission (see dispatch/addon_document_upload.rs).
      source: 'audio_capture',
      bytes: wavBytes.subarray(start, end),
    });
    if (resp && (resp.docRef != null || resp.doc_ref != null)) {
      docRef = resp.docRef ?? resp.doc_ref;
    }
  }
  return docRef;
}

function renderAudioCapture(component, ctx) {
  assertOnlyKnownFields(component.fields, AC_FIELD_KEYS, 'AudioCapture');

  const actionId = requireString(ctx.readField(component.fields, 0), 'AudioCapture.action_id');
  const mode = requireEnum(ctx.readField(component.fields, 1), AC_MODES, 'AudioCapture.mode');
  const silenceRaw = ctx.readField(component.fields, 2);
  const silenceMs = silenceRaw != null ? requireU16(silenceRaw, 'AudioCapture.silence_ms') : MIC_DEFAULTS.holdSilenceMs;
  const minSpeechRaw = ctx.readField(component.fields, 3);
  const minSpeechMs = minSpeechRaw != null ? requireU16(minSpeechRaw, 'AudioCapture.min_speech_ms') : MIC_DEFAULTS.minSpeechMs;
  const langRaw = ctx.readField(component.fields, 4);
  const languageHint = langRaw != null ? requireString(langRaw, 'AudioCapture.language_hint') : null;
  const recordingPathRaw = ctx.readField(component.fields, 5);
  const recordingPath = recordingPathRaw != null ? requirePath(recordingPathRaw, 'AudioCapture.recording_path') : null;
  const disabledBind = ctx.readField(component.fields, 6); // Option<BindRef>
  const activePathRaw = ctx.readField(component.fields, 7);
  const activePath = activePathRaw != null ? requirePath(activePathRaw, 'AudioCapture.active_path') : null;
  const variantRaw = ctx.readField(component.fields, 8);
  const variant = variantRaw != null ? requireEnum(variantRaw, AC_VARIANTS, 'AudioCapture.variant') : 'standalone';

  const addonId = ctx.store && ctx.store.addon_id;

  const wrapper = document.createElement('div');
  wrapper.classList.add('tf-audio-capture', `tf-audio-capture--${mode}`, `tf-audio-capture--${variant}`);

  // Decorative waveform bars (top + mirrored bottom) framing the mic; the RMS
  // level meter drives their amplitude via the shared `--tf-audio-capture-level`
  // custom property, so they idle small and swell while the mic hears speech.
  const makeWave = (mirror) => {
    const wave = document.createElement('div');
    wave.classList.add('tf-audio-capture__wave');
    if (mirror) wave.classList.add('tf-audio-capture__wave--mirror');
    wave.setAttribute('aria-hidden', 'true');
    for (let i = 0; i < 14; i += 1) wave.appendChild(document.createElement('span'));
    return wave;
  };
  wrapper.appendChild(makeWave(true));

  // Mic button wrapped with concentric pulse rings (recording indicator).
  const micWrap = document.createElement('div');
  micWrap.classList.add('tf-audio-capture__mic-wrap');
  for (let i = 0; i < 3; i += 1) {
    const ring = document.createElement('span');
    ring.classList.add('tf-audio-capture__ring');
    micWrap.appendChild(ring);
  }

  const button = document.createElement('button');
  button.type = 'button';
  button.classList.add('tf-audio-capture__button');
  button.setAttribute('aria-label',
    mode === 'push_to_talk' ? 'Hold to talk' : 'Toggle voice capture');

  const icon = document.createElement('span');
  icon.classList.add('tf-audio-capture__icon');
  icon.setAttribute('aria-hidden', 'true');
  button.appendChild(icon);

  const level = document.createElement('span');
  level.classList.add('tf-audio-capture__level');
  button.appendChild(level);

  micWrap.appendChild(button);
  wrapper.appendChild(micWrap);

  const label = document.createElement('span');
  label.classList.add('tf-audio-capture__label');
  label.textContent = mode === 'push_to_talk' ? 'Przytrzymaj, aby mówić' : 'Rozpocznij nagrywanie';

  wrapper.appendChild(makeWave(false));
  wrapper.appendChild(label);

  const status = document.createElement('span');
  status.classList.add('tf-audio-capture__status');
  status.setAttribute('role', 'status');
  wrapper.appendChild(status);

  // Live capture state owned by the renderer.
  let mic = null;              // openMicSource handle
  let recording = false;
  let vadActive = false;       // VAD toggle armed (mic open, listening)
  let pcmFrames = [];
  let pcmTotalSamples = 0;
  let speechActive = false;
  let speechStartIdx = 0;
  let silenceAccum = 0;
  let speechAccum = 0;
  let rafId = null;
  let lastTickAt = 0;
  let permissionDenied = false;
  let destroyed = false;
  // Utterance ordering: a monotonic per-mount sequence rides in the action
  // detail, and deliveries are chained so a slow upload can never let a later
  // utterance overtake an earlier one on the wire.
  let utteranceSeq = 0;
  let deliverChain = Promise.resolve();

  const setStatus = (text) => { status.textContent = text || ''; };
  // Ambient listening prompts are redundant in the docked variant (the
  // recording chip + pulse rings already convey the state) and would overlap
  // the dock's neighbours — only errors/transfers surface there.
  const setAmbientStatus = (text) => {
    setStatus(variant === 'docked' ? '' : text);
  };

  const setRecordingFlag = (value) => {
    recording = value;
    button.classList.toggle('tf-audio-capture__button--recording', value);
    if (recordingPath) {
      try {
        ctx.store.applyOverlay([{ path: recordingPath, value }]);
      } catch (e) {
        console.warn('[audio-capture] recording_path overlay failed:', e?.message ?? e);
      }
    }
  };

  const emitUtterance = (docRef, wavBytes, sampleRate, durationMs, seq) => {
    // The addon-declared handler for `action_id` receives these as params; the
    // eventDispatcher merges dom_event.detail into the backend action payload.
    const detail = {
      doc_ref: docRef,
      mime: 'audio/wav',
      sample_rate: sampleRate,
      duration_ms: Math.round(durationMs),
      size: wavBytes.length,
      seq,
    };
    if (languageHint) detail.language_hint = languageHint;
    ctx.eventDispatcher.emit({
      addon_id: ctx.store.addon_id,
      panel_id: ctx.store.panel_id,
      panel_epoch: ctx.store.panel_epoch,
      source_id: component.id,
      event_kind: 'submit',
      handler: { kind: 'backend', action_id: actionId, params: {} },
      dom_event: { detail },
    });
  };

  // Upload + emit of ONE captured utterance (already WAV-encoded). Runs on the
  // serialized delivery chain, so utterances reach the addon in seq order.
  const deliverUtterance = async (wav, durationMs, seq) => {
    setStatus('Wysyłanie…');
    try {
      const docRef = await uploadUtterance(addonId, wav);
      if (destroyed) return;
      if (docRef == null) {
        setStatus('Nie udało się zapisać nagrania.');
        return;
      }
      setStatus('');
      emitUtterance(docRef, wav, 16000, durationMs, seq);
    } catch (err) {
      if (destroyed) return;
      if (err && err.code === 'PolicyDenied') {
        // Host refused the audio upload — the addon lacks `audio.capture`.
        // Disable the control and explain via tooltip.
        permissionDenied = true;
        button.disabled = true;
        button.title = 'Brak uprawnienia audio.capture — poproś administratora o zgodę.';
        setStatus('Brak uprawnienia audio.capture.');
        stopMic();
      } else {
        setStatus('Błąd wysyłania nagrania.');
        console.warn('[audio-capture] upload failed:', err?.message ?? err);
      }
    }
  };

  // Flush the accumulated speech to a WAV and queue it for delivery. The PCM
  // buffer is drained SYNCHRONOUSLY (the next utterance starts clean); only
  // the upload+emit is deferred onto the ordered chain.
  const flushUtterance = () => {
    if (pcmFrames.length === 0) return;
    const frames = pcmFrames.slice(speechStartIdx);
    let totalSamples = 0;
    for (const f of frames) totalSamples += f.length;
    pcmFrames = [];
    pcmTotalSamples = 0;
    speechActive = false;
    speechStartIdx = 0;
    if (totalSamples === 0 || !mic) return;

    const merged = new Float32Array(totalSamples);
    let off = 0;
    for (const f of frames) { merged.set(f, off); off += f.length; }
    const sampleRate = mic.frameSampleRate;
    const durationMs = (totalSamples / sampleRate) * 1000;
    if (durationMs < minSpeechMs) return; // too short — false positive

    const wav = floatToWav(merged, sampleRate, 16000);
    if (!addonId) {
      setStatus('Brak kontekstu addonu — nagrania nie wysłano.');
      return;
    }
    const seq = utteranceSeq;
    utteranceSeq += 1;
    deliverChain = deliverChain
      .then(() => deliverUtterance(wav, durationMs, seq))
      .catch(() => {});
  };

  const onFrame = (frame) => {
    if (!frame || frame.length === 0 || !recording) return;
    pcmFrames.push(frame);
    pcmTotalSamples += frame.length;
    // Trim to maxRecordSec + tailKeepSec worth of samples (anti-OOM).
    if (!mic) return;
    const maxSamples = (MIC_DEFAULTS.maxRecordSec + MIC_DEFAULTS.tailKeepSec) * mic.frameSampleRate;
    while (pcmTotalSamples > maxSamples && pcmFrames.length > 1) {
      const dropped = pcmFrames.shift();
      pcmTotalSamples -= dropped.length;
      if (speechActive) speechStartIdx = Math.max(0, speechStartIdx - 1);
    }
  };

  // RAF loop drives the RMS level meter and (in VAD mode) end-of-utterance.
  const tick = () => {
    rafId = requestAnimationFrame(tick);
    if (!mic || !mic.analyser) return;
    const now = performance.now();
    const dtMs = Math.min(200, now - lastTickAt);
    lastTickAt = now;

    const buf = new Float32Array(mic.analyser.fftSize);
    mic.analyser.getFloatTimeDomainData(buf);
    const rms = rmsOf(buf);
    // Set on the wrapper so both the button level meter and the decorative
    // waveform bars (siblings of the button) inherit the live amplitude.
    wrapper.style.setProperty('--tf-audio-capture-level', String(Math.min(1, rms * 4)));

    if (mode !== 'vad' || !vadActive) return;

    const isSpeech = rms >= MIC_DEFAULTS.rmsThreshold;
    if (isSpeech) {
      if (!speechActive) {
        speechAccum += dtMs;
        if (speechAccum >= minSpeechMs) {
          speechActive = true;
          silenceAccum = 0;
          // Keep prePad frames before detection so STT hears the onset.
          const prePadSamples = (MIC_DEFAULTS.prePadMs / 1000) * mic.frameSampleRate;
          let cumulative = 0;
          let startIdx = pcmFrames.length;
          for (let i = pcmFrames.length - 1; i >= 0; i--) {
            cumulative += pcmFrames[i].length;
            startIdx = i;
            if (cumulative >= prePadSamples) break;
          }
          speechStartIdx = startIdx;
          setRecordingFlag(true);
          setAmbientStatus('Słucham…');
        }
      } else {
        silenceAccum = 0;
      }
    } else {
      speechAccum = 0;
      if (speechActive) {
        silenceAccum += dtMs;
        if (silenceAccum >= silenceMs) {
          setRecordingFlag(false);
          flushUtterance();
        }
      }
    }
  };

  const startMic = async () => {
    if (mic || permissionDenied) return;
    setStatus('Uruchamianie mikrofonu…');
    try {
      mic = await openMicSource({ onFrame });
    } catch (err) {
      setStatus('Brak dostępu do mikrofonu.');
      console.warn('[audio-capture] getUserMedia failed:', err?.message ?? err);
      return;
    }
    if (destroyed) { stopMic(); return; }
    pcmFrames = [];
    pcmTotalSamples = 0;
    speechActive = false;
    speechAccum = 0;
    silenceAccum = 0;
    lastTickAt = performance.now();
    rafId = requestAnimationFrame(tick);
    setStatus('');
  };

  const stopMic = () => {
    if (rafId !== null) { cancelAnimationFrame(rafId); rafId = null; }
    if (mic) { mic.close(); mic = null; }
  };

  // Mirrors the armed VAD state into the addon-controlled active_path, so
  // backend logic (pause buttons, dictation docks) sees user toggles too.
  const publishActive = (value) => {
    if (!activePath) return;
    try {
      ctx.store.applyOverlay([{ path: activePath, value }]);
    } catch (e) {
      console.warn('[audio-capture] active_path overlay failed:', e?.message ?? e);
    }
  };

  const startListening = async () => {
    if (vadActive) return;
    await startMic();
    if (!mic) return;
    vadActive = true;
    label.textContent = 'Zatrzymaj nagrywanie';
    setAmbientStatus('Czekam na mowę…');
    publishActive(true);
  };

  const stopListening = () => {
    if (!vadActive) return;
    vadActive = false;
    // Pausing mid-utterance flushes what was already heard instead of
    // silently dropping the tail of the user's speech.
    if (recording) {
      setRecordingFlag(false);
      flushUtterance();
    }
    stopMic();
    setStatus('');
    label.textContent = 'Rozpocznij nagrywanie';
    publishActive(false);
  };

  // Push-to-talk: hold the button. VAD: toggle listening on/off.
  const onPointerDown = async (e) => {
    if (button.disabled) return;
    if (mode === 'push_to_talk') {
      e.preventDefault();
      await startMic();
      if (!mic) return;
      pcmFrames = [];
      pcmTotalSamples = 0;
      speechStartIdx = 0;
      setRecordingFlag(true);
      setStatus('Nagrywam…');
    } else if (vadActive) {
      stopListening();
    } else {
      await startListening();
    }
  };

  const onPointerUp = () => {
    if (mode !== 'push_to_talk' || !recording) return;
    setRecordingFlag(false);
    // In PTT the whole recording is the utterance (speechStartIdx stays 0);
    // the WAV is drained synchronously, so the mic can close right away while
    // the delivery chain uploads in the background.
    flushUtterance();
    stopMic();
  };

  button.addEventListener('pointerdown', onPointerDown);
  button.addEventListener('pointerup', onPointerUp);
  button.addEventListener('pointerleave', onPointerUp);
  ctx.registerCleanup(() => {
    button.removeEventListener('pointerdown', onPointerDown);
    button.removeEventListener('pointerup', onPointerUp);
    button.removeEventListener('pointerleave', onPointerUp);
  });

  // Two-way active_path (VAD only): the addon pauses/resumes the mic by
  // patching the bound bool; equal-value writes from publishActive land in the
  // no-op branches, so there is no feedback loop.
  if (mode === 'vad' && activePath) {
    const applyActive = () => {
      let value = null;
      try {
        value = ctx.store.read(activePath);
      } catch {
        return;
      }
      if (value === true && !vadActive && !button.disabled && !permissionDenied) {
        void startListening();
      } else if (value === false && vadActive) {
        stopListening();
      }
    };
    // Adopt an already-armed state on (re)mount so a slot re-render mid-capture
    // resumes listening instead of silently dropping the mic.
    applyActive();
    ctx.registerCleanup(ctx.store.subscribe(activePath, applyActive));
  }

  // Reactive disabled — addon-controlled, ORed with a host permission denial.
  if (disabledBind != null) {
    const applyDisabled = () => {
      const v = resolveBindRef(disabledBind, ctx.store);
      button.disabled = permissionDenied || v === true;
    };
    applyDisabled();
    ctx.registerCleanup(subscribeBindRef(disabledBind, ctx.store, applyDisabled));
  }

  ctx.registerCleanup(() => {
    destroyed = true;
    stopMic();
  });

  return wrapper;
}

export function registerSpecializedAudioCaptureRenderer() {
  if (!lookupComponentRenderer(AUDIO_CAPTURE_TAG)) {
    registerComponentRenderer(AUDIO_CAPTURE_TAG, renderAudioCapture);
  }
}
