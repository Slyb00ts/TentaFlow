// =============================================================================
// File: modules/chat-audio.js — AudioPipeline (mic VAD + STT + LLM streaming +
// TTS + barge-in) for Chat audio mode. Owns the user microphone + AudioContext;
// emits user utterances to chat.js, consumes assistant deltas back, queues TTS
// sentence-by-sentence. Caller drives the LLM stream subscription itself —
// AudioPipeline does not import ApiBinary so it stays UI-only.
// =============================================================================

// SentenceBuffer akumuluje tokeny streamingu z LLM i wypluwa kompletne zdania
// dopiero gdy wykryje sygnal konca (kropka, !, ?, … z whitespace lub EOL z
// nastepnym duza litera). Skroty (np. tj. dr. mr. inc. itp.) i liczby z
// kropka (3.14) nie powinny byc rozpoznawane jako koniec zdania — heurystyka
// na bazie blacklisty + sasiadujacej cyfry. drain() flushuje reszte na koniec
// streamu (np. zdanie bez konczacej interpunkcji).
const SENTENCE_END_RE = /[.!?…]/g;
const ABBREV_BLOCKLIST = /\b(np|tj|dr|mr|mrs|ms|inc|etc|por|str|vs|ok|tzn|prof|sb|im|tj)$/i;
const MIN_SENTENCE_CHARS = 4;

export class SentenceBuffer {
  constructor() {
    this.buf = '';
  }

  push(token) {
    if (typeof token !== 'string' || token.length === 0) return [];
    this.buf += token;
    return this._flushSentences();
  }

  _flushSentences() {
    const out = [];
    let lastEnd = 0;
    SENTENCE_END_RE.lastIndex = 0;
    let m;
    while ((m = SENTENCE_END_RE.exec(this.buf)) !== null) {
      const idx = m.index;
      const next = this.buf[idx + 1];
      // Wymagamy whitespace/EOL/EOF za znakiem konca — w przeciwnym razie
      // mozemy byc w srodku liczby (3.14) albo URLa.
      if (next !== undefined && !/\s/.test(next)) continue;
      const before = this.buf.slice(lastEnd, idx);
      // Nie tnij na skrótach typu „np.", „dr." — sprawdzamy tylko gdy znak to
      // kropka (skróty z ! lub ? sa pomijalne).
      if (m[0] === '.' && ABBREV_BLOCKLIST.test(before)) continue;
      const sentence = this.buf.slice(lastEnd, idx + 1).trim();
      if (sentence.length >= MIN_SENTENCE_CHARS) {
        out.push(sentence);
        lastEnd = idx + 1;
      }
    }
    if (lastEnd > 0) this.buf = this.buf.slice(lastEnd);
    return out;
  }

  drain() {
    const rest = this.buf.trim();
    this.buf = '';
    return rest.length >= 1 ? [rest] : [];
  }

  reset() {
    this.buf = '';
  }
}

// AudioWorklet processor — generuje ramki Float32 z mikrofonu. Inline jako
// Blob, zeby modul byl samowystarczalny i nie wymagal extra pliku w www/.
const PCM_WORKLET_SRC = `
class PcmCollectorProcessor extends AudioWorkletProcessor {
  process(inputs) {
    const input = inputs[0];
    if (input && input[0]) {
      // Kopiujemy — bufor process() jest reuzywany, postMessage strukturalnie
      // klonuje, wiec slice() unika race condition na nastepnej ramce.
      this.port.postMessage(input[0].slice(0));
    }
    return true;
  }
}
registerProcessor('pcm-collector', PcmCollectorProcessor);
`;

