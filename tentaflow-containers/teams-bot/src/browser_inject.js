// =============================================================================
// Plik: browser_inject.js
// Opis: Wstrzykiwany do strony Teams skrypt — przechwytuje audio z elementow
//       <audio>/<video> przez captureStream() i wysyla PCM i16 mono 16kHz
//       do Rust przez WebSocket. Takze: injekcja audio bota do Teams przez
//       monkey-patch getUserMedia + MediaStreamTrackGenerator.
// =============================================================================

(function tentaflowAudioBridge() {
  'use strict';

  // Guard #1: uruchom TYLKO w top-frame (Teams ma kilkanascie iframe'ow,
  // kazdy dostaje evaluate_on_new_document — wszystkie prócz top powinny byc ignorowane)
  try {
    if (window.top !== window.self) return;
  } catch (_) {
    // Cross-origin iframe — nie rob nic
    return;
  }

  // Guard #2: re-injection
  if (window.__tentaflowBridge) {
    return;
  }

  // Guard #3: URL whitelist — pomijamy about:blank, chrome://, data: itp.
  const href = (location && location.href) || '';
  if (!/^https?:\/\//i.test(href)) {
    return;
  }

  window.__tentaflowBridge = true;
  console.log('[tentaflow] Bridge audio startuje w', href);

  const WS_URL = 'ws://127.0.0.1:9999/bridge';
  const TARGET_RATE = 16000;

  // Reconnect z backoffem
  let ws = null;
  let reconnectDelay = 500;
  const MAX_RECONNECT_DELAY = 10000;

  // Audio capture context (resample do 16kHz mono)
  let captureCtx = null;
  let scriptProcessor = null;
  // UWAGA: NIE uzywamy WeakSet — potrzebujemy jawnej kontroli, zeby po
  // ended track zwolnic element i pozwolic go ponownie podlaczyc przy
  // renegocjacji RTCPeerConnection (Teams rotuje track'i gdy ktos dolacza/
  // opuszcza rozmowe).
  const capturedElements = new Set();

  // Playback — MediaStreamTrackGenerator dla mic injection
  let micGenerator = null;
  let micWriter = null;
  let micBaseTimestamp = 0;

  // --------------------------------------------------------------------------
  // WebSocket bridge
  // --------------------------------------------------------------------------
  function connectWs() {
    try {
      ws = new WebSocket(WS_URL);
      ws.binaryType = 'arraybuffer';
    } catch (e) {
      console.warn('[tentaflow] WS new error', e);
      scheduleReconnect();
      return;
    }

    ws.onopen = () => {
      console.log('[tentaflow] WS polaczony z', WS_URL);
      reconnectDelay = 500;
    };

    ws.onmessage = (e) => {
      // Ramki binarne: [1 bajt typ][payload]
      // typ 0x01 = PCM i16 mono 16kHz do odtworzenia przez mic generator
      if (!(e.data instanceof ArrayBuffer)) return;
      const view = new DataView(e.data);
      if (view.byteLength < 1) return;
      const msgType = view.getUint8(0);
      if (msgType === 0x01) {
        // Skopiuj payload do osobnego, wyrownanego bufora — e.data offset 1
        // nie jest zgodne z 2-byte alignment Int16Array
        const payloadLen = e.data.byteLength - 1;
        const aligned = new ArrayBuffer(payloadLen);
        new Uint8Array(aligned).set(new Uint8Array(e.data, 1, payloadLen));
        handleMicPcm(new Int16Array(aligned));
      }
    };

    ws.onclose = () => {
      console.warn('[tentaflow] WS zamkniety');
      ws = null;
      scheduleReconnect();
    };

    ws.onerror = (e) => {
      console.warn('[tentaflow] WS blad', e);
    };
  }

  function scheduleReconnect() {
    setTimeout(() => {
      reconnectDelay = Math.min(reconnectDelay * 2, MAX_RECONNECT_DELAY);
      connectWs();
    }, reconnectDelay);
  }

  function sendCapturedPcm(i16) {
    if (!ws || ws.readyState !== WebSocket.OPEN) return;
    // Ramka: [0x02][PCM i16 LE]. Kopiujemy jako Uint8Array zeby uniknac
    // RangeError — Int16Array wymaga offset wyrownany do 2 bajtow.
    const buf = new ArrayBuffer(1 + i16.byteLength);
    const u8 = new Uint8Array(buf);
    u8[0] = 0x02;
    u8.set(new Uint8Array(i16.buffer, i16.byteOffset, i16.byteLength), 1);
    try {
      ws.send(buf);
    } catch (e) {
      console.warn('[tentaflow] ws.send blad:', e);
    }
  }

  // --------------------------------------------------------------------------
  // Audio capture przez element.captureStream()
  // --------------------------------------------------------------------------
  function ensureCaptureContext() {
    if (captureCtx) return;
    // Uzywamy DOMYSLNEGO sample rate (44.1/48kHz na Chromium). Cross-rate
    // createMediaStreamSource moze nie dzialac dobrze — pracujemy na natywnym
    // rate i downsamplowujemy w JS przed wyslaniem do Rust.
    captureCtx = new AudioContext();
    console.log('[tentaflow] AudioContext state:', captureCtx.state, 'sampleRate:', captureCtx.sampleRate);

    if (captureCtx.state === 'suspended') {
      captureCtx.resume().then(() => {
        console.log('[tentaflow] AudioContext wznowiony:', captureCtx.state);
      }).catch((e) => {
        console.warn('[tentaflow] resume() blad:', e);
      });
    }

    // ScriptProcessor — bufor 2048 @ 44.1/48kHz ~= 42-46ms chunki
    // Brak posredniego captureDest — remote streams lacza sie bezposrednio.
    scriptProcessor = captureCtx.createScriptProcessor(2048, 1, 1);
    scriptProcessor.connect(captureCtx.destination);

    const srcRate = captureCtx.sampleRate;
    const downsampleRatio = srcRate / TARGET_RATE;

    // Bufor akumulujacy probki — Rust VAD oczekuje chunków ~250ms
    // 250ms @ 16kHz = 4000 sampli (szybsza reakcja VAD)
    const CHUNK_SIZE = Math.floor(TARGET_RATE * 0.25);
    const sampleBuffer = new Int16Array(CHUNK_SIZE);
    let bufferOffset = 0;

    let processCallCount = 0;
    let lastMaxAbs = 0;
    scriptProcessor.onaudioprocess = (ev) => {
      processCallCount++;
      const f32 = ev.inputBuffer.getChannelData(0);

      const outLen = Math.floor(f32.length / downsampleRatio);
      for (let i = 0; i < outLen; i++) {
        const s = Math.max(-1, Math.min(1, f32[Math.floor(i * downsampleRatio)]));
        if (Math.abs(s) > lastMaxAbs) lastMaxAbs = Math.abs(s);
        sampleBuffer[bufferOffset++] = s < 0 ? s * 0x8000 : s * 0x7fff;
        if (bufferOffset >= CHUNK_SIZE) {
          sendCapturedPcm(sampleBuffer);
          bufferOffset = 0;
          if (processCallCount <= 5 || processCallCount % 200 === 0) {
            console.log('[tentaflow] Wyslano chunk 500ms, maxAbs od ostatniego:', lastMaxAbs.toFixed(4),
              'srcRate:', srcRate);
          }
          lastMaxAbs = 0;
        }
      }
    };
    console.log('[tentaflow] ScriptProcessor podlaczony, bufferSize:', scriptProcessor.bufferSize,
      'srcRate:', srcRate, 'targetRate:', TARGET_RATE, 'chunkSize:', CHUNK_SIZE);
  }

  // Podlacza stream (z elementu lub RTCPeerConnection) bezposrednio do procesora.
  // attachedSources: track.id -> { node, element? } — element jest przypisany
  // gdy stream pochodzi z HTMLAudioElement, zeby po ended umiec go zdjac z
  // capturedElements i pozwolic ponownie podlaczyc.
  const attachedTracks = new Set();
  const attachedSources = new Map();
  function attachStream(stream, source, element) {
    if (!stream || stream.getAudioTracks().length === 0) return;
    ensureCaptureContext();
    try {
      const tracks = stream.getAudioTracks();
      const t0 = tracks[0];
      // Ignoruj martwe track'i
      if (t0.readyState === 'ended') {
        console.log('[tentaflow] Track juz ended, nie podlaczam', t0.id, 'z', source);
        return;
      }
      // Deduplikacja po track id
      if (attachedSources.has(t0.id)) {
        return;
      }
      tracks.forEach((track) => {
        if (attachedTracks.has(track.id)) return;
        attachedTracks.add(track.id);
        track.addEventListener('mute', () => console.log('[tentaflow] track MUTE', source, track.id));
        track.addEventListener('unmute', () => console.log('[tentaflow] track UNMUTE', source, track.id));
        track.addEventListener('ended', () => {
          console.log('[tentaflow] track ENDED', source, track.id, '— zwalniam element i wymuszam rescan');
          const entry = attachedSources.get(track.id);
          if (entry) {
            try { entry.node.disconnect(); } catch (_) {}
            // Zwolnij element zeby mogl byc ponownie przeskanowany
            if (entry.element) {
              capturedElements.delete(entry.element);
            }
            attachedSources.delete(track.id);
          }
          attachedTracks.delete(track.id);
          // Natychmiastowy rescan — Teams moze juz miec nowy track
          setTimeout(scanAndAttach, 100);
          setTimeout(scanAndAttach, 500);
          setTimeout(scanAndAttach, 1500);
        });
      });
      const src = captureCtx.createMediaStreamSource(stream);
      src.connect(scriptProcessor);
      attachedSources.set(t0.id, { node: src, element });
      console.log('[tentaflow] Podlaczono stream z', source,
        'tracks:', tracks.length,
        'readyState:', t0 && t0.readyState,
        'muted:', t0 && t0.muted,
        'enabled:', t0 && t0.enabled,
        'id:', t0 && t0.id);
    } catch (e) {
      console.warn('[tentaflow] Blad createMediaStreamSource dla', source, e);
    }
  }

  function attachElementStream(el) {
    if (!el || capturedElements.has(el)) return;
    let stream = null;
    try {
      if (el.srcObject instanceof MediaStream) {
        stream = el.srcObject;
      } else if (typeof el.captureStream === 'function') {
        stream = el.captureStream();
      }
    } catch (e) {
      return;
    }
    if (!stream || stream.getAudioTracks().length === 0) return;
    const tracks = stream.getAudioTracks();
    // Jesli wszystkie track'i w tym streamie sa ended, pomijamy (nie ma sensu)
    if (tracks.every(t => t.readyState === 'ended')) {
      return;
    }
    capturedElements.add(el);
    try {
      if (el.muted) el.muted = false;
      if (el.volume === 0) el.volume = 1;
      if (el.paused && el.play) el.play().catch(() => {});
    } catch (_) {}
    attachStream(stream, 'element:' + el.tagName, el);
  }

  // Hook RTCPeerConnection — lapie remote audio tracks od razu gdy Teams je otrzyma.
  // To jest PRAWDZIWE zrodlo remote audio, a nie HTMLAudioElement (ktory moze byc
  // placeholder albo wyciszona kopia).
  function hookRTCPeerConnection() {
    if (typeof RTCPeerConnection === 'undefined') return;
    const OrigPC = window.RTCPeerConnection;
    function PatchedPC(...args) {
      const pc = new OrigPC(...args);
      console.log('[tentaflow] RTCPeerConnection utworzony');
      pc.addEventListener('track', (event) => {
        const track = event.track;
        console.log('[tentaflow] pc.ontrack kind:', track.kind, 'id:', track.id,
          'muted:', track.muted, 'readyState:', track.readyState,
          'streams:', event.streams.length);
        if (track.kind !== 'audio') return;
        // Stworz dedykowany MediaStream tylko z tym trackiem
        const stream = new MediaStream([track]);
        attachStream(stream, 'pc.ontrack');
        // Takze dolacz wszystkie streamy z event (Teams moze miec wiele)
        event.streams.forEach((s, i) => attachStream(s, 'pc.ontrack.streams[' + i + ']'));
      });
      return pc;
    }
    PatchedPC.prototype = OrigPC.prototype;
    Object.setPrototypeOf(PatchedPC, OrigPC);
    window.RTCPeerConnection = PatchedPC;
    console.log('[tentaflow] RTCPeerConnection hook zainstalowany');
  }

  function scanAndAttach() {
    const els = document.querySelectorAll('audio, video');
    els.forEach(attachElementStream);
  }

  // Health check — jesli ZADEN z podlaczonych sources nie ma live track'ow,
  // zwolnij wszystkie i zrob pelny rescan. Pomaga gdy Teams zmienia pipeline
  // audio w sposob ktory omija nasze event handlery.
  function healthCheck() {
    // Sprawdz ile attachedSources ma live tracks
    let liveCount = 0;
    let deadIds = [];
    for (const [trackId, entry] of attachedSources.entries()) {
      // Nie znamy bezposrednio track obiektu z id po fakcie — sprawdzamy
      // czy source node'a w AudioContext jest dalej podlaczone. Proxy: jesli
      // ma ended event juz przeszedl, to entry bylo juz usuniete, wiec
      // wszystko co jest w mapie jest "zywe" z nasza perspektywa.
      liveCount++;
    }

    // Alternatywny check: przejrzyj wszystkie audio/video i ich srcObject
    const els = document.querySelectorAll('audio, video');
    let liveElementTracks = 0;
    els.forEach((el) => {
      if (el.srcObject instanceof MediaStream) {
        el.srcObject.getAudioTracks().forEach((t) => {
          if (t.readyState === 'live' && !t.muted) liveElementTracks++;
        });
      }
    });

    if (liveCount === 0 && liveElementTracks > 0) {
      console.log('[tentaflow] Health check: 0 podlaczone, ale', liveElementTracks,
        'live element tracks — force rescan');
      // Reset capturedElements zeby rescan ich znow zlapal
      capturedElements.clear();
      scanAndAttach();
    }
  }

  // MutationObserver — wykrywa nowe elementy audio/video dodawane dynamicznie
  // ORAZ zmiany atrybutow na istniejacych (srcObject moze byc podmieniony bez
  // usuniecia elementu, np. gdy Teams rotuje audio pipeline).
  function installObserver() {
    const obs = new MutationObserver((muts) => {
      for (const m of muts) {
        m.addedNodes.forEach((node) => {
          if (!(node instanceof Element)) return;
          if (node.tagName === 'AUDIO' || node.tagName === 'VIDEO') {
            setTimeout(() => attachElementStream(node), 100);
          }
          node.querySelectorAll && node.querySelectorAll('audio, video').forEach((el) => {
            setTimeout(() => attachElementStream(el), 100);
          });
        });
      }
    });
    obs.observe(document.documentElement, { childList: true, subtree: true });

    // Re-scan co 1s — szybsza reakcja na podmiany srcObject (Teams renegocjacja).
    setInterval(scanAndAttach, 1000);
    // Health check co 2s — jesli wszystkie sources umarly, force recover.
    setInterval(healthCheck, 2000);
  }

  // --------------------------------------------------------------------------
  // Microphone injection — monkey-patch getUserMedia
  // Ostroznie: Teams ma skomplikowany pipeline media, wszystko w try/catch
  // zeby blad w naszym patchu nie wywalil calego Teams.
  // --------------------------------------------------------------------------
  function setupMicInjection() {
    // MediaStreamTrackGenerator dostepny w Chromium 94+ tylko po wlaczeniu
    // --enable-experimental-web-platform-features
    if (typeof MediaStreamTrackGenerator === 'undefined') {
      console.warn('[tentaflow] MediaStreamTrackGenerator niedostepny — mic injection wylaczone');
      return;
    }
    if (!navigator || !navigator.mediaDevices || !navigator.mediaDevices.getUserMedia) {
      console.warn('[tentaflow] navigator.mediaDevices niedostepne — mic injection wylaczone');
      return;
    }

    try {
      micGenerator = new MediaStreamTrackGenerator({ kind: 'audio' });
      micWriter = micGenerator.writable.getWriter();
      micBaseTimestamp = 0;
    } catch (e) {
      console.warn('[tentaflow] Blad tworzenia MediaStreamTrackGenerator', e);
      return;
    }

    const origGum = navigator.mediaDevices.getUserMedia.bind(navigator.mediaDevices);
    navigator.mediaDevices.getUserMedia = async function (constraints) {
      try {
        if (!constraints || !constraints.audio) {
          return origGum(constraints);
        }
        console.log('[tentaflow] Przechwycono getUserMedia — wstrzykuje bot mic');
        if (constraints.video) {
          try {
            const videoStream = await origGum({ video: constraints.video });
            const combined = new MediaStream();
            videoStream.getVideoTracks().forEach((t) => combined.addTrack(t));
            combined.addTrack(micGenerator);
            return combined;
          } catch (_) {
            return new MediaStream([micGenerator]);
          }
        }
        return new MediaStream([micGenerator]);
      } catch (e) {
        console.warn('[tentaflow] getUserMedia patch blad, fallback na oryginalny', e);
        return origGum(constraints);
      }
    };
  }

  function handleMicPcm(i16) {
    if (!micWriter) return;
    try {
      const audioData = new AudioData({
        format: 's16',
        sampleRate: TARGET_RATE,
        numberOfFrames: i16.length,
        numberOfChannels: 1,
        timestamp: micBaseTimestamp,
        data: i16,
      });
      micBaseTimestamp += Math.round((i16.length / TARGET_RATE) * 1_000_000);
      micWriter.write(audioData);
    } catch (e) {
      console.warn('[tentaflow] AudioData write error', e);
    }
  }

  // --------------------------------------------------------------------------
  // Bootstrap
  // --------------------------------------------------------------------------
  function bootstrap() {
    // WAZNE: hook RTCPeerConnection musi byc ZANIM Teams stworzy pc.
    // Tutaj jestesmy w evaluate_on_new_document, przed jakimkolwiek JS strony,
    // wiec jestesmy bezpieczni.
    try {
      hookRTCPeerConnection();
    } catch (e) {
      console.warn('[tentaflow] hookRTCPeerConnection blad', e);
    }
    try {
      setupMicInjection();
    } catch (e) {
      console.warn('[tentaflow] setupMicInjection blad', e);
    }
    try {
      scanAndAttach();
      installObserver();
    } catch (e) {
      console.warn('[tentaflow] install observer blad', e);
    }
    connectWs();
    console.log('[tentaflow] Bridge audio zainicjalizowany');
  }

  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', bootstrap);
  } else {
    bootstrap();
  }
})();
