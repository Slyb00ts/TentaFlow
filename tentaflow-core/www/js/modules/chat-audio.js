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
import { PCM_WORKLET_SRC, floatToWav, rmsOf } from '../lib/mic-capture.js';

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
  // Barge-in podczas TTS: mic lapie wlasny glos AI mimo echoCancellation. Próg
  // = rmsThreshold*mult + poziom_wyjscia_TTS*echoFactor. Dzieki temu echo NIE
  // przerywa odpowiedzi (urywany tekst!), a wyrazna mowa usera z bliska — tak.
  bargeInThresholdMult: 3.0,
  bargeInEchoFactor: 0.6,
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

export class AudioPipeline {
  constructor(opts) {
    if (!opts || !opts.faceHandle) {
      throw new Error('AudioPipeline: faceHandle is required');
    }
    this.opts = { ...DEFAULTS, ...(opts.config || {}) };
    this.conv = opts.conv || null;
    this.faceHandle = opts.faceHandle;
    this.onUserUtterance = opts.onUserUtterance || (() => {});
    // Binarny most: emituje całą wypowiedź (WAV Uint8Array + sampleRate) do
    // callera, który odpala flow przez FlowInvoke. Flow robi STT→LLM→TTS.
    this.onUtteranceAudio = opts.onUtteranceAudio || (() => {});
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
    // Surowe chunki audio (WAV bytes) z flow. Planujemy je na osi czasu
    // AudioContext (gapless) zamiast <audio>+onended (zawodne: latencja grafu,
    // niepewny onended -> nakladanie + leak RAF).
    this.ttsQueue = [];
    this.streamComplete = false;
    this.ttsSources = new Set(); // aktywne AudioBufferSourceNode
    this.ttsNextStartTime = 0; // kursor planowania na audioCtx.currentTime
    this.ttsPumping = false; // serializacja decode'u kolejki
    this.ttsGen = 0; // bump przy stop/barge-in — porzuca chunk dekodowany w locie
    this.ttsGain = null; // master gain (mute speakera) -> analyser -> destination
    this.ttsAbortController = null;
    this.sttAbortController = null;

    // RAF tick
    this.rafId = null;
    this.lastTickAt = 0;

    // TTS amplitude monitoring (osobny analyser dla audio output).
    this.ttsAnalyser = null;
    this.ttsAmpRafId = null;
    this.lastTtsRms = 0; // poziom wyjscia TTS — echo-guard dla barge-in
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
    // audioCtx zamkniety — persistent graf TTS (gain/analyser) jest martwy.
    // Zerujemy, zeby `_ensureTtsGraph` odtworzyl go po kolejnym start().
    this.ttsGain = null;
    this.ttsAnalyser = null;
    this.ttsNextStartTime = 0;
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
      // Mic OFF = twarz biala (idle) + brak zielonych paskow. FSM zostaje w
      // LISTENING, ale wizualnie pokazujemy ze nie nasluchujemy.
      this.faceHandle.setMode('idle');
    } else {
      // Mic ON — wroc do trybu zgodnego z aktualnym stanem FSM.
      this.faceHandle.setMode(FACE_MODE[this.state] || 'idle');
    }
  }

  toggleSpeaker() {
    this.speakerMuted = !this.speakerMuted;
    if (this.ttsGain) {
      this.ttsGain.gain.value = this.speakerMuted ? 0 : 1;
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

  // ---- Caller-driven flow response feed ---------------------------------
  // Flow (przez binary FlowInvoke) odsyła gotowe audio + tekst. chat.js woła
  // playAudioChunk dla każdego audio chunka i finishResponse na końcu stream'u.
  // Tekst trafia do bąbla po stronie chat.js, nie tutaj.

  playAudioChunk(bytes, mime) {
    if (!bytes || bytes.length === 0) return;
    if (this.state === STATES.THINKING || this.state === STATES.TRANSCRIBING) {
      this._setState(STATES.SPEAKING);
    }
    this._enqueueAudio({ bytes, mime: mime || 'audio/wav' });
  }

  finishResponse() {
    this.streamComplete = true;
    if (this.ttsSources.size === 0 && this.ttsQueue.length === 0 && !this.ttsPumping) {
      // Pusta odpowiedź albo całe audio już odtworzone — wracamy do listen.
      this._setState(STATES.LISTENING);
    }
  }

  feedAssistantError(_err) {
    // Caller juz zatoastowal blad — my tylko sprzatamy lokalny stan.
    this.streamComplete = true;
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
      if (!this.vadInSpeech) {
        this.speechMsAccumulated += dtMs;
        if (this.speechMsAccumulated >= this.opts.minSpeechMs) {
          // Debounce: wymagaj minSilenceBeforeSpeechMs ciszy od konca poprzedniej
          // wypowiedzi, zeby ogon poprzedniej mowy nie retriggerowal nowej.
          // UWAGA: `silenceSinceSpeechEnd` sprawdzamy PRZED wyzerowaniem — zeruje
          // sie dopiero gdy faktycznie startujemy wypowiedz, inaczej kazda
          // kolejna mowa po pierwszej widzialaby 0 i byla blokowana na zawsze.
          if (this.lastSpeechAt > 0 && this.silenceSinceSpeechEnd < this.opts.minSilenceBeforeSpeechMs) {
            this.speechMsAccumulated = 0;
            return;
          }
          this.silenceSinceSpeechEnd = 0;
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

    // Binarny most: NIE robimy STT po REST. Emitujemy całą wypowiedź (WAV) do
    // callera, który odpala flow przez binary FlowInvoke. Flow robi
    // STT→LLM→TTS i odsyła przeplatane tekst+audio (playAudioChunk +
    // onUserUtterance dla transkryptu).
    const wav = floatToWav(merged, this.frameSampleRate, 16000);
    this._setState(STATES.THINKING);
    this.streamComplete = false;
    this.onUtteranceAudio(wav, this.frameSampleRate >= 16000 ? 16000 : this.frameSampleRate);
  }

  _abortStt() {
    if (this.sttAbortController) {
      try { this.sttAbortController.abort(); } catch { /* ignore */ }
      this.sttAbortController = null;
    }
  }

  // ---- Barge-in ---------------------------------------------------------

  _bargeInStep(rms, dtMs) {
    // Echo-guard: mic lapie wlasny glos AI (echo) mimo echoCancellation. Próg
    // barge-in podnosimy o czesc poziomu wyjscia TTS — echo nie przerywa
    // odpowiedzi (urywany tekst), a wyrazna mowa usera z bliska przekracza próg.
    const guard =
      this.adaptiveThreshold * this.opts.bargeInThresholdMult +
      this.lastTtsRms * this.opts.bargeInEchoFactor;
    if (rms >= guard) {
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

  // ---- Audio playback (gapless scheduling) ------------------------------
  // Chunki audio (WAV bytes) z flow planujemy na osi czasu AudioContext: kazdy
  // start = max(now, kursor); kursor += dlugosc bufora. Daje brak nakladania i
  // luk. <audio>+createMediaElementSource+onended bylo zawodne (latencja grafu,
  // niepewny onended) — stad nakladajace sie zdania i spinning RAF (100% CPU).

  // Persistent graf wyjsciowy: master gain (mute speakera) -> analyser (RMS dla
  // twarzy) -> destination. Tworzony raz, kazdy BufferSource sie do niego pina.
  _ensureTtsGraph() {
    if (!this.audioCtx || this.audioCtx.state === 'closed') return null;
    if (!this.ttsGain) {
      this.ttsGain = this.audioCtx.createGain();
      this.ttsGain.gain.value = this.speakerMuted ? 0 : 1;
      this.ttsAnalyser = this.audioCtx.createAnalyser();
      this.ttsAnalyser.fftSize = 1024;
      this.ttsAnalyser.smoothingTimeConstant = 0.5;
      this.ttsGain.connect(this.ttsAnalyser);
      this.ttsAnalyser.connect(this.audioCtx.destination);
    }
    return this.ttsGain;
  }

  _enqueueAudio(item) {
    if (!item || !item.bytes || item.bytes.length === 0) return;
    this.ttsQueue.push(item);
    this._pumpTtsQueue();
  }

  // Dekoduje i planuje chunki po kolei. Serializowany flagą `ttsPumping`, bo
  // `decodeAudioData` jest async — bez tego dwa wywolania scigalyby sie o kursor.
  async _pumpTtsQueue() {
    if (this.ttsPumping) return;
    this.ttsPumping = true;
    try {
      while (this.ttsQueue.length > 0) {
        if (!this.audioCtx || this.audioCtx.state === 'closed') {
          this.ttsQueue = [];
          break;
        }
        const item = this.ttsQueue.shift();
        const gen = this.ttsGen;
        if (this.audioCtx.state === 'suspended') {
          try { await this.audioCtx.resume(); } catch { /* ignore */ }
        }
        let audioBuffer;
        try {
          // decodeAudioData chce ArrayBuffer i go „odbiera" — dajemy zawsze
          // swieza kopie. `bytes` moze byc Uint8Array (widok) lub ArrayBuffer.
          const b = item.bytes;
          const ab = b instanceof ArrayBuffer
            ? b.slice(0)
            : b.buffer.slice(b.byteOffset, b.byteOffset + b.byteLength);
          audioBuffer = await this.audioCtx.decodeAudioData(ab);
        } catch (err) {
          this.onError(err);
          continue;
        }
        // Stan mogl sie zmienic w trakcie await (stop/barge-in zamknal ctx
        // albo wyczyscil kolejke) — porzuc ten chunk zamiast grac po przerwaniu.
        if (gen !== this.ttsGen) continue;
        if (!this.audioCtx || this.audioCtx.state === 'closed') break;
        const gain = this._ensureTtsGraph();
        if (!gain) break;

        const src = this.audioCtx.createBufferSource();
        src.buffer = audioBuffer;
        src.connect(gain);

        const now = this.audioCtx.currentTime;
        const startAt = Math.max(now, this.ttsNextStartTime);
        src.start(startAt);
        this.ttsNextStartTime = startAt + audioBuffer.duration;
        this.ttsSources.add(src);

        this._setState(STATES.SPEAKING);
        this._startTtsAmpRaf();

        src.onended = () => {
          this.ttsSources.delete(src);
          try { src.disconnect(); } catch { /* ignore */ }
          // Cala odpowiedz odtworzona — zaden source nie gra i nic nie czeka.
          if (
            this.ttsSources.size === 0 &&
            this.ttsQueue.length === 0 &&
            !this.ttsPumping &&
            this.streamComplete
          ) {
            this._setState(STATES.LISTENING);
          }
        };
      }
    } finally {
      this.ttsPumping = false;
      // Jesli kolejka pusta, nic nie gra, a stream skonczony — wroc do listen
      // (gdy ostatni `onended` wyscignal `ttsPumping=false`).
      if (
        this.ttsSources.size === 0 &&
        this.ttsQueue.length === 0 &&
        this.streamComplete &&
        this.state === STATES.SPEAKING
      ) {
        this._setState(STATES.LISTENING);
      }
    }
  }

  _stopActiveTts(_drop) {
    this.ttsGen = (this.ttsGen + 1) | 0;
    if (this.ttsAbortController) {
      try { this.ttsAbortController.abort(); } catch { /* ignore */ }
      this.ttsAbortController = null;
    }
    for (const src of this.ttsSources) {
      try { src.onended = null; src.stop(); src.disconnect(); } catch { /* ignore */ }
    }
    this.ttsSources.clear();
    this.ttsNextStartTime = 0;
    this._stopTtsAmpRaf();
  }

  _startTtsAmpRaf() {
    if (this.ttsAmpRafId !== null) return;
    const tick = () => {
      // Zyje tylko dopoki cos faktycznie gra. Brak aktywnych source'ow albo
      // analysera = stop (NIE reschedule przed sprawdzeniem) — to gwarantuje ze
      // RAF nie kreci sie w nieskonczonosc po zakonczeniu audio.
      if (!this.ttsAnalyser || this.ttsSources.size === 0) {
        this.ttsAmpRafId = null;
        this.faceHandle.setSpeechAmplitude(0);
        return;
      }
      this.ttsAmpRafId = requestAnimationFrame(tick);
      const buf = new Float32Array(this.ttsAnalyser.fftSize);
      this.ttsAnalyser.getFloatTimeDomainData(buf);
      const rms = rmsOf(buf);
      this.lastTtsRms = rms; // echo-guard dla barge-in
      this.faceHandle.setSpeechAmplitude(Math.min(1, rms * 4));
    };
    this.ttsAmpRafId = requestAnimationFrame(tick);
  }

  _stopTtsAmpRaf() {
    if (this.ttsAmpRafId !== null) {
      cancelAnimationFrame(this.ttsAmpRafId);
      this.ttsAmpRafId = null;
    }
    this.lastTtsRms = 0;
    this.faceHandle.setSpeechAmplitude(0);
  }

  // ---- State management -------------------------------------------------

  _setState(next) {
    if (this.state === next) return;
    // Amplituda TTS (osobny RAF) ma zyc TYLKO w SPEAKING. Wychodzac z tego
    // stanu twardo go ubijamy — gwarancja ze zaden RAF nie przezyje konca
    // odpowiedzi (zabezpieczenie przed 100% CPU gdy `onended` nie odpali).
    if (this.state === STATES.SPEAKING && next !== STATES.SPEAKING) {
      this._stopTtsAmpRaf();
    }
    this.state = next;
    this.faceHandle.setMode(FACE_MODE[next] || 'idle');
    this.onStateChange(next);
  }
}

export default AudioPipeline;