// Parametry akustyczne. Komentarze WHY:
// - DEFAULT_RMS_THRESHOLD: 0.012 to RMS dla cichego pokoju z mikrofonem
//   laptopowym; wyzej = false negatives, nizej = false positives na hum.
// - HOLD_SILENCE_MS 800: za krotko (300ms) tnie mowe podczas pauz miedzy
//   slowami; za dlugo (1500ms) opóznia odpowiedz nieprzyjemnie.
// - MIN_SPEECH_MS 200: krotsze impulsy to klikniecia / szelesty.
// - PRE_PAD_MS 200: chcemy zachowac 200ms przed wykryciem speech, zeby STT
//   uslyszal poczatek slowa (RMS rozpoznaje speech dopiero po atak'u).
// - BARGE_IN_MS 250: 250ms ciagłej mowy podczas TTS = realny barge-in,
//   krocej = false positive od wlasnego TTS przeciekajacego przez glosnik.
const DEFAULTS = {
  sampleRate: 16000,
  frameMs: 50,
  rmsThreshold: 0.012,
  holdSilenceMs: 800,
  minSpeechMs: 200,
  minSilenceBeforeSpeechMs: 200,
  prePadMs: 200,
  bargeInMs: 250,
  maxRecordSec: 30,
  tailKeepSec: 2,
};

// Stany FSM. mapowanie na faceMode dzieje sie w _setState.
const STATES = {
  IDLE: 'idle',
  LISTENING: 'listening',
  TRANSCRIBING: 'transcribing',
  THINKING: 'thinking',
  SPEAKING: 'speaking',
  ERROR: 'error',
};

const FACE_MODE = {
  idle: 'idle',
  listening: 'listen',
  transcribing: 'think',
  thinking: 'think',
  speaking: 'speak',
  error: 'idle',
};

