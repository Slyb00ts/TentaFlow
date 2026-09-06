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

  // Obiekt-bridge (nie boolean) — trzyma flagi dostepnosci feature'ow dla Rusta
  // oraz marker setupDone zeby ponowny evaluate_on_new_document nie dublowal
  // intervali/patchow.
  window.__tentaflowBridge = { setupDone: false };
  window.__tentaflowVideoAvailable = false;
  console.log('[tentaflow] Bridge audio startuje w', href);

  // Two interval pools. Audio bridge intervals (mic capture, roster scan,
  // active speaker) get torn down whenever the audio websocket disconnects
  // because they push data through that socket — keeping them alive after
  // the bridge dies just produces noise. The video pipeline has nothing to
  // do with the audio websocket: the canvas captureStream feeds Teams
  // directly, so killing the draw loop on WS close left Teams holding the
  // last frame (usually still mostly empty) forever and the tile rendered
  // black. Keep video intervals in their own pool so cleanupTentaflow()
  // does not touch them.
  const __tfIntervals = [];
  const __tfVideoIntervals = [];
  function registerInterval(id) {
    __tfIntervals.push(id);
    return id;
  }
  function registerVideoInterval(id) {
    __tfVideoIntervals.push(id);
    return id;
  }
  function cleanupTentaflow() {
    while (__tfIntervals.length) {
      const id = __tfIntervals.pop();
      try { clearInterval(id); } catch (_) {}
    }
  }

  // Port mostu WS jest dynamiczny: dla native bota MeetingManager alokuje port
  // i wstrzykuje go przez `window.__tfBridgePort` przed zaladowaniem strony
  // (evaluate_on_new_document w browser.rs). Docker dziala w izolowanym
  // network namespace, wiec stary fallback 9999 jest tam wciaz prawidlowy.
  const BRIDGE_PORT = (typeof window !== 'undefined' && window.__tfBridgePort) || 9999;
  const WS_URL = `ws://127.0.0.1:${BRIDGE_PORT}/bridge`;
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
  let videoFace = null;
  let videoSpeech = null;
  let videoSpeechContext = null;
  let videoSpeechNextTime = 0;

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
      console.warn('[tentaflow] WS zamkniety — czyszcze interwaly');
      ws = null;
      cleanupTentaflow();
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
          // Licznik chunkow z cisza — dla healthCheck auto-rebuild
          if (lastMaxAbs < 0.0005) {
            silentChunkCount++;
          } else {
            silentChunkCount = 0;
          }
          if (processCallCount <= 5 || processCallCount % 200 === 0) {
            console.log('[tentaflow] Wyslano chunk 500ms, maxAbs od ostatniego:', lastMaxAbs.toFixed(4),
              'srcRate:', srcRate, 'silent:', silentChunkCount);
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
  // knownStreams: wszystkie streamy z pc.ontrack — uzywane przez rebuild
  // gdy healthCheck wykryje ze audio zamarlo. Klucz = track.id, wartosc =
  // { stream, source: 'pc.ontrack', element? } — trzymamy tylko dopoki
  // track jest live.
  const knownStreams = new Map();
  // Licznik chunkow z cisza — inkrementowany w onaudioprocess, sprawdzany
  // w healthCheck. > 20 (= 5s) -> force rebuild capture pipeline.
  let silentChunkCount = 0;
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
    if (!(window.__tentaflowPeerConnections instanceof Set)) {
      window.__tentaflowPeerConnections = new Set();
    }
    const OrigPC = window.RTCPeerConnection;
    function PatchedPC(...args) {
      const pc = new OrigPC(...args);
      window.__tentaflowPeerConnections.add(pc);
      pc.addEventListener('connectionstatechange', function () {
        if (pc.connectionState === 'closed' || pc.connectionState === 'failed') {
          window.__tentaflowPeerConnections.delete(pc);
        }
      });
      console.log('[tentaflow] RTCPeerConnection utworzony');
      pc.addEventListener('track', (event) => {
        const track = event.track;
        console.log('[tentaflow] pc.ontrack kind:', track.kind, 'id:', track.id,
          'muted:', track.muted, 'readyState:', track.readyState,
          'streams:', event.streams.length);
        if (track.kind !== 'audio') return;
        // Stworz dedykowany MediaStream tylko z tym trackiem
        const stream = new MediaStream([track]);
        // Zapamietaj dla rebuild — usuwamy gdy track ended
        knownStreams.set(track.id, stream);
        track.addEventListener('ended', () => { knownStreams.delete(track.id); });
        attachStream(stream, 'pc.ontrack');
        // Takze dolacz wszystkie streamy z event (Teams moze miec wiele)
        event.streams.forEach((s, i) => {
          s.getAudioTracks().forEach((t) => {
            knownStreams.set(t.id, s);
            t.addEventListener('ended', () => { knownStreams.delete(t.id); });
          });
          attachStream(s, 'pc.ontrack.streams[' + i + ']');
        });
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

  // Force rebuild capture pipeline — gdy dzwiek zamarl mimo ze track jest live.
  // Chromium nie zawsze emituje mute/ended event gdy MediaStreamSource przestaje
  // dostarczac data (np. po wewnetrznej renegocjacji transceivera). Jedyny
  // sposob naprawy: zniszcz AudioContext i odbuduj od zera z zapamietanymi
  // streamami + rescan DOM.
  function rebuildCapturePipeline(reason) {
    console.warn('[tentaflow] REBUILD capture pipeline, reason:', reason,
      'knownStreams:', knownStreams.size,
      'attachedSources:', attachedSources.size);
    try {
      // Disconnect wszystkich source nodes
      for (const [_, entry] of attachedSources.entries()) {
        try { entry.node.disconnect(); } catch (_) {}
      }
      attachedSources.clear();
      attachedTracks.clear();
      capturedElements.clear();
      // Zamknij stary AudioContext
      if (captureCtx) {
        try {
          if (scriptProcessor) scriptProcessor.disconnect();
        } catch (_) {}
        try { captureCtx.close(); } catch (_) {}
        captureCtx = null;
        scriptProcessor = null;
      }
    } catch (e) {
      console.warn('[tentaflow] rebuild cleanup blad:', e);
    }
    // Reset licznika ciszy zeby kolejny rebuild nie wystartowal od razu
    silentChunkCount = 0;
    // Re-attach wszystkie znane streamy (filtruje live tracks)
    const freshStreams = [];
    for (const [trackId, stream] of knownStreams.entries()) {
      const tracks = stream.getAudioTracks();
      if (tracks.length === 0 || tracks.every(t => t.readyState === 'ended')) {
        knownStreams.delete(trackId);
        continue;
      }
      freshStreams.push({ trackId, stream });
    }
    console.log('[tentaflow] rebuild — re-attach', freshStreams.length, 'streamow');
    freshStreams.forEach(({ stream }) => {
      attachStream(stream, 'rebuild:pc.ontrack');
    });
    // I rescan DOM na wypadek nowych <audio>/<video>
    scanAndAttach();
  }

  // Health check co 2s — dwa scenariusze:
  // 1. maxAbs cisza przez >20 chunkow (~5s) przy zywych trackach → rebuild
  // 2. Zadne attached sources, ale sa live elementy DOM → rescan (legacy)
  function healthCheck() {
    // Scenariusz 1: cisza przy zywych trackach
    const hasLiveKnown = Array.from(knownStreams.values()).some(s =>
      s.getAudioTracks().some(t => t.readyState === 'live'));
    if (silentChunkCount > 20 && hasLiveKnown) {
      rebuildCapturePipeline('silent_chunks=' + silentChunkCount);
      return;
    }

    // Scenariusz 2: brak attached sources przy obecnosci elementow DOM
    const els = document.querySelectorAll('audio, video');
    let liveElementTracks = 0;
    els.forEach((el) => {
      if (el.srcObject instanceof MediaStream) {
        el.srcObject.getAudioTracks().forEach((t) => {
          if (t.readyState === 'live' && !t.muted) liveElementTracks++;
        });
      }
    });
    if (attachedSources.size === 0 && liveElementTracks > 0) {
      console.log('[tentaflow] Health check: 0 podlaczone, ale', liveElementTracks,
        'live element tracks — force rescan');
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
    registerInterval(setInterval(scanAndAttach, 1000));
    // Health check co 2s — jesli wszystkie sources umarly, force recover.
    registerInterval(setInterval(healthCheck, 2000));
  }


  // --------------------------------------------------------------------------
  // Microphone injection — monkey-patch getUserMedia
  // Ostroznie: Teams ma skomplikowany pipeline media, wszystko w try/catch
  // zeby blad w naszym patchu nie wywalil calego Teams.
  // --------------------------------------------------------------------------
  function setupMicInjection() {
    if (window.__tentaflowBridge && window.__tentaflowBridge.micSetupDone) return;
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
      // Sync deviceId z fake entry z enumerateDevices override.
      try {
        const orig = (micGenerator.getSettings && micGenerator.getSettings()) || {};
        const patched = Object.assign({}, orig, {
          deviceId: 'tentaflow-mic-default',
          groupId: 'tentaflow-group',
        });
        Object.defineProperty(micGenerator, 'getSettings', {
          configurable: true,
          value: () => Object.assign({}, patched),
        });
      } catch (_) {}
      // Eksponuj na window zeby post-join replaceTrack mogl wymusic ze
      // KAZDY audio sender w pc uzywa naszego micGenerator.
      window.__tentaflowMicGenerator = micGenerator;
    } catch (e) {
      console.warn('[tentaflow] Blad tworzenia MediaStreamTrackGenerator', e);
      return;
    }

    const origGum = navigator.mediaDevices.getUserMedia.bind(navigator.mediaDevices);
    navigator.mediaDevices.getUserMedia = async function (constraints) {
      try {
        if (!constraints || (!constraints.audio && !constraints.video)) {
          return origGum(constraints);
        }
        console.log('[tentaflow] Przechwycono getUserMedia audio:', !!constraints.audio,
          'video:', !!constraints.video);

        // Teams' MediaAgent ('Active device not found') refuses every
        // frame from a track whose settings.deviceId is not present in
        // enumerateDevices(). Patch getSettings() on the synthetic tracks
        // to claim the same deviceId / groupId as the real Chromium fake
        // input. Without this the test harness shows healthy stream but
        // Teams renders the tile black.
        let realVid = null;
        let realAud = null;
        try {
          const realDevs = await navigator.mediaDevices.enumerateDevices();
          realVid = realDevs.find((d) => d.kind === 'videoinput') || null;
          realAud = realDevs.find((d) => d.kind === 'audioinput') || null;
        } catch (_) {}
        const reportSettings = (track, real) => {
          if (!track || !real) return;
          try {
            const orig = (track.getSettings && track.getSettings()) || {};
            const patched = Object.assign({}, orig, {
              deviceId: real.deviceId,
              groupId: real.groupId || orig.groupId,
            });
            Object.defineProperty(track, 'getSettings', {
              configurable: true,
              value: () => Object.assign({}, patched),
            });
          } catch (_) {}
        };
        const combined = new MediaStream();
        if (constraints.audio && micGenerator) {
          reportSettings(micGenerator, realAud);
          combined.addTrack(micGenerator);
        }
        if (constraints.video && videoGenerator) {
          reportSettings(videoGenerator, realVid);
          combined.addTrack(videoGenerator);
        }
        if (combined.getTracks().length > 0) return combined;
        return origGum(constraints);
      } catch (e) {
        console.warn('[tentaflow] getUserMedia patch blad, fallback na oryginalny', e);
        return origGum(constraints);
      }
    };

    // Teams calls enumerateDevices() before ever touching getUserMedia.
    // We deliberately do NOT override enumerateDevices, do NOT touch
    // navigator.permissions.query, and do NOT patch track.getSettings().
    // Stacking these proxies on top of Chromium's native device pipeline
    // ended up making Teams render the camera/mic toggles aria-disabled
    // (rolling your own PermissionStatus shape that does not satisfy
    // Teams' duck-typing, or relabelling enumerateDevices entries that
    // Teams cross-references in ways the override can't anticipate).
    // Browser.setPermission via CDP and getUserMedia replacement are
    // sufficient.

    if (window.__tentaflowBridge) window.__tentaflowBridge.micSetupDone = true;
  }

  // Roster i active-speaker zostaly przeniesione do installTentaflowDomBridge()
  // ponizej (push-based przez CDP binding `__tentaflowEvent`). Stary pollingowy
  // pipeline przez WS port 9999 (opcodes 0x03 / 0x04) zostal usuniety.

  // --------------------------------------------------------------------------
  // Video injection — kamerka bota (avatar 1280x720 @ 30fps)
  // --------------------------------------------------------------------------
  let videoGenerator = null;
  let videoWriter = null;
  let videoCanvas = null;
  let videoFrameTimestamp = 0;
  let videoWritePending = false;
  async function setupVideoInjection() {
    if (window.__tentaflowBridge && window.__tentaflowBridge.videoSetupDone) return;
    // Switched away from MediaStreamTrackGenerator + VideoFrame: setInterval
    // racing the async writer.write() pipe-locked the writer (Teams showed
    // "Your video stopped working" once a single write was still pending
    // when the next tick fired). HTMLCanvasElement.captureStream() owns the
    // frame timing internally, paces itself against compositor vsync, and
    // never throws on backpressure — it just drops the frame. Battle-tested
    // path that every webcam-replacement plugin uses.
    if (typeof HTMLCanvasElement === 'undefined' ||
        !HTMLCanvasElement.prototype.captureStream) {
      console.warn('[tentaflow] canvas.captureStream niedostepny — video injection wylaczone');
      window.__tentaflowVideoAvailable = false;
      return;
    }
    // Flag the capability the moment we know we will set the track up. The
    // Rust side polls this flag at the prejoin dialog with a 2s deadline; if
    // we wait until the end of setupVideoInjection (createElement, append,
    // captureStream, draw) the polling can finish first on slow machines
    // and the bot falls back to "Continue without audio or video".
    window.__tentaflowVideoAvailable = true;
    const W = 1280, H = 720;
    // captureStream samples whatever the compositor draws for this canvas.
    // A canvas that is never attached to the document never gets composited,
    // so the resulting MediaStreamTrack stays live-but-muted forever — that
    // is exactly the symptom we hit, Teams renders a black tile while the
    // track reports muted=true. Append the canvas off-screen at 1x1 so it
    // counts as part of the rendered tree without taking visible space.
    const canvas = document.createElement('canvas');
    canvas.width = W;
    canvas.height = H;
    // CSS size MUSI miec realny layout footprint (1280x720) zeby Chromium
    // compositor renderowal canvas w pelnym rozmiarze. Wczesniejsze 1x1
    // off-screen powodowalo ze captureStream sample'owal 1-pikselowy obszar
    // i Teams renderowal czarny kafelek. Position:fixed daleko za viewport
    // chowa to przed user'em (i tak headless), ale layout zostaje 1280x720.
    // Canvas w viewport (left:0, top:0, 1280x720) zamiast off-screen.
    // Bot leci w headless Chromium z Xvfb 1920x1080 — nikt nie patrzy na to
    // okno (poza VNC do diagnostyki). Pelne layout + viewport zmuszaja
    // Chromium compositor do realnego renderowania frames; off-screen
    // (left:-99999px) lub 1x1 powodowaly captureStream sample'owal pusty
    // backbuffer i Teams renderowal czarny kafelek.
    canvas.style.cssText =
      'position:fixed;left:0;top:0;width:1280px;height:720px;' +
      'pointer-events:none;z-index:99999;background:#000;';
    const attachCanvas = () => {
      if (!canvas.isConnected && document.body) {
        document.body.appendChild(canvas);
      }
    };
    if (document.body) {
      attachCanvas();
    } else {
      document.addEventListener('DOMContentLoaded', attachCanvas, { once: true });
    }
    videoCanvas = canvas;
    // alpha: false hands the encoder an opaque RGB buffer. Without it the
    // canvas keeps an alpha channel, the captured stream produces RGBA, and
    // Teams' video pipeline has historically rendered such frames as a
    // black tile when the upstream encoder picks YUV420 with the alpha
    // dropped.
    const ctx = canvas.getContext('2d', { alpha: false });
    try {
      // PLAN B — MediaStreamTrackGenerator zamiast canvas.captureStream.
      // captureStream w headless Xvfb byl bug-prone: compositor nie pulled
      // backbuffer'a regularnie, manual mode (captureStream(0)+requestFrame)
      // konczyl track jako 'ended' po pierwszym replaceTrack. MediaStreamTrack
      // Generator omija compositor calkowicie — my serializujemy canvas do
      // VideoFrame i piszemy do writable. Track zyje tak dlugo jak go
      // karmimy frame'ami. Backpressure: trzymamy referencje do ostatniego
      // pending write i skip'ujemy nowe gdy pending nadal nie zakonczony.
      videoGenerator = new MediaStreamTrackGenerator({ kind: 'video' });
      try { videoGenerator.contentHint = 'motion'; } catch (_) {}
      // Sync deviceId z fake entry z enumerateDevices override. Teams sprawdza
      // czy track.getSettings().deviceId pasuje do enumerated device i potrafi
      // wycielic track gdy id nie pasuje do zadnego enumerated entry.
      try {
        const orig = (videoGenerator.getSettings && videoGenerator.getSettings()) || {};
        const patched = Object.assign({}, orig, {
          deviceId: 'tentaflow-camera-default',
          groupId: 'tentaflow-group',
        });
        Object.defineProperty(videoGenerator, 'getSettings', {
          configurable: true,
          value: () => Object.assign({}, patched),
        });
      } catch (_) {}
      // Eksponuj na window — analogicznie do micGenerator. Post-join
      // replaceTrack wymusza zeby Teams uzywal naszego canvas track
      // (a nie wbudowanego Chromium fake-input).
      window.__tentaflowVideoTrack = videoGenerator;
      // Writer dla MediaStreamTrackGenerator. Backpressure przez pending
      // promise — jesli previous write nadal nie skonczyl, drop'ujemy nowe
      // frames (zamiast queueowac i blokowac draw loop).
      try {
        videoWriter = videoGenerator.writable.getWriter();
      } catch (e) {
        console.warn('[tentaflow] videoWriter init blad', e);
      }
      console.log('[tentaflow][video] track ready, muted=' + videoGenerator.muted +
        ' enabled=' + videoGenerator.enabled + ' state=' + videoGenerator.readyState +
        ' settings=' + JSON.stringify(videoGenerator.getSettings()));
      videoGenerator.addEventListener('mute', () => console.warn('[tentaflow][video] track became MUTED'));
      videoGenerator.addEventListener('unmute', () => console.log('[tentaflow][video] track became UNMUTED'));
      videoGenerator.addEventListener('ended', () => console.warn('[tentaflow][video] track ENDED'));
    } catch (e) {
      console.warn('[tentaflow] Blad tworzenia video stream', e);
      videoGenerator = null;
      window.__tentaflowVideoAvailable = false;
      return;
    }
    const FPS = 30;
    const [{ TfFace }, { FaceSpeechAnalyser }] = await window.__tentaflowFaceReady;
    videoFace = new TfFace();
    videoFace.setAttribute('mode', 'idle');
    videoSpeechContext = new AudioContext({ sampleRate: TARGET_RATE });
    const analyser = videoSpeechContext.createAnalyser();
    videoSpeech = new FaceSpeechAnalyser(analyser);
    const silent = videoSpeechContext.createGain();
    silent.gain.value = 0;
    analyser.connect(silent);
    silent.connect(videoSpeechContext.destination);
    // Autoplay permission must not block the camera's first frame.
    videoSpeechContext.resume().catch((error) => {
      console.warn('[tentaflow] avatar audio context could not start', error);
    });
    let speechUntil = 0;

    function renderFaceFrame(nowMs) {
      const features = videoSpeech.read();
      if (features.level > 0.015) speechUntil = nowMs + 250;
      videoFace.setAttribute('mode', nowMs < speechUntil ? 'speak' : 'idle');
      videoFace.setSpeechAmplitude(features.level, features);
      ctx.fillStyle = '#050818';
      ctx.fillRect(0, 0, W, H);
      videoFace.renderToCanvas(canvas, nowMs);
    }

    let videoFrameTs = 0;
    const drawAndWrite = () => {
      try {
        renderFaceFrame(performance.now());
        if (videoWriter && !videoWritePending) {
          try {
            videoWritePending = true;
            const ts = videoFrameTs;
            videoFrameTs += Math.round(1_000_000 / FPS);
            const frame = new VideoFrame(canvas, { timestamp: ts });
            videoWriter.write(frame).then(
              () => { videoWritePending = false; },
              (err) => {
                videoWritePending = false;
                console.warn('[tentaflow] videoWriter.write rejected', err);
              },
            );
            frame.close();
          } catch (e) {
            videoWritePending = false;
            console.warn('[tentaflow] VideoFrame push blad', e && e.message ? e.message : e);
          }
        }
      } catch (e) {
        console.warn('[tentaflow] video draw error:', e && e.message ? e.message : e);
      }
    };
    // Video draw loop lives in its own pool — the audio bridge WS reconnect
    // pump used to wipe every interval (including this draw loop) on every
    // hiccup, so the canvas froze and Teams kept showing whatever was on
    // the framebuffer when the draw stopped (usually mostly empty = black
    // tile).
    registerVideoInterval(setInterval(drawAndWrite, Math.round(1000 / FPS)));
    window.__tentaflowVideoAvailable = true;
    if (window.__tentaflowBridge) window.__tentaflowBridge.videoSetupDone = true;
    console.log('[tentaflow] Video injection zainicjalizowane (' + W + 'x' + H + ' @ ' + FPS + 'fps)');
    registerVideoInterval(setInterval(() => {
      try {
        // Pixel readback: prove the draw loop actually paints into the canvas
        // backbuffer. If the centre pixel (where we render the wireframe) and
        // a corner pixel (the backdrop gradient) both come back zero, the
        // problem is upstream in canvas rendering (e.g. software GL stack
        // never produced any frames). If they come back coloured but Teams
        // still shows black, the captureStream / encoder side is dropping
        // frames despite a healthy canvas.
        const pCenter = ctx.getImageData(W / 2, H / 2, 1, 1).data;
        const pCorner = ctx.getImageData(8, 8, 1, 1).data;
        console.log('[tentaflow][video] tick muted=' + videoGenerator.muted +
          ' enabled=' + videoGenerator.enabled +
          ' canvasInDom=' + canvas.isConnected +
          ' canvasParent=' + (canvas.parentNode ? canvas.parentNode.nodeName : 'null') +
          ' centerRGBA=' + pCenter[0] + ',' + pCenter[1] + ',' + pCenter[2] + ',' + pCenter[3] +
          ' cornerRGBA=' + pCorner[0] + ',' + pCorner[1] + ',' + pCorner[2] + ',' + pCorner[3]);
      } catch (e) {
        console.warn('[tentaflow][video] tick read error:', e && e.message ? e.message : e);
      }
    }, 5000));
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
      if (videoSpeech && videoSpeechContext.state === 'running') {
        // Analyse a silent copy on the audio clock; queued PCM must animate at
        // playback time rather than when a whole TTS packet arrives over WS.
        const buffer = videoSpeechContext.createBuffer(1, i16.length, TARGET_RATE);
        const samples = buffer.getChannelData(0);
        for (let i = 0; i < i16.length; i++) samples[i] = i16[i] / 32768;
        const source = videoSpeechContext.createBufferSource();
        source.buffer = buffer;
        source.connect(videoSpeech.analyser);
        source.onended = () => source.disconnect();
        const start = Math.max(videoSpeechContext.currentTime, videoSpeechNextTime);
        source.start(start);
        videoSpeechNextTime = start + buffer.duration;
      }
    } catch (e) {
      console.warn('[tentaflow] AudioData write error', e);
    }
  }

  // --------------------------------------------------------------------------
  // Bootstrap
  // --------------------------------------------------------------------------
  // ==========================================================================
  // EARLY MediaStreamTrack.prototype.stop GUARD
  // ==========================================================================
  // Teams po replaceTrack(naszGeneratorTrack) wywoluje .stop() na tym track
  // jako anti-spoofing — wynik widzielismy w videoWriter "Stream closed"
  // od pierwszego write'a. Patch blokuje stop() na naszych singletonach
  // (window.__tentaflowMicGenerator, __tentaflowVideoTrack). Teams moze
  // wolac, my pomijamy i dalej pchamy frames przez writer.
  try {
    const TrackProto = (typeof MediaStreamTrack !== 'undefined') ? MediaStreamTrack.prototype : null;
    if (TrackProto && TrackProto.stop) {
      const origStop = TrackProto.stop;
      TrackProto.stop = function () {
        if (this === window.__tentaflowMicGenerator
          || this === window.__tentaflowVideoTrack) {
          console.log('[tentaflow] track.stop() zablokowany dla generatora '
            + (this.kind || ''));
          return;
        }
        return origStop.call(this);
      };
    }
  } catch (e) {
    console.warn('[tentaflow] track.stop guard blad', e);
  }

  // ==========================================================================
  // EARLY navigator.mediaDevices.enumerateDevices OVERRIDE
  // ==========================================================================
  // Teams light-meetings dla anonim joinerow w Docker (bez real camera) widzi
  // pusty kontener "videoinput" / "audioinput" przez enumerateDevices i pokazuje
  // baner "Teams needs permission to access your camera". Jesli enumerate
  // zwraca minimum jedna fake camerę + mic, Teams uznaje urządzenie za istniejace
  // i wpina track do pc.transceiver. Wstawiamy syntetyczne entries DOOKOLA
  // tego co Chromium zwraca z faktycznego enumerate (jesli cos zwraca).
  try {
    if (navigator.mediaDevices && navigator.mediaDevices.enumerateDevices) {
      const origEnum = navigator.mediaDevices.enumerateDevices.bind(navigator.mediaDevices);
      navigator.mediaDevices.enumerateDevices = async function () {
        const real = await origEnum();
        const hasVideoIn = real.some((d) => d.kind === 'videoinput');
        const hasAudioIn = real.some((d) => d.kind === 'audioinput');
        const hasAudioOut = real.some((d) => d.kind === 'audiooutput');
        const fake = [];
        if (!hasVideoIn) fake.push({
          deviceId: 'tentaflow-camera-default',
          groupId: 'tentaflow-group',
          kind: 'videoinput',
          label: 'TentaFlow Camera',
        });
        if (!hasAudioIn) fake.push({
          deviceId: 'tentaflow-mic-default',
          groupId: 'tentaflow-group',
          kind: 'audioinput',
          label: 'TentaFlow Microphone',
        });
        if (!hasAudioOut) fake.push({
          deviceId: 'tentaflow-speaker-default',
          groupId: 'tentaflow-group',
          kind: 'audiooutput',
          label: 'TentaFlow Speaker',
        });
        return real.concat(fake);
      };
    }
  } catch (e) {
    console.warn('[tentaflow] enumerateDevices override blad', e);
  }

  // ==========================================================================
  // EARLY navigator.permissions.query OVERRIDE
  // ==========================================================================
  // Teams light-meetings sprawdza camera/microphone permission state przez
  // `navigator.permissions.query({name:'camera'})` zanim wpina track do
  // pc.transceiver. Mimo CDP setPermission Granted, query potrafi zwrocic
  // 'prompt' albo 'denied' w light-meetings flow (race miedzy permission
  // store a check). Jesli Teams widzi nie-'granted', pokazuje banner "Teams
  // needs permission to access your camera" i NIE wysyla video track.
  // Wymuszamy 'granted' dla camera + microphone na poziomie Permissions API.
  try {
    if (navigator.permissions && navigator.permissions.query) {
      const origQuery = navigator.permissions.query.bind(navigator.permissions);
      navigator.permissions.query = function (descriptor) {
        const name = descriptor && descriptor.name;
        if (name === 'camera' || name === 'microphone') {
          return Promise.resolve({
            state: 'granted',
            status: 'granted',
            onchange: null,
            addEventListener: function () {},
            removeEventListener: function () {},
            dispatchEvent: function () { return false; },
          });
        }
        return origQuery(descriptor);
      };
    }
  } catch (e) {
    console.warn('[tentaflow] permissions.query override blad', e);
  }

  // ==========================================================================
  // EARLY HOOKS — SYNCHRONICZNE, jeszcze przed DOMContentLoaded
  // ==========================================================================
  // Teams' bundle moze zawolac getUserMedia / new RTCPeerConnection ZANIM
  // DOMContentLoaded fires. Jesli nasze override'y nie sa wtedy gotowe, Teams
  // trafia na native gum -> Chrome odmawia (mimo setPermission Granted) -> Teams
  // pokazuje modal "Are you sure" sugerujacy klikanie camera icon w address
  // bar. Dlatego hooki ktore nie wymagaja DOM (gum override + RTC patch)
  // odpalamy natychmiast w IIFE, bez czekania na DOMContentLoaded. Video setup
  // wymaga DOM (canvas) i zostaje w bootstrap.
  try {
    hookRTCPeerConnection();
  } catch (e) {
    console.warn('[tentaflow] early hookRTCPeerConnection blad', e);
  }
  try {
    setupMicInjection();
  } catch (e) {
    console.warn('[tentaflow] early setupMicInjection blad', e);
  }

  function bootstrap() {
    if (window.__tentaflowBridge && window.__tentaflowBridge.setupDone) {
      console.log('[tentaflow] bootstrap juz wykonany — pomijam');
      return;
    }
    // hookRTCPeerConnection + setupMicInjection juz odpalone w EARLY HOOKS
    // wyzej. Re-call jest no-op (oba sprawdzaja flag setupDone).
    try {
      hookRTCPeerConnection();
    } catch (e) {
      console.warn('[tentaflow] hookRTCPeerConnection blad', e);
    }
    setupVideoInjection().catch((e) => {
      window.__tentaflowVideoAvailable = false;
      console.warn('[tentaflow] setupVideoInjection failed', e);
    });
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
    // Roster + active-speaker NIE leci juz przez WS — zasila je push CDP
    // bridge nizej (installTentaflowDomBridge), ktory tez zasila Arcs w
    // main.rs uzywane do STT extra_meta. Jeden kanal, mniej duplikatu.
    try {
      installTentaflowDomBridge();
    } catch (e) {
      console.warn('[tentaflow] installTentaflowDomBridge blad', e);
    }
    if (window.__tentaflowBridge) window.__tentaflowBridge.setupDone = true;
    console.log('[tentaflow] Bridge audio zainicjalizowany');
  }

  // ==========================================================================
  // Push-based DOM event bridge -> Rust (CDP binding `__tentaflowEvent`).
  // ==========================================================================
  // Zastepuje pollingowy `participant_scanner.rs` (`page.evaluate` co 3s)
  // i pollingowa petle `detect_meeting_progress` w browser.rs (`page.evaluate`
  // co 500ms-2s z body.innerText). MutationObserver fires na realnej zmianie
  // DOM; rAF dedupluje serie mutacji w jeden skan; 1s safety interval pokrywa
  // edge case'y gdy obserwator przegapi przejscie (np. iframe, dynamic root).
  //
  // Komunikacja: window.__tentaflowEvent(JSON.stringify({ type, ...data })).
  // Funkcja jest rejestrowana przez Rust przez CDP `Runtime.addBinding` przed
  // nawigacja do Teams (browser.rs::join_meeting -> dom_observer::start).
  function installTentaflowDomBridge() {
    let scheduled = false;
    let knownTiles = new Map(); // data-tid -> display name
    let lobbyEmitted = false;
    let joinedEmitted = false;
    let lastSpeakerKey = null;

    function emit(type, data) {
      if (typeof window.__tentaflowEvent !== 'function') return;
      try {
        const payload = Object.assign({ type: type }, data || {});
        window.__tentaflowEvent(JSON.stringify(payload));
      } catch (e) {
        // Cicho — binding moze byc nieobecny w niektorych iframe'ach.
      }
    }

    // data-tid w obecnym Teams light-meetings to czesto realna nazwa
    // uczestnika (np. "Piotr Jarocki"), ale React commit phase potrafi tam
    // chwilowo wpisac hash GUID (40+ znakow hex). Dlatego order: aria-label
    // (najpewniejsze, "Imie Nazwisko, video, ..." na video tiles), potem
    // child `[data-tid="participant-name"]` (panel People), a data-tid
    // bierzemy tylko gdy NIE wyglada na hash. null = skip ten tile.
    function tileDisplayName(tile) {
      const al = tile.getAttribute('aria-label') || '';
      const trimmedAl = al.split(',')[0].trim();
      if (trimmedAl) return trimmedAl;
      const nameEl = tile.querySelector('[data-tid="participant-name"]');
      if (nameEl) {
        const t = (nameEl.textContent || '').trim();
        if (t) return t;
      }
      const tid = tile.getAttribute('data-tid') || '';
      if (tid && !/^[a-f0-9-]{20,}$/i.test(tid)) return tid;
      return null;
    }

    function detectLobby() {
      const tids = ['lobby-screen', 'lobby-wait-screen', 'prejoin-meeting-info',
        'lobby-waiting-room', 'calling-lobby-screen'];
      for (const t of tids) {
        if (document.querySelector('[data-tid="' + t + '"]')) return true;
      }
      // Phrase scan ograniczony do prejoin/lobby kontenerow — body.innerText
      // serializuje setki KB i kosztuje 30-150ms. Tu querySelectorAll po
      // konkretnych prefixach wyciaga ~5-50 elementow.
      const candidates = document.querySelectorAll(
        '[data-tid^="prejoin"], [data-tid^="lobby"], [data-tid="calling-lobby-screen"]');
      for (const el of candidates) {
        const text = (el.innerText || '').toLowerCase();
        if (text.indexOf('let you in') !== -1
          || text.indexOf("you're in the lobby") !== -1
          || text.indexOf('wpusci') !== -1
          || text.indexOf('admit') !== -1) return true;
      }
      return false;
    }

    function detectJoined() {
      // Najpierw NEGATYW: gdy widoczny prejoin lobby waiting "Hi, X. Someone
      // will let you in shortly." albo device picker — to jeszcze nie call.
      // Teams light-meetings odpalalo tu fals positive bo audio.srcObject
      // zywil ostro przed faktycznym admittem.
      const prejoinMarkers = [
        '[data-tid^="prejoin"]',
        '[data-tid="lobby-screen"]',
        '[data-tid="lobby-wait-screen"]',
        '[data-tid="lobby-waiting-room"]',
        '[data-tid="calling-lobby-screen"]',
        '[data-tid="prejoin-meeting-info"]',
      ];
      for (const sel of prejoinMarkers) {
        if (document.querySelector(sel)) return false;
      }
      // Stage musi byc obecny i miec realne tiles. W call surface to kafelki
      // uczestnikow ze streamem; w prejoin sam stage moze byc renderowany
      // bez tile'ow — wymagamy 2+ tiles zeby uniknac false positive.
      const stage = document.querySelector('[data-tid="MixedStage-wrapper"]')
        || document.querySelector('[data-tid="stage-layouts-renderer"]');
      if (stage) {
        const tileCount = stage.querySelectorAll('[data-tid][data-stream-type]').length;
        if (tileCount >= 2) return true;
      }
      // Roster badge >=2 = realnie kilka uczestnikow w call.
      const rosterBadge = document.querySelector('#roster-button [data-tid="toolbar-item-badge"]');
      if (rosterBadge) {
        const n = parseInt((rosterBadge.textContent || '').trim(), 10) || 0;
        if (n >= 2) return true;
      }
      return false;
    }

    // Speaker detection — kolejnosc selektorow ta sama co w istniejacej
    // getActiveSpeaker() (sendActiveSpeakerIfChanged przez WS), ktora byla
    // zwalidowana na realnym DOM Teams. Zwracamy {id, name}: id to data-tid
    // tile'a (lub null gdy znamy tylko nazwe), name to display name.
    function detectActiveSpeaker() {
      // 1. Presenter — najpewniejszy sygnal dominujacego mowcy.
      const presenter = document.querySelector('[data-is-presenter="true"]');
      if (presenter) {
        const label = presenter.getAttribute('aria-label') || presenter.textContent || '';
        const m = label.match(/^(.+?)(?:,|$)/);
        const name = m && m[1] ? m[1].trim() : null;
        if (name) return { id: presenter.getAttribute('data-tid') || null, name: name };
      }
      // 2. Klasa active-speaker / data-tid active-speaker — current dominant.
      const active = document.querySelector('.active-speaker, [data-tid="active-speaker"]');
      if (active) {
        const nameEl = active.querySelector('.ts-tooltip-trigger, [data-tid="participant-name"]');
        if (nameEl) {
          const name = (nameEl.textContent || '').trim();
          if (name) return { id: active.getAttribute('data-tid') || null, name: name };
        }
        const label = active.getAttribute('aria-label') || '';
        const m = label.match(/^(.+?)(?:,|$)/);
        if (m && m[1]) return { id: active.getAttribute('data-tid') || null, name: m[1].trim() };
      }
      // 3. aria-label "X is speaking" — fallback gdy nie ma klasowych markerow.
      const speakingEl = document.querySelector('[aria-label*="is speaking"]');
      if (speakingEl) {
        const label = speakingEl.getAttribute('aria-label') || '';
        const m = label.match(/^(.+?)\s+is speaking/i);
        if (m && m[1]) return { id: speakingEl.getAttribute('data-tid') || null, name: m[1].trim() };
      }
      // 4. voice-level outline — Teams renderuje animowana ramke wokol kafelka
      // aktywnego mowcy (`data-tid="voice-level-stream-outline"` albo klasa
      // zawierajaca "voice-level"). Kafelek z taka ramka = aktywny speaker.
      // Wazne gdy starsza klasowa heurystyka (.active-speaker) nie zadziala
      // w light-meetings z obfuscowanym CSS-in-JS.
      const outline = document.querySelector(
        '[data-tid="voice-level-stream-outline"], [class*="voice-level"]'
      );
      if (outline) {
        const tile = outline.closest('[data-tid][data-stream-type], [data-tid="participant-list-item"]');
        if (tile) {
          const name = tileDisplayName(tile);
          if (name) return { id: tile.getAttribute('data-tid') || null, name: name };
        }
      }
      return null;
    }

    // Cache OffscreenCanvas per kafelek z kamera. Klucz = data-tid kafelka.
    // Trzymamy canvas zeby nie alokowac go per klatka — drawImage + convertToBlob
    // jest tani gdy wymiary sie nie zmieniaja.
    const videoCanvases = new Map(); // data-tid -> { canvas, ctx, w, h }

    // Buduje listę kafelków łącząc trzy źródła Teams: kafelki sceny
    // (`[data-tid][data-stream-type]`), panel People (`participant-list-item`)
    // oraz dynamiczne `people-list-item*`. Deduplikacja po lower-case nazwie:
    // ten sam uczestnik widoczny w MixedStage + People to jeden wpis (in_stage
    // ∧ in_roster). Off-camera user, ktory NIE jest na scenie, ma in_stage=false
    // — bez tego pkt 1 z requirementu (sidebar) by go gubil.
    function collectRosterEntries() {
      const byKey = new Map(); // lower-case name -> entry
      function pushTile(tile, fromStage, fromRoster) {
        const name = tileDisplayName(tile);
        if (!name) return;
        const tid = tile.getAttribute('data-tid') || name;
        const streamType = tile.getAttribute('data-stream-type') || '';
        const hasVideoAttr = streamType === 'Video' || streamType.indexOf('Video') !== -1;
        const hasAudioAttr = streamType === 'Audio' || streamType.indexOf('Audio') !== -1;
        // Realny `<video>` z aktywnym MediaStream uznajemy jako has_video
        // niezaleznie od atrybutu — atrybut potrafi opozniac sie wzgledem
        // realnej negocjacji RTC.
        let hasLiveVideo = hasVideoAttr;
        if (!hasLiveVideo) {
          const v = tile.querySelector('video');
          if (v && v.srcObject && v.srcObject.getVideoTracks
              && v.srcObject.getVideoTracks().length > 0
              && v.videoWidth > 0) {
            hasLiveVideo = true;
          }
        }
        const key = name.toLowerCase();
        const prev = byKey.get(key);
        if (prev) {
          prev.has_video = prev.has_video || hasLiveVideo;
          prev.has_audio = prev.has_audio || hasAudioAttr;
          prev.in_stage = prev.in_stage || fromStage;
          prev.in_roster = prev.in_roster || fromRoster;
        } else {
          byKey.set(key, {
            id: tid,
            name: name,
            has_video: hasLiveVideo,
            has_audio: hasAudioAttr,
            in_stage: fromStage,
            in_roster: fromRoster,
          });
        }
      }
      // Kafelki sceny (video/audio strumienie aktywnych uczestnikow).
      document.querySelectorAll('[data-tid][data-stream-type]').forEach(function (t) {
        pushTile(t, true, false);
      });
      // Roster panel (sidebar People). Selektory pokrywaja wariacje light-meetings:
      // - `[data-tid="participant-list-item"]` (klasyczny full-meeting roster)
      // - `[data-tid^="people-list-item"]` (light-meetings w nowym UI)
      // - `[role="listitem"]` wewnatrz `[data-tid="people-roster"]` / `[data-tid="roster"]`
      const rosterSelectors = [
        '[data-tid="participant-list-item"]',
        '[data-tid^="people-list-item"]',
        '[data-tid="people-roster"] [role="listitem"]',
        '[data-tid="roster"] [role="listitem"]',
      ];
      document.querySelectorAll(rosterSelectors.join(',')).forEach(function (t) {
        pushTile(t, false, true);
      });
      return Array.from(byKey.values());
    }

    function captureFrames(now) {
      // Klatki dla kafelkow z aktywnym wideo. Bez zewnetrznych OffscreenCanvas
      // tainted issue: MediaStream z RTCPeerConnection nie ma origin per W3C
      // spec — canvas nie jest tainted, drawImage + convertToBlob dziala bez
      // crossOrigin issues. Dlatego nie potrzebujemy CORS proxy.
      const seen = new Set();
      const tiles = document.querySelectorAll('[data-tid][data-stream-type="Video"]');
      tiles.forEach(function (tile) {
        const tid = tile.getAttribute('data-tid');
        if (!tid) return;
        const name = tileDisplayName(tile);
        if (!name) return;
        const video = tile.querySelector('video');
        if (!video || !video.srcObject) return;
        if (typeof MediaStream === 'undefined' || !(video.srcObject instanceof MediaStream)) return;
        if (video.readyState < 2 || video.videoWidth <= 0) return;
        seen.add(tid);
        // Skala do max 320px szerokosci (zachowujac proporcje). Wiekszy
        // rozmiar dla podgladu w GUI nie ma sensu i zwieksza koszt JPEG encode.
        const targetW = Math.min(video.videoWidth, 320);
        const scale = targetW / video.videoWidth;
        const targetH = Math.max(1, Math.round(video.videoHeight * scale));
        let entry = videoCanvases.get(tid);
        if (!entry || entry.w !== targetW || entry.h !== targetH) {
          const canvas = (typeof OffscreenCanvas !== 'undefined')
            ? new OffscreenCanvas(targetW, targetH)
            : Object.assign(document.createElement('canvas'), { width: targetW, height: targetH });
          const ctx = canvas.getContext('2d');
          entry = { canvas: canvas, ctx: ctx, w: targetW, h: targetH };
          videoCanvases.set(tid, entry);
        }
        try {
          entry.ctx.drawImage(video, 0, 0, entry.w, entry.h);
        } catch (e) {
          return;
        }
        const toBlobPromise = entry.canvas.convertToBlob
          ? entry.canvas.convertToBlob({ type: 'image/jpeg', quality: 0.6 })
          : new Promise(function (resolve) {
              entry.canvas.toBlob(function (b) { resolve(b); }, 'image/jpeg', 0.6);
            });
        toBlobPromise.then(function (blob) {
          if (!blob) return;
          return blob.arrayBuffer().then(function (buf) {
            const bytes = new Uint8Array(buf);
            // btoa pracuje na binary string — chunked build, bez stack overflow
            // przy duzych tablicach (apply spread limit ~64k).
            let binary = '';
            const chunk = 0x8000;
            for (let i = 0; i < bytes.length; i += chunk) {
              binary += String.fromCharCode.apply(null, bytes.subarray(i, i + chunk));
            }
            const b64 = btoa(binary);
            emit('video_frame', {
              participant_id: tid,
              name: name,
              ts_ms: now,
              jpeg_b64: b64,
            });
          });
        }).catch(function () {});
      });
      // Prune cache dla kafelkow ktore zniknely — bez tego canvasy zywa az
      // do unload strony.
      for (const tid of Array.from(videoCanvases.keys())) {
        if (!seen.has(tid)) videoCanvases.delete(tid);
      }
    }

    function scan() {
      scheduled = false;
      try {
        if (!lobbyEmitted && detectLobby()) {
          lobbyEmitted = true;
          emit('lobby');
        }
        if (!joinedEmitted && detectJoined()) {
          joinedEmitted = true;
          emit('joined');
        }
        // Snapshot rosteru: zamiast diff per-tile emit'ujemy całą aktualną
        // listę jednym eventem. Native dom_observer porównuje z poprzednim
        // stanem znanym lokalnie, a router robi tylko broadcast — koszt
        // sieciowy = 1 RT zamiast N.
        const entries = collectRosterEntries();
        const current = new Map();
        for (const e of entries) {
          current.set(e.id, e);
        }
        // Skip jeśli stan rosteru się nie zmienił od poprzedniego scan'u —
        // unikamy zalewania routera identycznymi snapshotami przy idle Teams.
        let changed = current.size !== knownTiles.size;
        if (!changed) {
          for (const [tid, e] of current) {
            const prev = knownTiles.get(tid);
            if (!prev
                || prev.name !== e.name
                || prev.has_video !== e.has_video
                || prev.has_audio !== e.has_audio
                || prev.in_stage !== e.in_stage
                || prev.in_roster !== e.in_roster) {
              changed = true; break;
            }
          }
        }
        if (changed) {
          emit('roster_snapshot', { entries: entries });
          knownTiles = current;
        }

        const sp = detectActiveSpeaker();
        const key = sp ? (sp.id || sp.name || '') : '';
        if (key !== lastSpeakerKey) {
          lastSpeakerKey = key;
          emit('active_speaker', {
            id: sp ? sp.id : null,
            name: sp ? sp.name : null,
          });
        }
      } catch (e) {
        console.warn('[tentaflow] dom_bridge scan blad:', e);
      }
    }

    function schedule() {
      if (scheduled) return;
      scheduled = true;
      requestAnimationFrame(scan);
    }

    const obs = new MutationObserver(schedule);
    let safetyIntervalId = null;
    let videoCaptureIntervalId = null;
    function attach() {
      if (!document.body) {
        setTimeout(attach, 50);
        return;
      }
      obs.observe(document.body, {
        subtree: true,
        childList: true,
        attributes: true,
        attributeFilter: ['class', 'data-tid', 'aria-label', 'aria-pressed', 'data-stream-type']
      });
      schedule();
      // MutationObserver gubi zmiany w Shadow DOM oraz w React commit phase,
      // gdy mutacje są flushowane w microtasku przed pełnym attach observera
      // — interval safety-net pokrywa edge cases (Teams light-meetings,
      // dynamiczny iframe roster) bez polegania na DOM mutations. 1500 ms
      // jest na tyle rzadkie, ze koszt CPU pojedynczego scanu jest pomijalny.
      safetyIntervalId = setInterval(scan, 1500);
      const captureMs = (typeof window.__tentaflowVideoCaptureMs === 'number'
        && window.__tentaflowVideoCaptureMs > 0)
        ? window.__tentaflowVideoCaptureMs : 1000;
      videoCaptureIntervalId = setInterval(function () {
        captureFrames(Date.now());
      }, captureMs);
    }
    // Reset hook — jesli Rust kiedykolwiek zechce wymienic bridge w locie,
    // wywola __tentaflowDomBridgeReset() i poprzednie intervale przestana
    // zywic stary listener. Wczesniej brak cleanupu prowadzil do zombie
    // intervals przy hot-reloadzie skryptu.
    window.__tentaflowDomBridgeReset = function () {
      try { obs.disconnect(); } catch (_) {}
      if (safetyIntervalId) { clearInterval(safetyIntervalId); safetyIntervalId = null; }
      if (videoCaptureIntervalId) { clearInterval(videoCaptureIntervalId); videoCaptureIntervalId = null; }
      videoCanvases.clear();
    };
    attach();
    console.log('[tentaflow] DOM event bridge zainstalowany (push via addBinding)');

    // Active speaker via WebRTC audioLevel — deterministyczne i odporne na
    // zmiany DOM. Teams light-meetings ma obfuscowane klasy CSS-in-JS bez
    // markera "speaking", wiec polegamy na inbound-rtp audioLevel z
    // RTCPeerConnection.getStats().
    //
    // Hystereza + debounce: bez tego speaker oscyluje miedzy sylabami (level
    // spada do 0 w ulamkowych pauzach miedzy slowami).
    //   * START_LEVEL — minimum zeby zaczac uznawac kogos za speakera
    //   * HOLD_LEVEL — minimum zeby przedluzyc trwajacego speakera
    //   * SILENCE_HOLD_MS — czas trzymania speakera mimo levelu < HOLD
    //
    // Adaptive sampling: 50ms (20Hz) gdy ktos mowi, 200ms (5Hz) w ciszy.
    // Chromium internal audioLevel stats aktualizuja sie co ~20ms; 50ms jest
    // blisko realnego limitu tej techniki gdy mamy aktywnego speakera. W idle
    // wystarczy 5Hz zeby zalapac poczatek wypowiedzi w <=200ms — i tak czekamy
    // SILENCE_HOLD_MS=300 zanim uznamy ze ktos przestal mowic. SILENCE_HOLD_MS
    // dalej debounce'uje pauzy miedzy sylabami.
    const SPEAKER_START_LEVEL = 0.03;
    const SPEAKER_HOLD_LEVEL = 0.005;
    const SPEAKER_SILENCE_HOLD_MS = 300;
    const SPEAKER_POLL_ACTIVE_MS = 50;
    const SPEAKER_POLL_IDLE_MS = 200;
    let lastBindingSpeaker = null;
    let silenceSince = 0;

    function trackedPeerConnections() {
      // hookRTCPeerConnection() wczesniej w pliku rejestruje wszystkie pc w
      // window.__tentaflowPeerConnections (Set). Bezpieczny fallback gdy hook
      // jeszcze nie zdazyl uzbroic.
      const set = window.__tentaflowPeerConnections;
      return set instanceof Set ? Array.from(set) : [];
    }

    function findRemoteName() {
      // Wez pierwszy remote tile (nie nasz). data-tid w light-meetings to
      // czesto nazwa, ale bezpieczniej wziac displayName z helpera.
      // Filtr bota: exact match lub `<botName> (` (sufix Teams typu
      // " (Unverified)"/" (External)"). Prefix-match po samym name'ie wycinal
      // np. "Botanik" gdy bot nazywal sie "Bot".
      const tiles = document.querySelectorAll('[data-tid][data-stream-type]');
      const ourName = (window.__tentaflowBotName || '').toString();
      for (const t of tiles) {
        const name = tileDisplayName(t);
        if (!name) continue;
        if (ourName && (name === ourName || name.indexOf(ourName + ' (') === 0)) continue;
        return name;
      }
      return null;
    }

    async function maxInboundAudioLevel() {
      const pcs = trackedPeerConnections();
      if (pcs.length === 0) return 0;
      let best = 0;
      for (const pc of pcs) {
        try {
          const stats = await pc.getStats();
          stats.forEach(function (rep) {
            if (rep.type === 'inbound-rtp' && rep.kind === 'audio') {
              const lvl = typeof rep.audioLevel === 'number' ? rep.audioLevel : 0;
              if (lvl > best) best = lvl;
            }
          });
        } catch (_) {}
      }
      return best;
    }

    async function pollActiveSpeaker() {
      try {
        const level = await maxInboundAudioLevel();
        const now = Date.now();
        let nextSpeaker = lastBindingSpeaker;
        if (lastBindingSpeaker) {
          // Trwajacy speaker — trzymamy az level spadnie ponizej HOLD na
          // dluzej niz SILENCE_HOLD_MS. To absorbuje pauzy miedzy sylabami.
          if (level >= SPEAKER_HOLD_LEVEL) {
            silenceSince = 0;
          } else {
            if (silenceSince === 0) silenceSince = now;
            if (now - silenceSince >= SPEAKER_SILENCE_HOLD_MS) {
              nextSpeaker = null;
              silenceSince = 0;
            }
          }
        } else {
          // Nikt nie mowi — start dopiero przy levelu >= START.
          if (level >= SPEAKER_START_LEVEL) {
            nextSpeaker = findRemoteName();
            silenceSince = 0;
          }
        }
        if (nextSpeaker !== lastBindingSpeaker) {
          lastBindingSpeaker = nextSpeaker;
          emit('active_speaker', {
            id: nextSpeaker,
            name: nextSpeaker,
          });
        }
      } catch (_) {
        // getStats moze rzucic gdy pc jest closed mid-poll — silent.
      }
      // Aktywny speaker => szybkie 20Hz sampling, idle => 5Hz. setTimeout
      // recursion (zamiast setInterval) zeby zmieniac tempo bez recreate'a.
      const nextDelay = lastBindingSpeaker ? SPEAKER_POLL_ACTIVE_MS : SPEAKER_POLL_IDLE_MS;
      setTimeout(pollActiveSpeaker, nextDelay);
    }
    setTimeout(pollActiveSpeaker, SPEAKER_POLL_ACTIVE_MS);
  }

  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', bootstrap);
  } else {
    bootstrap();
  }
})();