// Konwersja Float32 [-1..1] -> 16-bit PCM little-endian + WAV header.
// Wynik to Uint8Array gotowy do upload jako 'audio/wav'. Resampling z
// browser AudioContext sample rate (typowo 48kHz) do 16kHz — STT (whisper)
// preferuje 16kHz mono.
function floatToWav(float32, srcSampleRate, dstSampleRate = 16000) {
  // Linear interpolation resampling — adekwatne dla mowy w typowym 48k->16k
  // ratio. Bez aliasingu lowpass'a, ale STT jest tolerancyjny (nie audio HQ).
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

function writeStr(view, offset, str) {
  for (let i = 0; i < str.length; i++) view.setUint8(offset + i, str.charCodeAt(i));
}

function rmsOf(frame) {
  // RMS w domenie czasu — operujemy na Float32 [-1..1].
  let sum = 0;
  for (let i = 0; i < frame.length; i++) sum += frame[i] * frame[i];
  return Math.sqrt(sum / Math.max(1, frame.length));
}

export class AudioPipeline {
  constructor(opts) {
    if (!opts || !opts.faceHandle) {
      throw new Error('AudioPipeline: faceHandle is required');
    }
    this.opts = { ...DEFAULTS, ...(opts.config || {}) };
    this.conv = opts.conv || null;
    this.faceHandle = opts.faceHandle;
    this.onUserUtterance = opts.onUserUtterance || (() => {});
    this.onStateChange = opts.onStateChange || (() => {});
    this.onError = opts.onError || (() => {});
    this.bargeInAbort = opts.bargeInAbort || (() => {});
    this.i18n = opts.i18n || { t: (k) => k };

    this.state = STATES.IDLE;
    this.muted = false;
    this.speakerMuted = false;

    // Audio graph
    this.mediaStream = null;
    this.audioCtx = null;
    this.workletNode = null;
    this.sourceNode = null;
    this.analyser = null;

    // Bufor PCM (Float32Array per ramka). Trzymamy tail wzgledem max ramek
    // = (tailKeepSec + maxRecordSec) — po wyslaniu STT zachowujemy tail dla
    // ciaglosci. Limit chronie przed RAM leak gdy uzytkownik mówi non-stop.
    this.pcmFrames = [];
    this.pcmTotalSamples = 0;
    this.frameSampleRate = 0; // ustawiony w start() z audioCtx.sampleRate

    // VAD state
    this.vadInSpeech = false;
    this.speechStartFrameIdx = 0;
    this.silenceMsAccumulated = 0;
    this.speechMsAccumulated = 0;
    this.silenceSinceSpeechEnd = 0;
    this.adaptiveThreshold = this.opts.rmsThreshold;
    this.lastSpeechAt = 0;
    this.continuousSpeechMs = 0;

    // Push-to-talk override
    this.pttActive = false;

    // Barge-in
    this.bargeInSpeechMs = 0;

    // LLM stream + TTS queue
    this.sentenceBuf = new SentenceBuffer();
    this.ttsQueue = [];
    this.streamComplete = false;
    this.activeAudioEl = null;
    this.activeAudioUrl = null;
    this.activeAudioCleanup = null;
    this.ttsPlaying = false;
    this.ttsAbortController = null;
    this.sttAbortController = null;

    // RAF tick
    this.rafId = null;
    this.lastTickAt = 0;

    // TTS amplitude monitoring (osobny analyser dla audio output).
    this.ttsAnalyser = null;
    this.ttsAmpRafId = null;
  }

  getState() {
    return this.state;
  }

  isMuted() {
    return this.muted;
  }

  // ---- Lifecycle ---------------------------------------------------------

  async start() {
    if (this.state !== STATES.IDLE) return;
    let stream;
    try {
      stream = await navigator.mediaDevices.getUserMedia({
        audio: {
          echoCancellation: true,
          noiseSuppression: true,
          autoGainControl: true,
          channelCount: 1,
        },
      });
    } catch (err) {
      this._setState(STATES.ERROR);
      throw err;
    }
    this.mediaStream = stream;

    // Browser dyktuje sample rate AudioContext'u — typowo 48000 na desktopie,
    // 44100 na niektorych mobile. floatToWav() resamplowuje do 16k przy
    // wysylce STT. Probowalismy `sampleRate: 16000` w konstruktorze, ale
    // Safari ignoruje ten hint i mieszanie rate'ow w grafie wybucha errorem.
    const Ctx = window.AudioContext || window.webkitAudioContext;
    if (!Ctx) {
      this._setState(STATES.ERROR);
      throw new Error('AudioContext not supported');
    }
    this.audioCtx = new Ctx();
    this.frameSampleRate = this.audioCtx.sampleRate;

    // AudioWorklet jest standardem od ~2018 (Chrome 66, Firefox 76, Safari
    // 14.1). Brak wsparcia = zglaszamy blad, nie ma sensu robic ScriptProc
    // fallback w 2026. ScriptProcessorNode jest deprecated od dekady.
    if (!this.audioCtx.audioWorklet) {
      this._setState(STATES.ERROR);
      throw new Error('AudioWorklet not supported');
    }

    const blob = new Blob([PCM_WORKLET_SRC], { type: 'application/javascript' });
    const url = URL.createObjectURL(blob);
    try {
      await this.audioCtx.audioWorklet.addModule(url);
    } finally {
      URL.revokeObjectURL(url);
    }

    this.sourceNode = this.audioCtx.createMediaStreamSource(stream);
    this.workletNode = new AudioWorkletNode(this.audioCtx, 'pcm-collector');
    this.workletNode.port.onmessage = (e) => this._onWorkletFrame(e.data);

    // Equalizer-style analyser dla face listen amplitude — szybciej niz
    // liczyc RMS na kazdej ramce worklet'a, mamy time-domain getter.
    this.analyser = this.audioCtx.createAnalyser();
    this.analyser.fftSize = 1024;
    this.analyser.smoothingTimeConstant = 0.6;

    this.sourceNode.connect(this.workletNode);
    this.sourceNode.connect(this.analyser);
    // Worklet musi byc podlaczony do destination zeby process() byl wywolywany
    // (specyfikacja: leaf node bez output'u nie jest tickowany w niektorych
    // przegladarkach). Podlaczamy przez gain=0, zeby nie bylo audio feedbacku.
    const muteGain = this.audioCtx.createGain();
    muteGain.gain.value = 0;
    this.workletNode.connect(muteGain).connect(this.audioCtx.destination);

    this._setState(STATES.LISTENING);
    this._startRaf();
  }

  stop() {
    this._stopRaf();
    this._stopTtsAmpRaf();
    this._abortStt();
    this._stopActiveTts(true);
    this.ttsQueue = [];
    this.sentenceBuf.reset();
    this.streamComplete = false;

    if (this.workletNode) {
      try { this.workletNode.port.onmessage = null; this.workletNode.disconnect(); } catch { /* ignore */ }
      this.workletNode = null;
    }
    if (this.sourceNode) {
      try { this.sourceNode.disconnect(); } catch { /* ignore */ }
      this.sourceNode = null;
    }
    if (this.analyser) {
      try { this.analyser.disconnect(); } catch { /* ignore */ }
      this.analyser = null;
    }
    if (this.audioCtx) {
      try { this.audioCtx.close(); } catch { /* ignore */ }
      this.audioCtx = null;
    }
    if (this.mediaStream) {
      for (const t of this.mediaStream.getTracks()) {
        try { t.stop(); } catch { /* ignore */ }
      }
      this.mediaStream = null;
    }

    this.pcmFrames = [];
    this.pcmTotalSamples = 0;
    this.vadInSpeech = false;
    this.muted = false;

    this._setState(STATES.IDLE);
  }

  // Soft abort — zatrzymuje aktywny pipeline (STT in-flight + TTS), ale
  // zostawia mic + AudioContext. Uzywane przez "Przerwij" button.
  abort() {
    this._abortStt();
    this._stopActiveTts(true);
    this.ttsQueue = [];
    this.sentenceBuf.reset();
    this.streamComplete = false;
    this.bargeInAbort();
    this._resetVad();
    this._setState(STATES.LISTENING);
  }

  // Barge-in — wywolywane gdy podczas SPEAK wykryta zostanie wystarczajaco
  // dluga mowa uzytkownika. Tnie aktywne TTS, abortuje LLM stream przez
  // callback i zostawia mic w trybie listen (juz jestesmy w trakcie speech).
  interruptBot() {
    this._stopActiveTts(true);
    this.ttsQueue = [];
    this.sentenceBuf.reset();
    this.streamComplete = false;
    this.bargeInAbort();
    this._setState(STATES.LISTENING);
  }

  mute(muted) {
    this.muted = !!muted;
    if (this.mediaStream) {
      for (const t of this.mediaStream.getAudioTracks()) t.enabled = !this.muted;
    }
    if (this.muted) {
      // Wyczyscic VAD zeby po unmute nie wylecial natychmiast end-of-utterance
      // z dawno nagromadzonej "ciszy".
      this._resetVad();
      this.faceHandle.setListenAmplitude(0);
    }
  }

  toggleSpeaker() {
    this.speakerMuted = !this.speakerMuted;
    if (this.activeAudioEl) {
      this.activeAudioEl.muted = this.speakerMuted;
    }
    return this.speakerMuted;
  }

  pushToTalkStart() {
    // Manualny override — uzytkownik trzyma Spacje. Wymusza speech-mode,
    // ignoruje VAD threshold do momentu pushToTalkEnd().
    if (this.state !== STATES.LISTENING) return;
    if (this.muted) return;
    this.pttActive = true;
    if (!this.vadInSpeech) {
      this._onSpeechStart();
    }
  }

  pushToTalkEnd() {
    if (!this.pttActive) return;
    this.pttActive = false;
    if (this.vadInSpeech) {
      // Kierujemy do natychmiastowego end-of-utterance — uzytkownik puscil
      // klawisz, czekamy na transkrypcje.
      this._onSpeechEnd();
    }
  }

  // ---- Caller-driven LLM stream feed ------------------------------------

  feedAssistantDelta(delta) {
    if (this.state !== STATES.THINKING && this.state !== STATES.SPEAKING) {
      // Pierwsza chunk po _onSpeechEnd — przeskoczylismy z TRANSCRIBING.
      // Akceptuj tylko jesli aktualnie czekamy na bota (nie po abort()).
      if (this.state !== STATES.TRANSCRIBING) return;
    }
    if (this.state === STATES.TRANSCRIBING) {
      this._setState(STATES.THINKING);
    }
    const sentences = this.sentenceBuf.push(delta);
    for (const s of sentences) this._enqueueTts(s);
  }

  feedAssistantEnd() {
    this.streamComplete = true;
    const rest = this.sentenceBuf.drain();
    for (const s of rest) this._enqueueTts(s);
    if (!this.ttsPlaying && this.ttsQueue.length === 0) {
      // Pusty stream (LLM zwrocil 0 tokenow) — wracamy do listen od razu.
      this._setState(STATES.LISTENING);
    }
  }

  feedAssistantError(_err) {
    // Caller juz zatoastowal blad — my tylko sprzatamy lokalny stan.
    this.streamComplete = true;
    this.sentenceBuf.reset();
    this.ttsQueue = [];
    this._stopActiveTts(true);
    this._setState(STATES.LISTENING);
  }

  // ---- Worklet frame ingestion ------------------------------------------

  _onWorkletFrame(frame) {
    if (!frame || frame.length === 0) return;
    if (this.muted) return;

    this.pcmFrames.push(frame);
    this.pcmTotalSamples += frame.length;

    // Trim tail — keep maxRecordSec + tailKeepSec worth of data.
    const maxSamples = (this.opts.maxRecordSec + this.opts.tailKeepSec) * this.frameSampleRate;
    while (this.pcmTotalSamples > maxSamples && this.pcmFrames.length > 1) {
      const dropped = this.pcmFrames.shift();
      this.pcmTotalSamples -= dropped.length;
      // Jesli speech start wskazywal na drop'niete ramki — przesun w lewo
      // (clamp do 0). To rzadkie — uzytkownik mowiacy ciagle 30s+.
      if (this.vadInSpeech) {
        this.speechStartFrameIdx = Math.max(0, this.speechStartFrameIdx - 1);
      }
    }
  }

  // ---- RAF / VAD --------------------------------------------------------

  _startRaf() {
    if (this.rafId !== null) return;
    this.lastTickAt = performance.now();
    const tick = () => {
      this.rafId = requestAnimationFrame(tick);
      this._tick();
    };
    this.rafId = requestAnimationFrame(tick);
  }

  _stopRaf() {
    if (this.rafId !== null) {
      cancelAnimationFrame(this.rafId);
      this.rafId = null;
    }
  }

  _tick() {
    if (!this.analyser || this.muted) {
      this.faceHandle.setListenAmplitude(0);
      this.lastTickAt = performance.now();
      return;
    }
    const now = performance.now();
    const dtMs = Math.min(200, now - this.lastTickAt);
    this.lastTickAt = now;

    // RMS z analyser time-domain — wystarczajacy proxy dla VAD.
    const buf = new Float32Array(this.analyser.fftSize);
    this.analyser.getFloatTimeDomainData(buf);
    const rms = rmsOf(buf);

    // Aktualizuj amplitude twarzy w listen state.
    if (this.state === STATES.LISTENING) {
      // Skala perceptualna — RMS rzadko przekracza 0.3 dla normalnej mowy,
      // wiec mnozymy by uzyskac ladny zakres 0..1 dla animacji.
      this.faceHandle.setListenAmplitude(Math.min(1, rms * 4));
    }

    // VAD logika — driver speech/silence detection.
    this._vadStep(rms, dtMs);

    // Barge-in monitor — niezalezny od VAD, dziala w SPEAK state.
    if (this.state === STATES.SPEAKING) {
      this._bargeInStep(rms, dtMs);
    }

    // Adaptacja threshold — dluga cisza podnosi prog (false negative na
    // bardzo cichym mikrofonie ulatwiamy), dluga ciagla mowa tez podnosi
    // (chroni przed hałasem stałym typu wentylator).
    this._adaptThreshold(rms, dtMs);
  }

  _vadStep(rms, dtMs) {
    if (this.state !== STATES.LISTENING && this.state !== STATES.SPEAKING) return;
    // W SPEAKING VAD nie startuje regularnego utterance — robi to dopiero
    // _bargeInStep, a my tu tylko liczymy threshold dla niego.
    if (this.state === STATES.SPEAKING) return;

    const threshold = this.adaptiveThreshold;
    const isSpeech = this.pttActive || rms >= threshold;

    if (isSpeech) {
      this.silenceSinceSpeechEnd = 0;
      if (!this.vadInSpeech) {
        this.speechMsAccumulated += dtMs;
        if (this.speechMsAccumulated >= this.opts.minSpeechMs) {
          // Ignoruj jesli tuz po poprzednim end-of-utterance — debounce.
          if (this.silenceSinceSpeechEnd < this.opts.minSilenceBeforeSpeechMs && this.lastSpeechAt > 0) {
            this.speechMsAccumulated = 0;
            return;
          }
          this._onSpeechStart();
        }
      } else {
        this.silenceMsAccumulated = 0;
        this.continuousSpeechMs += dtMs;
      }
    } else {
      if (this.vadInSpeech) {
        this.silenceMsAccumulated += dtMs;
        if (this.silenceMsAccumulated >= this.opts.holdSilenceMs && !this.pttActive) {
          this._onSpeechEnd();
        }
      } else {
        // Cisza w trybie listen — accumulate dla minSilenceBeforeSpeech.
        this.silenceSinceSpeechEnd += dtMs;
        this.speechMsAccumulated = 0;
      }
    }
  }

  _onSpeechStart() {
    this.vadInSpeech = true;
    this.silenceMsAccumulated = 0;
    this.continuousSpeechMs = 0;
    // Zapamietaj index ramki PCM, od ktorej wycinamy utterance — z prepad'em.
    const prePadSamples = (this.opts.prePadMs / 1000) * this.frameSampleRate;
    let cumulative = 0;
    let startIdx = this.pcmFrames.length;
    for (let i = this.pcmFrames.length - 1; i >= 0; i--) {
      cumulative += this.pcmFrames[i].length;
      startIdx = i;
      if (cumulative >= prePadSamples) break;
    }
    this.speechStartFrameIdx = startIdx;
    this.lastSpeechAt = performance.now();
  }

  async _onSpeechEnd() {
    if (!this.vadInSpeech) return;
    this.vadInSpeech = false;
    this.speechMsAccumulated = 0;
    this.silenceMsAccumulated = 0;

    // Zlep ramki od speechStartFrameIdx do konca w jeden Float32Array.
    const frames = this.pcmFrames.slice(this.speechStartFrameIdx);
    let total = 0;
    for (const f of frames) total += f.length;
    if (total < (this.opts.minSpeechMs / 1000) * this.frameSampleRate) {
      // Za malo — false positive, wracamy do listen bez STT.
      return;
    }
    const merged = new Float32Array(total);
    let off = 0;
    for (const f of frames) { merged.set(f, off); off += f.length; }

    // Po wylowieniu utterance trim PCM bufor do tail — ciagle nasluchujemy.
    // Zachowujemy ostatnie tailKeepSec na wypadek gdyby uzytkownik kontynuowal.
    const tailSamples = this.opts.tailKeepSec * this.frameSampleRate;
    let newFrames = [];
    let newTotal = 0;
    for (let i = this.pcmFrames.length - 1; i >= 0; i--) {
      newFrames.unshift(this.pcmFrames[i]);
      newTotal += this.pcmFrames[i].length;
      if (newTotal >= tailSamples) break;
    }
    this.pcmFrames = newFrames;
    this.pcmTotalSamples = newTotal;

    this._setState(STATES.TRANSCRIBING);

    // STT call — multipart upload do /v1/audio/transcriptions.
    let text = '';
    try {
      const wav = floatToWav(merged, this.frameSampleRate, 16000);
      const blob = new Blob([wav], { type: 'audio/wav' });
      const fd = new FormData();
      fd.append('file', blob, 'utterance.wav');
      const cfg = this.conv?.audioConfig || {};
      fd.append('model', cfg.sttModel || 'whisper-1');
      if (cfg.language) fd.append('language', cfg.language);
      fd.append('response_format', 'json');
      this.sttAbortController = new AbortController();
      const resp = await fetch('/v1/audio/transcriptions', {
        method: 'POST',
        body: fd,
        signal: this.sttAbortController.signal,
        credentials: 'same-origin',
      });
      this.sttAbortController = null;
      if (!resp.ok) throw new Error(`STT HTTP ${resp.status}`);
      const json = await resp.json();
      text = (json && typeof json.text === 'string') ? json.text.trim() : '';
    } catch (err) {
      this.sttAbortController = null;
      if (err.name === 'AbortError') {
        this._setState(STATES.LISTENING);
        return;
      }
      this.onError(err);
      this._setState(STATES.LISTENING);
      return;
    }

    if (!text) {
      // Pusta transkrypcja — caller pokaze toast i wroci do listen.
      this.onUserUtterance('');
      this._setState(STATES.LISTENING);
      return;
    }

    // Mark thinking i emit do callera. Caller pushuje user msg + startuje
    // LLM stream subscription, a deltas wracaja przez feedAssistantDelta.
    this._setState(STATES.THINKING);
    this.streamComplete = false;
    this.sentenceBuf.reset();
    this.onUserUtterance(text);
  }

  _abortStt() {
    if (this.sttAbortController) {
      try { this.sttAbortController.abort(); } catch { /* ignore */ }
      this.sttAbortController = null;
    }
  }

  // ---- Barge-in ---------------------------------------------------------

  _bargeInStep(rms, dtMs) {
    if (rms >= this.adaptiveThreshold) {
      this.bargeInSpeechMs += dtMs;
      if (this.bargeInSpeechMs >= this.opts.bargeInMs) {
        this.bargeInSpeechMs = 0;
        this.interruptBot();
      }
    } else {
      this.bargeInSpeechMs = Math.max(0, this.bargeInSpeechMs - dtMs * 0.5);
    }
  }

  // ---- Adaptive threshold ----------------------------------------------

  _adaptThreshold(rms, dtMs) {
    // Trzymamy thresh w okolicy default'a, ale gdy >=10s ciagle "speech"
    // bez VAD startu — to znaczy ze RMS baseline jest powyzej, podnosimy.
    if (this.continuousSpeechMs > 10_000 && this.adaptiveThreshold < this.opts.rmsThreshold * 1.2) {
      this.adaptiveThreshold *= 1.05;
      this.continuousSpeechMs = 0;
    }
    // Pasywne dryfowanie z powrotem do default'u — wolne, zeby nie kasowac
    // adaptacji w 5s.
    if (this.adaptiveThreshold > this.opts.rmsThreshold) {
      this.adaptiveThreshold -= (this.adaptiveThreshold - this.opts.rmsThreshold) * 0.0001 * dtMs;
    }
    void rms;
  }

  _resetVad() {
    this.vadInSpeech = false;
    this.silenceMsAccumulated = 0;
    this.speechMsAccumulated = 0;
    this.silenceSinceSpeechEnd = 0;
    this.continuousSpeechMs = 0;
    this.bargeInSpeechMs = 0;
  }

  // ---- TTS queue --------------------------------------------------------

  _enqueueTts(sentence) {
    if (!sentence) return;
    this.ttsQueue.push(sentence);
    if (!this.ttsPlaying) this._playNextTts();
  }

  async _playNextTts() {
    if (this.ttsQueue.length === 0) {
      this.ttsPlaying = false;
      if (this.streamComplete) {
        // Caly stream + queue wyemitowane — wracamy do listen.
        this._setState(STATES.LISTENING);
      }
      return;
    }
    const sentence = this.ttsQueue.shift();
    this.ttsPlaying = true;

    let url;
    try {
      const cfg = this.conv?.audioConfig || {};
      this.ttsAbortController = new AbortController();
      const resp = await fetch('/v1/audio/speech', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        credentials: 'same-origin',
        signal: this.ttsAbortController.signal,
        body: JSON.stringify({
          model: cfg.ttsModel || 'tts-1',
          input: sentence,
          voice: cfg.voice || 'nova',
          language: cfg.language || 'pl',
          response_format: 'mp3',
        }),
      });
      this.ttsAbortController = null;
      if (!resp.ok) throw new Error(`TTS HTTP ${resp.status}`);
      const blob = await resp.blob();
      url = URL.createObjectURL(blob);
    } catch (err) {
      this.ttsAbortController = null;
      if (err.name !== 'AbortError') this.onError(err);
      this.ttsPlaying = false;
      // Po blędzie TTS zostanmy w aktualnym state — nastepne zdanie sprobuje.
      if (this.ttsQueue.length > 0) {
        this._playNextTts();
      } else if (this.streamComplete) {
        this._setState(STATES.LISTENING);
      }
      return;
    }

    this._setState(STATES.SPEAKING);

    const audio = new Audio();
    audio.src = url;
    audio.muted = this.speakerMuted;
    audio.crossOrigin = 'anonymous';
    this.activeAudioEl = audio;
    this.activeAudioUrl = url;

    // Podlaczamy do AudioContext zeby dostac amplitude dla setSpeechAmplitude.
    // MediaElementAudioSourceNode jest jednorazowy per <audio> element — nie
    // mozna go reuse'owac, wiec kazde zdanie = nowy element + nowy source.
    let mediaSource = null;
    let ttsAnalyser = null;
    if (this.audioCtx && this.audioCtx.state !== 'closed') {
      try {
        mediaSource = this.audioCtx.createMediaElementSource(audio);
        ttsAnalyser = this.audioCtx.createAnalyser();
        ttsAnalyser.fftSize = 1024;
        ttsAnalyser.smoothingTimeConstant = 0.5;
        mediaSource.connect(ttsAnalyser);
        mediaSource.connect(this.audioCtx.destination);
        this.ttsAnalyser = ttsAnalyser;
        this._startTtsAmpRaf();
      } catch {
        // Fallback — bez analyser. Twarz zostanie w speak ale bez RMS.
      }
    }

    const cleanup = () => {
      this._stopTtsAmpRaf();
      try { mediaSource && mediaSource.disconnect(); } catch { /* ignore */ }
      try { ttsAnalyser && ttsAnalyser.disconnect(); } catch { /* ignore */ }
      this.ttsAnalyser = null;
      try { audio.pause(); } catch { /* ignore */ }
      audio.src = '';
      try { URL.revokeObjectURL(url); } catch { /* ignore */ }
      if (this.activeAudioEl === audio) {
        this.activeAudioEl = null;
        this.activeAudioUrl = null;
        this.activeAudioCleanup = null;
      }
    };
    this.activeAudioCleanup = cleanup;

    audio.onended = () => {
      cleanup();
      this._playNextTts();
    };
    audio.onerror = () => {
      cleanup();
      this._playNextTts();
    };

    try {
      await audio.play();
    } catch (err) {
      // Autoplay block jest nieosiagalny — user juz kliknal mic, mamy gesture.
      // Ale jesli z innego powodu fail — cleanup + idz dalej.
      this.onError(err);
      cleanup();
      this._playNextTts();
    }
  }

  _stopActiveTts(_drop) {
    if (this.ttsAbortController) {
      try { this.ttsAbortController.abort(); } catch { /* ignore */ }
      this.ttsAbortController = null;
    }
    if (this.activeAudioCleanup) {
      this.activeAudioCleanup();
    }
    this.ttsPlaying = false;
  }

  _startTtsAmpRaf() {
    if (this.ttsAmpRafId !== null) return;
    const tick = () => {
      this.ttsAmpRafId = requestAnimationFrame(tick);
      if (!this.ttsAnalyser) return;
      const buf = new Float32Array(this.ttsAnalyser.fftSize);
      this.ttsAnalyser.getFloatTimeDomainData(buf);
      const rms = rmsOf(buf);
      this.faceHandle.setSpeechAmplitude(Math.min(1, rms * 4));
    };
    this.ttsAmpRafId = requestAnimationFrame(tick);
  }

  _stopTtsAmpRaf() {
    if (this.ttsAmpRafId !== null) {
      cancelAnimationFrame(this.ttsAmpRafId);
      this.ttsAmpRafId = null;
    }
    this.faceHandle.setSpeechAmplitude(0);
  }

  // ---- State management -------------------------------------------------

  _setState(next) {
    if (this.state === next) return;
    this.state = next;
    this.faceHandle.setMode(FACE_MODE[next] || 'idle');
    this.onStateChange(next);
  }
}

export default AudioPipeline;
