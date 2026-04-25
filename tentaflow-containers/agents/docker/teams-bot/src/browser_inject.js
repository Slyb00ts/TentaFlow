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
  function setupVideoInjection() {
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
    // Glow pass — replikuje dwu-canvasowy efekt z login screen (faceBackground.js):
    // ten sam wireframe rysujemy na offscreen canvasie, a potem kompozytujemy go
    // z blur + 'lighter' na main canvas. Daje halo, ktore wygladza gradient
    // jasny->ciemny po stronie tylnej twarzy.
    const glowCanvas = document.createElement('canvas');
    glowCanvas.width = W;
    glowCanvas.height = H;
    const glowCtx = glowCanvas.getContext('2d', { alpha: true });
    let stream = null;
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
    // captureStream samples this canvas internally at the rate we passed
    // to captureStream(30), so the drawing loop just has to keep the
    // backbuffer fresh — no manual VideoFrame timestamping, no writer
    // backpressure to drain.
    const FPS = 30;
    const TAU = Math.PI * 2;
    const cx = W / 2, cy = H / 2;
    const label = 'TENTAFLOW';

    // ----- Wireframe face mesh data + renderer (port faceBackground.js) ------
    // Pelny pipeline blendshape -> rotacja -> projekcja perspektywiczna ->
    // batchowany stroke krawedzi. Dane (NUM_VERTICES, BASE_POSITIONS,
    // BLENDSHAPE_DELTAS, LEFT_MASK, RIGHT_MASK, BS_INDEX, FACEMESH_CONTOURS,
    // FACEMESH_FILL) sa zinlinowane wprost z `tentaflow-core/www/js/data/...`
    // poniewaz bot dziala w izolowanym top-frame Teams i nie moze dynamicznie
    // importowac modulow z naszego serwera.

const NUM_VERTICES = 486;

const BASE_POSITIONS = new Float32Array([
  -0.043167, 0.488607, 0.475114, -0.160724, 0.467235, 0.461430, 0.076641, 0.461379, 0.469317, -0.061448, 0.044385, 0.784352,
  -0.059624, -0.085826, 0.752894, -0.363401, 0.103327, 0.497556, 0.270645, 0.098308, 0.515869, -0.055835, -0.192268, 0.670154,
  -0.050262, -0.394375, 0.517392, -0.048930, -0.508958, 0.455127, -0.054259, -0.289677, 0.590955, -0.686677, -0.417863, 0.123750,
  -0.717851, -0.444381, 0.093146, -0.653837, -0.398995, 0.150003, -0.049860, -1.200000, 0.409778, -0.342570, -1.188608, 0.378332,
  0.248744, -1.194009, 0.395704, -0.037405, 0.552764, 0.383950, -0.130083, 0.548589, 0.367725, 0.060388, 0.545128, 0.374659,
  -0.032572, 0.550321, 0.356740, -0.125720, 0.546040, 0.342907, 0.061816, 0.545112, 0.349370, -0.028665, 0.665058, 0.394550,
  -0.137723, 0.660646, 0.384756, 0.082296, 0.654781, 0.391245, -0.995550, -0.746467, -0.313346, -0.928935, -0.916860, -0.115134,
  -1.023202, -0.566604, -0.524881, -0.699502, -0.460732, 0.136523, -0.265606, 0.487016, 0.399990, -0.336327, 0.510898, 0.323635,
  -0.390897, 0.532574, 0.239420, -0.856017, -0.623530, 0.163356, -0.780038, -0.695485, 0.259128, -0.659309, -0.733138, 0.338133,
  -0.485813, -0.722078, 0.396201, -0.800539, -1.052725, 0.090301, -0.246007, -0.642841, 0.427300, -0.936048, 0.530979, -0.663427,
  -0.992723, 0.294258, -0.748271, -0.848806, 0.712720, -0.512944, -0.431554, 0.554498, 0.157669, -0.383160, 0.582261, 0.216308,
  -0.827677, -0.755076, 0.213703, -0.903069, -0.661838, 0.084396, -0.693842, -0.808167, 0.323897, -0.506118, -0.801272, 0.419455,
  -0.282369, -0.769438, 0.469978, -0.585598, -1.144982, 0.269844, -0.394684, 0.550561, 0.188851, -0.337504, 0.547339, 0.225988,
  -0.339121, 0.547497, 0.231943, -0.285905, 0.545603, 0.284290, -0.214650, 0.543724, 0.329144, -0.239747, 0.640535, 0.343264,
  -0.209971, 0.545535, 0.310457, -0.282296, 0.545318, 0.272090, -0.322565, 0.607459, 0.290316, -1.015007, 0.069765, -0.784352,
  -1.020859, -0.141288, -0.770660, -1.029923, -0.349135, -0.692943, -0.283699, -0.419951, 0.185550, -0.309077, -0.410186, 0.185928,
  -0.313906, -0.445440, 0.200167, -0.746741, 0.844033, -0.342866, -0.613495, 0.964019, -0.203847, -0.603635, -0.382170, 0.180691,
  -0.524984, -0.375702, 0.205814, -0.443225, -0.385910, 0.212822, -0.213265, 1.185040, 0.103581, -0.016284, 1.200000, 0.138096,
  -0.366674, 1.128497, 0.023165, -0.495227, 1.051043, -0.091682, 0.178135, 1.185912, 0.116972, -0.364165, -0.401826, 0.202728,
  -0.380811, -0.482277, 0.219777, -0.468079, -0.508194, 0.234753, -0.550693, -0.513390, 0.228624, -0.628745, -0.499580, 0.202905,
  -0.674662, -0.478394, 0.170164, 0.625446, -0.417458, 0.167725, 0.656726, -0.444381, 0.139736, 0.590001, -0.400163, 0.191626,
  0.974497, -0.757601, -0.245696, 0.887522, -0.927370, -0.054397, 1.017008, -0.580728, -0.453669, 0.636843, -0.459227, 0.180882,
  0.188204, 0.476183, 0.415532, 0.268663, 0.497255, 0.343802, 0.328831, 0.518291, 0.262606, 0.795895, -0.620442, 0.217384,
  0.705905, -0.691150, 0.306551, 0.574483, -0.729367, 0.376511, 0.393144, -0.718548, 0.422717, 0.739024, -1.062759, 0.140784,
  0.150377, -0.640294, 0.440182, 0.934478, 0.522184, -0.599785, 0.999051, 0.287002, -0.680546, 0.836683, 0.706314, -0.453298,
  0.373829, 0.537900, 0.182657, 0.329348, 0.565192, 0.237340, 0.756678, -0.749130, 0.264048, 0.848585, -0.659342, 0.140800,
  0.607568, -0.801993, 0.363921, 0.409815, -0.796577, 0.446238, 0.180362, -0.766742, 0.483141, 0.503072, -1.152277, 0.305872,
  0.340133, 0.537087, 0.209900, 0.282695, 0.536743, 0.243945, 0.280828, 0.535931, 0.250947, 0.224496, 0.534602, 0.299947,
  0.149263, 0.536474, 0.341323, 0.184070, 0.629700, 0.355302, 0.150591, 0.537920, 0.321779, 0.224268, 0.536505, 0.286372,
  0.267288, 0.594726, 0.306168, 1.023923, 0.061211, -0.712529, 1.030272, -0.153015, -0.697178, 1.032800, -0.362080, -0.618370,
  0.218008, -0.420692, 0.206676, 0.246096, -0.413324, 0.208569, 0.249469, -0.447025, 0.223800, 0.725905, 0.840634, -0.293072,
  0.586526, 0.965015, -0.163231, 0.538414, -0.385922, 0.219916, 0.461210, -0.380107, 0.240703, 0.376698, -0.389771, 0.242610,
  0.332271, 1.131755, 0.047726, 0.464207, 1.055147, -0.059416, 0.299054, -0.404678, 0.228463, 0.315000, -0.481057, 0.246085,
  0.400654, -0.506428, 0.265804, 0.484521, -0.511453, 0.264909, 0.560504, -0.498885, 0.243186, 0.610102, -0.476705, 0.212886,
  -0.060614, 0.144146, 0.748917, -0.143254, 0.142491, 0.738137, 0.022988, 0.140953, 0.741846, -0.052923, 0.236576, 0.502754,
  -0.095355, 0.215882, 0.563569, -0.049342, 0.320509, 0.474823, -0.182684, 0.321755, 0.462647, -0.015121, 0.213169, 0.565568,
  0.089556, 0.318297, 0.470264, -0.136524, -0.177697, 0.633978, -0.146763, -0.076039, 0.719840, -0.138003, -0.270563, 0.565248,
  -0.205051, -0.134770, 0.562971, 0.028471, -0.078786, 0.725421, -0.136842, -0.370267, 0.475673, 0.039418, -0.372230, 0.482621,
  0.034898, -0.271289, 0.570870, -0.050253, -0.624062, 0.462840, -0.049625, -0.750682, 0.488350, -0.159513, -0.493694, 0.385433,
  0.068879, -0.493077, 0.393919, -0.312622, -0.980631, 0.430433, -0.051043, -0.978842, 0.459144, 0.214299, -0.983353, 0.446481,
  -0.041589, 0.514682, 0.458229, -0.038427, 0.536292, 0.426572, -0.153841, 0.503310, 0.444642, 0.073298, 0.498483, 0.452303,
  -0.142439, 0.532130, 0.408340, 0.067186, 0.526273, 0.415656, -0.032177, 0.582085, 0.371393, -0.029458, 0.622097, 0.396761,
  -0.137090, 0.616291, 0.386431, -0.133261, 0.579195, 0.357712, 0.077359, 0.612264, 0.393041, 0.071322, 0.573016, 0.364702,
  -0.024127, 0.783306, 0.326249, -0.146914, 0.780539, 0.317885, -0.024275, 0.896575, 0.314160, -0.175888, 0.886694, 0.291495,
  0.095047, 0.775157, 0.323577, 0.123487, 0.887190, 0.300686, -0.057526, 0.195193, 0.697967, -0.107888, 0.191462, 0.690556,
  -0.007047, 0.189168, 0.693679, -0.150350, 0.191485, 0.565690, -0.215614, 0.191616, 0.473959, -0.251426, 0.131409, 0.596014,
  -0.199437, 0.212592, 0.477112, -0.297615, 0.143070, 0.499322, -0.157603, 0.174858, 0.652573, -0.128161, 0.204643, 0.564074,
  -0.879165, -0.836024, 0.074208, -0.950771, -0.697461, -0.102036, -0.425527, -0.334292, 0.224397, -0.520247, -0.322951, 0.215296,
  -0.342388, -0.358923, 0.218463, -0.531820, -0.250610, 0.203308, -0.415487, -0.274981, 0.224281, -0.609439, -0.325988, 0.187378,
  -0.650941, -0.249055, 0.161129, -0.678833, -0.343756, 0.144852, -0.741094, -0.276784, 0.101279, -0.719461, -0.380270, 0.105532,
  -0.788657, -0.315216, 0.052408, -0.755895, -0.437628, 0.072526, -0.812358, -0.408939, 0.031222, -0.287566, -0.386663, 0.207727,
  -0.321694, -0.309459, 0.233826, -0.573862, -0.607148, 0.265467, -0.463070, -0.597246, 0.267910, -0.671091, -0.587198, 0.232105,
  -0.470024, -0.641187, 0.305583, -0.606600, -0.651868, 0.295671, -0.364503, -0.556481, 0.240331, -0.306045, -0.579062, 0.289900,
  -0.731412, -0.547559, 0.190847, -0.715464, -0.627317, 0.253009, -0.784744, -0.579431, 0.190036, -0.764286, -0.500366, 0.132655,
  -0.880528, -0.246663, -0.005853, -0.823601, -0.183103, 0.079097, -0.366937, 0.942302, 0.185831, -0.387374, 1.046452, 0.124784,
  -0.220404, 1.108414, 0.222723, -0.322152, 0.839671, 0.234951, -0.208291, 1.007326, 0.275727, -0.492787, 0.850333, 0.093422,
  -1.024554, -0.348355, -0.373039, -0.986989, -0.536212, -0.263477, -0.955208, -0.357925, -0.138518, -0.931710, -0.524581, -0.037878,
  -1.032800, -0.150820, -0.414175, -0.888448, -0.376903, -0.042022, -0.810938, -0.501972, 0.104990, -0.877119, -0.509878, 0.059788,
  -0.502922, 0.051650, 0.295767, -0.410692, -0.127833, 0.286503, -0.554875, -0.063692, 0.252498, -0.396546, -0.021592, 0.319355,
  -0.448506, 0.173929, 0.293024, -0.643861, 0.163301, 0.248217, -0.526043, 0.269758, 0.266368, -0.231212, 0.532304, 0.366011,
  -0.301331, 0.537260, 0.299163, -0.247639, 0.512127, 0.385916, -0.318865, 0.523045, 0.318679, -0.358190, 0.542263, 0.240337,
  -0.376020, 0.538291, 0.238770, -0.461601, 0.627869, 0.173268, -0.539496, 0.545806, 0.146968, -0.374064, 0.684375, 0.229014,
  -0.545646, 0.669726, 0.116429, -0.441155, 0.767958, 0.172944, -0.154495, 0.043895, 0.770301, -0.236469, 0.054073, 0.687009,
  -0.211692, 0.124960, 0.690776, -0.227969, -0.044069, 0.659045, -0.315602, -0.185500, 0.324386, -0.251722, -0.247459, 0.362832,
  -0.316271, -0.252411, 0.274162, -0.315647, -0.093425, 0.359369, -0.242877, -0.305081, 0.293164, -0.255609, -0.159029, 0.421179,
  -0.364701, 0.051662, 0.470822, -0.305690, 0.008804, 0.561444, -0.337672, 0.134882, 0.508657, -0.332491, 0.159697, 0.460603,
  -0.364786, 0.143755, 0.443701, -0.377788, 0.094186, 0.415085, -0.380238, 0.099751, 0.319674, -0.324216, -0.020993, 0.410279,
  -0.763648, 0.044751, 0.165162, -0.711475, -0.150598, 0.168421, -0.957898, 0.016907, -0.101835, -0.835249, 0.249125, 0.051253,
  -0.750575, -0.933068, 0.221212, -0.227227, -0.491494, 0.259044, -0.277385, -0.480381, 0.219916, -0.504770, 0.464767, 0.224957,
  -0.628547, 0.559835, 0.104812, -0.999062, 0.254861, -0.437963, -0.957909, 0.446692, -0.406027, -0.296583, 0.158632, 0.461685,
  -0.271921, 0.177483, 0.443970, -0.302930, 0.189274, 0.429108, -0.408633, 0.550867, 0.185468, -0.419945, 0.552651, 0.173084,
  -0.370465, 0.565564, 0.232836, -0.353366, 0.557848, 0.234572, -0.540521, -0.977002, 0.353600, -0.311490, 0.582787, 0.304120,
  -0.279352, 0.114841, 0.615001, -0.211518, 0.139154, 0.644266, -0.272715, 0.741429, 0.284934, -0.222743, 0.568349, 0.327976,
  -0.232544, 0.600367, 0.349432, -0.297190, 0.561060, 0.288576, -0.438335, 0.395868, 0.304919, -0.354046, 0.344810, 0.368381,
  -0.597943, 0.391996, 0.203978, -1.022329, 0.048240, -0.443427, -0.055470, 0.218607, 0.571493, -0.183546, 0.230290, 0.474982,
  -0.326042, 0.199969, 0.374943, -0.412950, -0.207692, 0.242502, -0.540185, -0.166108, 0.216405, -0.966907, -0.176177, -0.116213,
  -0.258850, -0.340850, 0.242481, -0.253652, -0.416871, 0.201570, -0.211270, -0.394785, 0.258607, -0.199834, -0.221337, 0.493950,
  -0.193180, -0.315387, 0.412399, -0.307297, 0.072094, 0.594427, -0.190054, -0.378893, 0.325370, -0.943830, 0.206196, -0.142581,
  -0.140923, 0.184765, 0.676815, -0.267957, -0.073307, 0.482632, -0.764803, 0.733652, -0.175392, -0.873799, 0.614612, -0.288008,
  -0.643107, 0.855071, -0.083598, -0.845677, 0.476430, -0.104723, -0.735861, 0.573300, 0.013661, -0.922939, 0.366984, -0.182783,
  -0.520389, 0.949294, 0.015146, -0.020086, 1.126332, 0.252138, -0.620347, 0.744612, 0.025655, -0.024552, 1.021187, 0.308229,
  0.175872, 1.110105, 0.235454, -0.721305, 0.315326, 0.155697, 0.026670, -0.178835, 0.640213, 0.157971, 1.007702, 0.287504,
  0.102275, -0.138414, 0.573517, 0.043988, 0.188836, 0.570207, 0.118082, 0.186578, 0.480794, 0.147203, 0.126612, 0.608391,
  0.099737, 0.209510, 0.483596, 0.199442, 0.138890, 0.512628, 0.048151, 0.171499, 0.657091, 0.101706, 0.136870, 0.653216,
  0.019790, 0.204709, 0.567671, 0.819270, -0.842050, 0.128418, 0.911245, -0.703367, -0.041064, 0.362757, -0.337721, 0.253992,
  0.456465, -0.325146, 0.250292, 0.275944, -0.363053, 0.241662, 0.469012, -0.254885, 0.238386, 0.349072, -0.278985, 0.252288,
  0.548323, -0.329128, 0.227291, 0.592078, -0.255148, 0.203698, 0.621008, -0.347813, 0.189084, 0.688024, -0.280939, 0.150016,
  0.660992, -0.381754, 0.152768, 0.739480, -0.320921, 0.103957, 0.698535, -0.438514, 0.121219, 0.761468, -0.410823, 0.083444,
  0.222096, -0.388169, 0.228729, 0.253691, -0.312594, 0.254846, 0.502753, -0.607886, 0.303024, 0.391594, -0.596427, 0.297340,
  0.602875, -0.588501, 0.274315, 0.391936, -0.640216, 0.333193, 0.529666, -0.650452, 0.332039, 0.296310, -0.557510, 0.264380,
  0.230324, -0.577320, 0.309303, 0.667687, -0.549164, 0.236047, 0.642847, -0.626717, 0.296659, 0.719335, -0.579735, 0.238453,
  0.707110, -0.500672, 0.180189, 0.838577, -0.252566, 0.050541, 0.775184, -0.191022, 0.132546, 0.318006, 0.941474, 0.205753,
  0.346937, 1.047053, 0.147628, 0.269482, 0.836600, 0.251683, 0.448346, 0.847390, 0.120708, 1.007823, -0.359025, -0.306367,
  0.961426, -0.545070, -0.199423, 0.923794, -0.364001, -0.078225, 0.894029, -0.526913, 0.021964, 1.018936, -0.161320, -0.345337,
  0.848816, -0.381497, 0.015100, 0.756038, -0.501851, 0.156457, 0.827348, -0.510716, 0.115845, 0.431661, 0.046142, 0.325499,
  0.337476, -0.134947, 0.312835, 0.489127, -0.071252, 0.288051, 0.320906, -0.027147, 0.342777, 0.373105, 0.168320, 0.316535,
  0.582976, 0.156733, 0.285736, 0.461185, 0.263631, 0.294980, 0.160790, 0.525061, 0.379632, 0.238416, 0.525070, 0.315998,
  0.173808, 0.502371, 0.400428, 0.254061, 0.511848, 0.337369, 0.300534, 0.531961, 0.259974, 0.314507, 0.523873, 0.260625,
  0.403069, 0.621294, 0.197657, 0.480673, 0.536049, 0.177533, 0.316778, 0.679072, 0.248067, 0.495134, 0.664699, 0.145225,
  0.389671, 0.761776, 0.195596, 0.034621, 0.044661, 0.775449, 0.123825, 0.051169, 0.698209, 0.097848, 0.121668, 0.699692,
  0.118765, -0.046164, 0.669786, 0.236407, -0.189277, 0.344762, 0.166742, -0.250523, 0.379381, 0.241617, -0.256326, 0.295233,
  0.234444, -0.097298, 0.378522, 0.166001, -0.308007, 0.309174, 0.166690, -0.162547, 0.436715, 0.273366, 0.047569, 0.490107,
  0.206970, 0.004938, 0.576799, 0.240684, 0.131461, 0.524349, 0.240445, 0.154200, 0.476615, 0.276767, 0.138136, 0.461898,
  0.291839, 0.088761, 0.434680, 0.298403, 0.094687, 0.339279, 0.238597, -0.025252, 0.428136, 0.709434, 0.034736, 0.213911,
  0.654455, -0.158223, 0.214649, 0.926360, 0.007778, -0.038517, 0.793248, 0.239930, 0.102323, 0.675762, -0.938370, 0.265395,
  0.151812, -0.492261, 0.273257, 0.209033, -0.481592, 0.240508, 0.440561, 0.458917, 0.252939, 0.573456, 0.550786, 0.140362,
  0.988505, 0.247596, -0.372294, 0.942961, 0.437890, -0.343502, 0.200905, 0.154460, 0.475251, 0.179315, 0.172149, 0.454640,
  0.213655, 0.184916, 0.441895, 0.352009, 0.536479, 0.207588, 0.365827, 0.537072, 0.195941, 0.314127, 0.553577, 0.252039,
  0.295172, 0.545536, 0.253255, 0.449273, -0.980922, 0.383054, 0.253451, 0.571918, 0.319567, 0.171707, 0.109583, 0.628460,
  0.218232, 0.736478, 0.298296, 0.161991, 0.562021, 0.339025, 0.175068, 0.594014, 0.361635, 0.239032, 0.549888, 0.303199,
  0.364961, 0.388626, 0.327964, 0.271665, 0.338392, 0.386302, 0.537446, 0.386414, 0.237820, 1.012756, 0.038023, -0.376209,
  0.083653, 0.226265, 0.481609, 0.238883, 0.195537, 0.388955, 0.344943, -0.211502, 0.270264, 0.478295, -0.172336, 0.252086,
  0.935399, -0.187449, -0.054634, 0.185317, -0.342614, 0.260070, 0.185044, -0.418999, 0.219222, 0.136305, -0.395857, 0.271835,
  0.103026, -0.222730, 0.504939, 0.100818, -0.318055, 0.423770, 0.203577, 0.068264, 0.609259, 0.109589, -0.379295, 0.337409,
  0.917263, 0.195726, -0.080844, 0.026774, 0.181478, 0.681201, 0.175180, -0.077942, 0.497146, 0.734488, 0.730570, -0.127149,
  0.848417, 0.606580, -0.231610, 0.606626, 0.851981, -0.045612, 0.809480, 0.468849, -0.053374, 0.693087, 0.567955, 0.056427,
  0.896052, 0.356641, -0.125145, 0.481602, 0.951041, 0.044999, 0.574763, 0.740599, 0.060101, 0.669506, 0.307629, 0.197621,
  -0.528274, -0.450806, 0.239267, -0.436741, -0.450806, 0.239267, -0.463550, -0.386082, 0.239267, -0.528274, -0.359273, 0.239267,
  -0.592998, -0.386082, 0.239267, -0.619807, -0.450806, 0.239267, -0.592998, -0.515530, 0.239267, -0.528274, -0.542339, 0.239267,
  -0.463550, -0.515529, 0.239267, 0.480427, -0.450399, 0.235104, 0.571244, -0.450399, 0.235104, 0.544645, -0.386181, 0.235104,
  0.480427, -0.359582, 0.235104, 0.416210, -0.386181, 0.235104, 0.389610, -0.450399, 0.235104, 0.416210, -0.514616, 0.235104,
  0.480427, -0.541216, 0.235104, 0.544645, -0.514616, 0.235104
]);

const BLENDSHAPE_DELTAS = [
  // 0 — vis_ff
  new Float32Array([
    -0.021697, 0.014341, 0.026110, -0.025109, 0.008902, 0.029740, -0.019015, 0.017891, 0.020858, 0.001478, -0.001983, -0.005431,
    -0.000496, 0.001082, -0.012820, -0.013374, 0.007651, 0.010957, -0.010881, 0.016063, -0.009519, -0.003385, 0.004723, -0.019561,
    -0.009351, 0.010549, -0.035451, -0.012155, 0.009822, -0.043916, -0.006230, 0.008055, -0.028095, -0.033061, 0.023018, -0.009787,
    -0.034337, 0.024473, -0.010135, -0.031990, 0.021317, -0.010127, -0.015313, 0.021744, -0.096929, -0.011332, 0.019747, -0.085305,
    -0.025120, 0.024505, -0.105680, -0.027191, 0.028337, 0.030803, -0.030241, 0.025582, 0.034792, -0.026370, 0.033639, 0.026459,
    -0.029160, 0.030950, 0.044347, -0.032403, 0.028489, 0.047084, -0.027763, 0.035445, 0.039316, -0.033068, 0.033828, 0.051665,
    -0.035412, 0.030999, 0.054131, -0.030431, 0.038722, 0.046315, -0.044059, 0.030912, -0.024215, -0.031648, 0.024679, -0.041397,
    -0.052695, 0.036842, -0.005910, -0.031838, 0.023670, -0.011630, -0.033383, 0.011224, 0.035466, -0.038859, 0.016018, 0.040201,
    -0.041254, 0.020432, 0.045285, -0.038027, 0.011324, -0.021335, -0.033310, 0.006129, -0.030750, -0.028985, 0.001242, -0.037441,
    -0.023062, -0.004276, -0.042519, -0.020738, 0.020272, -0.057060, -0.017053, -0.006730, -0.046488, -0.048362, 0.060467, 0.073964,
    -0.052661, 0.055907, 0.059096, -0.046145, 0.064097, 0.082071, -0.040942, 0.024433, 0.051245, -0.041223, 0.026941, 0.051867,
    -0.035150, 0.007157, -0.033361, -0.040137, 0.013550, -0.022440, -0.029665, 0.001649, -0.041904, -0.023207, -0.004678, -0.047876,
    -0.017158, -0.009193, -0.054796, -0.014017, 0.018617, -0.072404, -0.041527, 0.024883, 0.048048, -0.040845, 0.026875, 0.047632,
    -0.039542, 0.024722, 0.043794, -0.036857, 0.024452, 0.040258, -0.033197, 0.024313, 0.037673, -0.037074, 0.029652, 0.053419,
    -0.035415, 0.027127, 0.047721, -0.038879, 0.027055, 0.047679, -0.039714, 0.028590, 0.052648, -0.057607, 0.051130, 0.044530,
    -0.060495, 0.047171, 0.029635, -0.058528, 0.043188, 0.013788, -0.022418, 0.008774, -0.024706, -0.024715, 0.009900, -0.023424,
    -0.022643, 0.010708, -0.024812, -0.046560, 0.066274, 0.085835, -0.048281, 0.068053, 0.087657, -0.031168, 0.019366, -0.011561,
    -0.030244, 0.017203, -0.013937, -0.029046, 0.014994, -0.017292, -0.046219, 0.064035, 0.088396, -0.045139, 0.063705, 0.082213,
    -0.047226, 0.066011, 0.089940, -0.048502, 0.068036, 0.089091, -0.041542, 0.065196, 0.073511, -0.026814, 0.012188, -0.020355,
    -0.024263, 0.015961, -0.023781, -0.025867, 0.021172, -0.021849, -0.025755, 0.023322, -0.019170, -0.026834, 0.023724, -0.015799,
    -0.029616, 0.023534, -0.013255, -0.033149, 0.024045, -0.060968, -0.033565, 0.024473, -0.063693, -0.031963, 0.022944, -0.058918,
    -0.073450, 0.041042, -0.103239, -0.063731, 0.035387, -0.112324, -0.080058, 0.047331, -0.088477, -0.033427, 0.023871, -0.063268,
    -0.016806, 0.029054, 0.017367, -0.018246, 0.039172, 0.015635, -0.021889, 0.047827, 0.015047, -0.032876, 0.012373, -0.083105,
    -0.023274, 0.006892, -0.084685, -0.015467, 0.001063, -0.080492, -0.009786, -0.005518, -0.072323, -0.051165, 0.030606, -0.114812,
    -0.008564, -0.008996, -0.060673, -0.071350, 0.069077, 0.001421, -0.075171, 0.065117, -0.018220, -0.064690, 0.070385, 0.015908,
    -0.027827, 0.055559, 0.018086, -0.026986, 0.054304, 0.024732, -0.027319, 0.008170, -0.090860, -0.039041, 0.015702, -0.088084,
    -0.016164, 0.001699, -0.087499, -0.008002, -0.005831, -0.078386, -0.004768, -0.010798, -0.069829, -0.035395, 0.026520, -0.112924,
    -0.027724, 0.053331, 0.020044, -0.028425, 0.050125, 0.023481, -0.028387, 0.048871, 0.018927, -0.027557, 0.044537, 0.020488,
    -0.026650, 0.039204, 0.022870, -0.028313, 0.044882, 0.038225, -0.027088, 0.040560, 0.033753, -0.027258, 0.046035, 0.028756,
    -0.026536, 0.050472, 0.032052, -0.077275, 0.060860, -0.035971, -0.079282, 0.057231, -0.054059, -0.081681, 0.053139, -0.070364,
    -0.030372, 0.012580, -0.049136, -0.029233, 0.014520, -0.049734, -0.031560, 0.013716, -0.051991, -0.056331, 0.070611, 0.029414,
    -0.048445, 0.070546, 0.041405, -0.030691, 0.021879, -0.056506, -0.029445, 0.020872, -0.053782, -0.028681, 0.019624, -0.051439,
    -0.040920, 0.067697, 0.062417, -0.043835, 0.069824, 0.051978, -0.028819, 0.017226, -0.049891, -0.033609, 0.017426, -0.054461,
    -0.033860, 0.021429, -0.057584, -0.033950, 0.023003, -0.060665, -0.033661, 0.023700, -0.062752, -0.033406, 0.023674, -0.062979,
    0.000000, 0.000000, -0.000000, -0.000885, -0.000184, 0.001935, -0.001010, 0.001618, -0.002438, -0.009390, 0.012159, 0.011069,
    -0.005630, 0.007700, 0.008146, -0.014359, 0.014982, 0.016489, -0.016869, 0.013373, 0.021680, -0.007428, 0.009203, 0.005887,
    -0.014691, 0.018608, 0.012585, -0.005229, 0.004947, -0.015322, -0.002055, 0.001140, -0.009085, -0.007776, 0.007996, -0.024087,
    -0.009125, 0.006709, -0.009717, -0.001287, 0.002733, -0.015302, -0.010705, 0.010408, -0.030352, -0.012215, 0.011641, -0.037788,
    -0.008249, 0.009255, -0.030475, -0.011329, -0.000924, -0.052642, -0.010986, -0.003933, -0.063199, -0.013153, 0.006512, -0.037804,
    -0.017819, 0.006259, -0.047158, -0.012761, 0.009570, -0.072589, -0.013266, 0.012170, -0.081686, -0.016839, 0.012907, -0.090645,
    -0.023404, 0.018547, 0.028442, -0.025540, 0.024513, 0.030174, -0.027251, 0.015205, 0.032127, -0.020816, 0.024025, 0.023225,
    -0.029058, 0.021868, 0.034439, -0.023734, 0.030164, 0.025747, -0.029981, 0.031108, 0.045682, -0.031486, 0.032146, 0.048425,
    -0.034159, 0.029065, 0.051185, -0.032996, 0.028505, 0.048261, -0.028956, 0.036653, 0.042898, -0.027949, 0.035765, 0.040265,
    -0.035822, 0.044245, 0.056406, -0.036942, 0.043006, 0.059315, -0.036828, 0.049788, 0.063694, -0.037965, 0.049309, 0.067912,
    -0.032947, 0.046037, 0.051651, -0.033114, 0.051332, 0.057617, -0.003479, 0.004335, 0.002838, -0.003443, 0.003876, 0.004789,
    -0.004764, 0.005373, 0.001142, -0.005781, 0.006845, 0.007842, -0.011100, 0.010128, 0.010972, -0.007453, 0.003154, 0.007166,
    -0.011564, 0.010789, 0.012485, -0.011239, 0.006001, 0.008677, -0.004368, 0.004530, 0.005344, -0.005445, 0.007441, 0.007927,
    -0.031167, 0.016238, -0.038743, -0.039785, 0.022334, -0.025161, -0.028747, 0.015913, -0.014680, -0.030232, 0.018500, -0.011451,
    -0.025731, 0.012330, -0.019719, -0.029345, 0.018730, -0.005903, -0.027239, 0.016643, -0.010364, -0.031268, 0.019905, -0.008859,
    -0.031151, 0.020201, -0.002256, -0.033432, 0.021962, -0.006719, -0.034192, 0.022383, 0.000247, -0.036004, 0.023559, -0.006131,
    -0.035862, 0.023210, -0.000507, -0.039123, 0.023380, -0.008702, -0.039467, 0.022655, -0.005956, -0.023103, 0.009571, -0.022558,
    -0.023607, 0.013624, -0.017201, -0.029414, 0.010731, -0.021444, -0.027423, 0.009729, -0.027303, -0.032246, 0.012072, -0.017600,
    -0.026685, 0.002730, -0.034855, -0.029596, 0.004878, -0.029965, -0.024998, 0.009492, -0.028510, -0.022668, 0.002879, -0.036112,
    -0.035337, 0.014365, -0.014844, -0.032146, 0.007956, -0.024291, -0.036162, 0.012385, -0.019945, -0.038028, 0.018220, -0.012021,
    -0.035418, 0.024046, 0.006757, -0.032393, 0.022764, 0.009226, -0.041424, 0.054435, 0.075965, -0.043671, 0.060348, 0.084437,
    -0.042014, 0.059360, 0.083109, -0.039962, 0.048774, 0.067285, -0.039322, 0.054409, 0.075988, -0.042200, 0.053058, 0.075305,
    -0.046822, 0.033221, 0.008268, -0.044852, 0.028344, -0.008995, -0.041822, 0.025827, 0.002180, -0.041691, 0.020719, -0.010680,
    -0.045867, 0.035949, 0.024848, -0.039329, 0.023215, -0.001782, -0.038471, 0.017929, -0.012661, -0.039231, 0.017888, -0.011938,
    -0.027091, 0.018880, 0.014004, -0.026840, 0.017500, -0.001327, -0.029407, 0.019053, 0.006569, -0.025074, 0.016545, 0.005737,
    -0.025535, 0.019458, 0.019905, -0.029866, 0.022355, 0.026537, -0.029451, 0.022523, 0.029635, -0.033047, 0.021316, 0.037805,
    -0.036822, 0.022597, 0.040915, -0.033097, 0.016515, 0.037023, -0.037794, 0.019279, 0.040620, -0.039841, 0.023717, 0.044260,
    -0.040569, 0.022070, 0.045170, -0.038832, 0.035350, 0.057422, -0.037143, 0.029952, 0.055223, -0.039292, 0.039551, 0.058631,
    -0.038457, 0.041160, 0.063600, -0.040565, 0.046273, 0.066077, 0.000156, -0.001380, -0.002785, -0.003495, 0.000443, 0.001501,
    -0.003288, 0.000735, 0.003750, -0.006066, 0.003005, -0.003556, -0.022224, 0.015843, -0.008964, -0.018213, 0.014365, -0.014710,
    -0.020663, 0.014296, -0.013389, -0.021649, 0.013841, -0.001640, -0.017469, 0.011512, -0.020298, -0.016599, 0.011755, -0.008687,
    -0.016245, 0.009029, 0.008716, -0.011692, 0.005550, 0.002653, -0.012128, 0.006796, 0.011236, -0.013339, 0.009481, 0.012841,
    -0.014999, 0.010961, 0.014071, -0.018062, 0.012187, 0.012091, -0.021352, 0.015482, 0.013017, -0.020307, 0.011675, 0.002688,
    -0.032415, 0.023031, 0.021682, -0.029426, 0.020565, 0.006797, -0.036371, 0.029931, 0.030067, -0.033293, 0.030316, 0.040416,
    -0.023543, 0.011847, -0.050678, -0.020167, 0.007074, -0.033243, -0.022146, 0.008833, -0.029375, -0.033220, 0.024409, 0.044449,
    -0.036031, 0.034551, 0.058964, -0.041140, 0.044621, 0.052118, -0.038821, 0.049407, 0.065278, -0.012392, 0.008122, 0.012184,
    -0.012172, 0.009926, 0.011198, -0.013931, 0.012017, 0.013943, -0.041277, 0.024726, 0.048448, -0.041040, 0.024485, 0.049350,
    -0.040945, 0.026534, 0.049936, -0.040729, 0.026440, 0.048527, -0.017102, 0.008723, -0.062944, -0.039375, 0.027343, 0.050790,
    -0.007253, 0.002717, 0.006322, -0.005237, 0.002664, 0.005056, -0.038330, 0.041683, 0.059323, -0.035944, 0.027126, 0.048609,
    -0.036605, 0.027932, 0.051214, -0.038835, 0.027033, 0.048312, -0.028694, 0.020759, 0.035657, -0.023046, 0.017257, 0.028793,
    -0.033084, 0.026440, 0.042140, -0.043877, 0.039544, 0.038394, -0.006180, 0.008409, 0.007352, -0.012412, 0.011507, 0.015421,
    -0.016830, 0.014493, 0.016958, -0.024743, 0.016912, -0.006236, -0.027248, 0.018849, 0.000174, -0.038581, 0.027789, 0.016274,
    -0.020971, 0.011261, -0.020977, -0.021464, 0.007829, -0.026282, -0.019151, 0.009533, -0.027891, -0.011820, 0.009581, -0.016457,
    -0.014325, 0.012419, -0.022714, -0.008534, 0.003591, 0.005812, -0.015772, 0.009382, -0.027580, -0.035315, 0.034516, 0.043972,
    -0.003675, 0.003872, 0.005216, -0.015284, 0.009335, -0.003786, -0.041064, 0.055731, 0.077360, -0.038935, 0.053004, 0.073407,
    -0.043935, 0.058787, 0.081032, -0.035642, 0.041457, 0.059881, -0.036950, 0.041680, 0.062552, -0.035374, 0.040230, 0.055180,
    -0.044639, 0.060750, 0.083406, -0.041526, 0.059110, 0.076976, -0.040773, 0.049378, 0.072032, -0.038561, 0.054480, 0.070105,
    -0.037491, 0.060528, 0.068499, -0.032522, 0.028538, 0.041378, -0.004637, 0.006033, -0.022428, -0.034724, 0.055726, 0.062301,
    -0.007623, 0.009367, -0.021939, -0.009728, 0.010562, 0.002369, -0.012217, 0.016653, 0.003070, -0.008206, 0.009290, -0.006838,
    -0.011209, 0.016675, 0.004868, -0.011256, 0.013730, -0.005810, -0.007968, 0.007705, -0.000194, -0.006907, 0.007336, -0.004831,
    -0.008794, 0.010014, 0.004171, -0.046400, 0.023860, -0.101662, -0.057444, 0.029652, -0.096096, -0.029096, 0.020107, -0.047851,
    -0.030846, 0.021808, -0.051150, -0.028412, 0.017088, -0.045878, -0.031048, 0.023591, -0.045365, -0.028537, 0.021046, -0.041533,
    -0.033291, 0.023077, -0.054827, -0.035185, 0.025827, -0.050240, -0.035073, 0.024767, -0.058103, -0.038465, 0.027661, -0.054680,
    -0.035203, 0.025871, -0.060348, -0.042834, 0.028346, -0.058719, -0.033811, 0.024768, -0.064714, -0.040739, 0.025902, -0.065736,
    -0.028134, 0.013938, -0.045995, -0.028023, 0.017711, -0.040791, -0.025718, 0.011964, -0.064102, -0.025734, 0.010897, -0.060937,
    -0.026322, 0.013635, -0.066618, -0.018481, 0.002783, -0.066525, -0.019602, 0.005929, -0.071316, -0.026776, 0.011140, -0.055822,
    -0.019803, 0.002025, -0.058145, -0.028099, 0.016406, -0.067401, -0.022908, 0.009819, -0.074306, -0.027769, 0.014115, -0.075725,
    -0.031011, 0.019947, -0.067154, -0.050989, 0.031523, -0.057445, -0.044852, 0.030695, -0.051533, -0.034659, 0.057165, 0.051261,
    -0.037477, 0.062388, 0.058143, -0.032438, 0.053010, 0.046235, -0.037076, 0.057526, 0.042268, -0.072015, 0.043571, -0.069936,
    -0.065568, 0.037223, -0.084477, -0.058689, 0.032577, -0.068172, -0.050625, 0.025554, -0.079405, -0.070855, 0.047173, -0.055472,
    -0.050524, 0.028265, -0.066593, -0.035021, 0.019873, -0.071877, -0.042113, 0.021107, -0.077052, -0.024017, 0.027284, -0.020068,
    -0.022094, 0.024290, -0.030871, -0.027146, 0.027283, -0.033391, -0.019290, 0.024300, -0.020538, -0.020398, 0.028966, -0.007677,
    -0.031458, 0.031426, -0.017673, -0.026087, 0.031806, -0.005142, -0.023795, 0.037049, 0.021886, -0.024744, 0.043380, 0.019980,
    -0.020018, 0.033247, 0.019995, -0.021178, 0.041256, 0.018054, -0.026161, 0.049168, 0.018204, -0.023683, 0.048775, 0.016702,
    -0.030651, 0.049825, 0.025958, -0.031121, 0.045924, 0.016325, -0.030343, 0.049351, 0.033662, -0.036519, 0.050668, 0.026786,
    -0.033840, 0.053415, 0.037442, 0.000245, 0.000301, -0.008185, -0.003173, 0.004259, -0.010981, -0.003812, 0.004659, -0.005805,
    -0.004915, 0.006386, -0.015887, -0.020506, 0.020949, -0.031228, -0.019131, 0.017808, -0.032592, -0.026325, 0.017960, -0.036439,
    -0.016343, 0.019661, -0.022909, -0.024149, 0.014340, -0.037973, -0.014420, 0.015765, -0.025621, -0.010830, 0.017422, -0.012485,
    -0.007904, 0.011628, -0.014312, -0.011185, 0.014926, -0.006118, -0.013101, 0.018664, -0.004187, -0.013365, 0.021032, -0.006045,
    -0.013465, 0.021606, -0.009736, -0.015843, 0.025067, -0.008462, -0.012668, 0.018506, -0.016778, -0.037014, 0.033544, -0.033851,
    -0.037619, 0.028638, -0.045211, -0.054691, 0.041363, -0.042371, -0.046552, 0.040928, -0.020091, -0.035333, 0.019153, -0.101312,
    -0.023357, 0.007965, -0.049463, -0.027721, 0.011050, -0.052272, -0.027726, 0.037970, 0.008932, -0.037099, 0.046133, 0.013949,
    -0.068141, 0.054781, -0.023052, -0.066147, 0.058789, -0.006674, -0.012506, 0.016235, -0.003489, -0.013589, 0.017723, -0.001143,
    -0.014425, 0.020891, -0.000397, -0.028000, 0.054174, 0.019599, -0.027917, 0.054854, 0.018914, -0.026772, 0.052820, 0.024951,
    -0.027084, 0.051280, 0.024402, -0.022538, 0.014421, -0.097592, -0.026202, 0.048636, 0.031622, -0.007069, 0.008803, -0.008798,
    -0.030503, 0.048508, 0.041207, -0.026792, 0.041283, 0.034432, -0.027120, 0.042725, 0.036413, -0.026731, 0.047026, 0.029513,
    -0.023436, 0.032192, 0.007567, -0.018399, 0.027407, 0.007230, -0.032102, 0.037050, 0.000241, -0.069021, 0.050475, -0.039780,
    -0.010564, 0.016872, 0.007712, -0.015508, 0.024313, 0.001062, -0.027708, 0.021705, -0.036610, -0.031537, 0.024952, -0.039613,
    -0.056996, 0.038909, -0.055251, -0.026832, 0.014549, -0.040249, -0.028573, 0.011474, -0.046573, -0.025486, 0.011545, -0.042877,
    -0.011657, 0.011966, -0.028450, -0.015671, 0.014570, -0.035444, -0.007190, 0.009706, -0.010760, -0.021974, 0.010823, -0.040336,
    -0.056700, 0.045728, -0.026312, -0.006135, 0.006319, 0.000743, -0.010921, 0.014022, -0.020333, -0.052421, 0.061111, 0.021404,
    -0.059757, 0.060661, 0.008209, -0.045916, 0.062569, 0.036116, -0.053750, 0.050496, -0.001026, -0.046008, 0.049706, 0.010751,
    -0.059108, 0.050274, -0.012139, -0.040276, 0.063625, 0.048013, -0.041988, 0.055240, 0.029575, -0.039338, 0.037856, -0.008970,
    -0.031237, 0.021950, 0.002867, -0.031237, 0.021950, 0.002867, -0.031237, 0.021950, 0.002867, -0.031237, 0.021950, 0.002867,
    -0.031237, 0.021950, 0.002867, -0.031237, 0.021950, 0.002867, -0.031237, 0.021950, 0.002867, -0.031237, 0.021950, 0.002867,
    -0.031237, 0.021950, 0.002867, -0.042456, 0.023180, 0.009718, -0.042456, 0.023180, 0.009718, -0.042456, 0.023180, 0.009718,
    -0.042456, 0.023180, 0.009718, -0.042456, 0.023180, 0.009718, -0.042456, 0.023180, 0.009718, -0.042456, 0.023180, 0.009718,
    -0.042456, 0.023180, 0.009718, -0.042456, 0.023180, 0.009718
  ]),
  // 1 — vis_ll
  new Float32Array([
    -0.017747, 0.036994, 0.045801, -0.015422, 0.033643, 0.048615, -0.021729, 0.040082, 0.036717, 0.001005, -0.003100, -0.005937,
    -0.002853, 0.000144, -0.015089, -0.009455, 0.022216, 0.015203, -0.024590, 0.026688, -0.013323, -0.007682, 0.004696, -0.023147,
    -0.016628, 0.011333, -0.041141, -0.020817, 0.011689, -0.049208, -0.012333, 0.008572, -0.032932, -0.049216, 0.033745, -0.004273,
    -0.051483, 0.035398, -0.004011, -0.047099, 0.032073, -0.004697, -0.029623, 0.017244, -0.107080, -0.027402, 0.016578, -0.091558,
    -0.039677, 0.020448, -0.119788, -0.023095, 0.050591, 0.050822, -0.017827, 0.051226, 0.057729, -0.030700, 0.056594, 0.046958,
    -0.025077, 0.117007, 0.061839, -0.020950, 0.115003, 0.065938, -0.030927, 0.120900, 0.055591, -0.029114, 0.150313, 0.047762,
    -0.033004, 0.146455, 0.053876, -0.024275, 0.152740, 0.043690, -0.071459, 0.051754, -0.019577, -0.056145, 0.037600, -0.037954,
    -0.080787, 0.065095, -0.000324, -0.048669, 0.033953, -0.005927, -0.017831, 0.049046, 0.057792, -0.016338, 0.068527, 0.067785,
    -0.012402, 0.087804, 0.077727, -0.054263, 0.023558, -0.014414, -0.047489, 0.016058, -0.022737, -0.041068, 0.009659, -0.031147,
    -0.034834, 0.001573, -0.038908, -0.042114, 0.026062, -0.054655, -0.027679, -0.004333, -0.046088, -0.052895, 0.117531, 0.086896,
    -0.066724, 0.105895, 0.070982, -0.043052, 0.127006, 0.094994, -0.002881, 0.107035, 0.084095, -0.016864, 0.116475, 0.079636,
    -0.050406, 0.016883, -0.026124, -0.058018, 0.027451, -0.015039, -0.042093, 0.009349, -0.036785, -0.034814, -0.000177, -0.045210,
    -0.027829, -0.008630, -0.054403, -0.032147, 0.018598, -0.074605, 0.000890, 0.104298, 0.079854, -0.000939, 0.106660, 0.073838,
    -0.002071, 0.090780, 0.074354, -0.004049, 0.075683, 0.069919, -0.011352, 0.060502, 0.064328, -0.031925, 0.138933, 0.061096,
    -0.013665, 0.111979, 0.068246, -0.006642, 0.109072, 0.070113, -0.026255, 0.127472, 0.068673, -0.078189, 0.096706, 0.055697,
    -0.085243, 0.088322, 0.038568, -0.086215, 0.078652, 0.020603, -0.039401, 0.016781, -0.023111, -0.042232, 0.018776, -0.021302,
    -0.038925, 0.018587, -0.022571, -0.042348, 0.132695, 0.096910, -0.047764, 0.138581, 0.095283, -0.045868, 0.030089, -0.006576,
    -0.045494, 0.027650, -0.009543, -0.045444, 0.025211, -0.013898, -0.043697, 0.150139, 0.076102, -0.035664, 0.151411, 0.065853,
    -0.048413, 0.146187, 0.084789, -0.050218, 0.142553, 0.090651, -0.027144, 0.147838, 0.055761, -0.044379, 0.021689, -0.017835,
    -0.039395, 0.023960, -0.021029, -0.040959, 0.029509, -0.017673, -0.040887, 0.032310, -0.014259, -0.042649, 0.033237, -0.010327,
    -0.046039, 0.033412, -0.007867, -0.050004, 0.033418, -0.074659, -0.050965, 0.035398, -0.077916, -0.048618, 0.031595, -0.071852,
    -0.081616, 0.054439, -0.129632, -0.072465, 0.043475, -0.137187, -0.089622, 0.065424, -0.115133, -0.051296, 0.034756, -0.077005,
    -0.026454, 0.061161, 0.033536, -0.034775, 0.084882, 0.035546, -0.045095, 0.108012, 0.039048, -0.048302, 0.025551, -0.100336,
    -0.039934, 0.018972, -0.097042, -0.033344, 0.012156, -0.090046, -0.027731, 0.003274, -0.079687, -0.062789, 0.033497, -0.135683,
    -0.023208, -0.003814, -0.065344, -0.106253, 0.107500, -0.014017, -0.107065, 0.097219, -0.037138, -0.096057, 0.115710, 0.003268,
    -0.060766, 0.131014, 0.042291, -0.045216, 0.137201, 0.045235, -0.042402, 0.020026, -0.105646, -0.053855, 0.029032, -0.108622,
    -0.033611, 0.011451, -0.099410, -0.025858, 0.001537, -0.087209, -0.020845, -0.007573, -0.075237, -0.049542, 0.024973, -0.130970,
    -0.065616, 0.125681, 0.044859, -0.061898, 0.123691, 0.043327, -0.060687, 0.107454, 0.042949, -0.053760, 0.088752, 0.044555,
    -0.041083, 0.070342, 0.045194, -0.025470, 0.150535, 0.041632, -0.040392, 0.122806, 0.050042, -0.052008, 0.123503, 0.045942,
    -0.032734, 0.143975, 0.042560, -0.104194, 0.089962, -0.056768, -0.099672, 0.083936, -0.077705, -0.095602, 0.076358, -0.096031,
    -0.044368, 0.021909, -0.056753, -0.042537, 0.024015, -0.057697, -0.046194, 0.023331, -0.059914, -0.077328, 0.120693, 0.019298,
    -0.054408, 0.127774, 0.031858, -0.046325, 0.030251, -0.068522, -0.043277, 0.029460, -0.064641, -0.041939, 0.028715, -0.061140,
    -0.028736, 0.141000, 0.047009, -0.039116, 0.134313, 0.039752, -0.041619, 0.026524, -0.058582, -0.049074, 0.028170, -0.062955,
    -0.050314, 0.032931, -0.066323, -0.051326, 0.034522, -0.070929, -0.052166, 0.035108, -0.074656, -0.051398, 0.034840, -0.076100,
    0.000000, 0.000000, -0.000000, -0.000481, 0.000895, 0.003643, -0.002270, 0.001861, -0.002499, -0.010927, 0.018752, 0.012051,
    -0.004942, 0.011060, 0.010399, -0.014069, 0.029814, 0.019933, -0.011879, 0.032817, 0.026496, -0.009836, 0.012142, 0.007253,
    -0.020306, 0.036443, 0.014282, -0.009688, 0.006762, -0.017385, -0.004247, 0.001909, -0.010406, -0.014268, 0.010202, -0.027313,
    -0.013015, 0.012115, -0.010008, -0.004769, 0.002339, -0.019188, -0.018820, 0.013138, -0.033804, -0.020094, 0.013853, -0.044097,
    -0.015053, 0.010674, -0.036136, -0.021704, 0.000959, -0.054844, -0.022487, -0.002806, -0.065088, -0.023557, 0.009591, -0.040914,
    -0.028593, 0.010304, -0.053532, -0.025998, 0.008041, -0.075229, -0.025386, 0.009298, -0.087826, -0.030749, 0.010483, -0.100297,
    -0.018788, 0.041571, 0.051092, -0.020940, 0.047637, 0.052204, -0.016091, 0.041480, 0.054593, -0.023992, 0.047759, 0.042709,
    -0.016889, 0.048662, 0.058132, -0.027665, 0.054443, 0.046973, -0.025493, 0.124823, 0.060955, -0.026832, 0.137902, 0.057027,
    -0.028438, 0.134109, 0.060883, -0.023747, 0.121162, 0.065015, -0.025053, 0.140099, 0.049865, -0.027691, 0.127052, 0.054377,
    -0.031083, 0.141251, 0.030833, -0.033310, 0.139812, 0.037772, -0.030443, 0.137994, 0.041904, -0.033199, 0.135957, 0.049293,
    -0.027749, 0.141282, 0.028225, -0.025963, 0.135777, 0.035865, -0.003624, 0.006158, 0.004247, -0.002900, 0.006489, 0.007014,
    -0.006365, 0.007435, 0.001929, -0.003241, 0.011880, 0.010408, -0.006423, 0.021982, 0.013572, -0.003399, 0.011054, 0.010271,
    -0.007585, 0.022460, 0.014957, -0.005773, 0.018757, 0.011851, -0.002325, 0.008628, 0.007739, -0.003916, 0.011852, 0.010360,
    -0.050577, 0.026377, -0.032794, -0.061878, 0.038811, -0.019220, -0.044187, 0.027974, -0.011808, -0.044644, 0.031672, -0.007057,
    -0.041924, 0.023075, -0.017445, -0.042004, 0.036535, 0.000183, -0.040528, 0.031566, -0.007021, -0.045553, 0.034789, -0.003073,
    -0.043757, 0.041197, 0.005261, -0.048615, 0.036974, 0.000060, -0.048215, 0.043930, 0.008975, -0.052352, 0.037910, 0.000982,
    -0.050007, 0.044794, 0.009273, -0.056880, 0.035556, -0.001611, -0.055427, 0.039999, 0.002803, -0.039296, 0.018694, -0.020847,
    -0.037605, 0.026012, -0.014557, -0.044885, 0.018277, -0.016993, -0.043116, 0.016045, -0.023683, -0.048230, 0.020458, -0.011992,
    -0.040581, 0.009018, -0.030954, -0.043830, 0.012398, -0.024573, -0.041242, 0.015159, -0.025884, -0.036817, 0.007349, -0.034150,
    -0.052098, 0.023734, -0.008913, -0.047526, 0.016166, -0.017668, -0.053008, 0.022080, -0.013234, -0.055381, 0.028961, -0.005923,
    -0.047375, 0.051595, 0.018368, -0.042284, 0.051695, 0.020200, -0.037704, 0.131614, 0.070215, -0.041862, 0.138022, 0.079123,
    -0.038781, 0.142686, 0.070055, -0.033719, 0.129988, 0.057785, -0.035016, 0.136888, 0.061789, -0.037358, 0.126638, 0.079593,
    -0.068494, 0.063492, 0.019405, -0.066113, 0.051650, -0.000926, -0.058077, 0.051898, 0.014252, -0.059279, 0.040351, -0.003048,
    -0.065693, 0.071813, 0.039355, -0.054792, 0.045568, 0.008406, -0.055382, 0.030670, -0.005659, -0.055708, 0.034550, -0.004513,
    -0.023285, 0.046605, 0.016525, -0.032414, 0.037535, 0.002301, -0.031280, 0.046813, 0.012113, -0.025730, 0.037808, 0.008177,
    -0.018159, 0.049603, 0.024473, -0.014794, 0.060151, 0.031355, -0.014081, 0.062139, 0.039576, -0.012604, 0.059799, 0.065303,
    -0.007248, 0.075438, 0.070866, -0.014890, 0.055106, 0.063363, -0.012283, 0.072008, 0.070073, -0.003621, 0.091419, 0.075999,
    -0.008288, 0.090038, 0.077916, -0.018546, 0.116577, 0.081035, -0.008234, 0.103015, 0.083608, -0.026377, 0.124816, 0.069555,
    -0.025465, 0.114361, 0.083233, -0.032894, 0.123503, 0.070853, -0.000415, -0.000875, -0.002332, -0.003462, 0.003559, 0.003857,
    -0.001888, 0.004051, 0.005948, -0.007974, 0.007432, -0.003365, -0.029796, 0.031027, -0.006502, -0.026986, 0.025241, -0.013257,
    -0.031457, 0.028196, -0.010426, -0.025443, 0.029322, 0.000317, -0.028373, 0.021717, -0.018562, -0.022034, 0.022253, -0.007493,
    -0.014934, 0.024430, 0.012590, -0.011969, 0.014902, 0.004956, -0.006883, 0.020857, 0.015194, -0.006633, 0.026310, 0.016605,
    -0.009045, 0.029679, 0.018318, -0.014809, 0.031783, 0.016152, -0.018121, 0.039167, 0.016051, -0.022000, 0.026938, 0.005271,
    -0.025271, 0.061499, 0.032543, -0.037825, 0.048442, 0.015848, -0.038922, 0.070435, 0.046274, -0.014768, 0.075976, 0.055142,
    -0.040194, 0.017277, -0.046537, -0.033828, 0.012165, -0.032342, -0.037889, 0.014533, -0.027226, -0.004945, 0.082913, 0.072636,
    -0.014896, 0.101411, 0.082418, -0.047560, 0.091531, 0.067428, -0.037025, 0.101504, 0.080940, -0.005986, 0.022816, 0.015329,
    -0.005374, 0.025027, 0.014129, -0.006433, 0.030446, 0.016911, 0.000296, 0.105285, 0.080771, -0.001297, 0.106280, 0.082015,
    -0.011590, 0.112481, 0.077210, -0.005786, 0.108829, 0.074971, -0.031615, 0.010195, -0.062213, -0.019530, 0.119867, 0.068959,
    -0.003946, 0.010561, 0.009347, -0.002493, 0.007993, 0.007449, -0.031570, 0.133046, 0.055278, -0.018398, 0.116656, 0.067308,
    -0.025736, 0.127279, 0.064399, -0.011833, 0.112937, 0.069248, -0.008562, 0.065129, 0.056676, -0.010669, 0.050279, 0.039766,
    -0.008020, 0.079830, 0.060627, -0.057475, 0.081333, 0.054023, -0.006900, 0.011170, 0.008557, -0.009171, 0.023166, 0.017215,
    -0.008994, 0.036414, 0.019543, -0.034687, 0.034200, -0.001624, -0.036130, 0.041295, 0.006917, -0.051110, 0.060847, 0.031617,
    -0.034677, 0.021524, -0.019434, -0.037084, 0.015224, -0.025120, -0.032253, 0.016854, -0.027069, -0.018368, 0.015599, -0.017112,
    -0.023107, 0.018722, -0.023214, -0.006862, 0.012016, 0.008783, -0.026939, 0.015961, -0.027368, -0.028534, 0.079664, 0.061340,
    -0.002579, 0.007282, 0.007631, -0.018387, 0.019879, -0.002082, -0.028867, 0.119958, 0.090821, -0.026559, 0.111931, 0.088240,
    -0.038922, 0.128254, 0.091190, -0.015479, 0.096228, 0.076343, -0.017569, 0.103762, 0.081820, -0.023356, 0.089532, 0.071984,
    -0.041853, 0.133234, 0.086267, -0.032247, 0.144434, 0.059664, -0.033021, 0.119983, 0.087078, -0.030380, 0.138787, 0.051780,
    -0.024049, 0.140493, 0.050069, -0.009397, 0.076123, 0.053352, -0.009902, 0.006971, -0.027127, -0.023584, 0.135224, 0.043291,
    -0.015103, 0.012908, -0.026965, -0.015668, 0.014463, 0.002741, -0.023548, 0.026139, 0.002661, -0.018069, 0.014193, -0.009258,
    -0.020846, 0.026321, 0.004471, -0.024724, 0.023047, -0.008288, -0.013225, 0.010529, 0.000002, -0.014076, 0.010341, -0.006235,
    -0.012893, 0.013694, 0.005047, -0.057690, 0.030822, -0.120620, -0.068905, 0.040856, -0.118234, -0.042032, 0.030893, -0.057812,
    -0.044295, 0.032728, -0.061936, -0.041392, 0.027609, -0.053646, -0.044793, 0.037622, -0.054324, -0.041845, 0.033926, -0.050043,
    -0.047531, 0.034350, -0.066649, -0.049407, 0.040999, -0.061072, -0.050538, 0.036118, -0.071034, -0.053305, 0.043273, -0.067211,
    -0.051119, 0.038091, -0.074018, -0.058385, 0.043993, -0.071618, -0.048991, 0.036396, -0.079244, -0.056202, 0.039887, -0.080297,
    -0.041758, 0.023751, -0.053250, -0.041241, 0.029455, -0.047230, -0.042618, 0.022139, -0.075107, -0.041625, 0.020099, -0.069582,
    -0.043693, 0.024179, -0.079076, -0.035211, 0.011662, -0.074416, -0.036513, 0.016001, -0.081199, -0.041198, 0.019076, -0.063352,
    -0.034666, 0.009223, -0.064573, -0.045342, 0.026802, -0.081060, -0.039206, 0.019924, -0.086506, -0.043588, 0.024833, -0.090266,
    -0.046872, 0.030705, -0.081912, -0.067047, 0.050461, -0.071131, -0.061132, 0.050694, -0.064018, -0.027715, 0.128281, 0.037370,
    -0.027625, 0.133033, 0.043191, -0.029304, 0.129121, 0.030769, -0.036449, 0.122258, 0.036299, -0.085407, 0.062182, -0.090160,
    -0.078114, 0.052378, -0.106514, -0.073694, 0.051122, -0.085604, -0.065250, 0.041184, -0.099117, -0.088831, 0.068254, -0.073090,
    -0.065300, 0.045119, -0.081966, -0.050436, 0.031993, -0.087761, -0.057555, 0.035493, -0.095306, -0.041809, 0.048252, -0.028845,
    -0.035432, 0.039894, -0.038026, -0.042902, 0.047901, -0.042180, -0.034460, 0.040822, -0.027407, -0.038741, 0.052436, -0.012273,
    -0.058181, 0.059698, -0.026669, -0.048083, 0.062971, -0.005857, -0.037152, 0.070577, 0.044415, -0.047455, 0.089484, 0.043901,
    -0.031315, 0.066752, 0.040709, -0.040106, 0.087634, 0.040608, -0.056604, 0.109743, 0.042786, -0.049924, 0.109551, 0.041637,
    -0.044959, 0.124775, 0.040932, -0.056159, 0.110760, 0.034394, -0.035622, 0.129265, 0.037834, -0.046319, 0.114690, 0.036180,
    -0.035071, 0.123487, 0.034093, -0.001111, -0.000160, -0.009765, -0.008293, 0.005210, -0.013652, -0.008607, 0.005918, -0.007278,
    -0.011049, 0.008664, -0.020307, -0.032108, 0.033097, -0.037311, -0.029962, 0.026850, -0.038237, -0.040081, 0.030250, -0.042386,
    -0.029233, 0.031758, -0.028922, -0.037331, 0.024056, -0.042831, -0.025166, 0.023819, -0.031034, -0.024236, 0.028884, -0.016869,
    -0.018055, 0.017775, -0.018646, -0.025105, 0.025246, -0.009026, -0.029003, 0.031423, -0.006988, -0.030210, 0.035072, -0.009441,
    -0.028955, 0.036448, -0.013969, -0.032299, 0.043952, -0.012928, -0.025553, 0.030113, -0.021668, -0.061811, 0.060802, -0.042737,
    -0.053463, 0.048723, -0.055890, -0.080502, 0.066993, -0.054123, -0.083121, 0.072850, -0.026078, -0.047972, 0.022432, -0.117035,
    -0.036560, 0.014747, -0.054740, -0.041603, 0.018507, -0.058666, -0.054077, 0.088349, 0.026774, -0.058659, 0.101968, 0.024889,
    -0.099921, 0.083568, -0.037126, -0.100994, 0.092928, -0.018688, -0.027593, 0.027276, -0.006527, -0.028938, 0.029377, -0.002835,
    -0.031420, 0.035432, -0.002805, -0.064950, 0.127729, 0.044707, -0.062740, 0.129510, 0.043831, -0.049795, 0.132510, 0.045487,
    -0.055579, 0.127648, 0.044475, -0.036855, 0.014390, -0.110554, -0.038600, 0.136038, 0.044399, -0.016982, 0.013760, -0.011735,
    -0.028573, 0.135977, 0.032843, -0.035668, 0.128020, 0.048746, -0.029397, 0.138870, 0.045096, -0.046072, 0.128052, 0.045182,
    -0.043990, 0.069350, 0.019486, -0.032991, 0.054901, 0.011064, -0.059841, 0.079909, 0.006682, -0.094378, 0.074931, -0.055515,
    -0.018311, 0.026771, 0.006771, -0.033765, 0.041650, -0.002043, -0.042088, 0.036124, -0.043499, -0.046558, 0.042447, -0.047766,
    -0.074178, 0.058951, -0.069812, -0.040175, 0.025139, -0.046076, -0.042825, 0.020156, -0.053151, -0.038443, 0.020043, -0.047602,
    -0.020381, 0.016553, -0.033799, -0.025011, 0.019829, -0.041090, -0.016811, 0.014984, -0.014385, -0.033904, 0.017882, -0.045047,
    -0.090739, 0.074931, -0.035214, -0.009238, 0.008638, 0.001361, -0.021285, 0.021845, -0.024476, -0.075235, 0.110298, 0.015862,
    -0.093768, 0.102724, -0.000453, -0.052713, 0.119294, 0.030693, -0.090025, 0.090314, -0.005530, -0.071285, 0.098548, 0.013853,
    -0.096005, 0.083472, -0.019699, -0.039129, 0.125978, 0.038151, -0.049170, 0.114543, 0.031492, -0.072741, 0.073333, -0.012137,
    -0.043259, 0.031924, 0.002214, -0.043259, 0.031924, 0.002214, -0.043259, 0.031924, 0.002214, -0.043259, 0.031924, 0.002214,
    -0.043259, 0.031924, 0.002214, -0.043259, 0.031924, 0.002214, -0.043259, 0.031924, 0.002214, -0.043259, 0.031924, 0.002214,
    -0.043259, 0.031924, 0.002214, -0.051905, 0.032213, 0.011997, -0.051905, 0.032213, 0.011997, -0.051905, 0.032213, 0.011997,
    -0.051905, 0.032213, 0.011997, -0.051905, 0.032213, 0.011997, -0.051905, 0.032213, 0.011997, -0.051905, 0.032213, 0.011997,
    -0.051905, 0.032213, 0.011997, -0.051905, 0.032213, 0.011997
  ]),
  // 2 — vis_ss
  new Float32Array([
    -0.018260, 0.007650, 0.010074, -0.023448, 0.003989, 0.016358, -0.016267, 0.008763, 0.003231, 0.000460, -0.003006, -0.002233,
    -0.004424, -0.004493, -0.004838, -0.017609, 0.002003, 0.015708, -0.015768, 0.003644, -0.014311, -0.010096, -0.005078, -0.006972,
    -0.020774, -0.006903, -0.012784, -0.025848, -0.008918, -0.015772, -0.015444, -0.005676, -0.010224, -0.053504, 0.000813, 0.023467,
    -0.056059, 0.000338, 0.024638, -0.050662, 0.001179, 0.022083, -0.035346, -0.018017, -0.034248, -0.034050, -0.019426, -0.019412,
    -0.044093, -0.015741, -0.048342, -0.024273, 0.017939, 0.011345, -0.029695, 0.016171, 0.017169, -0.023681, 0.020455, 0.004921,
    -0.024830, 0.021454, 0.022766, -0.030335, 0.019679, 0.027423, -0.023572, 0.023730, 0.015460, -0.028403, 0.033099, 0.023878,
    -0.033076, 0.031017, 0.028446, -0.025571, 0.035283, 0.016349, -0.075956, -0.000664, 0.033839, -0.063612, -0.008071, 0.023420,
    -0.082869, 0.006355, 0.042423, -0.053507, -0.000434, 0.023174, -0.033802, 0.005202, 0.022764, -0.041501, 0.008831, 0.026767,
    -0.046267, 0.012403, 0.030385, -0.052665, -0.002245, 0.025068, -0.045819, -0.005525, 0.017695, -0.039851, -0.009373, 0.009394,
    -0.034577, -0.014877, 0.000699, -0.050305, -0.014522, 0.010849, -0.030009, -0.019158, -0.008772, -0.057269, 0.040077, 0.063736,
    -0.064679, 0.033669, 0.061519, -0.051064, 0.045893, 0.062618, -0.047668, 0.015590, 0.033845, -0.047147, 0.020041, 0.030973,
    -0.048886, -0.005495, 0.018700, -0.056509, -0.002036, 0.027766, -0.040932, -0.010329, 0.009205, -0.034354, -0.016449, -0.000672,
    -0.029743, -0.022852, -0.011200, -0.039327, -0.018538, -0.004129, -0.047561, 0.015952, 0.029526, -0.044941, 0.017420, 0.028356,
    -0.044947, 0.016082, 0.026225, -0.040400, 0.015627, 0.023501, -0.035038, 0.015137, 0.020979, -0.037886, 0.028219, 0.029437,
    -0.035833, 0.018236, 0.028816, -0.041379, 0.017745, 0.028636, -0.043489, 0.024274, 0.029867, -0.074376, 0.027299, 0.058611,
    -0.082458, 0.020912, 0.055123, -0.085402, 0.014454, 0.050012, -0.039059, -0.006412, 0.004050, -0.040067, -0.005401, 0.005402,
    -0.039579, -0.005987, 0.005299, -0.047143, 0.050487, 0.059219, -0.044724, 0.054648, 0.055757, -0.047611, 0.001196, 0.019394,
    -0.044457, 0.000365, 0.015801, -0.042710, -0.001263, 0.011538, -0.038304, 0.053963, 0.042761, -0.038374, 0.051668, 0.032998,
    -0.039958, 0.055799, 0.049420, -0.042677, 0.056248, 0.052836, -0.039407, 0.049929, 0.021569, -0.041210, -0.003558, 0.008018,
    -0.041237, -0.004287, 0.007450, -0.043487, -0.002154, 0.011161, -0.044783, -0.000748, 0.015200, -0.047954, -0.000500, 0.019140,
    -0.051214, -0.000563, 0.021602, -0.053996, 0.000223, -0.049665, -0.055597, 0.000338, -0.051991, -0.052548, 0.000328, -0.047705,
    -0.090208, 0.003526, -0.078315, -0.080538, -0.001774, -0.077417, -0.097465, 0.008664, -0.074977, -0.055465, 0.000028, -0.050427,
    -0.015978, 0.013725, -0.004120, -0.017931, 0.019540, -0.009669, -0.021661, 0.024803, -0.014236, -0.061419, -0.001751, -0.063336,
    -0.053066, -0.004629, -0.059080, -0.045267, -0.008778, -0.052002, -0.037370, -0.014962, -0.041683, -0.068889, -0.007292, -0.071527,
    -0.030730, -0.019858, -0.029054, -0.098420, 0.031139, -0.040915, -0.103528, 0.027081, -0.049645, -0.088920, 0.033934, -0.031898,
    -0.026989, 0.029539, -0.014860, -0.024484, 0.032853, -0.009550, -0.057079, -0.004299, -0.063109, -0.066853, -0.001025, -0.067213,
    -0.046615, -0.009657, -0.055884, -0.036189, -0.016495, -0.044221, -0.029064, -0.022851, -0.033005, -0.054443, -0.012405, -0.061744,
    -0.027081, 0.028897, -0.012088, -0.027143, 0.028227, -0.007899, -0.027689, 0.027160, -0.010696, -0.026495, 0.025158, -0.005818,
    -0.024789, 0.022762, -0.000897, -0.023856, 0.036221, 0.005667, -0.023647, 0.025489, 0.007493, -0.025048, 0.027135, 0.000038,
    -0.023010, 0.035107, -0.001630, -0.103529, 0.022969, -0.056921, -0.102319, 0.018691, -0.064201, -0.101661, 0.014284, -0.069619,
    -0.052285, -0.002376, -0.030648, -0.051993, -0.001109, -0.031976, -0.053654, -0.002363, -0.033028, -0.077111, 0.036520, -0.021324,
    -0.063990, 0.041122, -0.010436, -0.051390, 0.000835, -0.044901, -0.050873, 0.001650, -0.041172, -0.051183, 0.001803, -0.037123,
    -0.044616, 0.047717, 0.010020, -0.053827, 0.044847, -0.000319, -0.051829, 0.000632, -0.033884, -0.055923, -0.001259, -0.035555,
    -0.056677, 0.000143, -0.038886, -0.056436, 0.000491, -0.043079, -0.056003, 0.000404, -0.047039, -0.055484, 0.000095, -0.049061,
    0.000000, 0.000000, -0.000000, -0.001668, 0.000276, 0.002998, -0.000901, 0.000607, -0.003461, -0.010436, 0.005032, 0.004416,
    -0.007195, 0.003612, 0.004612, -0.014336, 0.006770, 0.006013, -0.017897, 0.006137, 0.012747, -0.006741, 0.004183, 0.001275,
    -0.015687, 0.008135, -0.000584, -0.011482, -0.004336, -0.001756, -0.005559, -0.003992, 0.000387, -0.016759, -0.005071, -0.005363,
    -0.015042, -0.002366, 0.003031, -0.006219, -0.003783, -0.009241, -0.022358, -0.006118, -0.006863, -0.024353, -0.005626, -0.017525,
    -0.018288, -0.004762, -0.014538, -0.026495, -0.015035, -0.018573, -0.027169, -0.018748, -0.022232, -0.028429, -0.010620, -0.008618,
    -0.033600, -0.010236, -0.022066, -0.030243, -0.018645, -0.016659, -0.030965, -0.017226, -0.029469, -0.036879, -0.016229, -0.042425,
    -0.019744, 0.011400, 0.011020, -0.022042, 0.015643, 0.011751, -0.025717, 0.009035, 0.017017, -0.017909, 0.013813, 0.003939,
    -0.027958, 0.013815, 0.017631, -0.020836, 0.018190, 0.005297, -0.025268, 0.024406, 0.022819, -0.026497, 0.028392, 0.023381,
    -0.031453, 0.026056, 0.028290, -0.030261, 0.022179, 0.027607, -0.024062, 0.030218, 0.015585, -0.023383, 0.026250, 0.015142,
    -0.031586, 0.036598, 0.023219, -0.034070, 0.036434, 0.027844, -0.031546, 0.041587, 0.027242, -0.034261, 0.042348, 0.033351,
    -0.029677, 0.036929, 0.016574, -0.029269, 0.040970, 0.018767, -0.003324, 0.002875, 0.001068, -0.004618, 0.002769, 0.003757,
    -0.004012, 0.003233, -0.001547, -0.009673, 0.002971, 0.006224, -0.015852, 0.003965, 0.008617, -0.010194, 0.001982, 0.010590,
    -0.015259, 0.004312, 0.009012, -0.015420, 0.002765, 0.011574, -0.007748, 0.002709, 0.005075, -0.008309, 0.003508, 0.005367,
    -0.053411, -0.007671, 0.019615, -0.063834, -0.001770, 0.028882, -0.041846, -0.000236, 0.011500, -0.043821, 0.001473, 0.016181,
    -0.039652, -0.003036, 0.006336, -0.041435, 0.001981, 0.017803, -0.039443, 0.000621, 0.011840, -0.046132, 0.002063, 0.020530,
    -0.043899, 0.002677, 0.023591, -0.050752, 0.002068, 0.024197, -0.049078, 0.002847, 0.028427, -0.054888, 0.001111, 0.026144,
    -0.051733, 0.002744, 0.030309, -0.058133, -0.000266, 0.026384, -0.056565, 0.001101, 0.029366, -0.038206, -0.005416, 0.004137,
    -0.037343, -0.001989, 0.005785, -0.045487, -0.007482, 0.014689, -0.043209, -0.008753, 0.007683, -0.048465, -0.006084, 0.019300,
    -0.040386, -0.011823, 0.003665, -0.043124, -0.009835, 0.010667, -0.041548, -0.009049, 0.003971, -0.037502, -0.012875, -0.001826,
    -0.051997, -0.004696, 0.022037, -0.046668, -0.007338, 0.017247, -0.051901, -0.004373, 0.021612, -0.055460, -0.002633, 0.024420,
    -0.050252, 0.004671, 0.036057, -0.045538, 0.004891, 0.034492, -0.037580, 0.047279, 0.042033, -0.037583, 0.052587, 0.046706,
    -0.034765, 0.051489, 0.040686, -0.037498, 0.040922, 0.035560, -0.033851, 0.047787, 0.038192, -0.038967, 0.044219, 0.045001,
    -0.069705, 0.009194, 0.044660, -0.068549, 0.003311, 0.036964, -0.059387, 0.004736, 0.038893, -0.059766, 0.000859, 0.032807,
    -0.063855, 0.014350, 0.051401, -0.056289, 0.002785, 0.033717, -0.055232, -0.001347, 0.026251, -0.055777, -0.000122, 0.029556,
    -0.032166, 0.007218, 0.021796, -0.034688, 0.002847, 0.014027, -0.036674, 0.005145, 0.022025, -0.030952, 0.003880, 0.015319,
    -0.029492, 0.008689, 0.021106, -0.034332, 0.011560, 0.031029, -0.032501, 0.012439, 0.026982, -0.034461, 0.013105, 0.022267,
    -0.040190, 0.013978, 0.024731, -0.033750, 0.009369, 0.022635, -0.040798, 0.011330, 0.025591, -0.045313, 0.015017, 0.027321,
    -0.045804, 0.013728, 0.029088, -0.041418, 0.025102, 0.035834, -0.040415, 0.019062, 0.039477, -0.039538, 0.030200, 0.033262,
    -0.038735, 0.030531, 0.041830, -0.038931, 0.037126, 0.038270, -0.001226, -0.002442, 0.001710, -0.005909, -0.001151, 0.007214,
    -0.005047, 0.000861, 0.006541, -0.009988, -0.002253, 0.005408, -0.031596, 0.001047, 0.007879, -0.029189, -0.000952, 0.003789,
    -0.033575, -0.000678, 0.006706, -0.028379, 0.001601, 0.010250, -0.031490, -0.003417, 0.001992, -0.024520, -0.000498, 0.005628,
    -0.020904, 0.001449, 0.015156, -0.015566, -0.000339, 0.010510, -0.016322, 0.002670, 0.014112, -0.018388, 0.003525, 0.014495,
    -0.020434, 0.003461, 0.016579, -0.023237, 0.003368, 0.016632, -0.026744, 0.004840, 0.016390, -0.025519, 0.001591, 0.011575,
    -0.039616, 0.009686, 0.035228, -0.040923, 0.004220, 0.028514, -0.047061, 0.013222, 0.048034, -0.039294, 0.017829, 0.044641,
    -0.043200, -0.012666, 0.008842, -0.035562, -0.009847, -0.003458, -0.038481, -0.008278, 0.001601, -0.037181, 0.014058, 0.033343,
    -0.037418, 0.024194, 0.044428, -0.050113, 0.026087, 0.057700, -0.045865, 0.032111, 0.060247, -0.017370, 0.003366, 0.013179,
    -0.017808, 0.003954, 0.011288, -0.019529, 0.004546, 0.013295, -0.047702, 0.015745, 0.030056, -0.047689, 0.015591, 0.031413,
    -0.046487, 0.018569, 0.029414, -0.045453, 0.017750, 0.028572, -0.034228, -0.016977, -0.004208, -0.042695, 0.021097, 0.029249,
    -0.010105, 0.001384, 0.010838, -0.008050, 0.001974, 0.007317, -0.037046, 0.033890, 0.030738, -0.036182, 0.020297, 0.028990,
    -0.037201, 0.023550, 0.029387, -0.041565, 0.018994, 0.028553, -0.031500, 0.011072, 0.026219, -0.025335, 0.008151, 0.020781,
    -0.036765, 0.015961, 0.035690, -0.056241, 0.019882, 0.054928, -0.006566, 0.003907, 0.003217, -0.015185, 0.004644, 0.010011,
    -0.022270, 0.005297, 0.015055, -0.036190, 0.001496, 0.013015, -0.038338, 0.003072, 0.020012, -0.053329, 0.008758, 0.044091,
    -0.035629, -0.004118, 0.002417, -0.037529, -0.007129, 0.001533, -0.034387, -0.006440, -0.001629, -0.020668, -0.002929, 0.000267,
    -0.025648, -0.003620, -0.001572, -0.011696, 0.000416, 0.011512, -0.030425, -0.006336, -0.003068, -0.043751, 0.019228, 0.051502,
    -0.005957, 0.002701, 0.004433, -0.020798, -0.000039, 0.007333, -0.041342, 0.042210, 0.055380, -0.043220, 0.037881, 0.058587,
    -0.041244, 0.047436, 0.052477, -0.040760, 0.028394, 0.052308, -0.038108, 0.029933, 0.048875, -0.042878, 0.025301, 0.053747,
    -0.039961, 0.051123, 0.049849, -0.034040, 0.049757, 0.031010, -0.039537, 0.038661, 0.047960, -0.031681, 0.046533, 0.029205,
    -0.033549, 0.047941, 0.019976, -0.036746, 0.017830, 0.039854, -0.012452, -0.004263, -0.011991, -0.029825, 0.044999, 0.018737,
    -0.016580, -0.002010, -0.014614, -0.008227, 0.004582, -0.001848, -0.013234, 0.005955, -0.002645, -0.009905, 0.004022, -0.009934,
    -0.012403, 0.006134, -0.001851, -0.013851, 0.004776, -0.009649, -0.006602, 0.003977, -0.003043, -0.007130, 0.003641, -0.007057,
    -0.007444, 0.004477, -0.000220, -0.068474, -0.003194, -0.070018, -0.078233, 0.001270, -0.072531, -0.050249, 0.002284, -0.035998,
    -0.050905, 0.002248, -0.040695, -0.050167, 0.000706, -0.031166, -0.049829, 0.003002, -0.038726, -0.047924, 0.002585, -0.032935,
    -0.052957, 0.001512, -0.045259, -0.053849, 0.002713, -0.045079, -0.054924, 0.001179, -0.049181, -0.057614, 0.002778, -0.050327,
    -0.056242, 0.001154, -0.051480, -0.062374, 0.002641, -0.053165, -0.057077, 0.000424, -0.053930, -0.063521, 0.001493, -0.056338,
    -0.049867, -0.001430, -0.029308, -0.047241, 0.000704, -0.028187, -0.050037, -0.004909, -0.045226, -0.048983, -0.006120, -0.039709,
    -0.052096, -0.003690, -0.049777, -0.043376, -0.010733, -0.041261, -0.046097, -0.007704, -0.048126, -0.048798, -0.006517, -0.034654,
    -0.042342, -0.012498, -0.033040, -0.054360, -0.002665, -0.052362, -0.050002, -0.005151, -0.053988, -0.054725, -0.002920, -0.058097,
    -0.056318, -0.001501, -0.054078, -0.068856, 0.004617, -0.056250, -0.061710, 0.004737, -0.052602, -0.034631, 0.041885, 0.006709,
    -0.039045, 0.045109, 0.008808, -0.032305, 0.037994, 0.005098, -0.042370, 0.037315, -0.002363, -0.091356, 0.009737, -0.067578,
    -0.085302, 0.005427, -0.070910, -0.079250, 0.005057, -0.063512, -0.074226, 0.002181, -0.066095, -0.091962, 0.013233, -0.063851,
    -0.071554, 0.002954, -0.059488, -0.060305, -0.000729, -0.058541, -0.067118, 0.000383, -0.063457, -0.035383, 0.008239, -0.026600,
    -0.036432, 0.004470, -0.028096, -0.040905, 0.006390, -0.034719, -0.031069, 0.005489, -0.022335, -0.029236, 0.009654, -0.018720,
    -0.042194, 0.011422, -0.031322, -0.034234, 0.012641, -0.023080, -0.021957, 0.021052, -0.001155, -0.023617, 0.023723, -0.006209,
    -0.018675, 0.017754, -0.002601, -0.020430, 0.021730, -0.007843, -0.025243, 0.026652, -0.011098, -0.023140, 0.025894, -0.012901,
    -0.033225, 0.028934, -0.010359, -0.035427, 0.022724, -0.016939, -0.031179, 0.031804, -0.003910, -0.042325, 0.028531, -0.011617,
    -0.037316, 0.034390, -0.003423, -0.001649, -0.002192, -0.006067, -0.006166, -0.000407, -0.010428, -0.004739, 0.001943, -0.007349,
    -0.011187, -0.001798, -0.012393, -0.034466, 0.002486, -0.023968, -0.033011, 0.000167, -0.021825, -0.042987, 0.000836, -0.026253,
    -0.028437, 0.002872, -0.020389, -0.040430, -0.001866, -0.023516, -0.026079, 0.000394, -0.018830, -0.018399, 0.003121, -0.015833,
    -0.015050, 0.000952, -0.014219, -0.014590, 0.004389, -0.011347, -0.016574, 0.005520, -0.010419, -0.018351, 0.005530, -0.012818,
    -0.020945, 0.005080, -0.015238, -0.024123, 0.006650, -0.015074, -0.023095, 0.003060, -0.016743, -0.050694, 0.009816, -0.043807,
    -0.053515, 0.004649, -0.045956, -0.073293, 0.011389, -0.055710, -0.060252, 0.015525, -0.041006, -0.058333, -0.007745, -0.063360,
    -0.043360, -0.008191, -0.026401, -0.049003, -0.005553, -0.030953, -0.031136, 0.017212, -0.018811, -0.044774, 0.022970, -0.020160,
    -0.093081, 0.020607, -0.050432, -0.089775, 0.024546, -0.043106, -0.015178, 0.005277, -0.009255, -0.015439, 0.005785, -0.006711,
    -0.017699, 0.006487, -0.007605, -0.027157, 0.029094, -0.012750, -0.026954, 0.029240, -0.013563, -0.024391, 0.031094, -0.008253,
    -0.025313, 0.029558, -0.007746, -0.045230, -0.012963, -0.053809, -0.023034, 0.031722, -0.000146, -0.009728, 0.003101, -0.011281,
    -0.029187, 0.034997, 0.003830, -0.023031, 0.027918, 0.007082, -0.022780, 0.031467, 0.006418, -0.024032, 0.028893, -0.000031,
    -0.027215, 0.013847, -0.015441, -0.021959, 0.010921, -0.011016, -0.038845, 0.015895, -0.024122, -0.092579, 0.016428, -0.057629,
    -0.011947, 0.006201, -0.001089, -0.020358, 0.007366, -0.008170, -0.044534, 0.002896, -0.030534, -0.047679, 0.004003, -0.036914,
    -0.075909, 0.008577, -0.060040, -0.045713, -0.001475, -0.025398, -0.049841, -0.003388, -0.027512, -0.043863, -0.004204, -0.023435,
    -0.022755, -0.002362, -0.016946, -0.028963, -0.002804, -0.019683, -0.011596, 0.001864, -0.012749, -0.038102, -0.005104, -0.021304,
    -0.075221, 0.015990, -0.048967, -0.005043, 0.003450, -0.002126, -0.020933, 0.000926, -0.016027, -0.068029, 0.030868, -0.023630,
    -0.079083, 0.027813, -0.033811, -0.057489, 0.036269, -0.011709, -0.067897, 0.022382, -0.034237, -0.056962, 0.024119, -0.024888,
    -0.077005, 0.019941, -0.042205, -0.048118, 0.040931, -0.001081, -0.050279, 0.031657, -0.012891, -0.049269, 0.015666, -0.031405,
    -0.047661, 0.003399, 0.003144, -0.047661, 0.003399, 0.003144, -0.047661, 0.003399, 0.003144, -0.047661, 0.003399, 0.003144,
    -0.047661, 0.003399, 0.003144, -0.047661, 0.003399, 0.003144, -0.047661, 0.003399, 0.003144, -0.047661, 0.003399, 0.003144,
    -0.047661, 0.003399, 0.003144, -0.059022, 0.003459, 0.003648, -0.059022, 0.003459, 0.003648, -0.059022, 0.003459, 0.003648,
    -0.059022, 0.003459, 0.003648, -0.059022, 0.003459, 0.003648, -0.059022, 0.003459, 0.003648, -0.059022, 0.003459, 0.003648,
    -0.059022, 0.003459, 0.003648, -0.059022, 0.003459, 0.003648
  ]),
  // 3 — vis_ch
  new Float32Array([
    -0.016473, 0.017406, 0.004753, -0.016785, 0.013174, 0.008185, -0.016887, 0.018132, 0.000236, 0.000392, -0.002238, 0.000203,
    -0.002660, -0.005267, 0.000133, -0.011845, -0.001217, 0.008986, -0.010821, 0.000099, -0.010367, -0.006204, -0.008062, 0.000140,
    -0.013029, -0.015188, -0.000617, -0.016351, -0.020275, -0.000918, -0.009744, -0.011012, -0.000284, -0.034419, -0.021382, 0.022226,
    -0.037030, -0.021891, 0.023680, -0.032020, -0.021104, 0.021097, -0.022353, -0.040710, -0.001931, -0.021742, -0.041522, 0.007639,
    -0.029309, -0.040169, -0.011965, -0.020985, 0.021269, 0.000814, -0.021312, 0.019419, 0.005052, -0.021851, 0.023939, -0.002316,
    -0.021547, 0.026857, 0.009865, -0.022145, 0.025209, 0.012738, -0.021756, 0.030228, 0.006163, -0.024181, 0.041130, 0.003005,
    -0.025361, 0.039374, 0.006571, -0.021427, 0.044502, -0.000068, -0.051893, -0.030243, 0.034863, -0.042067, -0.034916, 0.031649,
    -0.057878, -0.024440, 0.036315, -0.035248, -0.022536, 0.023014, -0.020330, 0.014681, 0.011849, -0.023484, 0.019227, 0.015225,
    -0.025569, 0.024487, 0.018236, -0.035912, -0.018542, 0.028659, -0.031222, -0.020115, 0.026262, -0.026799, -0.022560, 0.020920,
    -0.022695, -0.027154, 0.014561, -0.032447, -0.038730, 0.026295, -0.019083, -0.030912, 0.006766, -0.035680, 0.013961, 0.027338,
    -0.042193, 0.004110, 0.030153, -0.031561, 0.024634, 0.024479, -0.025647, 0.029549, 0.019400, -0.028859, 0.032565, 0.016250,
    -0.031968, -0.020053, 0.027570, -0.037865, -0.019346, 0.030758, -0.026705, -0.023511, 0.021584, -0.022475, -0.028615, 0.014862,
    -0.019110, -0.034179, 0.007722, -0.025584, -0.040926, 0.017458, -0.025587, 0.028747, 0.016823, -0.024740, 0.027737, 0.015483,
    -0.023836, 0.026163, 0.013997, -0.022478, 0.022955, 0.011353, -0.021746, 0.019922, 0.008482, -0.026733, 0.038279, 0.009487,
    -0.022871, 0.024852, 0.014114, -0.024067, 0.026158, 0.014756, -0.028601, 0.035723, 0.012216, -0.051395, -0.003657, 0.032614,
    -0.058618, -0.010579, 0.034984, -0.060875, -0.017004, 0.036231, -0.021861, -0.028099, 0.009535, -0.022594, -0.027516, 0.010461,
    -0.022389, -0.027919, 0.011002, -0.030154, 0.034778, 0.020604, -0.031447, 0.044190, 0.017022, -0.029537, -0.021265, 0.019078,
    -0.027127, -0.022518, 0.016686, -0.025678, -0.024245, 0.014030, -0.031259, 0.054189, 0.001506, -0.030481, 0.053422, -0.005297,
    -0.031830, 0.053045, 0.008138, -0.032237, 0.049546, 0.012910, -0.026087, 0.051832, -0.012226, -0.023925, -0.026243, 0.012030,
    -0.024083, -0.026754, 0.012810, -0.027080, -0.025093, 0.016103, -0.029295, -0.023355, 0.018825, -0.031718, -0.022727, 0.021185,
    -0.033826, -0.022636, 0.022189, -0.035585, -0.020920, -0.026666, -0.036250, -0.021891, -0.027748, -0.034861, -0.020105, -0.025538,
    -0.068031, -0.032534, -0.040373, -0.059282, -0.035297, -0.036275, -0.074339, -0.027798, -0.042076, -0.036228, -0.022178, -0.026389,
    -0.018507, 0.024584, -0.004309, -0.020410, 0.032046, -0.006342, -0.022797, 0.039515, -0.008039, -0.038837, -0.023754, -0.031583,
    -0.031745, -0.024579, -0.026637, -0.026724, -0.025827, -0.021327, -0.022267, -0.028604, -0.014577, -0.049314, -0.037374, -0.029548,
    -0.018555, -0.030800, -0.007279, -0.072226, 0.004624, -0.041408, -0.079083, -0.004730, -0.043286, -0.060835, 0.015511, -0.037834,
    -0.026508, 0.046506, -0.009152, -0.022035, 0.047604, -0.006800, -0.035966, -0.025488, -0.028629, -0.044374, -0.024877, -0.034120,
    -0.028265, -0.027849, -0.023041, -0.020995, -0.030706, -0.014917, -0.016711, -0.034078, -0.007447, -0.037147, -0.038915, -0.021611,
    -0.027187, 0.044225, -0.006852, -0.026838, 0.040392, -0.004847, -0.027994, 0.039005, -0.007308, -0.026620, 0.033734, -0.005755,
    -0.023915, 0.028324, -0.004455, -0.019991, 0.047423, -0.003335, -0.022645, 0.033507, 0.002346, -0.024516, 0.037288, -0.001135,
    -0.019458, 0.048347, -0.005084, -0.080064, -0.011084, -0.043513, -0.079137, -0.016556, -0.043888, -0.078390, -0.021532, -0.043211,
    -0.037278, -0.022325, -0.013436, -0.037130, -0.021713, -0.014414, -0.038116, -0.022374, -0.014520, -0.049066, 0.025658, -0.032237,
    -0.037665, 0.035341, -0.026460, -0.034607, -0.019387, -0.023897, -0.034789, -0.019152, -0.021394, -0.035582, -0.019468, -0.018470,
    -0.026181, 0.047782, -0.017557, -0.030709, 0.041941, -0.021945, -0.036476, -0.020651, -0.015863, -0.039079, -0.021571, -0.016012,
    -0.038368, -0.020828, -0.017475, -0.037249, -0.020842, -0.020457, -0.036565, -0.021304, -0.023358, -0.036111, -0.021969, -0.025251,
    0.000000, 0.000000, -0.000000, -0.000894, -0.000169, 0.002317, -0.000649, 0.000050, -0.001870, -0.008624, 0.002029, -0.000200,
    -0.005906, 0.001064, 0.000979, -0.011919, 0.007702, 0.000672, -0.012217, 0.007128, 0.004751, -0.005272, 0.001589, -0.001226,
    -0.013438, 0.009455, -0.003441, -0.007397, -0.007538, 0.003215, -0.003781, -0.005011, 0.002974, -0.010811, -0.010728, 0.002703,
    -0.009940, -0.006665, 0.005297, -0.003621, -0.005134, -0.002992, -0.014211, -0.015107, 0.002923, -0.015321, -0.014655, -0.004251,
    -0.011338, -0.010746, -0.003365, -0.016549, -0.026094, 0.000148, -0.016716, -0.029765, 0.000492, -0.017494, -0.023901, 0.002765,
    -0.022405, -0.022565, -0.005957, -0.019230, -0.034798, 0.007266, -0.018989, -0.034330, -0.001589, -0.023454, -0.034376, -0.010278,
    -0.017710, 0.019229, 0.004812, -0.019499, 0.020642, 0.003892, -0.018250, 0.016382, 0.008323, -0.018383, 0.021329, 0.000346,
    -0.019990, 0.018505, 0.007574, -0.020550, 0.023184, 0.000146, -0.022031, 0.030736, 0.007636, -0.022870, 0.035878, 0.006006,
    -0.024049, 0.033860, 0.009184, -0.022753, 0.028649, 0.010783, -0.020886, 0.038900, 0.002149, -0.020966, 0.033636, 0.003921,
    -0.026262, 0.041903, -0.003560, -0.024942, 0.041468, -0.000143, -0.025690, 0.046331, -0.003170, -0.025280, 0.045763, 0.001276,
    -0.024318, 0.042693, -0.006616, -0.021846, 0.045369, -0.007894, -0.002606, 0.001374, 0.000078, -0.003469, 0.001127, 0.001908,
    -0.002842, 0.001500, -0.001550, -0.007698, -0.000100, 0.002418, -0.011372, 0.000126, 0.003345, -0.006889, -0.001271, 0.006306,
    -0.011311, 0.000515, 0.003270, -0.010345, -0.000738, 0.006181, -0.006042, 0.000015, 0.002637, -0.006751, 0.000613, 0.001653,
    -0.034814, -0.028245, 0.028923, -0.043046, -0.025367, 0.031572, -0.025158, -0.021636, 0.013103, -0.026786, -0.019710, 0.016481,
    -0.023039, -0.024052, 0.010453, -0.025619, -0.015260, 0.017189, -0.024058, -0.017648, 0.012624, -0.028930, -0.017987, 0.019697,
    -0.027892, -0.013620, 0.021096, -0.032594, -0.018325, 0.022356, -0.032146, -0.014597, 0.024220, -0.035867, -0.019975, 0.023884,
    -0.034192, -0.015925, 0.026337, -0.039536, -0.021963, 0.025074, -0.038719, -0.019458, 0.027258, -0.021694, -0.026134, 0.009157,
    -0.022609, -0.020278, 0.009795, -0.031645, -0.027761, 0.019262, -0.028276, -0.029339, 0.014765, -0.034095, -0.026138, 0.022385,
    -0.026754, -0.028806, 0.014159, -0.030013, -0.026787, 0.019075, -0.025608, -0.029774, 0.011415, -0.023235, -0.030311, 0.008931,
    -0.036368, -0.024861, 0.023631, -0.032921, -0.024739, 0.023270, -0.036433, -0.022665, 0.025413, -0.038083, -0.023451, 0.024480,
    -0.032330, -0.012242, 0.028894, -0.028820, -0.009161, 0.026793, -0.027050, 0.047271, 0.008030, -0.028830, 0.051513, 0.008384,
    -0.027818, 0.053305, 0.002426, -0.025462, 0.043304, 0.005778, -0.025694, 0.050038, 0.003104, -0.026969, 0.043579, 0.013134,
    -0.047524, -0.016418, 0.034898, -0.046817, -0.021970, 0.033603, -0.039726, -0.015495, 0.032363, -0.040326, -0.018783, 0.031010,
    -0.042465, -0.010238, 0.035568, -0.038114, -0.016972, 0.029541, -0.038089, -0.021201, 0.026633, -0.038105, -0.018982, 0.029161,
    -0.018344, 0.000123, 0.012568, -0.021782, -0.008262, 0.011888, -0.020988, -0.004066, 0.016157, -0.019693, -0.004164, 0.009841,
    -0.017934, 0.004161, 0.010592, -0.014814, 0.006075, 0.016845, -0.015526, 0.010018, 0.014353, -0.020935, 0.019379, 0.011239,
    -0.022527, 0.022183, 0.013314, -0.020199, 0.017419, 0.011943, -0.023013, 0.020773, 0.014351, -0.024380, 0.025891, 0.015755,
    -0.024953, 0.025302, 0.017279, -0.023964, 0.032610, 0.017248, -0.019376, 0.026783, 0.021478, -0.023991, 0.036933, 0.011981,
    -0.022664, 0.033172, 0.018828, -0.025503, 0.039593, 0.011782, -0.000904, -0.002077, 0.002723, -0.004299, -0.002552, 0.006104,
    -0.003459, -0.000969, 0.004446, -0.007022, -0.004476, 0.005454, -0.020598, -0.010244, 0.009177, -0.019333, -0.011991, 0.007704,
    -0.020936, -0.015498, 0.009771, -0.018822, -0.006896, 0.008237, -0.019689, -0.018006, 0.007364, -0.016416, -0.008425, 0.007055,
    -0.014560, -0.002453, 0.009111, -0.011118, -0.003871, 0.007444, -0.010823, -0.000571, 0.007614, -0.012055, 0.000314, 0.007236,
    -0.013460, 0.000241, 0.008574, -0.015566, -0.000537, 0.009092, -0.017435, -0.000011, 0.008019, -0.017559, -0.004575, 0.007907,
    -0.019186, 0.001557, 0.023174, -0.025653, -0.007789, 0.022387, -0.024812, -0.001981, 0.031996, -0.013863, 0.008462, 0.025320,
    -0.027319, -0.030352, 0.023289, -0.021224, -0.027990, 0.006216, -0.022587, -0.029293, 0.009443, -0.015437, 0.019821, 0.019856,
    -0.018054, 0.026459, 0.022823, -0.027076, 0.004015, 0.031238, -0.022855, 0.012116, 0.029105, -0.011552, -0.000187, 0.006357,
    -0.011747, 0.000296, 0.005186, -0.012299, 0.001114, 0.005829, -0.025743, 0.029030, 0.017120, -0.025696, 0.029252, 0.018073,
    -0.027669, 0.030520, 0.015534, -0.026034, 0.028865, 0.015052, -0.022198, -0.033134, 0.015397, -0.027110, 0.031667, 0.012591,
    -0.006839, -0.001604, 0.006821, -0.005747, -0.000742, 0.004477, -0.024195, 0.039950, 0.006434, -0.024041, 0.027883, 0.012638,
    -0.025659, 0.032521, 0.011041, -0.025242, 0.028418, 0.013418, -0.013747, 0.014052, 0.014961, -0.013267, 0.009433, 0.010091,
    -0.014485, 0.017256, 0.019878, -0.034140, -0.003196, 0.033790, -0.005301, 0.001463, -0.000094, -0.011466, 0.001111, 0.003118,
    -0.014061, 0.001824, 0.006143, -0.022368, -0.013155, 0.012910, -0.023653, -0.009994, 0.017234, -0.033962, -0.009896, 0.032874,
    -0.021361, -0.021875, 0.007648, -0.021187, -0.027774, 0.007795, -0.020472, -0.024105, 0.005692, -0.013550, -0.009679, 0.005195,
    -0.016717, -0.013738, 0.005515, -0.008174, -0.002270, 0.007449, -0.018861, -0.021087, 0.005403, -0.018411, 0.004554, 0.029898,
    -0.004567, 0.000771, 0.002279, -0.014267, -0.005584, 0.006838, -0.022588, 0.032218, 0.022214, -0.020998, 0.022643, 0.025576,
    -0.026629, 0.040860, 0.018442, -0.015435, 0.017687, 0.025581, -0.017167, 0.025709, 0.023756, -0.016599, 0.010722, 0.027862,
    -0.028951, 0.047349, 0.013414, -0.027492, 0.053471, -0.004546, -0.024147, 0.036834, 0.019493, -0.025691, 0.050778, -0.003297,
    -0.022331, 0.051235, -0.011217, -0.013392, 0.012908, 0.020923, -0.007536, -0.007766, -0.003457, -0.020717, 0.048426, -0.009613,
    -0.010176, -0.006786, -0.006318, -0.005593, 0.001392, -0.002887, -0.009993, 0.001817, -0.004093, -0.006852, 0.000389, -0.007094,
    -0.009282, 0.002226, -0.003877, -0.010397, 0.000810, -0.007561, -0.004055, 0.001131, -0.002678, -0.004583, 0.000557, -0.004922,
    -0.005404, 0.001546, -0.002001, -0.048679, -0.030107, -0.032254, -0.057098, -0.028795, -0.036793, -0.035287, -0.017574, -0.018582,
    -0.035309, -0.017056, -0.021496, -0.035520, -0.018834, -0.014448, -0.034677, -0.013526, -0.020587, -0.033485, -0.014767, -0.017171,
    -0.035988, -0.017097, -0.024306, -0.036814, -0.013230, -0.024872, -0.036875, -0.018191, -0.026793, -0.038867, -0.014898, -0.028456,
    -0.037579, -0.019596, -0.028146, -0.042791, -0.016751, -0.029724, -0.036478, -0.021893, -0.029059, -0.042203, -0.020556, -0.030656,
    -0.035710, -0.020662, -0.012968, -0.032994, -0.016377, -0.012758, -0.030278, -0.025696, -0.021023, -0.031274, -0.026147, -0.016970,
    -0.031195, -0.025110, -0.024168, -0.026693, -0.027726, -0.016412, -0.026941, -0.026287, -0.020948, -0.033139, -0.025835, -0.014209,
    -0.028165, -0.028125, -0.012026, -0.033139, -0.024866, -0.026487, -0.029249, -0.025478, -0.025179, -0.033206, -0.024658, -0.028740,
    -0.035283, -0.024305, -0.028464, -0.048314, -0.013697, -0.032678, -0.042969, -0.010219, -0.031156, -0.021939, 0.044143, -0.014418,
    -0.023233, 0.046612, -0.016224, -0.022327, 0.042412, -0.012689, -0.025686, 0.039757, -0.016811, -0.068713, -0.019994, -0.039913,
    -0.063241, -0.025036, -0.038803, -0.056614, -0.018449, -0.036360, -0.051699, -0.022299, -0.035408, -0.069418, -0.014717, -0.041021,
    -0.049430, -0.019077, -0.032884, -0.038778, -0.023322, -0.030686, -0.044886, -0.022158, -0.033710, -0.026250, 0.000971, -0.018831,
    -0.024945, -0.006827, -0.016174, -0.029651, -0.003206, -0.021577, -0.021381, -0.002690, -0.014622, -0.021850, 0.005107, -0.014411,
    -0.034870, 0.006094, -0.023527, -0.028641, 0.010396, -0.016737, -0.022510, 0.028253, -0.002758, -0.024637, 0.033418, -0.004800,
    -0.020392, 0.026952, -0.003210, -0.022248, 0.033046, -0.005350, -0.025996, 0.039509, -0.006712, -0.024126, 0.039778, -0.007286,
    -0.027428, 0.038905, -0.010013, -0.032229, 0.032938, -0.012628, -0.025218, 0.040584, -0.009365, -0.030030, 0.033569, -0.013713,
    -0.024933, 0.039203, -0.013401, -0.000664, -0.001991, -0.002271, -0.003499, -0.002208, -0.005694, -0.002768, -0.000192, -0.004573,
    -0.006597, -0.004488, -0.006141, -0.022455, -0.008932, -0.011963, -0.021308, -0.010921, -0.009459, -0.029453, -0.013240, -0.012302,
    -0.018759, -0.005723, -0.011739, -0.027508, -0.015374, -0.009336, -0.016566, -0.007817, -0.009047, -0.011763, -0.000925, -0.010933,
    -0.009074, -0.002966, -0.008740, -0.010605, 0.000792, -0.008838, -0.012365, 0.002028, -0.008766, -0.013222, 0.002071, -0.010238,
    -0.014168, 0.001119, -0.011336, -0.016763, 0.001965, -0.011721, -0.014721, -0.003314, -0.010440, -0.039808, 0.001374, -0.029031,
    -0.036998, -0.007722, -0.027142, -0.055685, -0.005168, -0.036804, -0.050516, 0.006409, -0.030799, -0.040223, -0.031389, -0.026153,
    -0.030049, -0.024440, -0.008988, -0.034329, -0.024562, -0.011913, -0.031371, 0.024644, -0.011384, -0.035117, 0.027445, -0.017308,
    -0.071601, -0.002925, -0.040359, -0.067240, 0.004942, -0.039153, -0.011591, 0.001389, -0.008456, -0.012110, 0.001795, -0.006439,
    -0.013876, 0.002981, -0.007545, -0.027119, 0.045037, -0.007333, -0.026690, 0.045713, -0.007922, -0.022968, 0.045157, -0.005624,
    -0.024466, 0.042817, -0.005302, -0.029408, -0.033138, -0.018465, -0.020798, 0.043982, -0.003553, -0.006555, -0.000047, -0.007582,
    -0.023607, 0.042252, -0.008566, -0.021355, 0.036940, 0.000521, -0.020051, 0.041696, -0.001472, -0.022807, 0.039973, -0.002421,
    -0.026784, 0.017788, -0.009893, -0.019883, 0.012722, -0.009017, -0.034828, 0.018014, -0.017309, -0.071146, -0.009280, -0.040944,
    -0.009114, 0.002632, -0.004029, -0.015787, 0.004139, -0.008437, -0.030941, -0.011338, -0.016115, -0.033346, -0.008994, -0.020715,
    -0.054791, -0.012548, -0.036713, -0.031973, -0.017798, -0.010712, -0.035771, -0.022320, -0.011201, -0.030581, -0.020072, -0.008262,
    -0.014272, -0.009475, -0.006270, -0.018375, -0.012968, -0.006866, -0.007271, -0.001197, -0.008380, -0.025673, -0.018399, -0.006987,
    -0.059772, 0.000604, -0.036207, -0.003339, 0.001346, -0.001987, -0.013107, -0.004989, -0.008447, -0.045281, 0.025111, -0.029522,
    -0.056629, 0.015510, -0.035305, -0.035011, 0.033726, -0.023521, -0.052958, 0.013514, -0.030933, -0.042413, 0.022529, -0.023489,
    -0.060578, 0.005897, -0.035141, -0.027691, 0.040876, -0.019665, -0.032580, 0.032738, -0.019171, -0.042192, 0.011414, -0.024705,
    -0.030839, -0.020507, 0.000134, -0.030839, -0.020507, 0.000134, -0.030839, -0.020507, 0.000134, -0.030839, -0.020507, 0.000134,
    -0.030839, -0.020507, 0.000134, -0.030839, -0.020507, 0.000134, -0.030839, -0.020507, 0.000134, -0.030839, -0.020507, 0.000134,
    -0.030839, -0.020507, 0.000134, -0.036514, -0.018735, 0.006460, -0.036514, -0.018735, 0.006460, -0.036514, -0.018735, 0.006460,
    -0.036514, -0.018735, 0.006460, -0.036514, -0.018735, 0.006460, -0.036514, -0.018735, 0.006460, -0.036514, -0.018735, 0.006460,
    -0.036514, -0.018735, 0.006460, -0.036514, -0.018735, 0.006460
  ]),
  // 4 — vis_aa
  new Float32Array([
    -0.011029, 0.034541, 0.059272, -0.012345, 0.034450, 0.059654, -0.012532, 0.038461, 0.051121, 0.000326, -0.002474, -0.008943,
    -0.001940, 0.005809, -0.022389, 0.004927, 0.033796, 0.012678, -0.026618, 0.036551, -0.007827, -0.004517, 0.015554, -0.034797,
    -0.010087, 0.032072, -0.061259, -0.012881, 0.036465, -0.073649, -0.007313, 0.024351, -0.049357, -0.032134, 0.066461, -0.033365,
    -0.032796, 0.067707, -0.034594, -0.031307, 0.065041, -0.032922, -0.021146, 0.067859, -0.159789, -0.012832, 0.068474, -0.147821,
    -0.034832, 0.070407, -0.167849, -0.014422, 0.045945, 0.070014, -0.012897, 0.049560, 0.075129, -0.018646, 0.052168, 0.067277,
    -0.019250, 0.164847, 0.075423, -0.019271, 0.164584, 0.077074, -0.020306, 0.167347, 0.069516, -0.022167, 0.191637, 0.060194,
    -0.031010, 0.188291, 0.064238, -0.011970, 0.191921, 0.056971, -0.044016, 0.106625, -0.068629, -0.028329, 0.091688, -0.093477,
    -0.055993, 0.119719, -0.040615, -0.029415, 0.065321, -0.036936, -0.017856, 0.054406, 0.067532, -0.017082, 0.079879, 0.074944,
    -0.012314, 0.106025, 0.083270, -0.036055, 0.054150, -0.054817, -0.030304, 0.044281, -0.064236, -0.024807, 0.036108, -0.071752,
    -0.019049, 0.026610, -0.076394, -0.017029, 0.079442, -0.113390, -0.014822, 0.019457, -0.078380, -0.035142, 0.156679, 0.097350,
    -0.050361, 0.147950, 0.071176, -0.024902, 0.162959, 0.111613, -0.000010, 0.135002, 0.089626, -0.016725, 0.151152, 0.085146,
    -0.032586, 0.048242, -0.070775, -0.038722, 0.062235, -0.057457, -0.025835, 0.038891, -0.080812, -0.019605, 0.027288, -0.085943,
    -0.015398, 0.016937, -0.092119, -0.012255, 0.071066, -0.133446, 0.004888, 0.132565, 0.085637, 0.001287, 0.145657, 0.078430,
    0.000022, 0.108237, 0.082598, -0.000679, 0.085342, 0.081642, -0.007691, 0.063919, 0.079258, -0.032133, 0.180486, 0.067792,
    -0.013283, 0.161059, 0.076744, -0.006177, 0.154416, 0.076133, -0.027067, 0.166971, 0.074182, -0.061275, 0.142318, 0.045988,
    -0.067307, 0.137924, 0.018551, -0.065178, 0.131506, -0.009025, -0.022592, 0.047823, -0.048092, -0.026609, 0.050712, -0.046391,
    -0.021871, 0.049983, -0.049118, -0.026226, 0.164511, 0.115930, -0.035177, 0.167691, 0.115386, -0.031001, 0.063508, -0.033752,
    -0.031238, 0.061537, -0.035724, -0.030840, 0.059065, -0.039380, -0.034110, 0.181290, 0.103891, -0.023474, 0.184275, 0.096630,
    -0.039395, 0.175607, 0.108864, -0.039841, 0.171464, 0.112341, -0.012463, 0.180440, 0.089377, -0.029409, 0.054695, -0.042915,
    -0.022412, 0.056261, -0.049136, -0.022689, 0.062246, -0.047403, -0.021580, 0.063936, -0.045137, -0.023001, 0.064125, -0.041694,
    -0.026556, 0.064330, -0.039115, -0.030446, 0.066919, -0.083787, -0.032021, 0.067707, -0.087447, -0.028883, 0.066043, -0.080956,
    -0.074533, 0.106869, -0.147711, -0.069327, 0.094685, -0.164584, -0.077370, 0.118154, -0.122785, -0.033922, 0.065958, -0.087809,
    -0.012675, 0.061990, 0.050060, -0.017344, 0.089268, 0.051932, -0.024938, 0.116968, 0.056043, -0.035538, 0.055049, -0.116567,
    -0.028786, 0.045958, -0.117624, -0.023189, 0.036161, -0.113874, -0.017951, 0.024117, -0.105517, -0.060578, 0.083642, -0.171128,
    -0.013925, 0.015554, -0.091990, -0.084987, 0.147807, 0.024756, -0.081846, 0.140127, -0.006049, -0.078579, 0.153598, 0.045447,
    -0.040443, 0.146899, 0.060343, -0.024130, 0.162246, 0.060930, -0.032115, 0.050357, -0.127967, -0.042063, 0.062690, -0.126366,
    -0.024118, 0.038965, -0.125827, -0.016665, 0.024725, -0.116007, -0.011591, 0.012915, -0.106779, -0.046523, 0.074355, -0.173589,
    -0.046392, 0.142709, 0.061068, -0.043109, 0.153627, 0.056845, -0.040054, 0.116064, 0.060475, -0.036547, 0.091210, 0.063498,
    -0.026226, 0.068552, 0.065390, -0.009366, 0.187271, 0.054083, -0.026460, 0.166105, 0.063545, -0.034925, 0.161021, 0.058872,
    -0.013543, 0.176287, 0.055720, -0.078630, 0.135738, -0.034128, -0.075585, 0.132997, -0.064456, -0.075999, 0.127907, -0.092314,
    -0.030921, 0.052701, -0.071986, -0.027543, 0.056304, -0.072244, -0.033151, 0.054032, -0.075715, -0.060243, 0.155992, 0.060287,
    -0.036572, 0.161332, 0.070096, -0.027032, 0.065700, -0.078043, -0.025116, 0.065454, -0.075070, -0.025173, 0.064105, -0.073097,
    -0.011902, 0.173559, 0.081813, -0.021293, 0.167276, 0.075859, -0.025777, 0.060491, -0.071826, -0.036226, 0.059091, -0.079085,
    -0.037516, 0.063870, -0.082286, -0.038526, 0.065117, -0.085698, -0.037911, 0.065762, -0.087882, -0.035447, 0.065652, -0.087987,
    0.000000, 0.000000, -0.000000, 0.001753, 0.002050, 0.002875, -0.003565, 0.001918, -0.001391, -0.005845, 0.026474, 0.018295,
    -0.000040, 0.016857, 0.014644, -0.007809, 0.035579, 0.027929, -0.004763, 0.039905, 0.032378, -0.008092, 0.017176, 0.012410,
    -0.013751, 0.042163, 0.023660, -0.004429, 0.018234, -0.029667, -0.001143, 0.008384, -0.018776, -0.006957, 0.026526, -0.044376,
    -0.005055, 0.025050, -0.021111, -0.004474, 0.008464, -0.024869, -0.009840, 0.034451, -0.054521, -0.013535, 0.034594, -0.061816,
    -0.010356, 0.026477, -0.050662, -0.012993, 0.024637, -0.083832, -0.013931, 0.022258, -0.099780, -0.011347, 0.035027, -0.066096,
    -0.020504, 0.033751, -0.075112, -0.013158, 0.047671, -0.123311, -0.017180, 0.048049, -0.132280, -0.024392, 0.047944, -0.141267,
    -0.011591, 0.038052, 0.066798, -0.013043, 0.043753, 0.069858, -0.012926, 0.041033, 0.067812, -0.013608, 0.044429, 0.059194,
    -0.012985, 0.047971, 0.074226, -0.016241, 0.050731, 0.065838, -0.019633, 0.170077, 0.075527, -0.020729, 0.180898, 0.071490,
    -0.026957, 0.178001, 0.072080, -0.022401, 0.167788, 0.076583, -0.013631, 0.181349, 0.064001, -0.016827, 0.170730, 0.068805,
    -0.021856, 0.182202, 0.043015, -0.029415, 0.179798, 0.048513, -0.020706, 0.174214, 0.057415, -0.027376, 0.170646, 0.062698,
    -0.012835, 0.180541, 0.041974, -0.011754, 0.170381, 0.053237, -0.002341, 0.008066, 0.006398, 0.000395, 0.009270, 0.008424,
    -0.006279, 0.009250, 0.004810, 0.004355, 0.018941, 0.012885, 0.005913, 0.032906, 0.016813, 0.006952, 0.018448, 0.008342,
    0.003333, 0.033052, 0.018934, 0.008192, 0.029061, 0.010766, 0.004143, 0.013528, 0.008625, 0.002576, 0.018311, 0.013709,
    -0.027990, 0.070245, -0.083061, -0.038621, 0.084374, -0.065572, -0.030099, 0.060430, -0.035370, -0.030872, 0.064290, -0.031576,
    -0.027387, 0.054378, -0.040298, -0.028457, 0.066560, -0.021056, -0.026669, 0.061611, -0.027261, -0.030963, 0.066717, -0.028771,
    -0.029194, 0.071113, -0.017748, -0.033036, 0.069219, -0.026900, -0.032385, 0.075196, -0.015660, -0.035549, 0.070671, -0.027230,
    -0.032854, 0.077814, -0.017324, -0.039308, 0.068679, -0.032576, -0.037231, 0.074650, -0.028131, -0.023909, 0.049048, -0.043901,
    -0.023106, 0.055325, -0.034668, -0.025765, 0.046512, -0.049813, -0.024055, 0.045139, -0.055145, -0.029927, 0.048520, -0.045412,
    -0.023089, 0.035747, -0.065253, -0.026272, 0.039280, -0.060886, -0.022519, 0.045661, -0.054869, -0.020265, 0.035119, -0.064402,
    -0.034475, 0.052602, -0.042154, -0.030016, 0.043694, -0.054810, -0.035183, 0.051405, -0.050323, -0.037999, 0.060050, -0.038546,
    -0.030283, 0.085006, -0.006689, -0.026625, 0.081765, -0.001515, -0.029883, 0.160411, 0.084903, -0.032487, 0.166197, 0.098799,
    -0.029614, 0.172531, 0.092537, -0.029108, 0.161565, 0.068432, -0.027230, 0.167564, 0.078859, -0.028591, 0.153292, 0.092649,
    -0.048510, 0.108637, -0.011414, -0.044332, 0.098074, -0.040155, -0.039136, 0.090470, -0.016104, -0.039476, 0.078850, -0.040793,
    -0.048664, 0.112945, 0.017467, -0.035771, 0.081821, -0.022125, -0.037215, 0.062991, -0.040412, -0.036549, 0.069114, -0.041297,
    -0.010604, 0.066613, 0.009525, -0.019580, 0.061382, -0.011080, -0.019132, 0.070074, -0.000721, -0.012437, 0.057781, 0.000409,
    -0.003767, 0.066997, 0.022546, -0.001515, 0.080851, 0.025886, -0.001099, 0.079872, 0.038012, -0.010338, 0.064497, 0.078622,
    -0.005406, 0.086571, 0.081431, -0.014169, 0.060245, 0.074882, -0.011934, 0.083328, 0.078971, -0.001803, 0.110242, 0.083457,
    -0.007518, 0.108801, 0.084323, -0.015499, 0.144179, 0.087394, -0.002095, 0.124967, 0.088546, -0.025264, 0.157329, 0.076344,
    -0.017562, 0.138898, 0.090270, -0.027634, 0.151246, 0.079663, 0.001757, 0.001181, -0.006404, 0.003128, 0.008506, -0.000743,
    0.004385, 0.007685, 0.003893, -0.000764, 0.015916, -0.011009, -0.016907, 0.054516, -0.021323, -0.014396, 0.048357, -0.029611,
    -0.017141, 0.054604, -0.028163, -0.012243, 0.048972, -0.009607, -0.014070, 0.047206, -0.037452, -0.010246, 0.040957, -0.019956,
    -0.000270, 0.037818, 0.008464, -0.000188, 0.025977, -0.000903, 0.007445, 0.031481, 0.014251, 0.008698, 0.038288, 0.016778,
    0.006689, 0.042810, 0.017789, 0.000333, 0.046313, 0.013730, -0.002961, 0.055706, 0.014126, -0.008079, 0.043410, -0.001521,
    -0.012379, 0.085680, 0.021489, -0.023383, 0.075739, -0.003356, -0.025219, 0.101701, 0.031730, -0.001116, 0.100850, 0.051530,
    -0.020054, 0.059477, -0.097676, -0.018886, 0.040860, -0.057919, -0.021032, 0.044842, -0.054063, 0.002383, 0.099704, 0.075327,
    -0.004246, 0.123164, 0.087260, -0.034029, 0.126963, 0.064382, -0.021555, 0.135483, 0.085804, 0.008813, 0.034165, 0.015522,
    0.009443, 0.036880, 0.015497, 0.008523, 0.043185, 0.018716, 0.004456, 0.133653, 0.086574, 0.002113, 0.134507, 0.087700,
    -0.011317, 0.148558, 0.082274, -0.004959, 0.146146, 0.079844, -0.014730, 0.050837, -0.112712, -0.019965, 0.161014, 0.074747,
    0.006562, 0.017912, 0.006392, 0.005581, 0.013384, 0.006098, -0.030619, 0.170012, 0.063207, -0.018335, 0.162694, 0.075366,
    -0.025952, 0.171079, 0.071743, -0.011474, 0.155873, 0.075105, -0.000916, 0.078819, 0.059928, -0.002054, 0.061489, 0.043789,
    0.004346, 0.097905, 0.060955, -0.043251, 0.118837, 0.041635, -0.003641, 0.016339, 0.012985, 0.000375, 0.033409, 0.022081,
    0.005848, 0.050226, 0.021976, -0.020866, 0.061297, -0.018839, -0.022821, 0.067943, -0.010606, -0.034811, 0.096347, 0.009133,
    -0.019904, 0.049669, -0.039606, -0.020755, 0.044911, -0.048700, -0.017567, 0.044701, -0.048787, -0.008628, 0.032916, -0.032118,
    -0.012288, 0.041076, -0.041758, 0.004187, 0.020513, 0.004869, -0.013365, 0.041327, -0.048041, -0.015126, 0.108910, 0.056061,
    0.002404, 0.010968, 0.008894, -0.006685, 0.034776, -0.011311, -0.012959, 0.147903, 0.102635, -0.009145, 0.142759, 0.097995,
    -0.025828, 0.154664, 0.105435, 0.000365, 0.123278, 0.080599, -0.003708, 0.128452, 0.087330, -0.008754, 0.118577, 0.073235,
    -0.030943, 0.160195, 0.103386, -0.021169, 0.176215, 0.085008, -0.021416, 0.145097, 0.097126, -0.020005, 0.171125, 0.071351,
    -0.011031, 0.171274, 0.078375, 0.005672, 0.097255, 0.051596, -0.007021, 0.018126, -0.036588, -0.010693, 0.166283, 0.065893,
    -0.012434, 0.025262, -0.033293, -0.015473, 0.019533, 0.007420, -0.022853, 0.035606, 0.009001, -0.020093, 0.019080, -0.005687,
    -0.019580, 0.035632, 0.011426, -0.026163, 0.031201, -0.003712, -0.014371, 0.013255, 0.003105, -0.016197, 0.013195, -0.003691,
    -0.012093, 0.018803, 0.009922, -0.052244, 0.072244, -0.146273, -0.060556, 0.083965, -0.137070, -0.025800, 0.064228, -0.068147,
    -0.026741, 0.066564, -0.070734, -0.026161, 0.059143, -0.066083, -0.029160, 0.067460, -0.060042, -0.027369, 0.063832, -0.057983,
    -0.029653, 0.067316, -0.074247, -0.033599, 0.070563, -0.065254, -0.031809, 0.069079, -0.077843, -0.036807, 0.073890, -0.070299,
    -0.032130, 0.070778, -0.080938, -0.042636, 0.076230, -0.075284, -0.029947, 0.069086, -0.088066, -0.039283, 0.073676, -0.087626,
    -0.027479, 0.054013, -0.066967, -0.028166, 0.058161, -0.058048, -0.029996, 0.048382, -0.091572, -0.030417, 0.046495, -0.088064,
    -0.029004, 0.051091, -0.093597, -0.024388, 0.035457, -0.096226, -0.024556, 0.040626, -0.101310, -0.030272, 0.046984, -0.081709,
    -0.024209, 0.033895, -0.086223, -0.028347, 0.055036, -0.093918, -0.025840, 0.046233, -0.104018, -0.028388, 0.053240, -0.105481,
    -0.028533, 0.061283, -0.092923, -0.052889, 0.082314, -0.070776, -0.046750, 0.079343, -0.061947, -0.012514, 0.158030, 0.061499,
    -0.012494, 0.163572, 0.073373, -0.012387, 0.160575, 0.049323, -0.021192, 0.150270, 0.061840, -0.069501, 0.105797, -0.090057,
    -0.066261, 0.096688, -0.116356, -0.059471, 0.088156, -0.088986, -0.052790, 0.078004, -0.110007, -0.070166, 0.108275, -0.063324,
    -0.051164, 0.079914, -0.086937, -0.034303, 0.063329, -0.099299, -0.044133, 0.068642, -0.106694, -0.032641, 0.066740, -0.022982,
    -0.026226, 0.062561, -0.039969, -0.031133, 0.069715, -0.039730, -0.028109, 0.059649, -0.025160, -0.034366, 0.068517, -0.003749,
    -0.047544, 0.077910, -0.015505, -0.040688, 0.078268, 0.005729, -0.022528, 0.069835, 0.063481, -0.030472, 0.093064, 0.062132,
    -0.017074, 0.066482, 0.058537, -0.022942, 0.091522, 0.057855, -0.036968, 0.119212, 0.060031, -0.029968, 0.118855, 0.058756,
    -0.026990, 0.147715, 0.059253, -0.041345, 0.126979, 0.054001, -0.016775, 0.159335, 0.054202, -0.032140, 0.137711, 0.057255,
    -0.018322, 0.150852, 0.053981, -0.002157, 0.001213, -0.011701, -0.009739, 0.008617, -0.013251, -0.010542, 0.007387, -0.005508,
    -0.010705, 0.016449, -0.023145, -0.024399, 0.055625, -0.043423, -0.022356, 0.048986, -0.047482, -0.029406, 0.055464, -0.051073,
    -0.024115, 0.050440, -0.030620, -0.027616, 0.048279, -0.054832, -0.020361, 0.041678, -0.036858, -0.025261, 0.041119, -0.012761,
    -0.018720, 0.027654, -0.017936, -0.027129, 0.033901, -0.003143, -0.030606, 0.041656, -0.000181, -0.031733, 0.046762, -0.002151,
    -0.029113, 0.049909, -0.007959, -0.030876, 0.059509, -0.006754, -0.023800, 0.045633, -0.020984, -0.048694, 0.083130, -0.032681,
    -0.039339, 0.074598, -0.054911, -0.064685, 0.097274, -0.040460, -0.069458, 0.095944, -0.006719, -0.043153, 0.062166, -0.148271,
    -0.025555, 0.041659, -0.074086, -0.029729, 0.047592, -0.076527, -0.041818, 0.100147, 0.043004, -0.046907, 0.120424, 0.046768,
    -0.078856, 0.119321, -0.011158, -0.082836, 0.127743, 0.013838, -0.028648, 0.036726, -0.000182, -0.029109, 0.039413, 0.003341,
    -0.031290, 0.046661, 0.004561, -0.046093, 0.144397, 0.061240, -0.043214, 0.145820, 0.060965, -0.029280, 0.158822, 0.059922,
    -0.035772, 0.155284, 0.058273, -0.031715, 0.051978, -0.147308, -0.020345, 0.169601, 0.057225, -0.018814, 0.018705, -0.008737,
    -0.009948, 0.171529, 0.047399, -0.021329, 0.168429, 0.061940, -0.014358, 0.177365, 0.057793, -0.028867, 0.163267, 0.057904,
    -0.033490, 0.079548, 0.033493, -0.025466, 0.063883, 0.023368, -0.051073, 0.094345, 0.022738, -0.073370, 0.112258, -0.037206,
    -0.016097, 0.036124, 0.014592, -0.032622, 0.054174, 0.006492, -0.029679, 0.062167, -0.048876, -0.032572, 0.067911, -0.049912,
    -0.059289, 0.092824, -0.064776, -0.028091, 0.052528, -0.058701, -0.029784, 0.049397, -0.068625, -0.027466, 0.046828, -0.063500,
    -0.015251, 0.033185, -0.044099, -0.017849, 0.041288, -0.054553, -0.018096, 0.021549, -0.011764, -0.024745, 0.041824, -0.060727,
    -0.074486, 0.103496, -0.013606, -0.009867, 0.010719, 0.004415, -0.018889, 0.035852, -0.027897, -0.061976, 0.140702, 0.048713,
    -0.079569, 0.135153, 0.033879, -0.038592, 0.149190, 0.062447, -0.078100, 0.116830, 0.022127, -0.059961, 0.122129, 0.039228,
    -0.081149, 0.112368, 0.007319, -0.023643, 0.156207, 0.069206, -0.036224, 0.140622, 0.057908, -0.062994, 0.091982, 0.005029,
    -0.029711, 0.065989, 0.003567, -0.029711, 0.065989, 0.003567, -0.029711, 0.065989, 0.003567, -0.029711, 0.065989, 0.003567,
    -0.029711, 0.065989, 0.003567, -0.029711, 0.065989, 0.003567, -0.029711, 0.065989, 0.003567, -0.029711, 0.065989, 0.003567,
    -0.029711, 0.065989, 0.003567, -0.040210, 0.067729, 0.009791, -0.040210, 0.067729, 0.009791, -0.040210, 0.067729, 0.009791,
    -0.040210, 0.067729, 0.009791, -0.040210, 0.067729, 0.009791, -0.040210, 0.067729, 0.009791, -0.040210, 0.067729, 0.009791,
    -0.040210, 0.067729, 0.009791, -0.040210, 0.067729, 0.009791
  ]),
  // 5 — angry
  new Float32Array([
    -0.007940, -0.039306, -0.023178, -0.014201, -0.042036, -0.021993, -0.000896, -0.037908, -0.023660, 0.000742, 0.003854, 0.006974,
    -0.000601, 0.000743, 0.014905, -0.021523, -0.023039, -0.002119, 0.010540, -0.019093, -0.007796, -0.002790, -0.005772, 0.022062,
    -0.007147, -0.016763, 0.038166, -0.009108, -0.009988, 0.045288, -0.005129, -0.012329, 0.029981, -0.008691, -0.042352, 0.032825,
    -0.014103, -0.033535, 0.037089, -0.003831, -0.048451, 0.031119, -0.010087, -0.024840, 0.092648, -0.011538, -0.027429, 0.091506,
    -0.007370, -0.023832, 0.086115, -0.010630, -0.031122, -0.034406, -0.013314, -0.033108, -0.034116, -0.007852, -0.028715, -0.035272,
    -0.011515, -0.036076, -0.033088, -0.013252, -0.037245, -0.032555, -0.010348, -0.032895, -0.033467, -0.013231, -0.032581, -0.044197,
    -0.013035, -0.033462, -0.043154, -0.012927, -0.028584, -0.043892, -0.030561, -0.068165, 0.070583, -0.022840, -0.050108, 0.083325,
    -0.035477, -0.082438, 0.056097, -0.011290, -0.025781, 0.038401, -0.022733, -0.043293, -0.024446, -0.025517, -0.041835, -0.027309,
    -0.025282, -0.039170, -0.030443, -0.009649, 0.004337, 0.063773, -0.000268, 0.022586, 0.072855, 0.007206, 0.032898, 0.071260,
    0.009699, 0.031770, 0.065738, -0.015905, -0.037303, 0.091091, 0.002420, 0.016696, 0.059680, -0.020857, -0.102585, -0.040210,
    -0.025421, -0.102648, -0.020981, -0.017382, -0.094520, -0.052567, -0.023881, -0.036417, -0.035682, -0.022524, -0.034893, -0.038251,
    -0.002034, 0.021214, 0.075420, -0.013025, -0.002324, 0.065807, 0.007325, 0.032560, 0.073074, 0.010053, 0.032862, 0.071195,
    0.005789, 0.023625, 0.069235, -0.012115, -0.029654, 0.092599, -0.022781, -0.035961, -0.035220, -0.020345, -0.034899, -0.034444,
    -0.020359, -0.037317, -0.035084, -0.019516, -0.037414, -0.034638, -0.016938, -0.036087, -0.034197, -0.015027, -0.032972, -0.043295,
    -0.016100, -0.037204, -0.032840, -0.018798, -0.035828, -0.033617, -0.019108, -0.033357, -0.041945, -0.033491, -0.100820, -0.002382,
    -0.037098, -0.098927, 0.017423, -0.037582, -0.093004, 0.036691, -0.028849, -0.048853, 0.028929, -0.023474, -0.053588, 0.029679,
    -0.025938, -0.041360, 0.031356, -0.015036, -0.081324, -0.063859, -0.017126, -0.066238, -0.070731, -0.001629, -0.054398, 0.028204,
    -0.002231, -0.059332, 0.027117, -0.008249, -0.059847, 0.027819, -0.025590, -0.038976, -0.090116, -0.022815, -0.036609, -0.093604,
    -0.024249, -0.044912, -0.083352, -0.021086, -0.054805, -0.076794, -0.016713, -0.037617, -0.094073, -0.015878, -0.056954, 0.028951,
    -0.018427, -0.027167, 0.032780, -0.013912, -0.013566, 0.038985, -0.009731, -0.008045, 0.039716, -0.007382, -0.011297, 0.039991,
    -0.008391, -0.019026, 0.038165, -0.017850, -0.040325, 0.019861, -0.013707, -0.033535, 0.023382, -0.021635, -0.044641, 0.018672,
    -0.018519, -0.057763, 0.050891, -0.018804, -0.040671, 0.065487, -0.017965, -0.071392, 0.035086, -0.015045, -0.027075, 0.024894,
    0.006076, -0.033935, -0.027845, 0.004424, -0.028047, -0.031370, -0.000152, -0.021863, -0.034420, -0.028197, 0.005583, 0.047611,
    -0.032062, 0.022223, 0.058702, -0.034706, 0.031116, 0.059500, -0.032623, 0.029200, 0.057834, -0.016963, -0.029133, 0.075888,
    -0.023796, 0.013674, 0.055527, -0.030571, -0.095976, -0.059353, -0.030957, -0.094912, -0.041067, -0.028575, -0.088754, -0.069646,
    -0.007395, -0.016034, -0.039867, -0.006630, -0.017178, -0.041116, -0.033643, 0.021719, 0.060723, -0.027857, 0.000465, 0.049420,
    -0.035162, 0.031143, 0.061034, -0.030673, 0.030510, 0.063028, -0.024507, 0.020955, 0.064706, -0.011035, -0.023716, 0.081709,
    -0.008268, -0.017967, -0.038065, -0.008980, -0.020640, -0.036836, -0.008364, -0.023374, -0.037974, -0.006127, -0.025956, -0.037219,
    -0.005439, -0.027934, -0.036236, -0.010743, -0.023398, -0.044928, -0.008588, -0.029090, -0.034435, -0.008196, -0.024264, -0.035562,
    -0.007808, -0.019343, -0.044068, -0.026271, -0.091078, -0.023053, -0.022748, -0.087755, -0.003911, -0.019947, -0.081503, 0.014897,
    -0.005812, -0.044836, 0.022433, -0.010775, -0.048834, 0.022967, -0.007669, -0.038823, 0.024170, -0.026804, -0.076826, -0.078551,
    -0.022296, -0.063545, -0.082806, -0.023947, -0.048920, 0.016705, -0.024702, -0.052804, 0.016999, -0.020881, -0.053442, 0.019084,
    -0.015891, -0.043600, -0.090776, -0.018393, -0.053272, -0.086520, -0.016275, -0.051322, 0.021044, -0.012564, -0.027649, 0.024481,
    -0.014393, -0.016648, 0.028996, -0.015911, -0.012328, 0.028226, -0.017332, -0.015032, 0.027385, -0.016906, -0.021613, 0.024936,
    0.000000, 0.000000, -0.000000, -0.003739, -0.002245, 0.001903, 0.002144, -0.000597, 0.000793, -0.003893, -0.016458, -0.011809,
    -0.007220, -0.011960, -0.007754, -0.004837, -0.024668, -0.015797, -0.012245, -0.027980, -0.015532, 0.000342, -0.010714, -0.008405,
    0.000208, -0.025321, -0.017428, -0.005827, -0.007802, 0.020758, -0.004239, -0.001681, 0.014447, -0.008382, -0.014179, 0.029158,
    -0.009999, -0.013027, 0.015254, 0.001759, -0.000624, 0.013012, -0.011262, -0.019240, 0.036269, -0.004950, -0.017980, 0.034313,
    -0.003246, -0.013129, 0.027439, -0.009370, 0.012742, 0.057642, -0.009401, 0.021738, 0.067284, -0.013823, -0.015554, 0.039576,
    -0.009482, -0.016199, 0.036895, -0.002511, -0.008256, 0.077848, -0.009134, -0.007407, 0.076032, -0.014453, -0.005976, 0.073056,
    -0.008477, -0.037456, -0.026662, -0.009357, -0.033720, -0.030394, -0.014194, -0.039354, -0.025678, -0.001973, -0.034588, -0.027383,
    -0.013828, -0.035649, -0.030348, -0.004952, -0.030816, -0.031777, -0.011860, -0.034824, -0.035914, -0.012357, -0.033592, -0.039101,
    -0.012381, -0.034699, -0.038464, -0.012435, -0.036034, -0.035252, -0.011914, -0.030063, -0.039157, -0.011030, -0.031526, -0.036060,
    -0.014903, -0.037077, -0.054981, -0.013543, -0.038060, -0.054276, -0.016091, -0.033085, -0.065136, -0.016408, -0.035290, -0.064441,
    -0.014446, -0.035930, -0.055103, -0.013315, -0.033681, -0.066646, -0.001901, -0.005747, -0.002338, -0.005738, -0.006965, -0.001902,
    0.000775, -0.005562, -0.002926, -0.014129, -0.014212, -0.005895, -0.018435, -0.021756, -0.007521, -0.014794, -0.014571, -0.000277,
    -0.016026, -0.021695, -0.008677, -0.019628, -0.020963, -0.003372, -0.012102, -0.010942, -0.002054, -0.010991, -0.013420, -0.006895,
    -0.009859, -0.020737, 0.077028, -0.020978, -0.038755, 0.066010, -0.007408, -0.057529, 0.022000, -0.002913, -0.056998, 0.024212,
    -0.014155, -0.053834, 0.026161, -0.005443, -0.052411, 0.023036, -0.008074, -0.052480, 0.018571, -0.003819, -0.052192, 0.027005,
    -0.007449, -0.050572, 0.024751, -0.008075, -0.049187, 0.029684, -0.011771, -0.050152, 0.026439, -0.013652, -0.044490, 0.032361,
    -0.016482, -0.049280, 0.032460, -0.015757, -0.035504, 0.038945, -0.019195, -0.039343, 0.041202, -0.021664, -0.050430, 0.027129,
    -0.013653, -0.049044, 0.024318, -0.011592, 0.006889, 0.043480, -0.018474, 0.001222, 0.040373, -0.006664, 0.002260, 0.045333,
    -0.006336, 0.012945, 0.049702, -0.002462, 0.015194, 0.055492, -0.024451, -0.014037, 0.035637, -0.013817, -0.005444, 0.040225,
    -0.005804, -0.009218, 0.042796, -0.003831, 0.007913, 0.056544, -0.008139, -0.004189, 0.053849, -0.010727, -0.021980, 0.040238,
    -0.019386, -0.054422, 0.027810, -0.013503, -0.054085, 0.020348, -0.020200, -0.038986, -0.068639, -0.022948, -0.040314, -0.077253,
    -0.023892, -0.033738, -0.082089, -0.015448, -0.038918, -0.060428, -0.020594, -0.033049, -0.072167, -0.017577, -0.044444, -0.063764,
    -0.029998, -0.073778, 0.037538, -0.027441, -0.059006, 0.052859, -0.025933, -0.050619, 0.038056, -0.021472, -0.030272, 0.053276,
    -0.027341, -0.079138, 0.020717, -0.023624, -0.044120, 0.040274, -0.014451, -0.022254, 0.047020, -0.018596, -0.022336, 0.050941,
    -0.014418, -0.040443, -0.005825, -0.011392, -0.042785, 0.010926, -0.008569, -0.045168, 0.009938, -0.016937, -0.038094, -0.000821,
    -0.022408, -0.039057, -0.012936, -0.010536, -0.044131, -0.013309, -0.016789, -0.040233, -0.018439, -0.019135, -0.038058, -0.030321,
    -0.021220, -0.038529, -0.032412, -0.020530, -0.040466, -0.027473, -0.023280, -0.040137, -0.029821, -0.022169, -0.037871, -0.032957,
    -0.023816, -0.038401, -0.031625, -0.013372, -0.037921, -0.041623, -0.013092, -0.037560, -0.036202, -0.012316, -0.038100, -0.046972,
    -0.014769, -0.044229, -0.045783, -0.015395, -0.041612, -0.054785, -0.003582, 0.001693, 0.007690, -0.009893, -0.005377, 0.007594,
    -0.009382, -0.006979, 0.002280, -0.010392, -0.007857, 0.010741, -0.014425, -0.037492, 0.015793, -0.015021, -0.031803, 0.021820,
    -0.012420, -0.043421, 0.021221, -0.017174, -0.032356, 0.005549, -0.015653, -0.037888, 0.025970, -0.015388, -0.025349, 0.013698,
    -0.021621, -0.025295, -0.000853, -0.016903, -0.016772, 0.004711, -0.021002, -0.022021, -0.003619, -0.023247, -0.025675, -0.005959,
    -0.024625, -0.028403, -0.005989, -0.023521, -0.030127, -0.004872, -0.023094, -0.036460, -0.009792, -0.019322, -0.028595, 0.002074,
    -0.008631, -0.051457, 0.002608, -0.008962, -0.050899, 0.015564, -0.014566, -0.066744, 0.007544, -0.006911, -0.059506, -0.016263,
    -0.000928, -0.008089, 0.079899, -0.018182, -0.026937, 0.034157, -0.025168, -0.033226, 0.034597, -0.013665, -0.037002, -0.026723,
    -0.012785, -0.045285, -0.038592, -0.017265, -0.086911, -0.016278, -0.012969, -0.088177, -0.033165, -0.021634, -0.023298, -0.007798,
    -0.021849, -0.024078, -0.006259, -0.023623, -0.027618, -0.008622, -0.023230, -0.036136, -0.035889, -0.023522, -0.036323, -0.035393,
    -0.021617, -0.034982, -0.036952, -0.020701, -0.034997, -0.036023, 0.001609, -0.004242, 0.079973, -0.018536, -0.034495, -0.039448,
    -0.014896, -0.013828, 0.001454, -0.012223, -0.011026, 0.000245, -0.012212, -0.037824, -0.050567, -0.014699, -0.035826, -0.035068,
    -0.014566, -0.034583, -0.039645, -0.017927, -0.035209, -0.035765, -0.015451, -0.036035, -0.021195, -0.017769, -0.033765, -0.017595,
    -0.013102, -0.041774, -0.024502, -0.022319, -0.083071, 0.002931, -0.003110, -0.010673, -0.009139, -0.012954, -0.021795, -0.011671,
    -0.025081, -0.031552, -0.011842, -0.008462, -0.047738, 0.017722, -0.006796, -0.048868, 0.017606, -0.020380, -0.064532, 0.023220,
    -0.017517, -0.044244, 0.024457, -0.027213, -0.044975, 0.028747, -0.020258, -0.038238, 0.029473, -0.011632, -0.018640, 0.021618,
    -0.013270, -0.024868, 0.029202, -0.015886, -0.013986, 0.002292, -0.017113, -0.031363, 0.032358, -0.010576, -0.070309, -0.011783,
    -0.008648, -0.008721, -0.002068, -0.015528, -0.020747, 0.009137, -0.011644, -0.069925, -0.056233, -0.011143, -0.081216, -0.047034,
    -0.016321, -0.058823, -0.063959, -0.007538, -0.068451, -0.036073, -0.009794, -0.057318, -0.041910, -0.008531, -0.073979, -0.027150,
    -0.019536, -0.049044, -0.071298, -0.020211, -0.030544, -0.085367, -0.015614, -0.051079, -0.053868, -0.017961, -0.029843, -0.074410,
    -0.013067, -0.032348, -0.085801, -0.009722, -0.049637, -0.025393, -0.001405, -0.006892, 0.018862, -0.012153, -0.031729, -0.075597,
    0.000332, -0.011271, 0.011989, 0.004644, -0.010804, -0.007481, 0.005878, -0.018544, -0.009484, 0.006654, -0.009936, -0.004274,
    0.004520, -0.018510, -0.010519, 0.008131, -0.017083, -0.007480, 0.004676, -0.007512, -0.003656, 0.005725, -0.006884, -0.002557,
    0.002750, -0.010981, -0.007806, -0.027657, -0.013332, 0.061241, -0.022256, -0.030988, 0.048192, -0.020809, -0.051886, 0.013524,
    -0.023457, -0.051338, 0.014066, -0.016665, -0.049061, 0.019257, -0.019546, -0.047321, 0.012952, -0.018258, -0.047995, 0.010357,
    -0.022222, -0.047163, 0.015395, -0.018031, -0.045471, 0.012624, -0.018862, -0.045452, 0.017078, -0.016080, -0.045792, 0.012362,
    -0.015494, -0.042210, 0.018812, -0.014471, -0.045235, 0.017491, -0.015373, -0.033618, 0.024410, -0.014587, -0.036080, 0.025445,
    -0.011226, -0.046300, 0.020967, -0.014212, -0.045436, 0.017780, -0.016105, 0.005398, 0.031641, -0.010838, -0.000200, 0.031257,
    -0.021515, 0.001525, 0.032042, -0.020552, 0.010911, 0.041667, -0.024247, 0.013981, 0.044403, -0.007638, -0.014205, 0.028156,
    -0.015140, -0.006579, 0.034696, -0.023610, -0.009003, 0.029074, -0.024618, 0.007815, 0.043199, -0.023397, -0.003491, 0.039277,
    -0.020530, -0.020705, 0.026085, -0.011873, -0.048774, 0.011022, -0.013424, -0.048361, 0.004860, -0.011562, -0.036639, -0.074317,
    -0.012554, -0.038425, -0.083777, -0.012852, -0.036027, -0.064546, -0.012749, -0.040543, -0.071003, -0.016572, -0.063754, 0.017311,
    -0.017530, -0.049913, 0.033837, -0.013291, -0.044303, 0.019613, -0.019384, -0.025289, 0.034904, -0.019422, -0.069228, 0.000107,
    -0.013082, -0.038798, 0.023005, -0.019780, -0.020136, 0.031611, -0.018998, -0.019013, 0.034175, -0.007515, -0.035641, -0.014096,
    -0.010931, -0.038438, 0.003710, -0.015240, -0.039875, 0.000080, -0.002905, -0.033732, -0.007509, 0.002810, -0.034514, -0.019502,
    -0.013240, -0.038615, -0.023377, -0.004752, -0.035469, -0.025757, -0.001717, -0.029094, -0.032912, -0.002984, -0.026080, -0.035432,
    0.002294, -0.030952, -0.030566, 0.000936, -0.026717, -0.033341, -0.005217, -0.022464, -0.035817, -0.002736, -0.021682, -0.034961,
    -0.011405, -0.028065, -0.045937, -0.011116, -0.027223, -0.042336, -0.012926, -0.031535, -0.050221, -0.011510, -0.037048, -0.052129,
    -0.012121, -0.036363, -0.059747, 0.003356, 0.002924, 0.006293, 0.005920, -0.002465, 0.004698, 0.005263, -0.003560, -0.000386,
    0.004221, -0.005701, 0.007452, -0.006299, -0.034068, 0.009819, -0.005060, -0.028925, 0.016857, -0.011124, -0.040760, 0.014989,
    -0.000464, -0.029158, -0.000044, -0.008204, -0.035899, 0.020624, -0.000446, -0.022749, 0.009003, 0.009381, -0.021451, -0.006750,
    0.007574, -0.013420, -0.000028, 0.009701, -0.018141, -0.008513, 0.010132, -0.021849, -0.010728, 0.011212, -0.024110, -0.011523,
    0.008945, -0.025730, -0.010820, 0.006746, -0.032020, -0.015774, 0.004682, -0.025157, -0.003231, -0.018504, -0.045358, -0.011104,
    -0.014912, -0.045283, 0.002462, -0.019161, -0.059520, -0.011063, -0.022055, -0.053070, -0.029962, -0.029643, -0.002430, 0.066725,
    -0.011718, -0.026003, 0.029872, -0.008085, -0.031524, 0.028288, -0.007452, -0.028956, -0.032575, -0.012853, -0.037557, -0.046558,
    -0.026886, -0.079532, -0.035293, -0.028521, -0.081046, -0.051338, 0.008613, -0.019789, -0.011670, 0.008498, -0.020895, -0.009661,
    0.009426, -0.024178, -0.012603, -0.007899, -0.017222, -0.038940, -0.007549, -0.016607, -0.038779, -0.007158, -0.017960, -0.039406,
    -0.007985, -0.019323, -0.038334, -0.022753, -0.000451, 0.070775, -0.008055, -0.020920, -0.041482, 0.007139, -0.009227, -0.002819,
    -0.013922, -0.033455, -0.052782, -0.009709, -0.027127, -0.036532, -0.009923, -0.025348, -0.041184, -0.008515, -0.022833, -0.037592,
    -0.003006, -0.029881, -0.026409, 0.002635, -0.028918, -0.021901, -0.010149, -0.035986, -0.032179, -0.023499, -0.074393, -0.016942,
    0.002344, -0.019027, -0.013642, 0.009839, -0.027906, -0.016168, -0.014799, -0.044034, 0.009731, -0.016620, -0.044273, 0.007543,
    -0.015432, -0.056106, 0.004414, -0.011241, -0.041534, 0.018852, -0.006936, -0.041668, 0.023171, -0.008710, -0.036305, 0.024678,
    -0.002451, -0.016819, 0.018381, -0.004877, -0.022865, 0.025737, 0.007981, -0.009755, -0.002361, -0.007061, -0.030240, 0.028610,
    -0.023760, -0.063336, -0.029337, 0.002562, -0.006232, -0.003266, 0.002685, -0.018191, 0.004420, -0.023344, -0.064911, -0.069901,
    -0.026345, -0.074668, -0.063520, -0.018420, -0.054767, -0.074615, -0.024007, -0.061418, -0.050176, -0.019023, -0.050940, -0.052554,
    -0.026205, -0.066981, -0.043446, -0.014755, -0.046277, -0.079691, -0.014876, -0.045600, -0.062544, -0.016904, -0.043887, -0.036247,
    -0.020324, -0.035500, -0.002060, -0.020324, -0.035500, -0.002060, -0.020324, -0.035500, -0.002060, -0.020324, -0.035500, -0.002060,
    -0.020324, -0.035500, -0.002060, -0.020324, -0.035500, -0.002060, -0.020324, -0.035500, -0.002060, -0.020324, -0.035500, -0.002060,
    -0.020324, -0.035500, -0.002060, -0.007552, -0.036213, 0.008051, -0.007552, -0.036213, 0.008051, -0.007552, -0.036213, 0.008051,
    -0.007552, -0.036213, 0.008051, -0.007552, -0.036213, 0.008051, -0.007552, -0.036213, 0.008051, -0.007552, -0.036213, 0.008051,
    -0.007552, -0.036213, 0.008051, -0.007552, -0.036213, 0.008051
  ]),
  // 6 — blink
  new Float32Array([
    0.001179, 0.017147, 0.009352, 0.003081, 0.016199, 0.010617, -0.001556, 0.016798, 0.006990, -0.000038, 0.003811, -0.000079,
    -0.001996, 0.009164, -0.000740, 0.004375, 0.004217, 0.004130, -0.018052, 0.003423, -0.005821, -0.004372, 0.013035, 0.000060,
    -0.007947, 0.016989, -0.002420, -0.009683, 0.019602, -0.003476, -0.006271, 0.015752, -0.001639, -0.011732, 0.029199, 0.007604,
    -0.015954, 0.048212, 0.009578, -0.004737, 0.015875, 0.006815, -0.011055, 0.026642, -0.015819, -0.009072, 0.025002, -0.011232,
    -0.012512, 0.026193, -0.021108, -0.002896, 0.017470, 0.001251, -0.004610, 0.017150, 0.003049, -0.004595, 0.017792, -0.000015,
    -0.004893, 0.021206, 0.019002, -0.007080, 0.020028, 0.019986, -0.004091, 0.021103, 0.017118, -0.007311, 0.027189, 0.010993,
    -0.010350, 0.026955, 0.012166, -0.004574, 0.027806, 0.009785, -0.054925, 0.001960, 0.012091, -0.037913, 0.009903, 0.008112,
    -0.063673, -0.002253, 0.016520, -0.002081, 0.070870, 0.010822, 0.000650, 0.018078, 0.011773, -0.004454, 0.019651, 0.013386,
    -0.008445, 0.020510, 0.015171, -0.036307, 0.024608, 0.013924, -0.032542, 0.032646, 0.012997, -0.029591, 0.034637, 0.009694,
    -0.025614, 0.025129, 0.005228, -0.021418, 0.016264, 0.003399, -0.016065, 0.007030, 0.002034, -0.052635, 0.007287, 0.018892,
    -0.054209, 0.003561, 0.017383, -0.047598, 0.011441, 0.019005, -0.011478, 0.020901, 0.016734, -0.013907, 0.022673, 0.017075,
    -0.040515, 0.025331, 0.011700, -0.044415, 0.017927, 0.016428, -0.036620, 0.025537, 0.006649, -0.028680, 0.017218, 0.003681,
    -0.018517, 0.007333, 0.000503, -0.012776, 0.022642, -0.004525, -0.012761, 0.020544, 0.015985, -0.011192, 0.021008, 0.017480,
    -0.009666, 0.020236, 0.011685, -0.007980, 0.019443, 0.008490, -0.006001, 0.018157, 0.005659, -0.012555, 0.026652, 0.013649,
    -0.008825, 0.019983, 0.019436, -0.010549, 0.020319, 0.018370, -0.014309, 0.025232, 0.015057, -0.057886, -0.000051, 0.017140,
    -0.062483, -0.002970, 0.017965, -0.065159, -0.003951, 0.018686, -0.011904, -0.011991, -0.000165, -0.009809, -0.017342, 0.000626,
    -0.006222, 0.010968, 0.001028, -0.041134, 0.015015, 0.017238, -0.036451, 0.019509, 0.017119, 0.000339, 0.003768, 0.004570,
    0.002070, -0.005394, 0.003044, -0.001074, -0.009612, 0.002234, -0.030111, 0.034335, 0.007850, -0.024027, 0.036683, 0.004065,
    -0.032852, 0.029447, 0.012581, -0.034316, 0.023900, 0.016037, -0.018134, 0.034599, 0.001523, -0.004389, -0.014608, 0.001658,
    0.008116, 0.055210, 0.003181, 0.013006, 0.100318, 0.008492, 0.017924, 0.120641, 0.009402, 0.018918, 0.114865, 0.010491,
    0.011496, 0.092474, 0.010612, -0.018543, 0.029609, -0.016584, -0.015325, 0.048211, -0.015955, -0.025266, 0.016614, -0.016187,
    -0.001019, 0.007799, -0.025843, -0.006202, 0.013927, -0.025918, 0.002457, 0.003881, -0.023050, -0.030407, 0.071805, -0.013789,
    -0.006358, 0.018616, 0.004225, -0.008594, 0.020711, 0.003950, -0.009942, 0.021962, 0.004896, 0.000081, 0.026065, -0.016086,
    0.001250, 0.033212, -0.012724, 0.002103, 0.034363, -0.010960, 0.002966, 0.024731, -0.009155, -0.011336, 0.019211, -0.024454,
    -0.005019, 0.006568, -0.005063, -0.006978, 0.011188, -0.016257, -0.005450, 0.007478, -0.019987, -0.009234, 0.014813, -0.012818,
    -0.011442, 0.022440, 0.006064, -0.006620, 0.025335, 0.008226, 0.008177, 0.025799, -0.015849, 0.004955, 0.020944, -0.018758,
    0.009862, 0.025020, -0.015495, 0.007708, 0.016410, -0.011104, -0.000358, 0.006821, -0.006881, -0.011595, 0.024273, -0.024080,
    -0.010151, 0.022023, 0.007212, -0.009526, 0.023687, 0.009789, -0.011642, 0.021049, 0.003488, -0.010412, 0.020139, 0.001336,
    -0.007868, 0.018868, 0.000208, -0.003368, 0.028835, 0.008462, -0.005235, 0.022206, 0.014389, -0.007777, 0.023347, 0.011914,
    -0.003721, 0.028416, 0.008206, -0.002195, 0.004088, -0.021871, 0.001617, 0.001973, -0.022044, 0.003643, 0.002035, -0.021643,
    -0.019234, -0.009396, -0.011953, -0.021646, -0.014455, -0.011952, -0.026834, 0.014162, -0.011912, -0.012397, 0.017577, -0.009147,
    -0.014203, 0.021377, -0.003976, -0.030653, 0.005070, -0.016778, -0.033707, -0.003309, -0.015793, -0.031197, -0.006877, -0.013930,
    -0.016190, 0.030164, 0.000302, -0.015655, 0.025275, -0.000770, -0.027488, -0.011551, -0.012540, -0.044577, 0.059096, -0.011174,
    -0.051219, 0.105050, -0.008433, -0.055196, 0.124894, -0.010314, -0.054640, 0.117872, -0.011841, -0.045626, 0.094416, -0.013221,
    0.000000, 0.000000, -0.000000, 0.001271, 0.000093, 0.001494, -0.002657, -0.000014, -0.000495, -0.001280, 0.002216, 0.003192,
    -0.000359, 0.002201, 0.002698, -0.001740, 0.007027, 0.005356, -0.000413, 0.007203, 0.007095, -0.002733, 0.002254, 0.001626,
    -0.008613, 0.006902, 0.003387, -0.004596, 0.012929, 0.000718, -0.001488, 0.008972, 0.001291, -0.006742, 0.015113, -0.000489,
    -0.005111, 0.011459, 0.000769, -0.003612, 0.009054, -0.001579, -0.008657, 0.015873, -0.000821, -0.008988, 0.016181, -0.004238,
    -0.007217, 0.015235, -0.003578, -0.009541, 0.020394, -0.002212, -0.009422, 0.020324, -0.004962, -0.008079, 0.008645, -0.002760,
    -0.014999, 0.008818, -0.007476, -0.008526, 0.024154, -0.009325, -0.010193, 0.024794, -0.013660, -0.009855, 0.024332, -0.018266,
    0.000422, 0.017258, 0.009077, -0.001052, 0.017106, 0.007622, 0.001117, 0.017076, 0.010621, -0.002220, 0.017564, 0.006927,
    -0.001454, 0.017068, 0.009341, -0.003450, 0.017427, 0.005788, -0.005070, 0.023639, 0.013627, -0.005637, 0.025763, 0.012834,
    -0.008332, 0.025346, 0.014099, -0.007542, 0.022813, 0.014847, -0.003296, 0.026064, 0.011209, -0.003081, 0.023866, 0.011953,
    -0.016014, 0.028552, 0.006247, -0.019069, 0.027332, 0.007642, -0.017137, 0.029583, 0.007021, -0.019790, 0.026845, 0.008651,
    -0.015580, 0.027911, 0.005626, -0.015944, 0.027264, 0.005060, -0.001277, -0.000131, 0.001018, -0.000160, 0.000293, 0.001923,
    -0.003147, 0.000163, 0.000154, -0.000052, 0.001759, 0.002686, 0.003171, 0.002615, 0.003477, 0.004792, 0.002109, 0.003130,
    0.000894, 0.002154, 0.003947, 0.009678, 0.003198, 0.003047, -0.000697, 0.000104, 0.002069, 0.000443, 0.002352, 0.002564,
    -0.032135, 0.019800, 0.007573, -0.046420, 0.010308, 0.010628, 0.000624, -0.007643, -0.000417, -0.000121, -0.002068, 0.001278,
    -0.001494, -0.013220, -0.000667, -0.009613, 0.000023, 0.003523, -0.005609, -0.004075, 0.000062, -0.003597, 0.005506, 0.003363,
    -0.015615, 0.005084, 0.006128, -0.011016, 0.014019, 0.006061, -0.020418, 0.011525, 0.008893, -0.020086, 0.025417, 0.008520,
    -0.024843, 0.012628, 0.011861, -0.027493, 0.033234, 0.011209, -0.031163, 0.019104, 0.013569, -0.006476, -0.016168, -0.000909,
    -0.005740, -0.008346, -0.000883, -0.020034, 0.034248, 0.009768, -0.020558, 0.025712, 0.004173, -0.018997, 0.035387, 0.011401,
    -0.024556, 0.018797, 0.002151, -0.026082, 0.027111, 0.007797, -0.016251, 0.013998, 0.001377, -0.019798, 0.005797, -0.002342,
    -0.017363, 0.033457, 0.010243, -0.026067, 0.028352, 0.010353, -0.026829, 0.028125, 0.009810, -0.022554, 0.032509, 0.009585,
    -0.027053, 0.003064, 0.012613, -0.022974, 0.003413, 0.009923, -0.024163, 0.025930, 0.011327, -0.028536, 0.028409, 0.011656,
    -0.026356, 0.031509, 0.008834, -0.020489, 0.024367, 0.010648, -0.022709, 0.028653, 0.009598, -0.024796, 0.022948, 0.013332,
    -0.050202, -0.001076, 0.016063, -0.052176, 0.002171, 0.013425, -0.038210, 0.002055, 0.015318, -0.043947, 0.010201, 0.014330,
    -0.044879, -0.000537, 0.016823, -0.033974, 0.008907, 0.013998, -0.027986, 0.023822, 0.011890, -0.035898, 0.016405, 0.013049,
    -0.009941, 0.008421, -0.000134, -0.010683, 0.006920, 0.000134, -0.013814, 0.008055, 0.002417, -0.007704, 0.007547, -0.000623,
    -0.005081, 0.008169, 0.002043, -0.011863, 0.009201, 0.002068, -0.008542, 0.010341, 0.005193, -0.003009, 0.018111, 0.011026,
    -0.006183, 0.019444, 0.011999, -0.000643, 0.018523, 0.011903, -0.004948, 0.019552, 0.013101, -0.009006, 0.020325, 0.013762,
    -0.008551, 0.020462, 0.014959, -0.013592, 0.020929, 0.016963, -0.010216, 0.018034, 0.016557, -0.015904, 0.023644, 0.014514,
    -0.018578, 0.019950, 0.016745, -0.020228, 0.021760, 0.013556, 0.000741, 0.004603, 0.001195, 0.001829, 0.003567, 0.002751,
    0.002526, 0.000746, 0.002288, -0.001317, 0.008002, 0.001974, -0.010793, 0.007778, -0.000660, -0.010475, 0.010175, -0.000102,
    -0.007452, -0.002391, -0.000378, -0.008645, 0.007259, -0.000806, -0.008278, -0.001074, -0.001486, -0.008326, 0.009902, -0.000256,
    0.000738, 0.005065, 0.003302, -0.000205, 0.005083, 0.002559, 0.007084, 0.003581, 0.004000, 0.006748, 0.004659, 0.004088,
    0.004725, 0.005396, 0.004135, 0.000294, 0.005998, 0.003239, -0.001169, 0.006243, 0.001260, -0.005549, 0.005848, 0.001182,
    -0.016627, 0.008081, 0.006863, -0.020297, 0.002899, 0.006538, -0.026655, 0.005054, 0.013915, -0.017542, 0.007922, 0.007976,
    -0.019528, 0.025307, 0.002721, -0.013828, -0.001297, -0.002839, -0.012303, -0.002580, -0.000144, -0.007110, 0.014905, 0.014623,
    -0.015098, 0.016861, 0.014892, -0.039989, 0.004854, 0.014938, -0.039166, 0.007438, 0.015403, 0.008734, 0.004306, 0.003256,
    0.007431, 0.004427, 0.003281, 0.006858, 0.005483, 0.003760, -0.012484, 0.020730, 0.016012, -0.011820, 0.020919, 0.016333,
    -0.012702, 0.022435, 0.016763, -0.011996, 0.021729, 0.016234, -0.012982, 0.025684, -0.003692, -0.013025, 0.023506, 0.015311,
    0.004184, 0.002072, 0.003188, 0.003112, 0.001360, 0.002311, -0.017533, 0.025453, 0.011158, -0.010489, 0.022120, 0.015401,
    -0.011394, 0.024415, 0.014612, -0.011677, 0.021892, 0.015554, -0.005296, 0.012024, 0.012167, -0.000813, 0.009440, 0.009090,
    -0.011153, 0.013764, 0.010194, -0.040957, 0.002183, 0.015982, -0.001232, 0.002201, 0.001710, -0.001454, 0.001885, 0.004453,
    0.005185, 0.006187, 0.003632, -0.008368, -0.001591, 0.002113, -0.014126, 0.000999, 0.004429, -0.032197, 0.003184, 0.016128,
    -0.007714, -0.008606, -0.001985, -0.010701, -0.014856, -0.001454, -0.010292, -0.006336, -0.002711, -0.007900, 0.012999, -0.000424,
    -0.009819, 0.013569, -0.000352, 0.002682, 0.003680, 0.003224, -0.009161, 0.001928, -0.001228, -0.025963, 0.006460, 0.012493,
    0.000295, 0.000250, 0.002013, -0.006023, 0.008141, 0.001019, -0.030460, 0.014383, 0.015233, -0.034689, 0.011198, 0.014909,
    -0.029974, 0.018634, 0.015899, -0.022502, 0.009865, 0.011504, -0.019329, 0.013947, 0.013104, -0.027199, 0.007634, 0.012168,
    -0.029479, 0.023411, 0.013999, -0.021179, 0.035157, 0.005421, -0.023604, 0.019107, 0.015852, -0.018789, 0.032124, 0.006646,
    -0.016189, 0.031887, 0.002947, -0.013541, 0.010669, 0.005961, -0.005568, 0.012939, -0.002676, -0.015629, 0.028670, 0.004336,
    -0.008325, 0.011697, -0.005114, -0.004910, 0.002111, 0.000042, -0.012100, 0.002467, -0.000104, -0.013298, 0.002503, -0.003622,
    -0.008819, 0.002100, 0.000567, -0.021474, 0.002853, -0.003935, -0.004806, 0.000371, -0.000606, -0.009563, 0.001164, -0.002425,
    -0.004642, 0.002319, 0.000944, -0.004576, 0.021859, -0.022573, -0.001213, 0.014200, -0.023663, -0.033707, -0.005523, -0.016145,
    -0.032787, -0.000252, -0.017405, -0.030501, -0.010860, -0.013274, -0.024059, 0.002306, -0.015208, -0.027029, -0.002089, -0.014837,
    -0.028848, 0.007015, -0.018329, -0.018771, 0.007742, -0.016640, -0.021179, 0.015358, -0.018209, -0.014420, 0.013930, -0.017408,
    -0.011808, 0.026716, -0.017320, -0.011406, 0.015107, -0.016218, -0.005558, 0.034545, -0.015769, -0.004461, 0.021282, -0.015375,
    -0.024479, -0.013959, -0.012316, -0.025529, -0.006376, -0.012317, -0.010593, 0.035480, -0.010670, -0.010371, 0.027213, -0.012218,
    -0.011878, 0.036445, -0.012041, -0.003093, 0.019029, -0.013265, -0.002425, 0.027454, -0.012171, -0.015458, 0.016225, -0.012053,
    -0.008167, 0.006299, -0.013324, -0.014157, 0.034398, -0.014941, -0.003549, 0.029140, -0.013845, -0.004949, 0.029031, -0.017237,
    -0.010095, 0.033672, -0.016771, -0.011125, 0.006428, -0.018698, -0.013822, 0.006493, -0.019152, -0.015515, 0.026559, 0.001032,
    -0.014729, 0.029382, 0.000561, -0.015745, 0.025160, 0.002715, -0.015237, 0.024884, -0.000183, -0.003738, 0.004868, -0.021920,
    -0.000784, 0.008008, -0.023158, -0.005318, 0.006880, -0.020970, -0.000009, 0.014899, -0.019277, -0.008514, 0.004814, -0.021969,
    -0.006016, 0.012371, -0.017896, -0.006535, 0.025314, -0.016950, -0.003174, 0.019258, -0.018752, -0.017876, 0.009195, -0.014949,
    -0.018231, 0.008065, -0.013673, -0.017995, 0.009913, -0.015763, -0.017846, 0.007687, -0.012712, -0.017100, 0.007278, -0.009904,
    -0.019746, 0.010855, -0.016067, -0.017545, 0.010316, -0.008882, -0.007673, 0.018621, 0.004977, -0.009817, 0.019991, 0.004521,
    -0.006924, 0.019059, 0.005026, -0.009251, 0.020430, 0.004613, -0.010871, 0.021175, 0.005431, -0.010347, 0.021634, 0.005472,
    -0.015052, 0.023412, 0.006566, -0.016723, 0.019627, 0.003340, -0.014489, 0.025856, 0.006445, -0.016058, 0.022148, 0.004007,
    -0.015784, 0.024062, 0.003899, -0.002008, 0.004512, -0.001343, -0.006611, 0.003262, -0.002892, -0.006703, 0.000544, -0.002248,
    -0.007624, 0.007797, -0.003807, -0.015852, 0.008652, -0.011321, -0.014073, 0.010828, -0.008775, -0.022014, -0.001025, -0.011483,
    -0.014496, 0.007447, -0.010962, -0.019082, 0.000223, -0.009124, -0.012429, 0.010143, -0.008576, -0.017013, 0.004187, -0.007000,
    -0.012549, 0.004862, -0.005673, -0.019558, 0.002783, -0.004420, -0.019650, 0.003504, -0.004103, -0.018941, 0.003913, -0.005458,
    -0.017993, 0.004904, -0.007299, -0.018704, 0.004881, -0.008977, -0.014698, 0.005819, -0.008289, -0.019582, 0.011112, -0.017939,
    -0.014599, 0.006073, -0.018145, -0.019917, 0.008878, -0.020464, -0.024285, 0.011341, -0.017893, -0.008541, 0.025932, -0.021709,
    -0.014286, -0.000248, -0.011007, -0.018544, -0.000909, -0.011610, -0.017436, 0.016104, 0.001607, -0.017836, 0.018650, -0.001147,
    -0.014731, 0.009127, -0.020883, -0.015539, 0.011702, -0.018210, -0.021288, 0.003291, -0.003832, -0.018769, 0.003168, -0.002422,
    -0.018673, 0.003785, -0.002999, -0.010721, 0.022229, 0.006904, -0.011216, 0.022312, 0.006711, -0.007229, 0.024843, 0.008451,
    -0.007881, 0.024233, 0.008225, -0.007728, 0.026128, -0.020615, -0.004515, 0.026530, 0.008568, -0.012194, 0.002379, -0.004150,
    -0.015036, 0.026892, 0.005155, -0.003522, 0.024239, 0.010149, -0.003148, 0.026153, 0.009178, -0.006040, 0.024795, 0.009038,
    -0.015966, 0.012813, 0.000827, -0.014955, 0.009567, 0.000156, -0.018316, 0.014470, -0.005438, -0.012669, 0.006305, -0.021825,
    -0.006224, 0.002077, 0.001044, -0.018958, 0.004032, -0.003662, -0.023019, -0.000025, -0.012453, -0.019456, 0.003541, -0.014408,
    -0.012164, 0.007441, -0.020695, -0.022067, -0.006921, -0.011459, -0.019751, -0.012758, -0.011475, -0.017881, -0.004888, -0.009197,
    -0.009643, 0.013267, -0.006328, -0.010989, 0.014010, -0.006435, -0.011900, 0.003518, -0.004810, -0.016756, 0.002949, -0.007328,
    -0.023484, 0.010873, -0.019994, -0.004283, 0.000177, -0.000045, -0.011733, 0.008432, -0.006949, -0.017163, 0.017150, -0.009669,
    -0.016523, 0.014903, -0.015250, -0.016380, 0.021311, -0.002992, -0.023943, 0.013468, -0.014435, -0.021453, 0.016560, -0.007241,
    -0.024186, 0.011829, -0.017889, -0.014799, 0.025488, -0.000887, -0.017981, 0.021407, -0.000291, -0.022223, 0.012497, -0.013980,
    0.031570, 0.048359, -0.058647, 0.031570, 0.048359, -0.058647, 0.031570, 0.048359, -0.058647, 0.031570, 0.048359, -0.058647,
    0.031570, 0.048359, -0.058647, 0.031570, 0.048359, -0.058647, 0.031570, 0.048359, -0.058647, 0.031570, 0.048359, -0.058647,
    0.031570, 0.048359, -0.058647, -0.071733, 0.049436, -0.023771, -0.071733, 0.049436, -0.023771, -0.071733, 0.049436, -0.023771,
    -0.071733, 0.049436, -0.023771, -0.071733, 0.049436, -0.023771, -0.071733, 0.049436, -0.023771, -0.071733, 0.049436, -0.023771,
    -0.071733, 0.049436, -0.023771, -0.071733, 0.049436, -0.023771
  ]),
  // 7 — brow_up
  new Float32Array([
    -0.012972, -0.004968, 0.053990, -0.012408, -0.004402, 0.058359, -0.016096, -0.002234, 0.049725, 0.000435, -0.001044, -0.008133,
    -0.003499, -0.002929, -0.013133, 0.002507, 0.005158, 0.036963, -0.028702, 0.006567, 0.016582, -0.007389, -0.004814, -0.014982,
    -0.013697, -0.011368, -0.025008, -0.016756, -0.046035, -0.027637, -0.010711, -0.005923, -0.018591, -0.047429, -0.008436, 0.056275,
    -0.044704, -0.024985, 0.054682, -0.045910, 0.003533, 0.054744, -0.020321, -0.017342, -0.064564, -0.014502, -0.022111, -0.045051,
    -0.033304, -0.017366, -0.065443, -0.018209, -0.013068, 0.065843, -0.018687, -0.012858, 0.071941, -0.022094, -0.011156, 0.063992,
    -0.018698, -0.010297, 0.079482, -0.019394, -0.008811, 0.085144, -0.022786, -0.008862, 0.076484, -0.021376, -0.018545, 0.086220,
    -0.018324, -0.016392, 0.090011, -0.028666, -0.016999, 0.081539, -0.027090, -0.040211, 0.069811, -0.021705, -0.042077, 0.036203,
    -0.032606, -0.038748, 0.104455, -0.043386, -0.032571, 0.050194, -0.015215, -0.004729, 0.068852, -0.018964, -0.004691, 0.082109,
    -0.023790, -0.001555, 0.095210, -0.017398, -0.121056, 0.016299, -0.024716, -0.146047, -0.017167, -0.039446, -0.163832, -0.026162,
    -0.052111, -0.175226, -0.029596, -0.019524, -0.040994, 0.003598, -0.043876, -0.157677, -0.034430, -0.029616, -0.043021, 0.190665,
    -0.030385, -0.044052, 0.183644, -0.029441, -0.040207, 0.185008, -0.025301, 0.003121, 0.110553, -0.026408, 0.000437, 0.107522,
    -0.021662, -0.150117, -0.007370, -0.017306, -0.121416, 0.025657, -0.038238, -0.165624, -0.019482, -0.046738, -0.179602, -0.035836,
    -0.036756, -0.174057, -0.047582, -0.015768, -0.033742, -0.023363, -0.027751, 0.001582, 0.106292, -0.026888, -0.000332, 0.103674,
    -0.026335, -0.002899, 0.097584, -0.023178, -0.006807, 0.087654, -0.021048, -0.010431, 0.079351, -0.020194, -0.009687, 0.100738,
    -0.022295, -0.006174, 0.091560, -0.024832, -0.002881, 0.097728, -0.024520, -0.003493, 0.105343, -0.030853, -0.045206, 0.174014,
    -0.032495, -0.045291, 0.159753, -0.033966, -0.042020, 0.137735, -0.036140, -0.001149, 0.039121, -0.036402, 0.004640, 0.037976,
    -0.034347, -0.007283, 0.037232, -0.029102, -0.038379, 0.180206, -0.028355, -0.040514, 0.169452, -0.041325, 0.012899, 0.053222,
    -0.035694, 0.018610, 0.048856, -0.032423, 0.018539, 0.044016, -0.034838, -0.042125, 0.142288, -0.039978, -0.041385, 0.134100,
    -0.031568, -0.043215, 0.150170, -0.029536, -0.042964, 0.159851, -0.048015, -0.043226, 0.127899, -0.033749, 0.012819, 0.040756,
    -0.030538, -0.020019, 0.039577, -0.028237, -0.032308, 0.032921, -0.030754, -0.039265, 0.035086, -0.036125, -0.041734, 0.039473,
    -0.041037, -0.037783, 0.047648, -0.037749, -0.008443, 0.005932, -0.043929, -0.024985, 0.001964, -0.035184, 0.004067, 0.006630,
    -0.091193, -0.029851, -0.010750, -0.077198, -0.031634, -0.036101, -0.100375, -0.029381, 0.020261, -0.042644, -0.032171, -0.000329,
    -0.024045, -0.001042, 0.051176, -0.031720, -0.000585, 0.058405, -0.037854, 0.001098, 0.066610, -0.066201, -0.112840, -0.044483,
    -0.048850, -0.136571, -0.068471, -0.025442, -0.154451, -0.065962, -0.002311, -0.168303, -0.057347, -0.059919, -0.031320, -0.055458,
    0.002239, -0.155014, -0.046878, -0.113845, -0.039719, 0.115476, -0.120460, -0.039550, 0.102939, -0.100359, -0.038510, 0.116906,
    -0.044295, 0.004001, 0.079958, -0.040401, 0.000480, 0.081503, -0.054058, -0.139254, -0.063004, -0.072100, -0.111465, -0.037556,
    -0.025150, -0.155740, -0.062659, -0.004083, -0.171910, -0.064791, -0.003435, -0.169340, -0.061308, -0.044299, -0.025674, -0.064128,
    -0.041597, 0.001783, 0.080218, -0.037455, -0.000550, 0.080604, -0.037868, -0.002177, 0.074128, -0.032976, -0.005305, 0.068668,
    -0.026642, -0.008386, 0.065223, -0.034385, -0.011209, 0.084973, -0.026810, -0.006341, 0.076821, -0.032657, -0.003438, 0.078921,
    -0.037091, -0.004662, 0.084963, -0.121191, -0.038904, 0.090300, -0.117623, -0.037451, 0.073866, -0.111205, -0.033576, 0.052151,
    -0.028457, 0.002892, 0.014893, -0.029453, 0.009518, 0.011778, -0.031546, -0.004081, 0.010380, -0.088392, -0.038678, 0.123226,
    -0.077331, -0.042629, 0.122950, -0.035282, 0.014807, 0.008900, -0.036645, 0.022447, 0.009478, -0.036961, 0.024075, 0.010058,
    -0.057562, -0.045476, 0.123335, -0.068228, -0.045649, 0.122729, -0.033726, 0.018335, 0.011431, -0.038704, -0.017939, 0.009398,
    -0.043415, -0.030499, -0.001759, -0.042859, -0.037625, -0.005358, -0.041793, -0.040100, -0.006251, -0.041473, -0.036915, -0.000879,
    0.000000, 0.000000, -0.000000, 0.000484, 0.000241, 0.002624, -0.003102, 0.001123, -0.001830, -0.007899, 0.006848, 0.033916,
    -0.005224, 0.004849, 0.024041, -0.011079, 0.003540, 0.044534, -0.010147, 0.003469, 0.049850, -0.006252, 0.005470, 0.021754,
    -0.017240, 0.004883, 0.041006, -0.006044, -0.005354, -0.005583, -0.001824, -0.003016, -0.006923, -0.009246, -0.007099, -0.011388,
    -0.006034, -0.004607, 0.010111, -0.007420, -0.002518, -0.012660, -0.011451, -0.012852, -0.014927, -0.020083, -0.011778, -0.022184,
    -0.015944, -0.006612, -0.017642, -0.016934, -0.119850, -0.046981, -0.017249, -0.140267, -0.059054, -0.019252, -0.052423, -0.004461,
    -0.021131, -0.051835, -0.013157, -0.024641, -0.059341, -0.046225, -0.018024, -0.049339, -0.058556, -0.018284, -0.055194, -0.063688,
    -0.013876, -0.007433, 0.056284, -0.016094, -0.010635, 0.059934, -0.014328, -0.007574, 0.061547, -0.017996, -0.005385, 0.053021,
    -0.016633, -0.010967, 0.065771, -0.020464, -0.009152, 0.058002, -0.018362, -0.012026, 0.080370, -0.019275, -0.014527, 0.080607,
    -0.018168, -0.012940, 0.085300, -0.018665, -0.010339, 0.085613, -0.025224, -0.013424, 0.076082, -0.023092, -0.010405, 0.076725,
    -0.026343, -0.024591, 0.104781, -0.022411, -0.023706, 0.107982, -0.027561, -0.027211, 0.108326, -0.022176, -0.026080, 0.114845,
    -0.033554, -0.024502, 0.100681, -0.035737, -0.027391, 0.105479, -0.003537, 0.003383, 0.006141, -0.003913, 0.002993, 0.009071,
    -0.004307, 0.003565, 0.005437, -0.006364, 0.005053, 0.024040, -0.007932, 0.006722, 0.033040, -0.001808, 0.003543, 0.022872,
    -0.008555, 0.007012, 0.034187, -0.001187, 0.004787, 0.034486, -0.005079, 0.004011, 0.013419, -0.005763, 0.005276, 0.023883,
    -0.027557, -0.081217, 0.013908, -0.022598, -0.071803, 0.049432, -0.031342, 0.012157, 0.051301, -0.033101, 0.011346, 0.050125,
    -0.033524, 0.006449, 0.038174, -0.026565, 0.000961, 0.048079, -0.028282, 0.003329, 0.052579, -0.033699, 0.004131, 0.050557,
    -0.023428, -0.005895, 0.055851, -0.037235, -0.005043, 0.053770, -0.023695, -0.013986, 0.065130, -0.037337, -0.017899, 0.057460,
    -0.019817, -0.020961, 0.062860, -0.036549, -0.034548, 0.055521, -0.024830, -0.040596, 0.057205, -0.036166, 0.000515, 0.036524,
    -0.030705, -0.000095, 0.034586, -0.026486, -0.084726, 0.028605, -0.025496, -0.075436, 0.026080, -0.030666, -0.082644, 0.030640,
    -0.037809, -0.112886, 0.004341, -0.032194, -0.113063, 0.001803, -0.029133, -0.054877, 0.032158, -0.040005, -0.089739, 0.016124,
    -0.035118, -0.069803, 0.038817, -0.026187, -0.104225, 0.008440, -0.023980, -0.089611, 0.021555, -0.034187, -0.055252, 0.048781,
    -0.010975, -0.022783, 0.076700, -0.013477, -0.015419, 0.075678, -0.021991, -0.031635, 0.130934, -0.024887, -0.037519, 0.139740,
    -0.028144, -0.036181, 0.129347, -0.022365, -0.023700, 0.122902, -0.023008, -0.031506, 0.119031, -0.021625, -0.027915, 0.137709,
    -0.018659, -0.039626, 0.105356, -0.021865, -0.051648, 0.078724, -0.012001, -0.047951, 0.078204, -0.016258, -0.081693, 0.056184,
    -0.014377, -0.036426, 0.124649, -0.013883, -0.047328, 0.065487, -0.024737, -0.065249, 0.045378, -0.016773, -0.081735, 0.047297,
    -0.010467, -0.005953, 0.066732, -0.016103, -0.004761, 0.044974, -0.013302, -0.008781, 0.050610, -0.011457, -0.002380, 0.056187,
    -0.009223, 0.002183, 0.070772, -0.005736, -0.010435, 0.081939, -0.008869, -0.004002, 0.082419, -0.019383, -0.008924, 0.073872,
    -0.022062, -0.006449, 0.085568, -0.016871, -0.006851, 0.071550, -0.020347, -0.005042, 0.083104, -0.025556, -0.002238, 0.094589,
    -0.024587, -0.001634, 0.094832, -0.020338, -0.008893, 0.112693, -0.014984, -0.005463, 0.111477, -0.022826, -0.011893, 0.113239,
    -0.018516, -0.015553, 0.121982, -0.022687, -0.019860, 0.124361, 0.001368, -0.000928, -0.004909, 0.000974, 0.000118, 0.002904,
    -0.000271, 0.001953, 0.009151, -0.001724, -0.002211, 0.006313, -0.016774, -0.004739, 0.033739, -0.017105, -0.006313, 0.020977,
    -0.025764, -0.003430, 0.032705, -0.010888, -0.002250, 0.043421, -0.024998, -0.006883, 0.022975, -0.011412, -0.003826, 0.026568,
    0.000600, 0.004752, 0.039484, -0.000463, 0.000914, 0.023800, 0.001449, 0.005108, 0.035408, -0.000923, 0.006131, 0.041467,
    0.000527, 0.006441, 0.045053, -0.000804, 0.005394, 0.048488, -0.005578, 0.006814, 0.064407, -0.005689, 0.001139, 0.041638,
    -0.006347, -0.014032, 0.076751, -0.017143, -0.010919, 0.069615, -0.003588, -0.023713, 0.105504, -0.002538, -0.018973, 0.104207,
    -0.034751, -0.084989, -0.009110, -0.031840, -0.039588, 0.021016, -0.033865, -0.025413, 0.027754, -0.010055, -0.004382, 0.095564,
    -0.013726, -0.012681, 0.117893, -0.014040, -0.034862, 0.154273, -0.015023, -0.033870, 0.164865, -0.003337, 0.005068, 0.045640,
    -0.005300, 0.006284, 0.037897, -0.004609, 0.007142, 0.045270, -0.026905, 0.002213, 0.108443, -0.025980, 0.002673, 0.108174,
    -0.026304, 0.000614, 0.105094, -0.026215, 0.000184, 0.104067, -0.034503, -0.079971, -0.030952, -0.024525, -0.002476, 0.102058,
    0.000507, 0.002568, 0.020317, -0.003298, 0.003566, 0.015313, -0.022469, -0.016992, 0.110623, -0.021537, -0.006844, 0.091653,
    -0.020742, -0.007588, 0.096628, -0.024424, -0.002617, 0.097888, -0.009530, -0.001884, 0.079663, -0.008600, 0.002621, 0.066030,
    -0.006991, -0.008308, 0.094908, -0.013438, -0.035025, 0.139805, -0.005390, 0.005044, 0.023176, -0.009702, 0.006575, 0.040782,
    -0.006483, 0.007433, 0.055445, -0.024974, -0.002191, 0.045744, -0.021855, -0.005716, 0.051464, -0.008113, -0.027491, 0.091392,
    -0.030964, -0.004341, 0.032974, -0.034795, -0.006561, 0.033852, -0.028733, -0.012477, 0.023584, -0.011003, -0.006402, 0.007908,
    -0.014768, -0.009491, 0.004004, 0.001006, 0.001485, 0.022386, -0.022277, -0.016395, 0.007255, -0.002601, -0.025134, 0.123354,
    -0.004471, 0.003607, 0.010589, -0.006271, -0.002086, 0.026244, -0.017985, -0.029113, 0.164944, -0.015382, -0.031069, 0.167518,
    -0.020656, -0.031821, 0.156458, -0.007548, -0.022688, 0.139074, -0.013690, -0.017931, 0.132265, -0.005093, -0.026575, 0.139519,
    -0.023149, -0.035518, 0.148839, -0.035022, -0.036293, 0.122460, -0.018216, -0.022791, 0.138346, -0.030198, -0.031814, 0.110995,
    -0.043771, -0.037321, 0.115813, -0.005194, -0.013685, 0.106903, -0.012005, -0.004947, -0.012524, -0.039641, -0.032775, 0.106591,
    -0.018162, -0.003802, -0.002051, -0.008080, 0.006532, 0.018570, -0.015877, 0.007961, 0.024993, -0.014303, 0.006590, 0.008757,
    -0.013896, 0.008035, 0.026529, -0.021268, 0.007154, 0.019940, -0.006844, 0.005558, 0.007856, -0.009250, 0.005901, 0.005375,
    -0.007100, 0.006117, 0.020221, -0.057640, -0.070604, -0.048980, -0.079227, -0.061160, -0.021938, -0.037189, 0.016902, 0.018266,
    -0.039098, 0.014300, 0.010759, -0.031482, 0.011393, 0.012319, -0.043770, 0.003821, 0.009114, -0.037589, 0.006872, 0.021903,
    -0.042769, 0.005633, 0.004908, -0.052550, -0.003800, 0.008354, -0.045245, -0.004061, 0.002520, -0.059450, -0.012002, 0.010615,
    -0.049783, -0.016638, 0.003769, -0.068279, -0.018300, 0.004863, -0.053508, -0.032679, 0.000289, -0.067490, -0.037372, -0.002258,
    -0.026667, 0.004840, 0.013239, -0.031067, 0.003393, 0.011430, -0.045254, -0.078004, -0.012569, -0.041571, -0.070054, -0.006375,
    -0.046588, -0.075753, -0.017180, -0.024303, -0.107362, -0.026053, -0.035738, -0.105768, -0.037428, -0.035331, -0.050803, 0.005437,
    -0.016405, -0.086373, -0.005588, -0.047156, -0.064046, -0.012709, -0.047522, -0.096504, -0.039790, -0.056895, -0.083016, -0.032818,
    -0.052487, -0.051320, -0.005550, -0.081797, -0.019414, 0.011326, -0.071317, -0.012567, 0.015055, -0.048016, -0.033343, 0.107638,
    -0.053694, -0.039870, 0.114510, -0.041915, -0.025098, 0.103045, -0.054494, -0.029761, 0.106769, -0.102798, -0.032051, 0.025707,
    -0.090933, -0.042698, 0.002181, -0.092886, -0.041717, 0.008992, -0.083371, -0.074116, -0.013285, -0.107171, -0.030205, 0.043071,
    -0.082944, -0.042765, -0.000347, -0.063095, -0.060516, -0.013214, -0.074690, -0.075577, -0.018040, -0.042728, -0.003567, 0.034384,
    -0.037852, -0.002280, 0.015863, -0.046804, -0.006112, 0.011962, -0.036772, -0.000452, 0.030605, -0.039454, 0.003284, 0.044193,
    -0.053828, -0.007988, 0.041063, -0.046899, -0.002683, 0.049842, -0.025398, -0.006211, 0.058631, -0.032114, -0.003854, 0.065611,
    -0.024593, -0.003410, 0.054931, -0.031896, -0.001768, 0.061422, -0.037741, -0.000966, 0.069713, -0.037510, 0.000519, 0.067815,
    -0.047516, -0.008587, 0.083296, -0.052827, -0.004980, 0.076518, -0.042098, -0.012573, 0.089225, -0.054040, -0.015451, 0.088234,
    -0.046809, -0.020736, 0.097580, -0.004358, -0.000246, -0.010197, -0.010827, 0.001587, -0.010138, -0.007633, 0.004010, -0.000366,
    -0.015094, -0.001346, -0.005874, -0.031839, -0.002774, 0.011577, -0.027567, -0.004733, 0.003050, -0.031253, -0.001574, 0.009758,
    -0.032100, -0.000729, 0.022345, -0.026960, -0.005151, 0.006383, -0.026420, -0.002787, 0.009713, -0.030904, 0.005854, 0.018335,
    -0.023150, 0.002090, 0.006905, -0.025645, 0.006784, 0.018042, -0.027199, 0.007867, 0.024484, -0.031678, 0.007990, 0.024995,
    -0.034577, 0.006465, 0.026709, -0.035589, 0.007635, 0.043676, -0.031705, 0.002150, 0.022220, -0.064392, -0.010964, 0.023105,
    -0.058396, -0.008071, 0.018213, -0.092135, -0.020793, 0.033039, -0.076611, -0.016442, 0.045962, -0.034971, -0.075081, -0.059122,
    -0.021807, -0.037361, 0.004866, -0.026986, -0.021787, 0.005345, -0.049827, -0.003310, 0.062452, -0.057687, -0.012126, 0.076939,
    -0.109267, -0.031341, 0.078051, -0.104578, -0.030727, 0.092431, -0.022168, 0.007119, 0.029309, -0.021006, 0.007752, 0.025601,
    -0.025456, 0.008483, 0.031044, -0.042731, 0.002596, 0.081628, -0.043518, 0.003118, 0.079965, -0.039197, 0.000315, 0.080863,
    -0.037894, -0.000054, 0.080734, -0.019102, -0.071982, -0.065175, -0.035022, -0.003527, 0.082638, -0.015812, 0.005354, 0.005190,
    -0.038061, -0.017883, 0.092694, -0.028039, -0.007594, 0.076599, -0.030726, -0.009048, 0.081066, -0.033583, -0.003390, 0.078939,
    -0.041646, -0.000022, 0.052453, -0.031722, 0.004837, 0.045016, -0.056015, -0.007180, 0.056509, -0.108482, -0.030629, 0.059926,
    -0.012455, 0.007699, 0.033359, -0.028387, 0.008589, 0.039858, -0.037201, -0.000249, 0.015941, -0.046423, -0.003452, 0.012448,
    -0.092047, -0.022977, 0.021105, -0.027020, -0.001120, 0.014134, -0.026620, -0.002909, 0.013752, -0.025604, -0.010040, 0.009884,
    -0.021395, -0.005629, -0.003941, -0.023690, -0.008060, -0.008698, -0.019298, 0.003493, 0.005830, -0.025301, -0.015100, -0.005092,
    -0.094644, -0.022566, 0.053713, -0.005113, 0.004622, 0.006207, -0.025640, -0.001079, 0.010128, -0.080856, -0.029244, 0.111113,
    -0.093775, -0.028825, 0.103635, -0.071969, -0.033464, 0.113349, -0.084170, -0.020824, 0.080274, -0.066999, -0.017573, 0.083673,
    -0.095224, -0.024000, 0.073101, -0.063279, -0.038314, 0.114606, -0.062627, -0.023570, 0.098589, -0.063391, -0.011949, 0.060791,
    -0.026762, 0.019119, 0.047592, -0.026762, 0.019119, 0.047592, -0.026762, 0.019119, 0.047592, -0.026762, 0.019119, 0.047592,
    -0.026762, 0.019119, 0.047592, -0.026762, 0.019119, 0.047592, -0.026762, 0.019119, 0.047592, -0.026762, 0.019119, 0.047592,
    -0.026762, 0.019119, 0.047592, -0.065055, 0.022343, 0.060303, -0.065055, 0.022343, 0.060303, -0.065055, 0.022343, 0.060303,
    -0.065055, 0.022343, 0.060303, -0.065055, 0.022343, 0.060303, -0.065055, 0.022343, 0.060303, -0.065055, 0.022343, 0.060303,
    -0.065055, 0.022343, 0.060303, -0.065055, 0.022343, 0.060303
  ]),
  // 8 — cheek_puff
  new Float32Array([
    -0.022082, -0.025312, -0.028945, -0.020786, -0.026048, -0.025429, -0.021814, -0.020774, -0.029240, 0.001625, 0.006407, 0.006835,
    0.001747, 0.010422, 0.016886, -0.009731, -0.013278, 0.007627, -0.002740, -0.004983, -0.001333, 0.001382, 0.012788, 0.027231,
    0.000122, 0.016204, 0.048016, -0.000900, 0.020868, 0.058764, 0.000787, 0.014476, 0.038333, -0.008548, 0.001343, 0.065651,
    -0.009653, 0.000991, 0.068744, -0.006842, 0.002326, 0.063608, 0.001482, 0.033329, 0.125924, 0.002961, 0.029397, 0.130000,
    -0.001370, 0.035708, 0.119459, -0.025619, -0.038105, -0.033940, -0.027680, -0.041350, -0.031769, -0.023862, -0.037163, -0.035793,
    -0.025684, -0.036151, -0.037919, -0.028188, -0.040383, -0.035898, -0.024012, -0.035797, -0.039267, -0.028770, -0.045606, -0.047618,
    -0.028678, -0.049950, -0.045366, -0.027890, -0.045068, -0.049183, 0.000887, -0.008776, 0.118368, 0.003263, 0.005794, 0.128763,
    -0.003842, -0.021019, 0.103994, -0.009380, 0.002348, 0.069005, -0.019942, -0.036605, -0.025329, -0.021551, -0.050178, -0.026431,
    -0.025594, -0.064001, -0.028327, 0.002303, 0.021364, 0.088085, 0.004885, 0.032677, 0.091277, 0.006794, 0.039717, 0.088839,
    0.007169, 0.040983, 0.083292, 0.004408, 0.017482, 0.132821, 0.003155, 0.034960, 0.074716, -0.030341, -0.067270, -0.012936,
    -0.029147, -0.058505, 0.013435, -0.026757, -0.071355, -0.031680, -0.032959, -0.077421, -0.031508, -0.029916, -0.072089, -0.037587,
    0.006212, 0.034110, 0.098849, 0.002450, 0.018745, 0.094902, 0.008787, 0.043020, 0.096908, 0.008298, 0.045470, 0.090363,
    0.004961, 0.041974, 0.085289, 0.004531, 0.025444, 0.133628, -0.034012, -0.071802, -0.032841, -0.031633, -0.063520, -0.033729,
    -0.032284, -0.062123, -0.031482, -0.030257, -0.054266, -0.030806, -0.028998, -0.046734, -0.030810, -0.028596, -0.056754, -0.043221,
    -0.029677, -0.047294, -0.034536, -0.030586, -0.055782, -0.033811, -0.028224, -0.064949, -0.041134, -0.024578, -0.050462, 0.037837,
    -0.017956, -0.042333, 0.062182, -0.011625, -0.033031, 0.083512, -0.004908, 0.007122, 0.059715, -0.003040, 0.006337, 0.059708,
    -0.004451, 0.006911, 0.062375, -0.021178, -0.071681, -0.047468, -0.017410, -0.070120, -0.058991, -0.004951, 0.003589, 0.061284,
    -0.002509, 0.004906, 0.059466, -0.001730, 0.005876, 0.058878, -0.028439, -0.071017, -0.085867, -0.036471, -0.070807, -0.091031,
    -0.022047, -0.069661, -0.077468, -0.018343, -0.069268, -0.068515, -0.040033, -0.070227, -0.093337, -0.001601, 0.006196, 0.058749,
    -0.003573, 0.006144, 0.064762, -0.004220, 0.006154, 0.067265, -0.005940, 0.006306, 0.068787, -0.007954, 0.005034, 0.069237,
    -0.008930, 0.003594, 0.068956, -0.008839, 0.002516, 0.039686, -0.008999, 0.000991, 0.041415, -0.009492, 0.004448, 0.038762,
    -0.030124, 0.010143, 0.078461, -0.023110, 0.022768, 0.092776, -0.034021, -0.001077, 0.061861, -0.008828, 0.002183, 0.042374,
    -0.028052, -0.025457, -0.032812, -0.032806, -0.034685, -0.036855, -0.035708, -0.044893, -0.041546, -0.017781, 0.024890, 0.056604,
    -0.014911, 0.032920, 0.062527, -0.013285, 0.037744, 0.065742, -0.011250, 0.037392, 0.067406, -0.015270, 0.032102, 0.103267,
    -0.006105, 0.030778, 0.066753, -0.034408, -0.054559, -0.048791, -0.032512, -0.042537, -0.025369, -0.037981, -0.062674, -0.064295,
    -0.034695, -0.055139, -0.046525, -0.035506, -0.054503, -0.049370, -0.017278, 0.035514, 0.068630, -0.020398, 0.024862, 0.062579,
    -0.014434, 0.041791, 0.073056, -0.010126, 0.042397, 0.074710, -0.004646, 0.037931, 0.076871, -0.007077, 0.036226, 0.112753,
    -0.033628, -0.052145, -0.045051, -0.032372, -0.048633, -0.043852, -0.032262, -0.046632, -0.042719, -0.029088, -0.041875, -0.039687,
    -0.025280, -0.038375, -0.037540, -0.029569, -0.048207, -0.049774, -0.025292, -0.038395, -0.040246, -0.028889, -0.043542, -0.041671,
    -0.033120, -0.052040, -0.049713, -0.032548, -0.032277, -0.002787, -0.034491, -0.023015, 0.019928, -0.035105, -0.012871, 0.040583,
    -0.010928, 0.005927, 0.047314, -0.012863, 0.005833, 0.046131, -0.012104, 0.005297, 0.048374, -0.041971, -0.066251, -0.074909,
    -0.045556, -0.068123, -0.081468, -0.011207, 0.006024, 0.038428, -0.013525, 0.006970, 0.039097, -0.014605, 0.006931, 0.041247,
    -0.042805, -0.069619, -0.090544, -0.045111, -0.069155, -0.086151, -0.014584, 0.006382, 0.043571, -0.014266, 0.003930, 0.048720,
    -0.013853, 0.003780, 0.048485, -0.011233, 0.004270, 0.046963, -0.008727, 0.003966, 0.044927, -0.008511, 0.003201, 0.043263,
    0.000000, 0.000000, -0.000000, 0.000188, -0.001558, 0.001725, -0.000691, 0.000528, -0.000274, -0.008295, -0.012894, -0.009168,
    -0.005702, -0.010424, -0.005131, -0.013378, -0.017516, -0.015538, -0.014249, -0.021321, -0.014453, -0.003855, -0.009051, -0.006182,
    -0.013100, -0.016546, -0.018416, 0.000840, 0.010708, 0.028168, 0.001238, 0.008081, 0.018608, 0.000226, 0.012267, 0.039005,
    -0.001928, 0.005450, 0.026052, 0.000851, 0.010101, 0.015251, -0.000951, 0.013933, 0.048577, -0.001549, 0.015130, 0.044583,
    -0.000757, 0.013796, 0.035915, -0.000692, 0.033125, 0.068850, -0.000110, 0.040732, 0.080701, -0.002676, 0.018852, 0.059575,
    -0.003758, 0.017649, 0.055244, 0.005211, 0.033527, 0.108057, 0.001253, 0.034681, 0.103586, -0.003250, 0.037129, 0.099188,
    -0.023452, -0.029757, -0.031934, -0.024717, -0.034526, -0.033727, -0.023139, -0.032707, -0.028858, -0.022547, -0.027645, -0.032801,
    -0.025934, -0.038237, -0.031881, -0.023139, -0.033448, -0.035558, -0.026392, -0.038471, -0.039433, -0.027485, -0.041464, -0.043108,
    -0.028174, -0.045493, -0.041102, -0.027723, -0.042776, -0.037260, -0.026464, -0.040572, -0.045014, -0.025209, -0.037806, -0.040838,
    -0.031263, -0.049567, -0.053945, -0.028928, -0.052441, -0.052018, -0.031861, -0.054225, -0.065553, -0.027630, -0.056050, -0.062676,
    -0.030326, -0.049145, -0.056205, -0.031495, -0.054008, -0.068074, -0.002512, -0.005612, -0.002872, -0.003014, -0.006112, -0.001787,
    -0.001821, -0.004700, -0.003427, -0.007130, -0.010343, -0.002179, -0.009672, -0.015178, -0.002640, -0.003728, -0.007538, 0.005725,
    -0.009402, -0.015934, -0.004613, -0.008111, -0.012279, 0.005440, -0.004575, -0.007329, -0.000270, -0.006430, -0.010604, -0.003608,
    0.007936, 0.018460, 0.111976, 0.003579, 0.003487, 0.105753, -0.002847, 0.002339, 0.054123, -0.003888, 0.000443, 0.055393,
    -0.002892, 0.003675, 0.054001, -0.004784, -0.005812, 0.048052, -0.004423, -0.002019, 0.046734, -0.005079, -0.001706, 0.057750,
    -0.005474, -0.009175, 0.051483, -0.007249, -0.003120, 0.060646, -0.006982, -0.010829, 0.055693, -0.008285, -0.003606, 0.063633,
    -0.007307, -0.010679, 0.061012, -0.007379, -0.000296, 0.069431, -0.006752, -0.004158, 0.069886, -0.003840, 0.005536, 0.055373,
    -0.005029, 0.000915, 0.047941, -0.004112, 0.020355, 0.071250, -0.004215, 0.019660, 0.069886, -0.002496, 0.018098, 0.072074,
    -0.000004, 0.029059, 0.075350, 0.000443, 0.028326, 0.078214, -0.004221, 0.016121, 0.066335, -0.001301, 0.023886, 0.068787,
    -0.001931, 0.012988, 0.071910, 0.000681, 0.023718, 0.079033, -0.000307, 0.016763, 0.079040, -0.004015, 0.007139, 0.071348,
    -0.008059, -0.017819, 0.057634, -0.007690, -0.019180, 0.048547, -0.022525, -0.060807, -0.063582, -0.021498, -0.064570, -0.071964,
    -0.026945, -0.065579, -0.079978, -0.023763, -0.058793, -0.056304, -0.026628, -0.060553, -0.071256, -0.018843, -0.061433, -0.056584,
    -0.009825, -0.023574, 0.076171, -0.002335, -0.011143, 0.093332, -0.008491, -0.014170, 0.072246, -0.002886, 0.000758, 0.085206,
    -0.015228, -0.032606, 0.056983, -0.007226, -0.009068, 0.070338, -0.003169, 0.006330, 0.075593, -0.002395, 0.004961, 0.080524,
    -0.013230, -0.020127, 0.016585, -0.008061, -0.010155, 0.031784, -0.009753, -0.017568, 0.030813, -0.011516, -0.013647, 0.020362,
    -0.016848, -0.026648, 0.003626, -0.017705, -0.029784, 0.009920, -0.019019, -0.034069, -0.003955, -0.027126, -0.045027, -0.030104,
    -0.028417, -0.054036, -0.030422, -0.023707, -0.041543, -0.028368, -0.025034, -0.052347, -0.028984, -0.030596, -0.063766, -0.030803,
    -0.028075, -0.064484, -0.029875, -0.022622, -0.066310, -0.039161, -0.023698, -0.064489, -0.029609, -0.023524, -0.061002, -0.045130,
    -0.018374, -0.063071, -0.040427, -0.020194, -0.061771, -0.050034, 0.001521, 0.004397, 0.008379, -0.000772, -0.000042, 0.010234,
    -0.000894, -0.003380, 0.004787, -0.001931, 0.002927, 0.017239, -0.007032, -0.003801, 0.034923, -0.005514, 0.002278, 0.038517,
    -0.006329, -0.001134, 0.041882, -0.009016, -0.006339, 0.024996, -0.006090, 0.004227, 0.045298, -0.005971, 0.000692, 0.029693,
    -0.010524, -0.011977, 0.011331, -0.006536, -0.004598, 0.014393, -0.009125, -0.013414, 0.004665, -0.011557, -0.017484, 0.003064,
    -0.013193, -0.018969, 0.004294, -0.013093, -0.016926, 0.008317, -0.014788, -0.020124, 0.007799, -0.010123, -0.008900, 0.017977,
    -0.015535, -0.028985, 0.025970, -0.006920, -0.017420, 0.042165, -0.017573, -0.034911, 0.035425, -0.023214, -0.041638, 0.008497,
    0.009704, 0.029347, 0.113285, -0.003380, 0.015274, 0.061138, -0.004613, 0.011645, 0.063287, -0.022671, -0.052339, -0.021815,
    -0.019923, -0.060287, -0.027218, -0.027462, -0.050500, 0.015474, -0.027700, -0.058444, -0.005570, -0.009789, -0.015040, 0.003109,
    -0.010968, -0.016630, 0.001240, -0.013729, -0.020234, 0.000001, -0.033773, -0.073980, -0.032721, -0.033331, -0.075865, -0.032067,
    -0.030827, -0.069217, -0.036723, -0.031547, -0.066558, -0.035353, 0.009212, 0.034739, 0.112000, -0.029254, -0.061737, -0.038862,
    -0.003833, -0.006596, 0.007442, -0.003064, -0.006010, 0.004089, -0.026175, -0.056571, -0.049322, -0.029062, -0.049965, -0.035892,
    -0.028933, -0.052997, -0.039751, -0.029829, -0.058760, -0.035423, -0.019829, -0.040164, -0.018051, -0.017774, -0.031451, -0.013914,
    -0.021998, -0.047260, -0.011466, -0.021677, -0.041390, 0.037106, -0.004923, -0.009861, -0.005986, -0.009160, -0.016550, -0.007744,
    -0.016388, -0.023508, -0.001617, -0.006211, -0.006057, 0.039940, -0.006516, -0.011736, 0.039927, -0.011572, -0.025404, 0.054191,
    -0.005461, 0.004117, 0.048799, -0.005044, 0.008164, 0.057292, -0.005216, 0.008612, 0.052986, -0.002407, 0.007304, 0.034738,
    -0.003049, 0.008767, 0.044080, -0.004739, -0.005674, 0.009645, -0.005059, 0.010000, 0.050675, -0.024098, -0.043787, 0.015661,
    -0.003563, -0.006729, -0.001385, -0.006532, -0.001860, 0.023155, -0.020264, -0.065275, -0.038336, -0.025207, -0.063625, -0.024383,
    -0.016442, -0.065582, -0.052184, -0.025247, -0.055472, -0.012290, -0.020590, -0.058748, -0.024620, -0.026210, -0.051776, -0.000466,
    -0.017650, -0.064340, -0.063378, -0.034420, -0.064604, -0.085260, -0.016864, -0.062479, -0.044512, -0.032586, -0.059103, -0.076262,
    -0.036208, -0.064320, -0.087504, -0.022225, -0.041936, -0.001671, 0.000048, 0.012260, 0.024863, -0.032741, -0.058838, -0.078542,
    -0.001135, 0.008775, 0.020405, -0.001690, -0.007441, -0.004715, -0.005590, -0.010068, -0.006375, -0.002206, -0.002444, -0.000525,
    -0.006804, -0.011294, -0.008172, -0.003949, -0.005703, -0.001006, -0.001029, -0.004766, -0.002771, -0.001098, -0.002127, -0.000378,
    -0.002886, -0.008525, -0.005401, -0.021688, 0.029751, 0.079842, -0.025451, 0.017502, 0.070494, -0.014629, 0.004460, 0.037176,
    -0.013987, 0.003833, 0.035169, -0.013395, 0.004628, 0.040805, -0.013941, 0.000629, 0.027661, -0.013227, 0.001734, 0.030910,
    -0.012971, 0.003296, 0.034467, -0.013646, -0.000333, 0.026723, -0.011461, 0.001389, 0.034579, -0.013295, -0.002061, 0.027740,
    -0.011308, -0.000512, 0.036039, -0.014324, -0.001682, 0.031214, -0.012800, 0.001405, 0.040743, -0.015489, 0.001810, 0.039349,
    -0.011194, 0.005290, 0.043620, -0.010965, 0.003007, 0.036221, -0.011239, 0.018993, 0.049040, -0.011654, 0.017883, 0.052692,
    -0.012073, 0.017893, 0.046696, -0.011563, 0.025877, 0.059055, -0.011577, 0.026979, 0.056317, -0.011120, 0.014706, 0.052742,
    -0.010127, 0.020553, 0.058298, -0.013443, 0.014015, 0.044879, -0.011878, 0.024548, 0.052854, -0.014210, 0.019008, 0.050517,
    -0.014186, 0.008616, 0.043090, -0.015596, -0.004377, 0.024727, -0.013534, -0.005456, 0.018007, -0.034632, -0.057922, -0.075051,
    -0.039274, -0.063498, -0.084646, -0.032100, -0.054524, -0.066225, -0.036736, -0.056486, -0.071963, -0.026431, -0.005100, 0.037026,
    -0.027279, 0.005846, 0.055674, -0.020770, -0.001009, 0.036862, -0.021467, 0.011496, 0.050946, -0.024216, -0.013641, 0.017023,
    -0.018251, 0.001539, 0.037287, -0.016006, 0.010303, 0.045612, -0.018506, 0.012557, 0.048173, -0.009344, -0.009340, -0.000843,
    -0.009514, -0.002434, 0.016530, -0.010735, -0.006100, 0.009445, -0.007443, -0.005280, 0.007491, -0.008888, -0.016068, -0.009266,
    -0.009828, -0.015994, -0.013336, -0.012403, -0.021762, -0.020241, -0.025278, -0.035654, -0.037280, -0.029140, -0.040565, -0.039797,
    -0.026147, -0.031158, -0.035760, -0.030537, -0.037765, -0.038788, -0.032625, -0.046907, -0.042602, -0.034032, -0.046204, -0.042492,
    -0.034526, -0.052081, -0.053673, -0.030252, -0.046666, -0.047877, -0.032403, -0.051857, -0.056559, -0.033411, -0.051018, -0.058017,
    -0.034206, -0.054058, -0.063824, 0.000401, 0.006511, 0.005943, -0.000174, 0.004361, 0.004250, -0.000905, 0.000492, 0.000457,
    -0.000209, 0.006930, 0.011635, -0.007142, 0.001489, 0.023885, -0.005488, 0.005905, 0.029611, -0.008663, 0.002434, 0.030243,
    -0.005089, -0.000354, 0.014956, -0.006248, 0.006040, 0.036339, -0.003416, 0.005030, 0.021768, -0.002387, -0.003874, 0.001944,
    -0.000552, 0.001618, 0.006845, -0.003377, -0.005935, -0.002988, -0.004275, -0.009493, -0.004424, -0.003934, -0.009994, -0.004536,
    -0.004022, -0.008110, -0.001368, -0.005293, -0.011190, -0.001757, -0.003255, -0.002162, 0.009189, -0.010434, -0.013224, -0.003251,
    -0.012483, -0.004497, 0.015695, -0.014032, -0.016779, -0.001303, -0.011795, -0.024947, -0.023245, -0.016287, 0.037342, 0.087469,
    -0.008584, 0.013497, 0.053710, -0.009644, 0.010181, 0.052201, -0.023207, -0.036141, -0.037408, -0.028565, -0.044893, -0.049503,
    -0.022096, -0.033733, -0.021591, -0.025812, -0.044014, -0.040893, -0.004692, -0.008130, -0.004177, -0.005167, -0.009946, -0.004325,
    -0.004782, -0.012400, -0.006297, -0.033960, -0.053329, -0.045379, -0.034342, -0.054265, -0.045749, -0.033783, -0.052192, -0.047344,
    -0.032582, -0.050376, -0.045497, -0.009866, 0.040002, 0.094588, -0.031307, -0.048645, -0.046890, -0.001677, -0.001022, 0.000701,
    -0.031054, -0.050607, -0.057266, -0.026574, -0.040798, -0.041781, -0.027635, -0.043929, -0.045933, -0.029907, -0.045872, -0.043268,
    -0.018389, -0.026837, -0.029807, -0.014446, -0.021221, -0.022978, -0.017430, -0.032205, -0.031809, -0.022061, -0.023083, -0.001854,
    -0.008412, -0.012305, -0.011258, -0.005197, -0.014688, -0.008478, -0.010977, -0.000282, 0.024281, -0.012305, -0.002545, 0.019189,
    -0.017294, -0.007670, 0.017719, -0.008985, 0.004829, 0.039418, -0.009356, 0.007099, 0.047475, -0.007426, 0.008237, 0.045873,
    -0.002459, 0.010212, 0.029043, -0.003612, 0.011152, 0.037448, -0.001025, 0.000611, 0.002292, -0.005047, 0.010207, 0.044094,
    -0.014454, -0.026244, -0.019908, -0.001457, -0.004464, -0.003370, -0.002222, 0.003238, 0.015368, -0.035675, -0.056651, -0.064960,
    -0.030331, -0.052018, -0.055523, -0.040452, -0.059919, -0.074227, -0.021548, -0.040453, -0.043391, -0.027558, -0.045256, -0.050853,
    -0.018848, -0.035497, -0.034533, -0.041162, -0.062397, -0.080305, -0.035744, -0.053186, -0.065294, -0.013791, -0.026855, -0.027737,
    -0.007739, 0.007092, 0.012017, -0.007739, 0.007092, 0.012017, -0.007739, 0.007092, 0.012017, -0.007739, 0.007092, 0.012017,
    -0.007739, 0.007092, 0.012017, -0.007739, 0.007092, 0.012017, -0.007739, 0.007092, 0.012017, -0.007739, 0.007092, 0.012017,
    -0.007739, 0.007092, 0.012017, -0.024836, 0.005399, 0.013186, -0.024836, 0.005399, 0.013186, -0.024836, 0.005399, 0.013186,
    -0.024836, 0.005399, 0.013186, -0.024836, 0.005399, 0.013186, -0.024836, 0.005399, 0.013186, -0.024836, 0.005399, 0.013186,
    -0.024836, 0.005399, 0.013186, -0.024836, 0.005399, 0.013186
  ]),
  // 9 — frown
  new Float32Array([
    0.001772, -0.045830, 0.047333, -0.010945, -0.041489, 0.049889, 0.010135, -0.043811, 0.042066, -0.000547, 0.001853, -0.010493,
    -0.003790, 0.010074, -0.024434, -0.004426, 0.026306, 0.008312, -0.014714, 0.023009, -0.009440, -0.007717, 0.016778, -0.037438,
    -0.014897, 0.030450, -0.063266, -0.018718, 0.040886, -0.078285, -0.011451, 0.022833, -0.051573, -0.033594, 0.062046, -0.047202,
    -0.034895, 0.063164, -0.047839, -0.033051, 0.060384, -0.046895, -0.030733, 0.071275, -0.178687, -0.027327, 0.071593, -0.170042,
    -0.033793, 0.072254, -0.187162, -0.004373, -0.031735, 0.054728, -0.016461, -0.025062, 0.058422, 0.000495, -0.027533, 0.051112,
    -0.006372, -0.036147, 0.073273, -0.017906, -0.029355, 0.076930, -0.001397, -0.030421, 0.069287, -0.007043, -0.043196, 0.095264,
    -0.015750, -0.033746, 0.098402, -0.003428, -0.035306, 0.090955, -0.064511, 0.082632, -0.089776, -0.051592, 0.081253, -0.115297,
    -0.071390, 0.083722, -0.061723, -0.032838, 0.061626, -0.050417, -0.043062, -0.022901, 0.058009, -0.065619, 0.000677, 0.067075,
    -0.080269, 0.027493, 0.076728, -0.038325, 0.079561, -0.064153, -0.029782, 0.083074, -0.072578, -0.021411, 0.083022, -0.080417,
    -0.017033, 0.069818, -0.087032, -0.037939, 0.078366, -0.136044, -0.014216, 0.044899, -0.087772, -0.052416, 0.065356, 0.112474,
    -0.055320, 0.070103, 0.076564, -0.050195, 0.059677, 0.134380, -0.079963, 0.051554, 0.087568, -0.072752, 0.036073, 0.094116,
    -0.034241, 0.085172, -0.082925, -0.043748, 0.082240, -0.073475, -0.025069, 0.081626, -0.093841, -0.016779, 0.066524, -0.098547,
    -0.012890, 0.048505, -0.103801, -0.028323, 0.074330, -0.155996, -0.074035, 0.042212, 0.084639, -0.066481, 0.024586, 0.084779,
    -0.063126, 0.023433, 0.077327, -0.049242, 0.005030, 0.069437, -0.032714, -0.012475, 0.063293, -0.032245, -0.010719, 0.098390,
    -0.034427, -0.014374, 0.079327, -0.052553, 0.006240, 0.081834, -0.055492, 0.016955, 0.097084, -0.064012, 0.074641, 0.041764,
    -0.072049, 0.079949, 0.006043, -0.073916, 0.084408, -0.027000, -0.027192, 0.049550, -0.062048, -0.028682, 0.049530, -0.059611,
    -0.028279, 0.051547, -0.063759, -0.046594, 0.052074, 0.147977, -0.041945, 0.042014, 0.158187, -0.032189, 0.057827, -0.047745,
    -0.031164, 0.054286, -0.049525, -0.031109, 0.051567, -0.052993, -0.025743, 0.006866, 0.178150, -0.020383, 0.002247, 0.174232,
    -0.030165, 0.017551, 0.174228, -0.036352, 0.030941, 0.167336, -0.017946, 0.004575, 0.165791, -0.030299, 0.049809, -0.056011,
    -0.030112, 0.055141, -0.065576, -0.031014, 0.058678, -0.063243, -0.030173, 0.060623, -0.060642, -0.030177, 0.061162, -0.056025,
    -0.031437, 0.060986, -0.053086, -0.034461, 0.061538, -0.090361, -0.034106, 0.063164, -0.092989, -0.033597, 0.059899, -0.087971,
    -0.044200, 0.084652, -0.156236, -0.045820, 0.083467, -0.174933, -0.040806, 0.085873, -0.131389, -0.034713, 0.061598, -0.093816,
    0.030338, -0.027126, 0.041764, 0.037663, -0.004179, 0.044922, 0.035458, 0.021951, 0.050332, -0.037892, 0.078524, -0.116548,
    -0.038629, 0.079272, -0.118026, -0.038940, 0.075087, -0.117073, -0.033876, 0.059218, -0.111769, -0.044452, 0.080615, -0.184579,
    -0.029687, 0.034391, -0.099837, -0.028570, 0.070254, 0.049052, -0.032550, 0.075546, 0.010202, -0.024519, 0.061194, 0.077614,
    0.020616, 0.044527, 0.058910, 0.020903, 0.030645, 0.069757, -0.039052, 0.082174, -0.131161, -0.038692, 0.082659, -0.127101,
    -0.036546, 0.074138, -0.132450, -0.033194, 0.055582, -0.124283, -0.030644, 0.037976, -0.116706, -0.038804, 0.075500, -0.190192,
    0.016142, 0.034859, 0.059698, 0.015802, 0.018873, 0.062827, 0.013636, 0.016103, 0.055325, 0.011758, -0.001234, 0.051620,
    0.007393, -0.017439, 0.050009, 0.004123, -0.013011, 0.083312, 0.005632, -0.016476, 0.065721, 0.011885, 0.002069, 0.064159,
    0.014109, 0.012694, 0.077283, -0.032407, 0.079252, -0.027660, -0.032324, 0.083831, -0.065359, -0.035826, 0.086945, -0.098305,
    -0.032220, 0.046903, -0.081917, -0.031083, 0.047521, -0.081065, -0.032066, 0.049404, -0.085774, -0.023319, 0.049561, 0.100034,
    -0.020732, 0.036997, 0.119503, -0.033033, 0.057452, -0.085645, -0.032215, 0.053654, -0.083062, -0.030621, 0.050882, -0.081369,
    -0.017992, 0.013892, 0.151106, -0.019234, 0.026142, 0.136139, -0.030188, 0.048677, -0.080428, -0.032148, 0.053248, -0.090180,
    -0.032377, 0.057084, -0.092445, -0.033781, 0.059536, -0.094530, -0.035026, 0.060824, -0.094845, -0.035071, 0.061075, -0.094652,
    0.000000, 0.000000, -0.000000, 0.000760, 0.001373, 0.001675, -0.002255, 0.001336, -0.002042, -0.005030, 0.012868, 0.021147,
    -0.002811, 0.011812, 0.014793, -0.004985, -0.002075, 0.030878, -0.006664, 0.000042, 0.036968, -0.003779, 0.012112, 0.012816,
    -0.008223, -0.001693, 0.029078, -0.007690, 0.017834, -0.033235, -0.003226, 0.011478, -0.021615, -0.011510, 0.023804, -0.047457,
    -0.008508, 0.021538, -0.025856, -0.005582, 0.010921, -0.026423, -0.016452, 0.031284, -0.058116, -0.016303, 0.030918, -0.064110,
    -0.013232, 0.023382, -0.052950, -0.020241, 0.044263, -0.090908, -0.021942, 0.048397, -0.109106, -0.016567, 0.041104, -0.074348,
    -0.026392, 0.036061, -0.082216, -0.018768, 0.060704, -0.139825, -0.026581, 0.059719, -0.147694, -0.032465, 0.059914, -0.155177,
    0.000828, -0.043141, 0.051714, -0.001430, -0.036558, 0.055025, -0.013169, -0.036731, 0.054629, 0.009391, -0.039045, 0.046799,
    -0.014753, -0.029391, 0.059065, 0.005212, -0.032057, 0.051545, -0.005798, -0.040087, 0.075581, -0.005889, -0.042302, 0.083897,
    -0.015293, -0.034211, 0.088418, -0.016150, -0.032965, 0.079375, -0.002151, -0.035748, 0.080380, -0.001288, -0.033957, 0.071354,
    -0.014407, -0.017988, 0.112026, -0.013752, -0.013079, 0.115104, -0.014294, -0.013099, 0.132778, -0.014236, -0.007933, 0.136389,
    -0.018215, -0.014747, 0.108785, -0.017376, -0.010218, 0.128114, -0.001105, 0.003752, 0.006048, -0.000045, 0.004077, 0.007669,
    -0.002623, 0.004370, 0.004516, -0.002823, 0.013448, 0.012420, -0.005181, 0.019348, 0.017703, -0.004271, 0.014253, 0.004946,
    -0.006394, 0.017540, 0.020590, -0.002866, 0.020728, 0.007404, -0.001820, 0.007553, 0.007446, -0.002437, 0.012439, 0.013556,
    -0.039155, 0.079526, -0.100277, -0.051963, 0.079038, -0.082783, -0.029344, 0.048981, -0.048369, -0.030104, 0.051534, -0.043788,
    -0.028330, 0.047398, -0.051752, -0.027395, 0.046642, -0.029964, -0.026654, 0.045379, -0.037881, -0.031292, 0.053854, -0.040906,
    -0.028841, 0.048709, -0.027507, -0.034281, 0.057113, -0.039277, -0.032652, 0.053199, -0.027571, -0.036913, 0.060014, -0.039919,
    -0.035599, 0.057284, -0.030216, -0.041816, 0.063116, -0.045117, -0.043273, 0.063551, -0.040781, -0.026267, 0.047463, -0.056193,
    -0.025996, 0.043928, -0.043902, -0.036563, 0.059174, -0.063485, -0.030030, 0.055006, -0.070058, -0.041358, 0.062037, -0.057902,
    -0.026054, 0.059237, -0.077612, -0.031804, 0.065954, -0.070168, -0.023811, 0.052349, -0.071035, -0.023327, 0.049101, -0.079790,
    -0.043849, 0.062689, -0.054643, -0.036594, 0.068212, -0.063023, -0.041641, 0.069564, -0.058072, -0.043298, 0.064717, -0.050790,
    -0.032367, 0.055740, -0.019537, -0.027515, 0.049637, -0.011878, -0.020649, 0.007975, 0.147602, -0.026086, 0.011764, 0.164171,
    -0.020036, -0.000978, 0.168592, -0.017663, 0.005632, 0.131818, -0.015806, -0.005152, 0.153885, -0.026409, 0.020832, 0.141144,
    -0.050707, 0.072811, -0.028790, -0.055378, 0.075947, -0.058383, -0.043430, 0.065332, -0.034319, -0.047928, 0.075291, -0.054002,
    -0.044671, 0.065933, 0.001815, -0.042495, 0.064066, -0.035417, -0.043732, 0.067872, -0.051086, -0.043661, 0.072019, -0.052219,
    -0.016688, 0.034134, 0.009716, -0.022470, 0.040050, -0.015088, -0.022054, 0.038181, -0.002393, -0.017130, 0.038210, -0.003639,
    -0.014807, 0.034969, 0.024325, -0.016492, 0.031217, 0.030593, -0.016704, 0.029462, 0.042929, -0.034938, -0.014353, 0.063488,
    -0.053203, 0.005180, 0.069999, -0.038255, -0.017867, 0.061623, -0.058940, 0.003596, 0.069243, -0.069234, 0.026643, 0.077262,
    -0.074777, 0.028026, 0.077961, -0.032241, 0.028744, 0.103476, -0.030722, 0.034963, 0.091328, -0.023164, 0.015643, 0.110734,
    -0.028341, 0.028219, 0.112837, -0.023499, 0.018401, 0.125007, -0.000133, 0.003965, -0.008327, -0.000703, 0.009547, -0.003293,
    -0.000197, 0.006418, 0.001610, -0.003612, 0.015709, -0.014499, -0.021298, 0.037580, -0.026507, -0.020211, 0.035381, -0.035069,
    -0.021424, 0.040694, -0.034983, -0.016345, 0.035297, -0.015176, -0.020234, 0.039923, -0.044224, -0.014713, 0.031171, -0.025546,
    -0.006576, 0.030447, 0.003292, -0.004640, 0.022998, -0.005864, -0.003420, 0.022867, 0.010601, -0.005158, 0.027488, 0.013848,
    -0.006782, 0.031462, 0.014227, -0.009557, 0.034143, 0.009359, -0.012198, 0.036593, 0.010718, -0.011730, 0.033654, -0.007026,
    -0.022212, 0.039503, 0.018085, -0.024126, 0.044127, -0.011621, -0.029836, 0.052096, 0.021877, -0.020829, 0.038754, 0.050736,
    -0.026057, 0.077164, -0.115811, -0.023643, 0.045977, -0.072707, -0.024758, 0.048593, -0.068689, -0.025047, 0.026241, 0.071890,
    -0.028364, 0.032983, 0.095076, -0.034341, 0.057048, 0.061675, -0.035409, 0.053702, 0.091921, -0.004023, 0.023958, 0.011563,
    -0.004644, 0.024269, 0.014755, -0.005789, 0.027900, 0.017557, -0.076207, 0.045733, 0.084619, -0.078167, 0.048688, 0.085739,
    -0.069631, 0.032517, 0.090764, -0.066898, 0.028117, 0.087480, -0.018421, 0.068171, -0.130331, -0.053409, 0.012998, 0.091336,
    -0.003643, 0.014060, 0.002658, -0.001054, 0.010533, 0.003760, -0.016614, 0.001067, 0.114565, -0.032812, -0.013996, 0.082257,
    -0.032362, -0.012455, 0.090176, -0.051761, 0.009129, 0.084927, -0.019546, 0.018202, 0.058297, -0.015319, 0.014970, 0.046296,
    -0.021456, 0.031376, 0.062853, -0.037773, 0.060619, 0.031234, -0.003304, 0.011435, 0.013964, -0.007459, 0.016042, 0.025213,
    -0.008544, 0.030775, 0.021932, -0.022237, 0.041807, -0.025727, -0.023284, 0.041548, -0.016852, -0.036808, 0.057670, -0.007235,
    -0.024656, 0.043233, -0.050039, -0.024983, 0.047197, -0.062248, -0.024136, 0.043228, -0.059406, -0.013261, 0.027029, -0.037011,
    -0.018616, 0.033262, -0.047348, -0.003566, 0.017541, 0.000718, -0.019452, 0.040429, -0.056217, -0.025850, 0.048637, 0.050829,
    0.000053, 0.005067, 0.007932, -0.010346, 0.027580, -0.017221, -0.036748, 0.042446, 0.128411, -0.036906, 0.048327, 0.114064,
    -0.035909, 0.034434, 0.144510, -0.026727, 0.040400, 0.087200, -0.027379, 0.034545, 0.097889, -0.027404, 0.046802, 0.074563,
    -0.031880, 0.026152, 0.156510, -0.017281, -0.005274, 0.165287, -0.029778, 0.029058, 0.126564, -0.015160, -0.009751, 0.151090,
    -0.016269, -0.003426, 0.157150, -0.020371, 0.032863, 0.054476, -0.009437, 0.017245, -0.039326, -0.016436, -0.007781, 0.143368,
    -0.013058, 0.020530, -0.036271, -0.005451, 0.013803, 0.007659, -0.010445, 0.017936, 0.011323, -0.005802, 0.014367, -0.007235,
    -0.008431, 0.016492, 0.014422, -0.013093, 0.018766, -0.005088, -0.003611, 0.008340, 0.002602, -0.005730, 0.010259, -0.004769,
    -0.004879, 0.012795, 0.010354, -0.043633, 0.081330, -0.152614, -0.040436, 0.081272, -0.142934, -0.030911, 0.047952, -0.076320,
    -0.032267, 0.050640, -0.077376, -0.030028, 0.045755, -0.073899, -0.032157, 0.045901, -0.063560, -0.030547, 0.043999, -0.064635,
    -0.033143, 0.053555, -0.079695, -0.033333, 0.048731, -0.068265, -0.032748, 0.056927, -0.082426, -0.034137, 0.053576, -0.074553,
    -0.032306, 0.060049, -0.085798, -0.035373, 0.057913, -0.079998, -0.028623, 0.064514, -0.092669, -0.030764, 0.065192, -0.091581,
    -0.030969, 0.045075, -0.075735, -0.029007, 0.041994, -0.064252, -0.025875, 0.057172, -0.098808, -0.030578, 0.051046, -0.097924,
    -0.023762, 0.061997, -0.098486, -0.031330, 0.053092, -0.103589, -0.028716, 0.062043, -0.105022, -0.036178, 0.048011, -0.093700,
    -0.031659, 0.042523, -0.097415, -0.023502, 0.064225, -0.098205, -0.027922, 0.066999, -0.105165, -0.028274, 0.069896, -0.105413,
    -0.026583, 0.066329, -0.096796, -0.038784, 0.057021, -0.074663, -0.037189, 0.050352, -0.063970, -0.019345, 0.003549, 0.127334,
    -0.016713, 0.008178, 0.142659, -0.020832, 0.001339, 0.114182, -0.020567, 0.016011, 0.113927, -0.041843, 0.075867, -0.095590,
    -0.038900, 0.078749, -0.122333, -0.039169, 0.068114, -0.092732, -0.037782, 0.077120, -0.112503, -0.042510, 0.069545, -0.066799,
    -0.036421, 0.065868, -0.091046, -0.029831, 0.068817, -0.101255, -0.035291, 0.072462, -0.107273, -0.027041, 0.031926, -0.018738,
    -0.025246, 0.038938, -0.039937, -0.028843, 0.037508, -0.036154, -0.024341, 0.035738, -0.025859, -0.021972, 0.030720, 0.000494,
    -0.030658, 0.029020, -0.005714, -0.025490, 0.025315, 0.013085, 0.014714, -0.018796, 0.049346, 0.020025, -0.000683, 0.051326,
    0.022896, -0.021888, 0.046395, 0.028723, -0.001530, 0.048869, 0.021804, 0.019849, 0.054803, 0.028802, 0.022009, 0.053179,
    -0.011950, 0.023065, 0.076311, -0.015210, 0.028454, 0.058394, -0.017469, 0.011090, 0.088506, -0.020397, 0.022712, 0.081757,
    -0.020982, 0.013722, 0.100842, -0.002436, 0.003333, -0.012899, -0.006116, 0.008633, -0.013121, -0.004141, 0.006205, -0.006628,
    -0.009785, 0.014529, -0.025027, -0.022614, 0.036563, -0.045358, -0.021116, 0.034586, -0.050177, -0.028537, 0.038243, -0.054584,
    -0.020979, 0.033104, -0.033158, -0.026936, 0.037128, -0.060021, -0.018911, 0.029733, -0.039956, -0.017198, 0.026946, -0.015032,
    -0.013952, 0.021026, -0.020442, -0.013499, 0.020047, -0.004459, -0.014452, 0.023872, -0.000915, -0.015454, 0.027392, -0.003240,
    -0.017709, 0.030254, -0.009628, -0.019980, 0.031858, -0.008714, -0.019127, 0.030661, -0.023825, -0.033063, 0.038987, -0.029295,
    -0.034563, 0.043860, -0.056267, -0.040202, 0.053219, -0.040214, -0.036787, 0.038318, 0.000504, -0.044886, 0.077627, -0.158243,
    -0.029393, 0.041287, -0.085716, -0.033583, 0.044743, -0.087954, -0.015612, 0.019865, 0.040720, -0.022255, 0.028057, 0.057845,
    -0.040738, 0.061695, -0.002506, -0.035589, 0.057798, 0.030858, -0.013988, 0.021288, -0.000965, -0.013601, 0.021369, 0.004301,
    -0.015166, 0.023844, 0.005127, 0.017733, 0.038275, 0.058949, 0.019447, 0.041522, 0.059119, 0.018924, 0.026781, 0.067966,
    0.016525, 0.022206, 0.065404, -0.038505, 0.067145, -0.159464, 0.012823, 0.008578, 0.072756, -0.006399, 0.013739, -0.010424,
    -0.019667, -0.002012, 0.098655, 0.004836, -0.016626, 0.068343, 0.004693, -0.015269, 0.075326, 0.011134, 0.004754, 0.067124,
    -0.014725, 0.012926, 0.032858, -0.011175, 0.010422, 0.027133, -0.025668, 0.026081, 0.027821, -0.042758, 0.065138, -0.035781,
    -0.006850, 0.015172, 0.018697, -0.015855, 0.025900, 0.007981, -0.030582, 0.039903, -0.052002, -0.032628, 0.040543, -0.051026,
    -0.039152, 0.059783, -0.066859, -0.028456, 0.040539, -0.066776, -0.032373, 0.043904, -0.079114, -0.027705, 0.039583, -0.073203,
    -0.016196, 0.026118, -0.047179, -0.018610, 0.032712, -0.057860, -0.009760, 0.016319, -0.013558, -0.026321, 0.036487, -0.067008,
    -0.042037, 0.050293, -0.008993, -0.003539, 0.005669, 0.004041, -0.016903, 0.025681, -0.031692, -0.027019, 0.038978, 0.082051,
    -0.030057, 0.048799, 0.059236, -0.022351, 0.029472, 0.107991, -0.036440, 0.039292, 0.037055, -0.030257, 0.030511, 0.055696,
    -0.040898, 0.047753, 0.018141, -0.018467, 0.021528, 0.127431, -0.024826, 0.023448, 0.091718, -0.032146, 0.029808, 0.012948,
    -0.036788, 0.057231, -0.101303, -0.036788, 0.057231, -0.101303, -0.036788, 0.057231, -0.101303, -0.036788, 0.057231, -0.101303,
    -0.036788, 0.057231, -0.101303, -0.036788, 0.057231, -0.101303, -0.036788, 0.057231, -0.101303, -0.036788, 0.057231, -0.101303,
    -0.036788, 0.057231, -0.101303, -0.027802, 0.056351, -0.093982, -0.027802, 0.056351, -0.093982, -0.027802, 0.056351, -0.093982,
    -0.027802, 0.056351, -0.093982, -0.027802, 0.056351, -0.093982, -0.027802, 0.056351, -0.093982, -0.027802, 0.056351, -0.093982,
    -0.027802, 0.056351, -0.093982, -0.027802, 0.056351, -0.093982
  ]),
  // 10 — vis_ee
  new Float32Array([
    -0.014291, 0.041526, 0.054264, -0.009487, 0.039127, 0.054815, -0.020928, 0.043615, 0.046886, 0.000777, -0.004120, -0.008563,
    -0.001838, -0.001024, -0.020623, 0.001399, 0.026925, 0.010322, -0.025245, 0.029799, -0.009137, -0.005091, 0.003834, -0.031490,
    -0.011420, 0.012073, -0.054255, -0.014220, 0.013452, -0.065094, -0.008379, 0.008240, -0.043640, -0.036752, 0.041806, -0.027523,
    -0.037312, 0.042373, -0.028451, -0.036208, 0.040970, -0.027109, -0.020045, 0.018211, -0.142481, -0.017161, 0.018290, -0.130387,
    -0.028781, 0.021351, -0.150330, -0.019726, 0.055060, 0.061391, -0.013348, 0.056668, 0.066617, -0.029459, 0.060069, 0.059497,
    -0.023354, 0.109140, 0.075400, -0.017798, 0.108688, 0.077748, -0.030653, 0.112529, 0.071373, -0.028344, 0.142179, 0.065380,
    -0.030034, 0.140596, 0.069750, -0.026091, 0.144266, 0.063469, -0.052573, 0.061299, -0.060290, -0.039038, 0.045278, -0.081440,
    -0.062236, 0.075527, -0.036867, -0.034562, 0.040165, -0.030423, -0.010950, 0.056717, 0.063498, -0.010206, 0.078119, 0.074634,
    -0.007409, 0.098814, 0.086288, -0.041372, 0.028075, -0.046150, -0.035356, 0.019157, -0.054394, -0.029337, 0.011984, -0.060985,
    -0.023548, 0.004411, -0.065811, -0.027159, 0.031529, -0.098354, -0.018217, -0.000207, -0.068527, -0.037494, 0.127121, 0.088154,
    -0.050502, 0.116575, 0.063570, -0.030046, 0.134416, 0.102838, 0.001673, 0.119162, 0.094589, -0.012926, 0.124373, 0.092952,
    -0.037889, 0.020172, -0.060564, -0.044432, 0.032648, -0.049856, -0.030481, 0.011457, -0.069353, -0.023917, 0.002372, -0.074547,
    -0.018840, -0.005362, -0.080179, -0.019754, 0.021610, -0.116654, 0.004954, 0.114639, 0.091350, 0.002072, 0.111972, 0.086034,
    0.002296, 0.099457, 0.084772, 0.000369, 0.083331, 0.079067, -0.006197, 0.067119, 0.072793, -0.028398, 0.137282, 0.076325,
    -0.010315, 0.108923, 0.079610, -0.003496, 0.110221, 0.081916, -0.022748, 0.130852, 0.083337, -0.062160, 0.107904, 0.040214,
    -0.069047, 0.099713, 0.015282, -0.068908, 0.089869, -0.009458, -0.026897, 0.025094, -0.041289, -0.030741, 0.027904, -0.039610,
    -0.026622, 0.026292, -0.041825, -0.032369, 0.137838, 0.110904, -0.040477, 0.141422, 0.114085, -0.036159, 0.039990, -0.027705,
    -0.036549, 0.038321, -0.029420, -0.036019, 0.035783, -0.032902, -0.041628, 0.148422, 0.106702, -0.034656, 0.149862, 0.100050,
    -0.044688, 0.145378, 0.111101, -0.044604, 0.143654, 0.113272, -0.026211, 0.147677, 0.092636, -0.034014, 0.031555, -0.036216,
    -0.027614, 0.030575, -0.041541, -0.028843, 0.035035, -0.039973, -0.028484, 0.037094, -0.037696, -0.029637, 0.038245, -0.034460,
    -0.032372, 0.039027, -0.032198, -0.035222, 0.041626, -0.076719, -0.036532, 0.042373, -0.080110, -0.033707, 0.040626, -0.074017,
    -0.060004, 0.065547, -0.137860, -0.053845, 0.051506, -0.151519, -0.064858, 0.078082, -0.117388, -0.037563, 0.040901, -0.080116,
    -0.028206, 0.065025, 0.047237, -0.037374, 0.089292, 0.052966, -0.047497, 0.112673, 0.059949, -0.034171, 0.029250, -0.106501,
    -0.028467, 0.020268, -0.106866, -0.024048, 0.011681, -0.102483, -0.020154, 0.002857, -0.094603, -0.047040, 0.038044, -0.155601,
    -0.016396, -0.002551, -0.082121, -0.087071, 0.120875, 0.017896, -0.084145, 0.111576, -0.012013, -0.080188, 0.127488, 0.038581,
    -0.062782, 0.135137, 0.066130, -0.047790, 0.137779, 0.069823, -0.029974, 0.021709, -0.116646, -0.038351, 0.033997, -0.115581,
    -0.024018, 0.011083, -0.113465, -0.018567, 0.000537, -0.104176, -0.014232, -0.007790, -0.094896, -0.036988, 0.026700, -0.156509,
    -0.066223, 0.128511, 0.067734, -0.061632, 0.122809, 0.065676, -0.060880, 0.110162, 0.063692, -0.053600, 0.091483, 0.062181,
    -0.041076, 0.073308, 0.060083, -0.028672, 0.143861, 0.064104, -0.040847, 0.115922, 0.068007, -0.052197, 0.119476, 0.066104,
    -0.036166, 0.140774, 0.066326, -0.077520, 0.104647, -0.038309, -0.070975, 0.098559, -0.065954, -0.067611, 0.090313, -0.090962,
    -0.032963, 0.029525, -0.064766, -0.029920, 0.032550, -0.065099, -0.034711, 0.030425, -0.067974, -0.064336, 0.130775, 0.056459,
    -0.045242, 0.135691, 0.069659, -0.031443, 0.040146, -0.070972, -0.028611, 0.039989, -0.067943, -0.027920, 0.038889, -0.065958,
    -0.026089, 0.143130, 0.084763, -0.033362, 0.139664, 0.077665, -0.028172, 0.035904, -0.064848, -0.037538, 0.034442, -0.070918,
    -0.038927, 0.038153, -0.074026, -0.039801, 0.039305, -0.077366, -0.039680, 0.040197, -0.079586, -0.038215, 0.040481, -0.079888,
    0.000000, 0.000000, -0.000000, 0.000668, 0.001297, 0.001972, -0.002470, 0.001880, -0.002256, -0.007505, 0.022365, 0.015432,
    -0.001627, 0.013711, 0.011895, -0.010669, 0.032554, 0.025200, -0.006044, 0.036350, 0.030310, -0.007755, 0.014354, 0.009726,
    -0.019285, 0.038931, 0.021988, -0.006221, 0.006340, -0.026573, -0.002117, 0.001179, -0.017224, -0.009574, 0.010478, -0.038997,
    -0.007976, 0.013048, -0.018910, -0.004104, 0.001536, -0.023361, -0.012911, 0.014660, -0.048062, -0.014597, 0.014965, -0.055280,
    -0.010966, 0.010754, -0.045150, -0.014655, 0.002900, -0.073789, -0.015278, -0.001767, -0.087638, -0.015650, 0.013478, -0.058049,
    -0.020718, 0.012686, -0.066753, -0.016789, 0.009374, -0.107160, -0.017311, 0.010305, -0.116159, -0.022185, 0.010896, -0.124882,
    -0.015474, 0.046061, 0.060622, -0.017622, 0.051856, 0.062555, -0.010663, 0.047194, 0.062039, -0.023493, 0.051338, 0.054110,
    -0.011760, 0.054018, 0.066968, -0.026839, 0.057631, 0.059435, -0.024236, 0.116610, 0.074694, -0.025937, 0.129283, 0.072324,
    -0.025489, 0.127599, 0.074562, -0.020878, 0.114994, 0.077005, -0.026366, 0.131255, 0.067674, -0.028027, 0.118661, 0.070457,
    -0.030519, 0.135611, 0.052176, -0.030852, 0.135379, 0.057766, -0.030250, 0.134582, 0.066834, -0.031981, 0.133511, 0.072519,
    -0.028800, 0.136065, 0.051597, -0.025789, 0.133330, 0.063360, -0.002478, 0.007264, 0.004843, -0.000958, 0.007798, 0.006706,
    -0.005428, 0.008287, 0.003190, 0.002066, 0.015099, 0.010592, 0.002566, 0.027032, 0.014059, 0.002794, 0.013812, 0.006583,
    0.000765, 0.027479, 0.016026, 0.003596, 0.023305, 0.008648, 0.001558, 0.010651, 0.006904, 0.000375, 0.014714, 0.011230,
    -0.035829, 0.032012, -0.071076, -0.046020, 0.046191, -0.056279, -0.034954, 0.038371, -0.029094, -0.035812, 0.041985, -0.025594,
    -0.031812, 0.032554, -0.034014, -0.033294, 0.045726, -0.016202, -0.031369, 0.040937, -0.021954, -0.035961, 0.043925, -0.023169,
    -0.034318, 0.049384, -0.013187, -0.037449, 0.045609, -0.021596, -0.037137, 0.051736, -0.011649, -0.039570, 0.045904, -0.021952,
    -0.038088, 0.052331, -0.013386, -0.042895, 0.042709, -0.026538, -0.042278, 0.047122, -0.022722, -0.028135, 0.027257, -0.037643,
    -0.027659, 0.034661, -0.029126, -0.031865, 0.021275, -0.041310, -0.029832, 0.019599, -0.046272, -0.035368, 0.023975, -0.037279,
    -0.028204, 0.012079, -0.054937, -0.031686, 0.015234, -0.050556, -0.027885, 0.020324, -0.046602, -0.024900, 0.012049, -0.055035,
    -0.038959, 0.028096, -0.034342, -0.035171, 0.019755, -0.044889, -0.040025, 0.026540, -0.040934, -0.041830, 0.034616, -0.031278,
    -0.036571, 0.059202, -0.003909, -0.032558, 0.059325, 0.001093, -0.035304, 0.131513, 0.092517, -0.039351, 0.137127, 0.103697,
    -0.037828, 0.140370, 0.098431, -0.031134, 0.130018, 0.078507, -0.034266, 0.134821, 0.087371, -0.033892, 0.128761, 0.098994,
    -0.054070, 0.072121, -0.009212, -0.051086, 0.059800, -0.034473, -0.045518, 0.059532, -0.013279, -0.046201, 0.047096, -0.034351,
    -0.052025, 0.080462, 0.016331, -0.042264, 0.053031, -0.017791, -0.042297, 0.036470, -0.032807, -0.042970, 0.040715, -0.033827,
    -0.013662, 0.052179, 0.010096, -0.023044, 0.044310, -0.008243, -0.022679, 0.053458, 0.000953, -0.014979, 0.043938, 0.000919,
    -0.006407, 0.055836, 0.022191, -0.006464, 0.065598, 0.026645, -0.005065, 0.067737, 0.039782, -0.006637, 0.066747, 0.073246,
    -0.002098, 0.083629, 0.079809, -0.008562, 0.062567, 0.070351, -0.006540, 0.080939, 0.078117, 0.000761, 0.100946, 0.086021,
    -0.003584, 0.100448, 0.087287, -0.014930, 0.122201, 0.094631, -0.003261, 0.110843, 0.092595, -0.022665, 0.126717, 0.086011,
    -0.021643, 0.119701, 0.096710, -0.029483, 0.126257, 0.088936, 0.000716, -0.001567, -0.006170, 0.000468, 0.004522, -0.001173,
    0.001547, 0.005279, 0.002873, -0.003360, 0.008225, -0.010305, -0.020842, 0.036662, -0.017501, -0.018798, 0.029534, -0.025103,
    -0.022007, 0.035683, -0.023279, -0.015327, 0.034515, -0.008069, -0.019022, 0.028094, -0.032033, -0.013977, 0.025707, -0.017162,
    -0.003281, 0.029288, 0.006773, -0.003576, 0.018128, -0.001537, 0.003299, 0.025464, 0.011625, 0.004789, 0.031902, 0.013837,
    0.003535, 0.035698, 0.014817, -0.002220, 0.037461, 0.011490, -0.004881, 0.045865, 0.012393, -0.010922, 0.032069, -0.001581,
    -0.016380, 0.068481, 0.021324, -0.028990, 0.056039, -0.000230, -0.028418, 0.078626, 0.030236, -0.005695, 0.083284, 0.049513,
    -0.027126, 0.020710, -0.084079, -0.022916, 0.018285, -0.050343, -0.025415, 0.021457, -0.046535, 0.001246, 0.089991, 0.078387,
    -0.009960, 0.108339, 0.090250, -0.034014, 0.100021, 0.058206, -0.024269, 0.109392, 0.078931, 0.004374, 0.027980, 0.012991,
    0.005191, 0.030564, 0.012885, 0.005304, 0.036588, 0.015736, 0.004413, 0.116314, 0.092119, 0.003010, 0.117909, 0.092986,
    -0.007753, 0.119599, 0.090323, -0.002362, 0.115189, 0.087621, -0.020341, 0.011893, -0.097513, -0.015988, 0.122428, 0.082496,
    0.002392, 0.013228, 0.004890, 0.002077, 0.009900, 0.004737, -0.028126, 0.131419, 0.073629, -0.015194, 0.114239, 0.078941,
    -0.022153, 0.125004, 0.077743, -0.008722, 0.114929, 0.081441, -0.001038, 0.070858, 0.060876, -0.002588, 0.055411, 0.042688,
    -0.000993, 0.086807, 0.062913, -0.044040, 0.090000, 0.037556, -0.004422, 0.013589, 0.011058, -0.001458, 0.028033, 0.019238,
    0.003611, 0.043144, 0.018978, -0.025607, 0.042527, -0.014665, -0.027814, 0.049397, -0.007126, -0.039762, 0.068619, 0.009467,
    -0.024353, 0.029122, -0.033920, -0.025047, 0.023062, -0.042335, -0.021771, 0.023596, -0.042597, -0.012355, 0.017205, -0.027949,
    -0.016244, 0.021532, -0.036314, 0.000263, 0.014833, 0.003534, -0.017992, 0.021337, -0.042026, -0.017450, 0.087952, 0.052168,
    0.000181, 0.008790, 0.007166, -0.010293, 0.023072, -0.010045, -0.021181, 0.125037, 0.101965, -0.016157, 0.118418, 0.093483,
    -0.033494, 0.131473, 0.108093, -0.006068, 0.103368, 0.077834, -0.010902, 0.110079, 0.088515, -0.012236, 0.097682, 0.068740,
    -0.037725, 0.134521, 0.107401, -0.032209, 0.142027, 0.091603, -0.028235, 0.124128, 0.101570, -0.030523, 0.136251, 0.080522,
    -0.023631, 0.139425, 0.084588, -0.001616, 0.082433, 0.052085, -0.007393, 0.006487, -0.033323, -0.023396, 0.133836, 0.074619,
    -0.012525, 0.013577, -0.030636, -0.014138, 0.016445, 0.005316, -0.022650, 0.029697, 0.006460, -0.016631, 0.015397, -0.006866,
    -0.020064, 0.030000, 0.008726, -0.023729, 0.025740, -0.005144, -0.012056, 0.011498, 0.001543, -0.012895, 0.011014, -0.004722,
    -0.011005, 0.015791, 0.007558, -0.042414, 0.036118, -0.132915, -0.050282, 0.049097, -0.125665, -0.028236, 0.040958, -0.061209,
    -0.029910, 0.043128, -0.063908, -0.028362, 0.036473, -0.059283, -0.031526, 0.047057, -0.054266, -0.029427, 0.043168, -0.051892,
    -0.032990, 0.043987, -0.067639, -0.035491, 0.049839, -0.059540, -0.035533, 0.045253, -0.071390, -0.038438, 0.051932, -0.064846,
    -0.035912, 0.046309, -0.074405, -0.042497, 0.052463, -0.069930, -0.034020, 0.043480, -0.080806, -0.039872, 0.047600, -0.080840,
    -0.029576, 0.031763, -0.060250, -0.029894, 0.037534, -0.051882, -0.032183, 0.023511, -0.081999, -0.032087, 0.022070, -0.078390,
    -0.031829, 0.026238, -0.084264, -0.026435, 0.012447, -0.085509, -0.026737, 0.016574, -0.090296, -0.031755, 0.022910, -0.072738,
    -0.026069, 0.012015, -0.076342, -0.031847, 0.030191, -0.084852, -0.027968, 0.021859, -0.093185, -0.030391, 0.028307, -0.094942,
    -0.032193, 0.035916, -0.084454, -0.049754, 0.059438, -0.066428, -0.045648, 0.059463, -0.057722, -0.025837, 0.129701, 0.069687,
    -0.025254, 0.134797, 0.078802, -0.028224, 0.129376, 0.060107, -0.032655, 0.126692, 0.068965, -0.061609, 0.073294, -0.085609,
    -0.056669, 0.062297, -0.108311, -0.053812, 0.060427, -0.083037, -0.047107, 0.048601, -0.101348, -0.065806, 0.079839, -0.062041,
    -0.047679, 0.053649, -0.080918, -0.035196, 0.037594, -0.090287, -0.041358, 0.041805, -0.097530, -0.036943, 0.053742, -0.021298,
    -0.028284, 0.046333, -0.036268, -0.034945, 0.054881, -0.036941, -0.030383, 0.046308, -0.023564, -0.037212, 0.057894, -0.002895,
    -0.051954, 0.065457, -0.013720, -0.045496, 0.068218, 0.008505, -0.038032, 0.073596, 0.059348, -0.048538, 0.092496, 0.061834,
    -0.032964, 0.070205, 0.055284, -0.042234, 0.091339, 0.058404, -0.057546, 0.112935, 0.063545, -0.051838, 0.113501, 0.062690,
    -0.044823, 0.127934, 0.067338, -0.055362, 0.116286, 0.058795, -0.036491, 0.129427, 0.064830, -0.042993, 0.120388, 0.064432,
    -0.033103, 0.126745, 0.063980, -0.001592, -0.000999, -0.011317, -0.008343, 0.005523, -0.013499, -0.008366, 0.006200, -0.006270,
    -0.010235, 0.009196, -0.022002, -0.025602, 0.038248, -0.038899, -0.023442, 0.030588, -0.042495, -0.030606, 0.037218, -0.045571,
    -0.025296, 0.036269, -0.028165, -0.028701, 0.029509, -0.048878, -0.020979, 0.026673, -0.033398, -0.024738, 0.032448, -0.013345,
    -0.017752, 0.020088, -0.017756, -0.025182, 0.028116, -0.004943, -0.029220, 0.035144, -0.002225, -0.031056, 0.039141, -0.004052,
    -0.029406, 0.040657, -0.009020, -0.032000, 0.049111, -0.007115, -0.023982, 0.034350, -0.019992, -0.052349, 0.068674, -0.031112,
    -0.040337, 0.057106, -0.050277, -0.064296, 0.077781, -0.039775, -0.072480, 0.081664, -0.007085, -0.035418, 0.024779, -0.133743,
    -0.027055, 0.019418, -0.066012, -0.031483, 0.024444, -0.068397, -0.054087, 0.093534, 0.047046, -0.054659, 0.108954, 0.050550,
    -0.081282, 0.095609, -0.014509, -0.084664, 0.104530, 0.009774, -0.026866, 0.030675, -0.002179, -0.028237, 0.033154, 0.001248,
    -0.031538, 0.039594, 0.002311, -0.065982, 0.131015, 0.067799, -0.064302, 0.133230, 0.067138, -0.051976, 0.132431, 0.069209,
    -0.056729, 0.127147, 0.067334, -0.027238, 0.014796, -0.131567, -0.041233, 0.132332, 0.066642, -0.016096, 0.014918, -0.009627,
    -0.030126, 0.132832, 0.058838, -0.036784, 0.121362, 0.067139, -0.031908, 0.131957, 0.065557, -0.047150, 0.124323, 0.065769,
    -0.044393, 0.073583, 0.035674, -0.033361, 0.058556, 0.023233, -0.056909, 0.086464, 0.025585, -0.073111, 0.087043, -0.038795,
    -0.017496, 0.030475, 0.012045, -0.034216, 0.046310, 0.004369, -0.031365, 0.044287, -0.043802, -0.034785, 0.050854, -0.045285,
    -0.055818, 0.068926, -0.061429, -0.029806, 0.032014, -0.052408, -0.031643, 0.027201, -0.061797, -0.028951, 0.025757, -0.056781,
    -0.016026, 0.017813, -0.039564, -0.018847, 0.022098, -0.048875, -0.016595, 0.016610, -0.012403, -0.025642, 0.022104, -0.054408,
    -0.076417, 0.085720, -0.014901, -0.008197, 0.009458, 0.002820, -0.019058, 0.024418, -0.025751, -0.064852, 0.119767, 0.049306,
    -0.080621, 0.113410, 0.031320, -0.045145, 0.127036, 0.065958, -0.079913, 0.100099, 0.020958, -0.064088, 0.106986, 0.041266,
    -0.082936, 0.094347, 0.005092, -0.033888, 0.130999, 0.073953, -0.043693, 0.121404, 0.063011, -0.065795, 0.080407, 0.006613,
    -0.032590, 0.039600, 0.003069, -0.032590, 0.039600, 0.003069, -0.032590, 0.039600, 0.003069, -0.032590, 0.039600, 0.003069,
    -0.032590, 0.039600, 0.003069, -0.032590, 0.039600, 0.003069, -0.032590, 0.039600, 0.003069, -0.032590, 0.039600, 0.003069,
    -0.032590, 0.039600, 0.003069, -0.041678, 0.039790, 0.009909, -0.041678, 0.039790, 0.009909, -0.041678, 0.039790, 0.009909,
    -0.041678, 0.039790, 0.009909, -0.041678, 0.039790, 0.009909, -0.041678, 0.039790, 0.009909, -0.041678, 0.039790, 0.009909,
    -0.041678, 0.039790, 0.009909, -0.041678, 0.039790, 0.009909
  ]),
  // 11 — vis_mm
  new Float32Array([
    -0.021628, 0.040941, 0.015588, -0.028395, 0.036891, 0.021724, -0.018762, 0.041857, 0.009683, -0.000084, -0.000312, -0.003576,
    -0.004620, 0.005251, -0.007961, -0.019872, 0.011245, 0.016050, -0.014094, 0.017318, -0.010599, -0.009596, 0.011395, -0.012678,
    -0.019047, 0.021165, -0.024037, -0.023534, 0.021520, -0.030094, -0.014217, 0.016846, -0.018650, -0.047687, 0.040839, 0.012843,
    -0.049154, 0.041457, 0.013204, -0.045855, 0.040104, 0.011725, -0.039500, 0.051503, -0.066677, -0.033705, 0.049564, -0.051214,
    -0.052432, 0.054200, -0.077589, -0.025757, 0.051073, 0.020456, -0.034119, 0.048090, 0.025592, -0.023276, 0.052410, 0.014039,
    -0.028235, 0.054942, 0.035400, -0.037059, 0.052233, 0.039464, -0.024102, 0.055892, 0.027792, -0.028662, 0.060238, 0.046086,
    -0.038187, 0.056988, 0.049470, -0.022088, 0.061281, 0.037730, -0.054562, 0.059183, 0.014906, -0.046262, 0.055045, -0.001325,
    -0.060642, 0.061612, 0.030611, -0.046117, 0.041529, 0.011799, -0.038475, 0.033515, 0.027489, -0.048488, 0.030256, 0.030646,
    -0.057223, 0.026144, 0.033597, -0.048047, 0.031988, 0.005586, -0.044077, 0.026282, -0.005337, -0.040167, 0.020039, -0.013558,
    -0.034541, 0.011488, -0.020988, -0.039076, 0.050967, -0.018322, -0.028756, 0.005959, -0.027974, -0.044726, 0.062006, 0.077904,
    -0.051858, 0.063588, 0.070752, -0.038297, 0.060002, 0.079701, -0.064118, 0.022819, 0.039135, -0.061704, 0.032399, 0.039124,
    -0.046035, 0.030104, -0.004985, -0.049815, 0.036151, 0.007828, -0.041577, 0.023831, -0.015139, -0.035753, 0.014148, -0.024550,
    -0.030469, 0.005486, -0.034307, -0.034114, 0.048787, -0.035463, -0.065264, 0.026913, 0.035087, -0.060369, 0.035485, 0.035747,
    -0.059420, 0.032286, 0.031599, -0.051317, 0.037981, 0.029277, -0.042054, 0.043930, 0.027953, -0.047107, 0.050348, 0.046654,
    -0.046071, 0.047543, 0.039569, -0.054927, 0.041737, 0.038060, -0.055924, 0.041523, 0.043211, -0.059668, 0.063770, 0.063429,
    -0.065486, 0.063755, 0.055910, -0.065078, 0.063568, 0.045843, -0.029826, 0.031054, -0.006095, -0.032512, 0.032717, -0.004966,
    -0.030649, 0.033220, -0.005535, -0.035480, 0.058794, 0.078837, -0.034500, 0.058240, 0.077760, -0.043917, 0.039489, 0.009309,
    -0.041428, 0.039107, 0.005847, -0.038735, 0.038062, 0.001710, -0.029801, 0.048157, 0.074941, -0.030536, 0.045792, 0.066734,
    -0.031116, 0.052347, 0.078034, -0.033466, 0.056600, 0.078125, -0.030945, 0.045383, 0.055698, -0.035357, 0.035362, -0.001940,
    -0.033419, 0.038400, -0.003421, -0.036376, 0.043352, -0.000723, -0.037030, 0.044679, 0.002880, -0.039727, 0.043826, 0.006813,
    -0.043499, 0.042337, 0.010091, -0.047681, 0.041273, -0.053712, -0.048562, 0.041457, -0.056646, -0.046829, 0.040605, -0.051790,
    -0.102592, 0.068933, -0.087542, -0.092916, 0.065115, -0.093165, -0.107154, 0.071813, -0.076455, -0.049531, 0.041820, -0.055361,
    -0.019922, 0.042759, 0.002855, -0.021339, 0.043022, -0.003077, -0.022816, 0.042090, -0.007810, -0.053931, 0.026082, -0.075140,
    -0.044245, 0.018765, -0.076532, -0.035127, 0.011078, -0.070835, -0.028619, 0.002581, -0.060449, -0.080174, 0.060681, -0.093205,
    -0.024993, -0.002047, -0.047317, -0.086889, 0.062682, -0.016793, -0.092930, 0.067346, -0.029610, -0.078131, 0.055618, -0.005932,
    -0.024080, 0.041608, -0.006472, -0.021584, 0.048229, 0.000994, -0.050175, 0.022075, -0.080360, -0.062389, 0.031104, -0.077976,
    -0.037940, 0.014745, -0.075266, -0.028402, 0.004812, -0.064543, -0.023471, -0.002216, -0.054576, -0.064604, 0.056075, -0.087868,
    -0.022825, 0.044593, -0.004006, -0.023958, 0.049623, 0.001650, -0.024209, 0.047290, -0.003115, -0.023425, 0.050064, 0.001751,
    -0.023376, 0.052207, 0.007395, -0.018696, 0.058968, 0.024159, -0.022702, 0.055455, 0.019178, -0.022360, 0.053189, 0.010936,
    -0.018804, 0.053931, 0.013509, -0.096915, 0.069781, -0.041424, -0.100666, 0.071450, -0.053135, -0.105580, 0.072180, -0.063567,
    -0.051183, 0.027963, -0.037588, -0.049605, 0.030888, -0.038987, -0.052580, 0.029915, -0.040493, -0.066666, 0.050764, 0.006306,
    -0.053942, 0.049040, 0.018337, -0.046106, 0.040142, -0.049210, -0.046321, 0.039864, -0.046016, -0.047245, 0.038414, -0.042781,
    -0.035135, 0.046799, 0.042785, -0.043799, 0.048578, 0.030420, -0.048790, 0.034977, -0.040365, -0.054676, 0.034926, -0.042682,
    -0.054068, 0.040635, -0.046624, -0.053707, 0.042885, -0.050456, -0.052119, 0.043186, -0.053713, -0.050551, 0.042507, -0.054463,
    0.000000, 0.000000, -0.000000, -0.001460, -0.000160, 0.002812, -0.000989, 0.001293, -0.002863, -0.009507, 0.015969, 0.009953,
    -0.005700, 0.008710, 0.007866, -0.014910, 0.025502, 0.012975, -0.020117, 0.024387, 0.019286, -0.006808, 0.010090, 0.004921,
    -0.015021, 0.027748, 0.007076, -0.011246, 0.011972, -0.007086, -0.006331, 0.005560, -0.002975, -0.015563, 0.017254, -0.013823,
    -0.015627, 0.014083, -0.001340, -0.005424, 0.006751, -0.011971, -0.020118, 0.021732, -0.018291, -0.022640, 0.022190, -0.028159,
    -0.016499, 0.018029, -0.022217, -0.024316, 0.010292, -0.037851, -0.026250, 0.008415, -0.046736, -0.024508, 0.020039, -0.021839,
    -0.030815, 0.016646, -0.034128, -0.031026, 0.032382, -0.046331, -0.033515, 0.034914, -0.058014, -0.040255, 0.034847, -0.069480,
    -0.022585, 0.044508, 0.016511, -0.024096, 0.048662, 0.018097, -0.030157, 0.041434, 0.022282, -0.019479, 0.046134, 0.010185,
    -0.032204, 0.045859, 0.023667, -0.021168, 0.050126, 0.011946, -0.027739, 0.056172, 0.038518, -0.028034, 0.057829, 0.041729,
    -0.037452, 0.054549, 0.045825, -0.037156, 0.053102, 0.042399, -0.021989, 0.058698, 0.033292, -0.022548, 0.057148, 0.030318,
    -0.029675, 0.048070, 0.053185, -0.035297, 0.047677, 0.056614, -0.028063, 0.045731, 0.057196, -0.031746, 0.047034, 0.061907,
    -0.024280, 0.048792, 0.045632, -0.023758, 0.046425, 0.048479, -0.003066, 0.004215, 0.002264, -0.003428, 0.003892, 0.004797,
    -0.004313, 0.005132, 0.000062, -0.007021, 0.007973, 0.008605, -0.014365, 0.013967, 0.011749, -0.011026, 0.002850, 0.010866,
    -0.013922, 0.015334, 0.012796, -0.016653, 0.007392, 0.012697, -0.005122, 0.004782, 0.006148, -0.006131, 0.008653, 0.008197,
    -0.043276, 0.042507, -0.005908, -0.049346, 0.047838, 0.008491, -0.038504, 0.037119, 0.003180, -0.041252, 0.038567, 0.006831,
    -0.034326, 0.033555, -0.003135, -0.039795, 0.035944, 0.009208, -0.037094, 0.035137, 0.004825, -0.042752, 0.038396, 0.010209,
    -0.041558, 0.036269, 0.014716, -0.045973, 0.039418, 0.013514, -0.044615, 0.038460, 0.019515, -0.049145, 0.040355, 0.015483,
    -0.045765, 0.040211, 0.020120, -0.051226, 0.040374, 0.014543, -0.049271, 0.040750, 0.017028, -0.030828, 0.030858, -0.005521,
    -0.032616, 0.032445, -0.003186, -0.039238, 0.030822, 0.001079, -0.037226, 0.029816, -0.006134, -0.042750, 0.031834, 0.005186,
    -0.037417, 0.021156, -0.014183, -0.040279, 0.024223, -0.007773, -0.034619, 0.029384, -0.008394, -0.032859, 0.020547, -0.017105,
    -0.046247, 0.033453, 0.008360, -0.043162, 0.027598, -0.001346, -0.046757, 0.031709, 0.003713, -0.049640, 0.036344, 0.011513,
    -0.043967, 0.040546, 0.027067, -0.041491, 0.036951, 0.026555, -0.032738, 0.047963, 0.067023, -0.030220, 0.049644, 0.073496,
    -0.027559, 0.047014, 0.071052, -0.036456, 0.045563, 0.060240, -0.028887, 0.046802, 0.066588, -0.035321, 0.046368, 0.065704,
    -0.053327, 0.053105, 0.035355, -0.052287, 0.051257, 0.021986, -0.048773, 0.045547, 0.027268, -0.049723, 0.042035, 0.016987,
    -0.050709, 0.051436, 0.046987, -0.047569, 0.042025, 0.021642, -0.048662, 0.036792, 0.011562, -0.048215, 0.037670, 0.013662,
    -0.034302, 0.027944, 0.022062, -0.035370, 0.030202, 0.008574, -0.038261, 0.030386, 0.016875, -0.032440, 0.026447, 0.014058,
    -0.032617, 0.025817, 0.024140, -0.037486, 0.029070, 0.033222, -0.037238, 0.027400, 0.030239, -0.040703, 0.041083, 0.027061,
    -0.049728, 0.035850, 0.029207, -0.039423, 0.037275, 0.027095, -0.048854, 0.032955, 0.029594, -0.058670, 0.030080, 0.031353,
    -0.057968, 0.027872, 0.032661, -0.051708, 0.032655, 0.045927, -0.052484, 0.026299, 0.045742, -0.046586, 0.038809, 0.048638,
    -0.042006, 0.037362, 0.053614, -0.040265, 0.042080, 0.056655, -0.001956, 0.000801, 0.000006, -0.006696, 0.002300, 0.005112,
    -0.005224, 0.000615, 0.006234, -0.011439, 0.007810, 0.002855, -0.030895, 0.028843, 0.001367, -0.027383, 0.027427, -0.003827,
    -0.030302, 0.030276, -0.001430, -0.029191, 0.024180, 0.007123, -0.027228, 0.027434, -0.007320, -0.024559, 0.022112, 0.000537,
    -0.023752, 0.014207, 0.014862, -0.018227, 0.010133, 0.009024, -0.017780, 0.009187, 0.015256, -0.019129, 0.012658, 0.016578,
    -0.021605, 0.015229, 0.018406, -0.025378, 0.017467, 0.017644, -0.028903, 0.021849, 0.018940, -0.027764, 0.019263, 0.010176,
    -0.040775, 0.031832, 0.031897, -0.038927, 0.033502, 0.021654, -0.042737, 0.039978, 0.044417, -0.040136, 0.034835, 0.046113,
    -0.038491, 0.037761, -0.019161, -0.029668, 0.025647, -0.016219, -0.030596, 0.029500, -0.010725, -0.047043, 0.025263, 0.036280,
    -0.042954, 0.031294, 0.052271, -0.041730, 0.050819, 0.062764, -0.037184, 0.050373, 0.069909, -0.017859, 0.010199, 0.015947,
    -0.017144, 0.013204, 0.013621, -0.019539, 0.016431, 0.016745, -0.065105, 0.025352, 0.035623, -0.064633, 0.024054, 0.036713,
    -0.061671, 0.033110, 0.037591, -0.061401, 0.034099, 0.036759, -0.033025, 0.032919, -0.033793, -0.056060, 0.040913, 0.042067,
    -0.011361, 0.003234, 0.010536, -0.007449, 0.001964, 0.007609, -0.041518, 0.043530, 0.052602, -0.046842, 0.047818, 0.041890,
    -0.047229, 0.048713, 0.044766, -0.055402, 0.041085, 0.039612, -0.038581, 0.025992, 0.029585, -0.029411, 0.024955, 0.025889,
    -0.043206, 0.027413, 0.039534, -0.046309, 0.050634, 0.054984, -0.005875, 0.009288, 0.006512, -0.014101, 0.016763, 0.015465,
    -0.022921, 0.020270, 0.019777, -0.034555, 0.032537, 0.006123, -0.037400, 0.033008, 0.012877, -0.045164, 0.042676, 0.036420,
    -0.029760, 0.029852, -0.006494, -0.029168, 0.028968, -0.008768, -0.027949, 0.027913, -0.012171, -0.019980, 0.019442, -0.006619,
    -0.023703, 0.024780, -0.010889, -0.013662, 0.005889, 0.010821, -0.025690, 0.024947, -0.014082, -0.040553, 0.040371, 0.052776,
    -0.004098, 0.003917, 0.005512, -0.022223, 0.017014, 0.004100, -0.034010, 0.049125, 0.072489, -0.034457, 0.049218, 0.072988,
    -0.034318, 0.050862, 0.071505, -0.037823, 0.040745, 0.060554, -0.037903, 0.038543, 0.058406, -0.038791, 0.042246, 0.059652,
    -0.032752, 0.051361, 0.072794, -0.027814, 0.043845, 0.063210, -0.036985, 0.043537, 0.062817, -0.026720, 0.043996, 0.059133,
    -0.026672, 0.044443, 0.052452, -0.040006, 0.031618, 0.044617, -0.011226, 0.012774, -0.016410, -0.023816, 0.045170, 0.049069,
    -0.014346, 0.016030, -0.017271, -0.008962, 0.010968, 0.001460, -0.012965, 0.019205, 0.001561, -0.008288, 0.007947, -0.007393,
    -0.012364, 0.019824, 0.002983, -0.011781, 0.013596, -0.006190, -0.007750, 0.007101, -0.001043, -0.006925, 0.005744, -0.005246,
    -0.008087, 0.010844, 0.003211, -0.072973, 0.047308, -0.087312, -0.084287, 0.052540, -0.083442, -0.045705, 0.037233, -0.040251,
    -0.045952, 0.038867, -0.045082, -0.046475, 0.033078, -0.037437, -0.044324, 0.037915, -0.042773, -0.042791, 0.035973, -0.036303,
    -0.047897, 0.039225, -0.049667, -0.048552, 0.039002, -0.048411, -0.049609, 0.040306, -0.053354, -0.053055, 0.041007, -0.052643,
    -0.050212, 0.041211, -0.055274, -0.059173, 0.042419, -0.056185, -0.050675, 0.040425, -0.058553, -0.059302, 0.041010, -0.060936,
    -0.047115, 0.029096, -0.036082, -0.043241, 0.032163, -0.034286, -0.046832, 0.027107, -0.053900, -0.046598, 0.025336, -0.049695,
    -0.046904, 0.029166, -0.058009, -0.038185, 0.014712, -0.055426, -0.039921, 0.019039, -0.062158, -0.047019, 0.025043, -0.043677,
    -0.038624, 0.013230, -0.045329, -0.047541, 0.032416, -0.059588, -0.043606, 0.024037, -0.066790, -0.047935, 0.028830, -0.069100,
    -0.049312, 0.035774, -0.059937, -0.067720, 0.043888, -0.057643, -0.058492, 0.041382, -0.053016, -0.027405, 0.044697, 0.034894,
    -0.031385, 0.044595, 0.039851, -0.025127, 0.044434, 0.031638, -0.035286, 0.042701, 0.022505, -0.095299, 0.060770, -0.065997,
    -0.091461, 0.057889, -0.075599, -0.080515, 0.047473, -0.065166, -0.073845, 0.041791, -0.072630, -0.091397, 0.058913, -0.057433,
    -0.070734, 0.043053, -0.063626, -0.054534, 0.034948, -0.065618, -0.063172, 0.035976, -0.070683, -0.030914, 0.033134, -0.022615,
    -0.032239, 0.034494, -0.030256, -0.035727, 0.035657, -0.036102, -0.027888, 0.031132, -0.020200, -0.024842, 0.031855, -0.012299,
    -0.035551, 0.034232, -0.024920, -0.028294, 0.033056, -0.015947, -0.022098, 0.049589, 0.005097, -0.022752, 0.047911, 0.000139,
    -0.020603, 0.046018, 0.003774, -0.021873, 0.045251, -0.001560, -0.023516, 0.045659, -0.004575, -0.022590, 0.043746, -0.006527,
    -0.024634, 0.040267, 0.002705, -0.026209, 0.035572, -0.006751, -0.022709, 0.042669, 0.013763, -0.034607, 0.040364, 0.004091,
    -0.029726, 0.042855, 0.018059, -0.001500, 0.002061, -0.007053, -0.005066, 0.005268, -0.010835, -0.004098, 0.003680, -0.006207,
    -0.009302, 0.010222, -0.013230, -0.031351, 0.031820, -0.027765, -0.029949, 0.029175, -0.027270, -0.038656, 0.030821, -0.031779,
    -0.025184, 0.027642, -0.020461, -0.036846, 0.026938, -0.030584, -0.023091, 0.024665, -0.021432, -0.015769, 0.019858, -0.012678,
    -0.012113, 0.014392, -0.013027, -0.012975, 0.015322, -0.007382, -0.015406, 0.019307, -0.005631, -0.017118, 0.022147, -0.007832,
    -0.018768, 0.023738, -0.010812, -0.021408, 0.027892, -0.009436, -0.019599, 0.023590, -0.015153, -0.044256, 0.038012, -0.041719,
    -0.048772, 0.038401, -0.046954, -0.069178, 0.046592, -0.050368, -0.051605, 0.040393, -0.033391, -0.060726, 0.042237, -0.084726,
    -0.041067, 0.020841, -0.036855, -0.047827, 0.025249, -0.040312, -0.024451, 0.033244, -0.011972, -0.036255, 0.036575, -0.007698,
    -0.084110, 0.055057, -0.034781, -0.079852, 0.052694, -0.023453, -0.013671, 0.016529, -0.004038, -0.014818, 0.019032, -0.002362,
    -0.016832, 0.022397, -0.002004, -0.023363, 0.043612, -0.004610, -0.023699, 0.042658, -0.005578, -0.021262, 0.048698, 0.002188,
    -0.021801, 0.049035, 0.002575, -0.047877, 0.036432, -0.078601, -0.019148, 0.053380, 0.014113, -0.007620, 0.008038, -0.009201,
    -0.021452, 0.045991, 0.026730, -0.020304, 0.056098, 0.021067, -0.019117, 0.057051, 0.022831, -0.020567, 0.053179, 0.012508,
    -0.022817, 0.032570, -0.008704, -0.019974, 0.030738, -0.003201, -0.031488, 0.033615, -0.016245, -0.087331, 0.056644, -0.046673,
    -0.011983, 0.020783, 0.005356, -0.018947, 0.026169, -0.001062, -0.039395, 0.034441, -0.034234, -0.042115, 0.036463, -0.039880,
    -0.075355, 0.049388, -0.058109, -0.042595, 0.028335, -0.031925, -0.048099, 0.026034, -0.034936, -0.041791, 0.024894, -0.031798,
    -0.020226, 0.020990, -0.022224, -0.026387, 0.025698, -0.027639, -0.009284, 0.010560, -0.010766, -0.035524, 0.022834, -0.030832,
    -0.067545, 0.046174, -0.039138, -0.005786, 0.005933, -0.000330, -0.017722, 0.020162, -0.016986, -0.060107, 0.044419, -0.000069,
    -0.070443, 0.047604, -0.011650, -0.049927, 0.045056, 0.013853, -0.059608, 0.042806, -0.018466, -0.049004, 0.039085, -0.009386,
    -0.068076, 0.046069, -0.027995, -0.040236, 0.044762, 0.027376, -0.043274, 0.041710, 0.007274, -0.040987, 0.036323, -0.021700,
    -0.042263, 0.045885, 0.005374, -0.042263, 0.045885, 0.005374, -0.042263, 0.045885, 0.005374, -0.042263, 0.045885, 0.005374,
    -0.042263, 0.045885, 0.005374, -0.042263, 0.045885, 0.005374, -0.042263, 0.045885, 0.005374, -0.042263, 0.045885, 0.005374,
    -0.042263, 0.045885, 0.005374, -0.061778, 0.044408, 0.009707, -0.061778, 0.044408, 0.009707, -0.061778, 0.044408, 0.009707,
    -0.061778, 0.044408, 0.009707, -0.061778, 0.044408, 0.009707, -0.061778, 0.044408, 0.009707, -0.061778, 0.044408, 0.009707,
    -0.061778, 0.044408, 0.009707, -0.061778, 0.044408, 0.009707
  ]),
  // 12 — mouth_open
  new Float32Array([
    -0.010622, 0.020452, 0.064383, -0.012896, 0.023403, 0.062143, -0.010719, 0.027139, 0.057001, 0.000658, 0.001881, -0.009773,
    0.000092, 0.015832, -0.024174, 0.018675, 0.039893, 0.012369, -0.034529, 0.043478, -0.000497, -0.000891, 0.029726, -0.036848,
    -0.002605, 0.052224, -0.062110, -0.003493, 0.059176, -0.074764, -0.001906, 0.041788, -0.051137, -0.016195, 0.080026, -0.038183,
    -0.016660, 0.077042, -0.039188, -0.014645, 0.081750, -0.037673, -0.010163, 0.102071, -0.163978, 0.001922, 0.102226, -0.155660,
    -0.025677, 0.103950, -0.169551, -0.010150, 0.033725, 0.079502, -0.009250, 0.039434, 0.083013, -0.011972, 0.041662, 0.078137,
    -0.009888, 0.235901, 0.053477, -0.013970, 0.233148, 0.053414, -0.007068, 0.237121, 0.049766, -0.009749, 0.261509, 0.023482,
    -0.025018, 0.254105, 0.027278, 0.006146, 0.259285, 0.023648, -0.012239, 0.121477, -0.070592, 0.001175, 0.114884, -0.099641,
    -0.023392, 0.126505, -0.037086, -0.013436, 0.070060, -0.043281, -0.019520, 0.048780, 0.068427, -0.016551, 0.081143, 0.071621,
    -0.008004, 0.116530, 0.077089, -0.002658, 0.081987, -0.060515, 0.003025, 0.077708, -0.069048, 0.007637, 0.074427, -0.077519,
    0.009050, 0.067398, -0.081812, 0.008868, 0.108762, -0.122084, 0.003722, 0.056865, -0.081800, -0.001518, 0.134630, 0.113715,
    -0.015476, 0.127307, 0.088715, 0.006185, 0.142145, 0.123933, 0.010873, 0.158137, 0.079076, -0.008603, 0.185574, 0.066952,
    0.003631, 0.085163, -0.076277, -0.003751, 0.090425, -0.062017, 0.008932, 0.080785, -0.087509, 0.009519, 0.071464, -0.092247,
    0.005015, 0.060932, -0.096979, 0.008422, 0.103712, -0.142790, 0.016588, 0.156020, 0.074883, 0.010552, 0.187904, 0.061316,
    0.006807, 0.117614, 0.078430, 0.005240, 0.085955, 0.082803, -0.003799, 0.058332, 0.084737, -0.028064, 0.236867, 0.032942,
    -0.008674, 0.223511, 0.053824, 0.000574, 0.207604, 0.054750, -0.021272, 0.211936, 0.045307, -0.024973, 0.127292, 0.062178,
    -0.031220, 0.129421, 0.031525, -0.030835, 0.130039, 0.000015, -0.004070, 0.070271, -0.050512, -0.007530, 0.073219, -0.048431,
    -0.002427, 0.067721, -0.052660, 0.001038, 0.143683, 0.117597, -0.012875, 0.147249, 0.105073, -0.013027, 0.082087, -0.038447,
    -0.011683, 0.081342, -0.040051, -0.010736, 0.079706, -0.042975, -0.021614, 0.179930, 0.073015, -0.012169, 0.188346, 0.065723,
    -0.025014, 0.165370, 0.081500, -0.021756, 0.154114, 0.092540, -0.002712, 0.182363, 0.062656, -0.009581, 0.076546, -0.045612,
    -0.000328, 0.063457, -0.055014, 0.001158, 0.059672, -0.055007, -0.000890, 0.058236, -0.053346, -0.005922, 0.059907, -0.049629,
    -0.010662, 0.064618, -0.046625, -0.014676, 0.080976, -0.072020, -0.015915, 0.077042, -0.074721, -0.014813, 0.083684, -0.070000,
    -0.058335, 0.123855, -0.125437, -0.057542, 0.119228, -0.149036, -0.057309, 0.127486, -0.093880, -0.017333, 0.069973, -0.077628,
    -0.005538, 0.056961, 0.057823, -0.007475, 0.091549, 0.057872, -0.014918, 0.129025, 0.060613, -0.033887, 0.079522, -0.102622,
    -0.030951, 0.074508, -0.106007, -0.028131, 0.069287, -0.106628, -0.022279, 0.060957, -0.102017, -0.052118, 0.113401, -0.162146,
    -0.012095, 0.051268, -0.091307, -0.078969, 0.125040, 0.063582, -0.069153, 0.120186, 0.035158, -0.077858, 0.131427, 0.077930,
    -0.035091, 0.171580, 0.061412, -0.012055, 0.198983, 0.053038, -0.036108, 0.082170, -0.115610, -0.040482, 0.087881, -0.111208,
    -0.031547, 0.075643, -0.118309, -0.023432, 0.065076, -0.112837, -0.012503, 0.054977, -0.107278, -0.039360, 0.106587, -0.170621,
    -0.040420, 0.167631, 0.060687, -0.033020, 0.197485, 0.049232, -0.029375, 0.126924, 0.065384, -0.027718, 0.092796, 0.072078,
    -0.018137, 0.063215, 0.076249, 0.010137, 0.247177, 0.026417, -0.012502, 0.230640, 0.047150, -0.022224, 0.216299, 0.045638,
    0.003204, 0.224800, 0.035690, -0.061528, 0.121965, 0.006861, -0.054994, 0.126247, -0.025914, -0.054015, 0.128720, -0.057632,
    -0.018328, 0.073275, -0.066681, -0.015583, 0.077054, -0.066138, -0.020372, 0.069954, -0.070801, -0.060538, 0.135662, 0.078904,
    -0.034326, 0.144614, 0.073464, -0.015373, 0.085280, -0.068224, -0.015596, 0.085279, -0.066556, -0.015602, 0.083940, -0.065929,
    -0.003809, 0.168889, 0.062404, -0.015768, 0.155417, 0.066919, -0.014686, 0.080878, -0.065306, -0.024478, 0.064393, -0.075526,
    -0.027179, 0.059157, -0.078847, -0.025344, 0.057409, -0.080903, -0.021941, 0.059809, -0.080946, -0.018676, 0.064760, -0.079570,
    0.000000, 0.000000, -0.000000, 0.004905, 0.002446, 0.002662, -0.006229, 0.002416, 0.000056, -0.004215, 0.027537, 0.020669,
    0.002444, 0.018085, 0.017181, -0.006329, 0.033340, 0.029863, -0.000584, 0.038330, 0.031414, -0.008334, 0.018162, 0.015728,
    -0.013822, 0.041267, 0.026108, 0.001414, 0.031895, -0.031893, 0.003220, 0.018028, -0.021391, 0.000860, 0.043186, -0.046490,
    0.002991, 0.037326, -0.022562, -0.004117, 0.018518, -0.025001, -0.000612, 0.053726, -0.055853, -0.006590, 0.053651, -0.060805,
    -0.006113, 0.043310, -0.050637, -0.003718, 0.055714, -0.084941, -0.004707, 0.059711, -0.101768, 0.001058, 0.059526, -0.068486,
    -0.012029, 0.057276, -0.074183, 0.003232, 0.081555, -0.129310, -0.007128, 0.081492, -0.135746, -0.019140, 0.081515, -0.141398,
    -0.010145, 0.024246, 0.073711, -0.009991, 0.031154, 0.078395, -0.012653, 0.029589, 0.072240, -0.009648, 0.032635, 0.066907,
    -0.011010, 0.037583, 0.081232, -0.010508, 0.040159, 0.075619, -0.008988, 0.241316, 0.052110, -0.008974, 0.252011, 0.043619,
    -0.021146, 0.246446, 0.041065, -0.017082, 0.236191, 0.050513, 0.003446, 0.251138, 0.036802, -0.001657, 0.240605, 0.046679,
    -0.011699, 0.229486, 0.000129, -0.026280, 0.221733, 0.006098, -0.011339, 0.204153, 0.011511, -0.021234, 0.194521, 0.017018,
    0.002281, 0.224616, 0.002350, -0.001349, 0.196336, 0.010626, -0.001662, 0.007477, 0.007698, 0.002897, 0.009044, 0.009340,
    -0.007076, 0.009019, 0.007020, 0.008768, 0.021363, 0.014535, 0.015269, 0.036684, 0.019615, 0.014269, 0.021547, 0.007766,
    0.011209, 0.036354, 0.021419, 0.018875, 0.032993, 0.011299, 0.008161, 0.014530, 0.009392, 0.006238, 0.020048, 0.015800,
    0.004069, 0.098132, -0.089618, -0.006209, 0.104469, -0.071128, -0.010639, 0.078440, -0.038247, -0.011528, 0.081000, -0.034554,
    -0.009466, 0.074197, -0.041383, -0.008760, 0.079259, -0.021678, -0.008187, 0.076316, -0.028575, -0.011762, 0.082145, -0.031647,
    -0.008263, 0.082369, -0.018810, -0.015001, 0.083084, -0.029642, -0.010925, 0.085265, -0.017096, -0.017571, 0.082158, -0.030454,
    -0.009627, 0.088381, -0.018460, -0.020261, 0.079922, -0.036596, -0.013830, 0.087778, -0.030866, -0.006829, 0.070504, -0.044966,
    -0.007069, 0.072178, -0.034125, -0.004961, 0.060413, -0.059234, -0.000410, 0.060144, -0.062236, -0.009125, 0.062806, -0.053904,
    0.002096, 0.062353, -0.070808, -0.001505, 0.064378, -0.067535, 0.001462, 0.063559, -0.060297, 0.001285, 0.062090, -0.068575,
    -0.013253, 0.066717, -0.049516, -0.004585, 0.067008, -0.061293, -0.009053, 0.072410, -0.055653, -0.016059, 0.074370, -0.044389,
    -0.005128, 0.093565, -0.006647, -0.003963, 0.089430, -0.001464, -0.017230, 0.161796, 0.048207, -0.017097, 0.160754, 0.065566,
    -0.017510, 0.175591, 0.054300, -0.021563, 0.174207, 0.030094, -0.017303, 0.177597, 0.035525, -0.012879, 0.149077, 0.063673,
    -0.018115, 0.113104, -0.009417, -0.013344, 0.110116, -0.041990, -0.010384, 0.100579, -0.015147, -0.007817, 0.097337, -0.045354,
    -0.018530, 0.110266, 0.022701, -0.009155, 0.094242, -0.023158, -0.011804, 0.080220, -0.045493, -0.006903, 0.088690, -0.046077,
    0.004797, 0.071985, 0.009904, -0.003896, 0.070776, -0.009125, -0.002507, 0.076374, 0.002544, 0.002250, 0.065967, 0.002007,
    0.012913, 0.071669, 0.021863, 0.019707, 0.084476, 0.024352, 0.018928, 0.082534, 0.034696, -0.008404, 0.059421, 0.082898,
    -0.001311, 0.088354, 0.081752, -0.014275, 0.054717, 0.077246, -0.009659, 0.084993, 0.077430, 0.004696, 0.121276, 0.079219,
    -0.002179, 0.119907, 0.078839, -0.002466, 0.156729, 0.067742, 0.016243, 0.131731, 0.075655, -0.018256, 0.177896, 0.047723,
    -0.000302, 0.140319, 0.069147, -0.016758, 0.155558, 0.049172, 0.005005, 0.006169, -0.008047, 0.009848, 0.013362, -0.001671,
    0.009971, 0.009350, 0.003084, 0.006548, 0.024884, -0.013359, -0.003199, 0.066348, -0.020182, -0.002078, 0.062424, -0.029081,
    -0.002543, 0.069063, -0.027092, 0.001885, 0.060004, -0.008356, -0.000953, 0.064575, -0.036204, 0.001631, 0.053891, -0.019800,
    0.014582, 0.046438, 0.007984, 0.011130, 0.034207, -0.002275, 0.019659, 0.035828, 0.014462, 0.021786, 0.043261, 0.017757,
    0.021383, 0.049034, 0.018510, 0.015692, 0.054052, 0.014195, 0.012769, 0.062837, 0.015156, 0.006506, 0.053547, -0.001237,
    0.008088, 0.088958, 0.025044, -0.003068, 0.083297, -0.003407, 0.000245, 0.100448, 0.036281, 0.024653, 0.098583, 0.054548,
    0.009890, 0.092843, -0.105193, -0.001955, 0.065534, -0.059778, -0.002334, 0.066085, -0.057013, 0.020174, 0.102864, 0.067792,
    0.017667, 0.123186, 0.074352, -0.005400, 0.114110, 0.074093, 0.007814, 0.121125, 0.094066, 0.020232, 0.038529, 0.016232,
    0.020837, 0.041397, 0.017584, 0.020786, 0.047862, 0.020282, 0.016560, 0.157221, 0.076101, 0.013587, 0.158006, 0.077522,
    -0.002876, 0.186006, 0.063248, 0.003476, 0.186108, 0.061013, 0.009263, 0.085766, -0.119942, -0.013718, 0.209449, 0.046904,
    0.014397, 0.021262, 0.005319, 0.011671, 0.015307, 0.005794, -0.027833, 0.201766, 0.026803, -0.013273, 0.223756, 0.048665,
    -0.021462, 0.230575, 0.040190, -0.004446, 0.206849, 0.050626, 0.012326, 0.080135, 0.056484, 0.007255, 0.062461, 0.042737,
    0.029749, 0.098478, 0.054713, -0.013594, 0.110593, 0.050349, -0.002500, 0.017096, 0.015533, 0.006814, 0.036118, 0.023224,
    0.018572, 0.055051, 0.023378, -0.003588, 0.073120, -0.018316, -0.004082, 0.077083, -0.009676, -0.008786, 0.099618, 0.012542,
    -0.005303, 0.068425, -0.039395, -0.003568, 0.067813, -0.049999, -0.003503, 0.066023, -0.048591, 0.000975, 0.047853, -0.033116,
    -0.001552, 0.057732, -0.041882, 0.013459, 0.025929, 0.003568, -0.000944, 0.061698, -0.047366, 0.011503, 0.103268, 0.062365,
    0.005793, 0.011276, 0.009685, 0.004498, 0.045665, -0.011126, 0.014441, 0.134555, 0.094347, 0.020860, 0.128990, 0.099647,
    -0.002992, 0.141018, 0.087018, 0.028269, 0.115878, 0.080168, 0.021188, 0.123972, 0.077790, 0.018595, 0.109973, 0.078355,
    -0.011069, 0.148275, 0.077100, -0.010735, 0.185069, 0.046556, -0.001079, 0.138822, 0.077565, -0.010299, 0.187242, 0.027480,
    -0.003954, 0.177080, 0.044252, 0.032867, 0.096586, 0.047305, -0.004810, 0.032110, -0.036329, -0.003389, 0.178781, 0.026294,
    -0.011058, 0.038013, -0.030416, -0.016777, 0.021100, 0.011050, -0.026852, 0.039206, 0.014486, -0.022738, 0.021314, -0.001190,
    -0.023112, 0.039048, 0.016482, -0.031265, 0.035088, 0.002154, -0.015600, 0.013684, 0.005824, -0.018841, 0.014503, -0.000485,
    -0.013315, 0.020192, 0.013377, -0.047333, 0.099537, -0.132777, -0.050788, 0.104341, -0.119486, -0.016639, 0.081413, -0.060480,
    -0.017963, 0.083176, -0.060967, -0.014872, 0.077543, -0.058827, -0.023305, 0.080124, -0.048038, -0.020075, 0.077846, -0.049174,
    -0.019775, 0.083433, -0.062323, -0.027480, 0.082130, -0.051026, -0.018851, 0.083478, -0.064171, -0.028427, 0.084099, -0.054107,
    -0.017809, 0.082097, -0.066728, -0.033758, 0.086364, -0.057811, -0.015274, 0.079230, -0.074036, -0.027789, 0.085223, -0.071172,
    -0.015125, 0.073970, -0.060594, -0.017940, 0.073895, -0.049835, -0.020916, 0.058942, -0.087645, -0.024747, 0.058362, -0.084637,
    -0.018309, 0.062251, -0.086688, -0.022904, 0.058514, -0.091925, -0.021219, 0.061699, -0.095336, -0.024301, 0.062747, -0.078505,
    -0.019181, 0.058937, -0.083193, -0.016329, 0.067069, -0.084565, -0.021093, 0.065896, -0.094960, -0.021342, 0.071661, -0.093105,
    -0.016275, 0.074016, -0.081098, -0.046274, 0.090561, -0.050202, -0.041585, 0.087587, -0.042341, -0.007355, 0.163551, 0.032001,
    -0.008251, 0.163075, 0.048125, -0.003136, 0.177066, 0.017724, -0.018758, 0.149853, 0.043063, -0.053926, 0.111187, -0.062556,
    -0.052111, 0.109370, -0.093554, -0.049074, 0.097483, -0.065825, -0.044523, 0.094649, -0.091738, -0.057082, 0.106265, -0.032006,
    -0.041426, 0.091176, -0.067085, -0.024856, 0.078552, -0.085347, -0.037077, 0.085850, -0.090448, -0.031445, 0.072676, -0.011655,
    -0.022346, 0.072128, -0.028615, -0.027185, 0.076564, -0.024254, -0.027699, 0.068172, -0.014567, -0.038916, 0.074169, 0.005240,
    -0.051102, 0.081661, -0.003472, -0.046950, 0.081812, 0.014139, -0.014762, 0.064984, 0.073625, -0.021421, 0.095741, 0.070184,
    -0.009281, 0.061271, 0.067299, -0.013609, 0.094211, 0.064709, -0.027301, 0.131593, 0.065042, -0.020209, 0.131395, 0.063468,
    -0.023205, 0.163047, 0.050610, -0.045060, 0.135223, 0.053892, -0.005997, 0.183362, 0.035132, -0.032201, 0.141628, 0.048416,
    -0.012039, 0.158967, 0.033468, -0.003777, 0.006669, -0.011421, -0.013105, 0.013854, -0.010102, -0.013745, 0.008854, -0.002934,
    -0.012005, 0.026088, -0.021053, -0.019252, 0.067430, -0.034911, -0.015663, 0.063110, -0.041085, -0.021928, 0.069332, -0.042572,
    -0.023111, 0.061428, -0.021878, -0.018828, 0.064716, -0.047270, -0.017758, 0.054698, -0.030723, -0.032966, 0.050319, -0.005375,
    -0.023151, 0.036439, -0.013086, -0.033876, 0.038677, 0.003510, -0.037577, 0.047117, 0.007124, -0.040228, 0.053601, 0.006054,
    -0.036706, 0.058375, 0.000601, -0.037087, 0.067378, 0.002401, -0.026887, 0.055845, -0.013546, -0.046832, 0.086777, -0.011902,
    -0.035652, 0.082894, -0.038330, -0.060508, 0.096605, -0.012855, -0.073397, 0.093406, 0.014977, -0.041834, 0.094266, -0.139564,
    -0.015588, 0.064406, -0.070552, -0.018326, 0.066911, -0.072075, -0.047082, 0.104445, 0.047796, -0.053119, 0.121498, 0.048252,
    -0.072631, 0.106304, 0.023099, -0.082628, 0.112026, 0.045552, -0.034096, 0.041104, 0.005927, -0.034108, 0.044026, 0.009976,
    -0.037025, 0.052006, 0.011470, -0.040851, 0.169445, 0.061345, -0.037858, 0.170884, 0.061682, -0.017558, 0.198445, 0.050709,
    -0.024724, 0.197100, 0.049006, -0.030377, 0.085551, -0.143342, -0.004956, 0.221027, 0.037711, -0.022258, 0.021458, -0.004344,
    0.004816, 0.207268, 0.017971, -0.005799, 0.231829, 0.041819, 0.003341, 0.239708, 0.033103, -0.015647, 0.216618, 0.041505,
    -0.036461, 0.081714, 0.040485, -0.027685, 0.065779, 0.030308, -0.061710, 0.095600, 0.029858, -0.063042, 0.104374, -0.003064,
    -0.018831, 0.039235, 0.018453, -0.038006, 0.059841, 0.013887, -0.024595, 0.073793, -0.038598, -0.028735, 0.077345, -0.036379,
    -0.051422, 0.096445, -0.039291, -0.016566, 0.069948, -0.052139, -0.017338, 0.070231, -0.063276, -0.015455, 0.066564, -0.057688,
    -0.011455, 0.048290, -0.040995, -0.010795, 0.057820, -0.050705, -0.022182, 0.027187, -0.006981, -0.015109, 0.061141, -0.055990,
    -0.073994, 0.097967, 0.015031, -0.011364, 0.011062, 0.006813, -0.019158, 0.047061, -0.022385, -0.067610, 0.127949, 0.056932,
    -0.085115, 0.119825, 0.055142, -0.040672, 0.139046, 0.057956, -0.085078, 0.108556, 0.040740, -0.066821, 0.118594, 0.045907,
    -0.083675, 0.103122, 0.033777, -0.023081, 0.148831, 0.053918, -0.038233, 0.137615, 0.051834, -0.072393, 0.091289, 0.016373,
    -0.008958, 0.082054, 0.003862, -0.008958, 0.082054, 0.003862, -0.008958, 0.082054, 0.003862, -0.008958, 0.082054, 0.003862,
    -0.008958, 0.082054, 0.003862, -0.008958, 0.082054, 0.003862, -0.008958, 0.082054, 0.003862, -0.008958, 0.082054, 0.003862,
    -0.008958, 0.082054, 0.003862, -0.024284, 0.083078, 0.013487, -0.024284, 0.083078, 0.013487, -0.024284, 0.083078, 0.013487,
    -0.024284, 0.083078, 0.013487, -0.024284, 0.083078, 0.013487, -0.024284, 0.083078, 0.013487, -0.024284, 0.083078, 0.013487,
    -0.024284, 0.083078, 0.013487, -0.024284, 0.083078, 0.013487
  ]),
  // 13 — vis_oo
  new Float32Array([
    -0.022921, 0.025072, 0.047933, -0.015690, 0.019744, 0.048965, -0.032068, 0.024736, 0.039948, 0.001486, -0.002054, -0.006280,
    -0.001837, 0.001254, -0.014282, -0.004128, 0.023684, 0.012445, -0.028247, 0.027026, -0.008947, -0.006288, 0.005106, -0.020635,
    -0.014460, 0.010846, -0.034560, -0.018213, 0.009742, -0.039855, -0.010605, 0.008324, -0.028248, -0.044664, 0.028221, -0.001895,
    -0.045655, 0.027951, -0.001685, -0.043424, 0.028049, -0.002154, -0.023218, 0.020199, -0.086174, -0.020069, 0.019042, -0.074107,
    -0.032472, 0.022603, -0.095424, -0.030373, 0.073880, 0.049418, -0.021715, 0.072751, 0.055774, -0.043090, 0.076891, 0.047593,
    -0.033778, 0.082922, 0.066193, -0.026044, 0.080475, 0.069532, -0.043653, 0.084915, 0.062136, -0.039514, 0.158885, 0.042990,
    -0.044382, 0.153831, 0.048917, -0.034455, 0.158702, 0.041462, -0.057586, 0.039417, -0.014005, -0.043944, 0.031143, -0.030132,
    -0.067766, 0.046778, 0.003006, -0.042667, 0.026596, -0.003365, -0.018414, 0.040839, 0.057684, -0.016273, 0.063752, 0.069568,
    -0.010328, 0.084357, 0.081402, -0.041038, 0.018466, -0.012073, -0.034655, 0.012934, -0.018574, -0.029692, 0.008318, -0.024926,
    -0.026510, 0.001456, -0.030671, -0.031418, 0.023981, -0.043917, -0.023002, -0.003371, -0.036206, -0.050111, 0.079102, 0.075863,
    -0.062151, 0.069738, 0.062732, -0.042559, 0.088180, 0.081745, -0.000240, 0.103375, 0.087994, -0.018561, 0.109992, 0.083072,
    -0.035550, 0.014682, -0.021364, -0.043580, 0.022142, -0.013886, -0.028999, 0.009235, -0.029686, -0.024750, 0.001480, -0.036283,
    -0.020794, -0.005472, -0.042941, -0.022857, 0.019591, -0.060633, 0.000481, 0.098828, 0.084956, -0.002650, 0.091547, 0.079487,
    -0.000121, 0.090759, 0.077489, -0.005247, 0.083746, 0.070932, -0.012713, 0.076634, 0.063379, -0.042899, 0.140124, 0.059121,
    -0.017326, 0.081199, 0.072353, -0.008812, 0.085835, 0.075135, -0.033557, 0.122260, 0.069326, -0.072345, 0.063938, 0.050917,
    -0.077477, 0.059307, 0.036872, -0.075738, 0.054296, 0.021370, -0.037353, 0.016537, -0.016126, -0.040297, 0.018614, -0.014800,
    -0.036593, 0.017867, -0.015619, -0.043617, 0.095087, 0.083304, -0.050572, 0.102784, 0.081633, -0.042850, 0.027631, -0.003546,
    -0.042930, 0.026406, -0.005844, -0.042998, 0.024685, -0.009194, -0.053673, 0.121245, 0.059773, -0.047605, 0.125195, 0.051232,
    -0.055501, 0.114391, 0.068811, -0.054715, 0.108587, 0.075944, -0.039044, 0.123335, 0.044279, -0.042353, 0.021506, -0.011899,
    -0.036323, 0.021475, -0.014395, -0.036904, 0.024993, -0.011981, -0.036280, 0.026605, -0.009718, -0.037323, 0.026680, -0.006846,
    -0.039994, 0.026371, -0.004886, -0.043199, 0.028062, -0.055001, -0.044896, 0.027951, -0.057447, -0.041783, 0.027873, -0.052862,
    -0.072097, 0.045950, -0.096838, -0.063019, 0.038899, -0.104761, -0.079208, 0.052856, -0.083530, -0.045820, 0.027002, -0.057011,
    -0.040305, 0.050595, 0.039285, -0.054064, 0.076748, 0.044796, -0.070441, 0.100170, 0.050981, -0.045042, 0.021394, -0.076804,
    -0.037901, 0.015673, -0.074999, -0.032233, 0.009369, -0.069914, -0.026637, 0.000794, -0.061807, -0.053402, 0.031853, -0.105104,
    -0.021682, -0.005153, -0.051049, -0.096613, 0.080444, -0.000193, -0.096363, 0.070982, -0.018775, -0.088301, 0.089970, 0.012417,
    -0.089485, 0.121353, 0.054852, -0.069008, 0.125664, 0.056092, -0.040356, 0.017679, -0.081368, -0.050089, 0.025469, -0.083097,
    -0.032209, 0.009930, -0.077142, -0.025214, 0.000574, -0.068161, -0.020195, -0.007127, -0.058939, -0.041485, 0.025574, -0.103174,
    -0.089316, 0.115137, 0.057249, -0.083215, 0.104320, 0.055619, -0.085087, 0.104117, 0.052767, -0.073232, 0.094441, 0.051332,
    -0.057825, 0.084593, 0.048752, -0.037315, 0.149011, 0.044503, -0.056242, 0.089390, 0.058897, -0.071115, 0.096624, 0.056687,
    -0.049864, 0.134713, 0.049205, -0.092375, 0.066135, -0.033990, -0.087900, 0.062973, -0.050945, -0.084545, 0.059021, -0.066622,
    -0.037422, 0.020447, -0.041474, -0.035509, 0.022913, -0.042268, -0.039796, 0.021210, -0.043761, -0.073663, 0.096860, 0.024289,
    -0.056030, 0.104972, 0.033176, -0.039799, 0.027971, -0.050361, -0.037209, 0.028034, -0.047419, -0.035898, 0.027719, -0.044854,
    -0.038319, 0.117228, 0.039956, -0.044967, 0.111267, 0.037080, -0.034892, 0.025651, -0.042822, -0.044064, 0.024098, -0.045944,
    -0.046442, 0.026868, -0.048646, -0.047599, 0.027619, -0.052458, -0.047894, 0.027664, -0.055339, -0.046662, 0.027134, -0.056329,
    0.000000, 0.000000, -0.000000, 0.000607, 0.000780, 0.002048, -0.003265, 0.001707, -0.002589, -0.012498, 0.018327, 0.010971,
    -0.004997, 0.011495, 0.009034, -0.017432, 0.023730, 0.019510, -0.011345, 0.026896, 0.024755, -0.011082, 0.012305, 0.006668,
    -0.027406, 0.029881, 0.015433, -0.007826, 0.006722, -0.015776, -0.002640, 0.002790, -0.010485, -0.012381, 0.009633, -0.023532,
    -0.010787, 0.011477, -0.009139, -0.004400, 0.003400, -0.017280, -0.016863, 0.012015, -0.028282, -0.017952, 0.012848, -0.036135,
    -0.013375, 0.010340, -0.030233, -0.019138, 0.000183, -0.043271, -0.019323, -0.002302, -0.051049, -0.021577, 0.008761, -0.032403,
    -0.025009, 0.008140, -0.041854, -0.019385, 0.010575, -0.059746, -0.020521, 0.012067, -0.069412, -0.026636, 0.012623, -0.078587,
    -0.024595, 0.039371, 0.053479, -0.027515, 0.059003, 0.054041, -0.017728, 0.040288, 0.055513, -0.035137, 0.044996, 0.046552,
    -0.019089, 0.060478, 0.059298, -0.040108, 0.064727, 0.050738, -0.034832, 0.104698, 0.061300, -0.036773, 0.132959, 0.055228,
    -0.038256, 0.128756, 0.058333, -0.031570, 0.100128, 0.064773, -0.035685, 0.133556, 0.050339, -0.038865, 0.104809, 0.057134,
    -0.042057, 0.133251, 0.020664, -0.043076, 0.130470, 0.027397, -0.041550, 0.123885, 0.030256, -0.042581, 0.119350, 0.037124,
    -0.040126, 0.132682, 0.019968, -0.038369, 0.121080, 0.026744, -0.004016, 0.005688, 0.003300, -0.002501, 0.005871, 0.005403,
    -0.007287, 0.006694, 0.001551, -0.001723, 0.012831, 0.008758, -0.002686, 0.022859, 0.011770, -0.001733, 0.010739, 0.007835,
    -0.004426, 0.023131, 0.012919, -0.001972, 0.019351, 0.009815, -0.001145, 0.008295, 0.006020, -0.003293, 0.012373, 0.008847,
    -0.038696, 0.021878, -0.026196, -0.049468, 0.029938, -0.014666, -0.042422, 0.026469, -0.007476, -0.042501, 0.028984, -0.003908,
    -0.040662, 0.022210, -0.011804, -0.039972, 0.032426, 0.002382, -0.039114, 0.029056, -0.003256, -0.042620, 0.030568, -0.000885,
    -0.041165, 0.035067, 0.006319, -0.044699, 0.031021, 0.001644, -0.044390, 0.035629, 0.009425, -0.047409, 0.030292, 0.002565,
    -0.045268, 0.035588, 0.009839, -0.050555, 0.027187, 0.000403, -0.049364, 0.030379, 0.004284, -0.038022, 0.018251, -0.014476,
    -0.036525, 0.024437, -0.009328, -0.037143, 0.014034, -0.012281, -0.036433, 0.013104, -0.016695, -0.039920, 0.014983, -0.008642,
    -0.033283, 0.007330, -0.022975, -0.034959, 0.009574, -0.018702, -0.036675, 0.013614, -0.017891, -0.032734, 0.006834, -0.024731,
    -0.043947, 0.017045, -0.006171, -0.037788, 0.011803, -0.013689, -0.043179, 0.015960, -0.010247, -0.047635, 0.021320, -0.003682,
    -0.044019, 0.040580, 0.017267, -0.039906, 0.042219, 0.018452, -0.043720, 0.107442, 0.058295, -0.048647, 0.109831, 0.064507,
    -0.048797, 0.116849, 0.054436, -0.040388, 0.110658, 0.048272, -0.044188, 0.114517, 0.047808, -0.041463, 0.103251, 0.070257,
    -0.061374, 0.044538, 0.019466, -0.056045, 0.037639, 0.001614, -0.051720, 0.038524, 0.013799, -0.048944, 0.030314, -0.001519,
    -0.061038, 0.049510, 0.036487, -0.048240, 0.034492, 0.009024, -0.046988, 0.022779, -0.003560, -0.046020, 0.025908, -0.002962,
    -0.019423, 0.039814, 0.014196, -0.028834, 0.033550, 0.003116, -0.027887, 0.040031, 0.010631, -0.021168, 0.034982, 0.007652,
    -0.013235, 0.045514, 0.022668, -0.011269, 0.049352, 0.027191, -0.010356, 0.053352, 0.038211, -0.012290, 0.068364, 0.066751,
    -0.006600, 0.079115, 0.073819, -0.014997, 0.055358, 0.064416, -0.011310, 0.071538, 0.072633, -0.001422, 0.089810, 0.080208,
    -0.005673, 0.087804, 0.082029, -0.020292, 0.105267, 0.081590, -0.006781, 0.094425, 0.084556, -0.030791, 0.111377, 0.067366,
    -0.027347, 0.098104, 0.080889, -0.037100, 0.104660, 0.065058, 0.000641, 0.000007, -0.003513, -0.001364, 0.004358, 0.001735,
    -0.000481, 0.003916, 0.003883, -0.005855, 0.008017, -0.004429, -0.026837, 0.027924, -0.003984, -0.024777, 0.022493, -0.009793,
    -0.030406, 0.026271, -0.006403, -0.021206, 0.027614, 0.001117, -0.027421, 0.020674, -0.012781, -0.019005, 0.020668, -0.005683,
    -0.009476, 0.026096, 0.010404, -0.008438, 0.016158, 0.003305, -0.002305, 0.021704, 0.012474, -0.001525, 0.027917, 0.014046,
    -0.002950, 0.031705, 0.015516, -0.009056, 0.032961, 0.013889, -0.011923, 0.039259, 0.014824, -0.016971, 0.027005, 0.004860,
    -0.021785, 0.050814, 0.028233, -0.035991, 0.041247, 0.014925, -0.036172, 0.053797, 0.040892, -0.011023, 0.059620, 0.048873,
    -0.029431, 0.016111, -0.037298, -0.031482, 0.012086, -0.023171, -0.035658, 0.014174, -0.018982, -0.001951, 0.075645, 0.074679,
    -0.014446, 0.087525, 0.080255, -0.044552, 0.062755, 0.059054, -0.034763, 0.070629, 0.070195, -0.001720, 0.023783, 0.012950,
    -0.000710, 0.026366, 0.012222, -0.001026, 0.032099, 0.014628, 0.000356, 0.100498, 0.085825, 0.000164, 0.102162, 0.086633,
    -0.013116, 0.104083, 0.081171, -0.007759, 0.097737, 0.079110, -0.022573, 0.011409, -0.049807, -0.025619, 0.109614, 0.070225,
    -0.001764, 0.010577, 0.006833, -0.001188, 0.007435, 0.005423, -0.038810, 0.120780, 0.049392, -0.025682, 0.097442, 0.068105,
    -0.034903, 0.118276, 0.063676, -0.016899, 0.096842, 0.071567, -0.005593, 0.058046, 0.057441, -0.008381, 0.045054, 0.038340,
    -0.004871, 0.069454, 0.059531, -0.054236, 0.055975, 0.048451, -0.007750, 0.011320, 0.008305, -0.006791, 0.023308, 0.014971,
    -0.003365, 0.037891, 0.017207, -0.033350, 0.031140, 0.000989, -0.034559, 0.036186, 0.007762, -0.047758, 0.045248, 0.028276,
    -0.033763, 0.020524, -0.013237, -0.035506, 0.014977, -0.017736, -0.031002, 0.016355, -0.019005, -0.016104, 0.014327, -0.014267,
    -0.021255, 0.016665, -0.018497, -0.003968, 0.012822, 0.006406, -0.025642, 0.015410, -0.020600, -0.024666, 0.059936, 0.054064,
    -0.001920, 0.006791, 0.005938, -0.015067, 0.019335, -0.001768, -0.028726, 0.090059, 0.080223, -0.025053, 0.080778, 0.076768,
    -0.040723, 0.098615, 0.080386, -0.011478, 0.073239, 0.068118, -0.015505, 0.083521, 0.076204, -0.019265, 0.066465, 0.063483,
    -0.046058, 0.104098, 0.073609, -0.044285, 0.121554, 0.045616, -0.033984, 0.097324, 0.080461, -0.041996, 0.119615, 0.038920,
    -0.036677, 0.118469, 0.039049, -0.005600, 0.061755, 0.049088, -0.008977, 0.007248, -0.023144, -0.036651, 0.115700, 0.033487,
    -0.014305, 0.012668, -0.021937, -0.017816, 0.014661, 0.002992, -0.027629, 0.025680, 0.003564, -0.018213, 0.013009, -0.006876,
    -0.025188, 0.025855, 0.005007, -0.026824, 0.022090, -0.005240, -0.014476, 0.009751, 0.000163, -0.014612, 0.009210, -0.004929,
    -0.014589, 0.013760, 0.004898, -0.051397, 0.027415, -0.092102, -0.061807, 0.034959, -0.088703, -0.036287, 0.029227, -0.042225,
    -0.038637, 0.030415, -0.045351, -0.034928, 0.026089, -0.039162, -0.040210, 0.034710, -0.038866, -0.036919, 0.031659, -0.035801,
    -0.041736, 0.031264, -0.048870, -0.044570, 0.037177, -0.043837, -0.044121, 0.031511, -0.052003, -0.047440, 0.037816, -0.048146,
    -0.044709, 0.031580, -0.054094, -0.051619, 0.037744, -0.051288, -0.042887, 0.028654, -0.058203, -0.049125, 0.032618, -0.058402,
    -0.034949, 0.022335, -0.038898, -0.035679, 0.027499, -0.034025, -0.040386, 0.016538, -0.056154, -0.039344, 0.015591, -0.051418,
    -0.040813, 0.017996, -0.059139, -0.033366, 0.008014, -0.055928, -0.034526, 0.011768, -0.061767, -0.037487, 0.016092, -0.046127,
    -0.031458, 0.006881, -0.047472, -0.041252, 0.020155, -0.060454, -0.036409, 0.015190, -0.065805, -0.039810, 0.019404, -0.068350,
    -0.041811, 0.023626, -0.060889, -0.058987, 0.043986, -0.050333, -0.055093, 0.045502, -0.045081, -0.039658, 0.109947, 0.033026,
    -0.038370, 0.112554, 0.036855, -0.041601, 0.113819, 0.027438, -0.046076, 0.106737, 0.036796, -0.075162, 0.049798, -0.062614,
    -0.069334, 0.043478, -0.077374, -0.064908, 0.042545, -0.060575, -0.058815, 0.034360, -0.073526, -0.078198, 0.053861, -0.047835,
    -0.057805, 0.037672, -0.059136, -0.045254, 0.025521, -0.065401, -0.052051, 0.029227, -0.071093, -0.045436, 0.043243, -0.020550,
    -0.035051, 0.036884, -0.027573, -0.044286, 0.043649, -0.030841, -0.036611, 0.038394, -0.019296, -0.044622, 0.048757, -0.005325,
    -0.064469, 0.052487, -0.017783, -0.057263, 0.056277, 0.002925, -0.054374, 0.076687, 0.050910, -0.068262, 0.090125, 0.053030,
    -0.047116, 0.064455, 0.047301, -0.060736, 0.083792, 0.050025, -0.081624, 0.103975, 0.054175, -0.075898, 0.102818, 0.053553,
    -0.064013, 0.114342, 0.049997, -0.076413, 0.103053, 0.045431, -0.051063, 0.117412, 0.042636, -0.059060, 0.103275, 0.043780,
    -0.047337, 0.109575, 0.036397, -0.001795, 0.000862, -0.009135, -0.009403, 0.005880, -0.011484, -0.009513, 0.005495, -0.006144,
    -0.011189, 0.009436, -0.017250, -0.030426, 0.030557, -0.027298, -0.027379, 0.024403, -0.028725, -0.036004, 0.028262, -0.030577,
    -0.029707, 0.030214, -0.020911, -0.033123, 0.022197, -0.030986, -0.024166, 0.022518, -0.023413, -0.027442, 0.029661, -0.011649,
    -0.019143, 0.018629, -0.014444, -0.028267, 0.024749, -0.005671, -0.033141, 0.031353, -0.003589, -0.035013, 0.035480, -0.005251,
    -0.033285, 0.036669, -0.008670, -0.037167, 0.043011, -0.006958, -0.027214, 0.029942, -0.015318, -0.063981, 0.054601, -0.029109,
    -0.049837, 0.044727, -0.039354, -0.075264, 0.057804, -0.034799, -0.086024, 0.062906, -0.013275, -0.042688, 0.020997, -0.090320,
    -0.031782, 0.013158, -0.039804, -0.035862, 0.016727, -0.042677, -0.072464, 0.081894, 0.038670, -0.071552, 0.092500, 0.034913,
    -0.091744, 0.065185, -0.019268, -0.094172, 0.073228, -0.004657, -0.030579, 0.026578, -0.003296, -0.032907, 0.028870, -0.000442,
    -0.036328, 0.035109, -0.000134, -0.089786, 0.117401, 0.057245, -0.089632, 0.119604, 0.056373, -0.072955, 0.119110, 0.056421,
    -0.077399, 0.111893, 0.055353, -0.033166, 0.014863, -0.086207, -0.055986, 0.121776, 0.051510, -0.017603, 0.012903, -0.009053,
    -0.042481, 0.124982, 0.032212, -0.049034, 0.106147, 0.054358, -0.041991, 0.127278, 0.049348, -0.063403, 0.108269, 0.053229,
    -0.058598, 0.062701, 0.028812, -0.043861, 0.049131, 0.016404, -0.073067, 0.072739, 0.017161, -0.084564, 0.058787, -0.033714,
    -0.022915, 0.025969, 0.007014, -0.039225, 0.041219, 0.001036, -0.038503, 0.033648, -0.030761, -0.043781, 0.039036, -0.033736,
    -0.065816, 0.049867, -0.047434, -0.034662, 0.023223, -0.033349, -0.036142, 0.018591, -0.038767, -0.033239, 0.018223, -0.034360,
    -0.018812, 0.015624, -0.026867, -0.022199, 0.017968, -0.032102, -0.018234, 0.015214, -0.011040, -0.029743, 0.016056, -0.033977,
    -0.088404, 0.063316, -0.018736, -0.010313, 0.007931, 0.001190, -0.021365, 0.021463, -0.018695, -0.076408, 0.092561, 0.022828,
    -0.090744, 0.083613, 0.009483, -0.057864, 0.101186, 0.033948, -0.092760, 0.076241, 0.005551, -0.078595, 0.086233, 0.023364,
    -0.094673, 0.069602, -0.005951, -0.046322, 0.107150, 0.036648, -0.058084, 0.100380, 0.037188, -0.080443, 0.064007, -0.001892,
    -0.040556, 0.029241, 0.009857, -0.040556, 0.029241, 0.009857, -0.040556, 0.029241, 0.009857, -0.040556, 0.029241, 0.009857,
    -0.040556, 0.029241, 0.009857, -0.040556, 0.029241, 0.009857, -0.040556, 0.029241, 0.009857, -0.040556, 0.029241, 0.009857,
    -0.040556, 0.029241, 0.009857, -0.050755, 0.029920, 0.020373, -0.050755, 0.029920, 0.020373, -0.050755, 0.029920, 0.020373,
    -0.050755, 0.029920, 0.020373, -0.050755, 0.029920, 0.020373, -0.050755, 0.029920, 0.020373, -0.050755, 0.029920, 0.020373,
    -0.050755, 0.029920, 0.020373, -0.050755, 0.029920, 0.020373
  ]),
  // 14 — smile
  new Float32Array([
    -0.006373, -0.130804, -0.020915, -0.049922, -0.130479, -0.015540, 0.036535, -0.125186, -0.020074, 0.001620, -0.000576, -0.005010,
    0.000592, -0.005827, -0.009496, -0.039560, -0.061187, 0.011750, 0.027359, -0.053714, 0.000294, -0.001061, -0.011142, -0.014313,
    -0.004100, -0.022815, -0.021465, -0.005814, -0.023614, -0.028225, -0.002555, -0.015278, -0.019487, -0.010131, -0.040927, -0.013021,
    -0.016892, -0.031905, -0.013474, -0.002057, -0.046297, -0.013375, 0.000404, -0.025979, -0.055138, 0.005875, -0.029935, -0.052068,
    -0.011532, -0.024435, -0.063564, -0.009827, -0.153169, 0.001979, -0.060797, -0.164030, -0.001059, 0.041550, -0.158195, -0.005260,
    -0.014106, -0.000006, -0.024744, -0.055355, -0.008664, -0.030388, 0.028151, -0.003668, -0.034692, -0.013115, -0.020314, -0.008540,
    -0.050346, -0.028527, -0.016329, 0.025305, -0.025108, -0.020504, -0.019297, -0.048343, 0.004369, -0.004857, -0.038431, -0.015819,
    -0.034346, -0.059589, 0.027134, -0.012116, -0.026576, -0.015310, -0.083460, -0.158319, -0.020712, -0.109585, -0.188328, -0.047813,
    -0.133184, -0.211572, -0.073475, -0.006342, -0.020644, -0.017210, 0.003275, -0.014778, -0.025632, 0.009011, -0.013703, -0.032866,
    0.009798, -0.020071, -0.036343, 0.003173, -0.033425, -0.036501, 0.001376, -0.027850, -0.036681, -0.037369, -0.071555, 0.059972,
    -0.047349, -0.071992, 0.065007, -0.021642, -0.066748, 0.053002, -0.150616, -0.229140, -0.083180, -0.132577, -0.158661, -0.092158,
    0.003991, -0.012899, -0.026041, -0.007063, -0.024193, -0.012587, 0.011150, -0.011904, -0.034411, 0.011897, -0.018715, -0.037966,
    0.006599, -0.027303, -0.044267, 0.007017, -0.031665, -0.045859, -0.147621, -0.213682, -0.091604, -0.124440, -0.133029, -0.092110,
    -0.145847, -0.205322, -0.068584, -0.120392, -0.194813, -0.041484, -0.097168, -0.180274, -0.016641, -0.081066, -0.051959, -0.051469,
    -0.086749, -0.035389, -0.047325, -0.111363, -0.079846, -0.070010, -0.108525, -0.096525, -0.074098, -0.052280, -0.071697, 0.063062,
    -0.052209, -0.071871, 0.056271, -0.047340, -0.068323, 0.046264, -0.016170, -0.023865, -0.024605, -0.009719, -0.028729, -0.023548,
    -0.015035, -0.019139, -0.026475, -0.007060, -0.062635, 0.034105, -0.000209, -0.052372, 0.022629, 0.003412, -0.050428, -0.015492,
    0.006361, -0.049989, -0.017895, 0.002718, -0.043522, -0.019930, -0.018133, -0.031256, 0.037473, -0.025563, -0.029279, 0.034508,
    -0.012701, -0.033201, 0.031175, -0.005546, -0.040362, 0.024698, -0.032944, -0.030127, 0.029395, -0.002086, -0.034274, -0.022013,
    -0.011400, -0.013787, -0.026977, -0.007264, -0.008894, -0.025013, -0.000942, -0.008594, -0.022400, -0.000427, -0.014553, -0.019982,
    -0.005805, -0.021462, -0.017664, -0.021167, -0.039148, -0.043979, -0.016174, -0.031905, -0.046079, -0.027412, -0.043300, -0.043151,
    -0.060251, -0.028269, -0.042979, -0.055060, -0.020670, -0.057760, -0.063059, -0.038449, -0.023003, -0.018124, -0.027535, -0.046821,
    0.069508, -0.147981, -0.029604, 0.092126, -0.173383, -0.058657, 0.109400, -0.191802, -0.085230, -0.041256, -0.015542, -0.054812,
    -0.038358, -0.012479, -0.057358, -0.034376, -0.013595, -0.057360, -0.025746, -0.021797, -0.052833, -0.044253, -0.019247, -0.069954,
    -0.014002, -0.030998, -0.044334, -0.062522, -0.065065, 0.016732, -0.061912, -0.060222, 0.018038, -0.065890, -0.065065, 0.013392,
    0.117430, -0.204355, -0.095342, 0.099808, -0.139840, -0.102086, -0.045414, -0.009329, -0.060767, -0.048299, -0.016593, -0.053397,
    -0.038384, -0.010498, -0.060774, -0.026311, -0.020761, -0.054633, -0.013462, -0.030763, -0.052475, -0.026724, -0.021985, -0.069105,
    0.114649, -0.191379, -0.101403, 0.091936, -0.116680, -0.100981, 0.117100, -0.187150, -0.078212, 0.096688, -0.180635, -0.050103,
    0.077171, -0.169492, -0.023667, 0.055152, -0.045703, -0.058244, 0.058845, -0.026430, -0.054051, 0.080995, -0.066868, -0.077863,
    0.078798, -0.084536, -0.082131, -0.061558, -0.055637, 0.014316, -0.062837, -0.053643, 0.005896, -0.063946, -0.048251, -0.005362,
    -0.015834, -0.021837, -0.039576, -0.022024, -0.026154, -0.040096, -0.016103, -0.017410, -0.043018, -0.067205, -0.062694, 0.001213,
    -0.062947, -0.053181, -0.003782, -0.031939, -0.046633, -0.043247, -0.034618, -0.046357, -0.042596, -0.031980, -0.040878, -0.041410,
    -0.040053, -0.032256, 0.016357, -0.052282, -0.040884, 0.004047, -0.028425, -0.031831, -0.040685, -0.017881, -0.013343, -0.045679,
    -0.020144, -0.010329, -0.046776, -0.024118, -0.011775, -0.047924, -0.024707, -0.017601, -0.048588, -0.021765, -0.023214, -0.048118,
    0.000000, 0.000000, -0.000000, -0.003676, -0.002558, 0.000665, 0.002077, -0.000343, -0.001821, -0.004292, -0.033773, 0.007515,
    -0.008852, -0.014693, 0.007633, -0.005874, -0.068974, -0.001754, -0.039154, -0.083798, -0.003029, 0.000471, -0.013638, 0.006457,
    0.025399, -0.079318, -0.007384, 0.001034, -0.014981, -0.010814, 0.001870, -0.009929, -0.005384, 0.000050, -0.018918, -0.017456,
    -0.004320, -0.024041, -0.006404, -0.002502, -0.008757, -0.009542, -0.003804, -0.024478, -0.019686, -0.007578, -0.023348, -0.023959,
    -0.007518, -0.017917, -0.021025, -0.005070, -0.026838, -0.039438, -0.003289, -0.026633, -0.049875, -0.006361, -0.028066, -0.026837,
    -0.010550, -0.028489, -0.031842, 0.007743, -0.029738, -0.056331, -0.000323, -0.026252, -0.057697, -0.012127, -0.026960, -0.065980,
    -0.006578, -0.138390, -0.017792, -0.007840, -0.145976, -0.007663, -0.052697, -0.146148, -0.015566, 0.040461, -0.140403, -0.020307,
    -0.057404, -0.157100, -0.008236, 0.042133, -0.150987, -0.012240, -0.013042, -0.008580, -0.017534, -0.012673, -0.015968, -0.011487,
    -0.051469, -0.025127, -0.020599, -0.053865, -0.018072, -0.025744, 0.027996, -0.020805, -0.025025, 0.028579, -0.013240, -0.030093,
    -0.019401, -0.019589, 0.008878, -0.050656, -0.027504, 0.001448, -0.018723, -0.027456, 0.009393, -0.035520, -0.032896, 0.004947,
    0.014141, -0.025321, -0.002156, -0.001608, -0.031527, -0.000262, -0.002597, -0.004517, 0.002416, -0.007217, -0.006083, 0.003457,
    0.001969, -0.004566, 0.001464, -0.019464, -0.018853, 0.007629, -0.034920, -0.047512, 0.011013, -0.020538, -0.018394, 0.008638,
    -0.032710, -0.049223, 0.011130, -0.037163, -0.041258, 0.010741, -0.014905, -0.009249, 0.004697, -0.014063, -0.017005, 0.007383,
    0.002691, -0.026157, -0.029022, -0.011247, -0.035031, -0.011860, 0.001116, -0.047323, -0.020436, 0.002445, -0.054867, -0.017914,
    -0.003579, -0.037203, -0.022776, -0.001859, -0.067346, -0.015583, -0.002816, -0.054406, -0.018718, -0.000526, -0.057678, -0.014373,
    -0.004169, -0.075370, -0.012062, -0.009534, -0.057219, -0.011184, -0.012084, -0.072985, -0.006971, -0.019241, -0.050196, -0.010208,
    -0.017486, -0.065197, -0.004326, -0.024558, -0.037187, -0.012098, -0.021751, -0.044576, -0.008221, -0.009768, -0.030502, -0.022669,
    -0.006559, -0.043417, -0.021464, -0.003993, -0.013970, -0.027810, -0.006532, -0.014003, -0.034192, -0.006643, -0.017496, -0.023972,
    -0.000556, -0.016024, -0.039966, 0.000754, -0.015295, -0.035834, -0.012012, -0.018930, -0.033514, -0.007750, -0.022162, -0.039296,
    -0.012777, -0.022601, -0.021710, -0.002958, -0.017925, -0.031752, -0.010847, -0.021869, -0.027768, -0.019845, -0.028360, -0.018018,
    -0.020786, -0.081205, 0.000861, -0.016525, -0.098183, -0.002793, -0.029145, -0.047366, -0.001816, -0.014996, -0.038009, 0.015282,
    -0.017157, -0.030830, 0.021202, -0.048256, -0.058339, -0.018090, -0.024202, -0.032723, 0.009629, -0.024543, -0.076446, -0.015600,
    -0.038979, -0.063152, 0.017706, -0.026181, -0.050013, 0.004579, -0.025734, -0.057165, 0.007546, -0.016706, -0.039406, -0.004419,
    -0.045515, -0.072214, 0.027712, -0.020546, -0.054108, -0.001331, -0.016992, -0.029879, -0.017870, -0.013259, -0.034484, -0.012414,
    -0.043854, -0.091697, 0.002624, -0.018472, -0.072658, -0.007701, -0.031194, -0.096575, 0.000074, -0.035495, -0.079476, 0.000342,
    -0.056649, -0.104942, 0.000834, -0.067093, -0.110564, 0.002812, -0.069363, -0.120248, -0.019926, -0.094073, -0.176196, -0.021800,
    -0.118528, -0.193376, -0.046035, -0.088610, -0.168564, -0.024830, -0.113158, -0.191070, -0.048942, -0.141643, -0.207829, -0.072064,
    -0.137467, -0.210518, -0.075220, -0.095748, -0.162019, -0.072598, -0.109351, -0.204756, -0.060734, -0.087945, -0.103926, -0.063487,
    -0.049959, -0.141417, -0.052761, -0.048279, -0.098676, -0.041702, 0.001568, -0.006155, -0.003008, -0.007723, -0.014359, 0.003601,
    -0.010822, -0.006671, 0.003727, -0.005007, -0.022106, -0.000205, -0.012447, -0.053255, -0.013757, -0.008467, -0.039207, -0.016251,
    -0.008753, -0.051578, -0.018482, -0.025200, -0.058318, -0.004631, -0.010732, -0.041280, -0.021743, -0.013146, -0.039599, -0.009519,
    -0.038182, -0.067134, 0.009522, -0.021545, -0.042506, 0.005732, -0.039900, -0.052413, 0.012447, -0.047605, -0.068357, 0.013662,
    -0.052376, -0.081146, 0.013751, -0.048506, -0.081149, 0.011173, -0.055078, -0.093101, 0.008174, -0.031747, -0.062414, 0.002405,
    -0.057855, -0.123244, 0.003971, -0.015091, -0.103905, -0.008626, -0.051242, -0.110218, 0.014176, -0.082130, -0.122843, 0.008568,
    0.009881, -0.022209, -0.039278, -0.012463, -0.027288, -0.030083, -0.015264, -0.023797, -0.030732, -0.112220, -0.183049, -0.061967,
    -0.066616, -0.173262, -0.039551, -0.057109, -0.083566, 0.040919, -0.048329, -0.080824, 0.038532, -0.042728, -0.052926, 0.012820,
    -0.045838, -0.058038, 0.012294, -0.056146, -0.077352, 0.013332, -0.146572, -0.219257, -0.091170, -0.148775, -0.224725, -0.087968,
    -0.132187, -0.152758, -0.094421, -0.129673, -0.145388, -0.092220, 0.012042, -0.026821, -0.048024, -0.110612, -0.094353, -0.071174,
    -0.021004, -0.021887, 0.007984, -0.017200, -0.010517, 0.005631, -0.076018, -0.055741, -0.039195, -0.085478, -0.045178, -0.046828,
    -0.083921, -0.049465, -0.049154, -0.109233, -0.089883, -0.069283, -0.095309, -0.146747, -0.044031, -0.074735, -0.119338, -0.016827,
    -0.086387, -0.163249, -0.030026, -0.053774, -0.082853, 0.037131, -0.004091, -0.013702, 0.007206, -0.030752, -0.051782, 0.009544,
    -0.064508, -0.095306, 0.012952, -0.008564, -0.065738, -0.014061, -0.013109, -0.085476, -0.009852, -0.032232, -0.083283, 0.011161,
    -0.011162, -0.037172, -0.022540, -0.014932, -0.026193, -0.023906, -0.014400, -0.032078, -0.024476, -0.004283, -0.026385, -0.013879,
    -0.004790, -0.029053, -0.017456, -0.021522, -0.033408, 0.007953, -0.011859, -0.033760, -0.021047, -0.070309, -0.111658, 0.019183,
    -0.010280, -0.007220, 0.003842, -0.017264, -0.042303, -0.002293, -0.021329, -0.088345, 0.005359, -0.039701, -0.085249, 0.025445,
    -0.006987, -0.075725, -0.005962, -0.069997, -0.112861, 0.009524, -0.055278, -0.131506, -0.019556, -0.071648, -0.106153, 0.020247,
    -0.008696, -0.055855, 0.003456, -0.022266, -0.028053, 0.017169, -0.024331, -0.107132, -0.028576, -0.019655, -0.029502, 0.006505,
    -0.027009, -0.029685, 0.013219, -0.077806, -0.129386, -0.005739, -0.005030, -0.014068, -0.014796, -0.014821, -0.031102, 0.002234,
    -0.003322, -0.021714, -0.013400, 0.010740, -0.015449, 0.004733, 0.021722, -0.042927, 0.006497, 0.012187, -0.013630, 0.001058,
    0.019971, -0.045482, 0.006843, 0.024707, -0.035095, 0.002691, 0.008337, -0.006274, 0.001842, 0.010985, -0.005886, 0.000434,
    0.005124, -0.014807, 0.005433, -0.051698, -0.014848, -0.067548, -0.055643, -0.021187, -0.055428, -0.032074, -0.043924, -0.041060,
    -0.033378, -0.050428, -0.042590, -0.027649, -0.034213, -0.038789, -0.029850, -0.060589, -0.040486, -0.027889, -0.049812, -0.037924,
    -0.031735, -0.051818, -0.042781, -0.030404, -0.066403, -0.042623, -0.025281, -0.051682, -0.042867, -0.027016, -0.063613, -0.041639,
    -0.017230, -0.045836, -0.043505, -0.027555, -0.055121, -0.041357, -0.013893, -0.034116, -0.046711, -0.026991, -0.037348, -0.045746,
    -0.021404, -0.027797, -0.037102, -0.023774, -0.039775, -0.035747, -0.022324, -0.013180, -0.053327, -0.020732, -0.012886, -0.053561,
    -0.021597, -0.015800, -0.053894, -0.023639, -0.016286, -0.057766, -0.026262, -0.014502, -0.060181, -0.017003, -0.016627, -0.049296,
    -0.016805, -0.022969, -0.051549, -0.018927, -0.019348, -0.053771, -0.026652, -0.015593, -0.061808, -0.026297, -0.017882, -0.061349,
    -0.017163, -0.024524, -0.051812, -0.032801, -0.067251, -0.040701, -0.026902, -0.084030, -0.041183, -0.012451, -0.044640, -0.014296,
    -0.031443, -0.036160, 0.001111, 0.010774, -0.055530, -0.027713, -0.019598, -0.072689, -0.031977, -0.052880, -0.043952, -0.031936,
    -0.054955, -0.032046, -0.042818, -0.046403, -0.043119, -0.039512, -0.049003, -0.027499, -0.048479, -0.045421, -0.053149, -0.022867,
    -0.039656, -0.042549, -0.042875, -0.027671, -0.024516, -0.054469, -0.041046, -0.025991, -0.052757, 0.020454, -0.080782, -0.016881,
    -0.004735, -0.065362, -0.026366, 0.005530, -0.084922, -0.025084, 0.014890, -0.071217, -0.015206, 0.035637, -0.094104, -0.013767,
    0.038196, -0.094571, -0.021466, 0.045043, -0.106447, -0.036644, 0.077628, -0.164896, -0.029659, 0.098127, -0.178288, -0.055367,
    0.074496, -0.157294, -0.033404, 0.094978, -0.175862, -0.059129, 0.114696, -0.188732, -0.082447, 0.112278, -0.190861, -0.086400,
    0.062197, -0.148052, -0.084969, 0.078688, -0.185679, -0.076310, 0.054593, -0.095981, -0.073533, 0.012989, -0.131101, -0.068619,
    0.009778, -0.092850, -0.054555, -0.000875, -0.004611, -0.005971, 0.003744, -0.010396, -0.003682, 0.006794, -0.002241, -0.001418,
    0.000117, -0.019171, -0.007028, -0.008121, -0.048254, -0.027564, -0.009932, -0.035614, -0.027278, -0.018024, -0.047115, -0.032878,
    0.006751, -0.052745, -0.017412, -0.013695, -0.038192, -0.032048, -0.001946, -0.035795, -0.019731, 0.024856, -0.059923, -0.002668,
    0.011635, -0.037238, -0.003898, 0.027483, -0.045331, 0.002751, 0.034115, -0.060579, 0.004051, 0.038558, -0.072299, 0.002537,
    0.033175, -0.072718, -0.001173, 0.037842, -0.083409, -0.003682, 0.015660, -0.056225, -0.009099, 0.023606, -0.105639, -0.030180,
    -0.019239, -0.090771, -0.041643, -0.007442, -0.090697, -0.031527, 0.036547, -0.102934, -0.026948, -0.043627, -0.013876, -0.069096,
    -0.014252, -0.026455, -0.039370, -0.015313, -0.021667, -0.043824, 0.088327, -0.165642, -0.076361, 0.032177, -0.158819, -0.060170,
    -0.029402, -0.067722, -0.005365, -0.033528, -0.068982, -0.005124, 0.029304, -0.046535, 0.003618, 0.032027, -0.051860, 0.005276,
    0.041198, -0.069708, 0.005404, 0.113997, -0.196321, -0.101251, 0.116107, -0.200531, -0.098924, 0.100934, -0.134510, -0.103944,
    0.098267, -0.128164, -0.101407, -0.026998, -0.021737, -0.067933, 0.081646, -0.081768, -0.079319, 0.012887, -0.016739, -0.000357,
    0.041098, -0.051365, -0.046414, 0.059146, -0.036581, -0.053779, 0.059103, -0.041898, -0.056034, 0.080248, -0.076484, -0.077365,
    0.075011, -0.133299, -0.056269, 0.057178, -0.109518, -0.026788, 0.058486, -0.146607, -0.049168, -0.034747, -0.064313, -0.011623,
    0.018172, -0.048699, 0.005494, 0.048285, -0.086350, 0.004458, -0.020002, -0.059564, -0.033340, -0.016780, -0.075884, -0.035311,
    -0.032191, -0.065219, -0.037279, -0.018200, -0.034411, -0.033930, -0.015885, -0.023567, -0.035943, -0.013281, -0.030160, -0.032362,
    -0.006691, -0.024169, -0.020984, -0.010250, -0.026791, -0.025171, 0.012733, -0.027628, -0.001325, -0.010647, -0.031880, -0.028593,
    0.009126, -0.091718, -0.023847, 0.004848, -0.004748, 0.001609, 0.004699, -0.037977, -0.011261, -0.039085, -0.081650, -0.026882,
    -0.031478, -0.075945, -0.013602, -0.047539, -0.071079, -0.030754, 0.014028, -0.096499, -0.025791, 0.009713, -0.118678, -0.046477,
    0.007616, -0.089066, -0.020263, -0.041898, -0.054091, -0.015438, -0.021295, -0.099700, -0.049550, 0.041775, -0.111821, -0.032260,
    -0.010201, -0.022368, -0.021433, -0.010201, -0.022368, -0.021433, -0.010201, -0.022368, -0.021433, -0.010201, -0.022368, -0.021433,
    -0.010201, -0.022368, -0.021433, -0.010201, -0.022368, -0.021433, -0.010201, -0.022368, -0.021433, -0.010201, -0.022368, -0.021433,
    -0.010201, -0.022368, -0.021433, -0.020374, -0.019866, -0.020734, -0.020374, -0.019866, -0.020734, -0.020374, -0.019866, -0.020734,
    -0.020374, -0.019866, -0.020734, -0.020374, -0.019866, -0.020734, -0.020374, -0.019866, -0.020734, -0.020374, -0.019866, -0.020734,
    -0.020374, -0.019866, -0.020734, -0.020374, -0.019866, -0.020734
  ]),
];

const LEFT_MASK = new Float32Array([
  1.000000, 1.000000, 0.000000, 1.000000, 1.000000, 1.000000, 0.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000,
  1.000000, 1.000000, 1.000000, 1.000000, 0.000000, 0.967565, 1.000000, 0.000000, 0.907146, 1.000000, 0.000000, 0.858316,
  1.000000, 0.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000,
  1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000,
  1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000,
  1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 0.703549,
  1.000000, 1.000000, 0.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 0.000000, 0.000000, 0.000000,
  0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000,
  0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000,
  0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000,
  0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000,
  0.000000, 0.000000, 0.000000, 0.000000, 1.000000, 1.000000, 0.212654, 1.000000, 1.000000, 1.000000, 1.000000, 0.689013,
  0.000000, 1.000000, 1.000000, 1.000000, 1.000000, 0.144110, 1.000000, 0.007279, 0.063771, 1.000000, 1.000000, 1.000000,
  0.000000, 1.000000, 1.000000, 0.000000, 1.000000, 0.980341, 1.000000, 0.000000, 1.000000, 0.000000, 0.902212, 0.868220,
  1.000000, 1.000000, 0.000000, 0.000000, 0.801587, 1.000000, 0.803443, 1.000000, 0.000000, 0.000000, 1.000000, 1.000000,
  0.588082, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000,
  1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000,
  1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000,
  1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000,
  1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000,
  1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000,
  1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000,
  1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000,
  1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000,
  1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000,
  1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000,
  1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 0.751081, 1.000000, 0.806903,
  0.000000, 1.000000, 0.166622, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000,
  0.252625, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000,
  0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000,
  0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000,
  0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000,
  0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000,
  0.000000, 0.067243, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000,
  0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000,
  0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000,
  0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000,
  0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000,
  0.000000, 0.165331, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000,
  1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 0.000000, 0.000000, 0.000000,
  0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000
]);

const RIGHT_MASK = new Float32Array([
  0.000000, 0.000000, 1.000000, 0.000000, 0.000000, 0.000000, 1.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000,
  0.000000, 0.000000, 0.000000, 0.000000, 1.000000, 0.032435, 0.000000, 1.000000, 0.092854, 0.000000, 1.000000, 0.141684,
  0.000000, 1.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000,
  0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000,
  0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000,
  0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.296451,
  0.000000, 0.000000, 1.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 1.000000, 1.000000, 1.000000,
  1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000,
  1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000,
  1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000,
  1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000,
  1.000000, 1.000000, 1.000000, 1.000000, 0.000000, 0.000000, 0.787346, 0.000000, 0.000000, 0.000000, 0.000000, 0.310987,
  1.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.855890, 0.000000, 0.992721, 0.936229, 0.000000, 0.000000, 0.000000,
  1.000000, 0.000000, 0.000000, 1.000000, 0.000000, 0.019659, 0.000000, 1.000000, 0.000000, 1.000000, 0.097788, 0.131780,
  0.000000, 0.000000, 1.000000, 1.000000, 0.198413, 0.000000, 0.196557, 0.000000, 1.000000, 1.000000, 0.000000, 0.000000,
  0.411918, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000,
  0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000,
  0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000,
  0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000,
  0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000,
  0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000,
  0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000,
  0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000,
  0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000,
  0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000,
  0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000,
  0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.248919, 0.000000, 0.193097,
  1.000000, 0.000000, 0.833378, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000,
  0.747375, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000,
  1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000,
  1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000,
  1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000,
  1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000,
  1.000000, 0.932757, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000,
  1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000,
  1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000,
  1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000,
  1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000,
  1.000000, 0.834669, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000,
  0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 1.000000, 1.000000, 1.000000,
  1.000000, 1.000000, 1.000000, 1.000000, 1.000000, 1.000000
]);

const BS_INDEX = Object.freeze({
  mouth_open: 12,
  smile:      14,
  frown:      9,
  blink:      6,
  brow_up:    7,
  cheek_puff: 8,
  angry:      5,
  vis_aa:     4,
  vis_oo:     13,
  vis_ee:     10,
  vis_mm:     11,
  vis_ff:     0,
  vis_ll:     1,
  vis_ss:     2,
  vis_ch:     3,
});

const FACEMESH_CONTOURS = [
  [0,1], [0,2], [3,4], [3,5], [3,6], [4,7], [8,9], [8,10], [11,12], [11,13],
  [14,15], [14,16], [17,18], [17,19], [20,21], [20,22], [23,24], [23,25], [26,27], [26,28],
  [12,29], [1,30], [30,31], [31,32], [33,34], [35,34], [35,36], [27,37], [38,36], [39,40],
  [39,41], [42,43], [42,32], [44,45], [44,46], [47,46], [47,48], [49,37], [49,15], [50,51],
  [50,52], [53,54], [53,52], [54,18], [24,55], [21,56], [57,51], [57,56], [58,43], [58,55],
  [59,40], [59,60], [61,28], [61,60], [62,63], [62,64], [65,66], [65,41], [67,68], [67,13],
  [68,69], [70,71], [70,72], [73,66], [73,72], [71,74], [69,75], [75,63], [76,77], [76,64],
  [77,78], [78,79], [79,80], [80,29], [7,10], [81,82], [81,83], [84,85], [84,86], [82,87],
  [2,88], [88,89], [89,90], [91,92], [93,92], [93,94], [85,95], [96,94], [97,98], [97,99],
  [100,101], [100,90], [102,103], [102,104], [105,104], [105,106], [107,95], [107,16], [108,109], [108,110],
  [111,112], [111,110], [112,19], [25,113], [22,114], [115,109], [115,114], [116,101], [116,113], [117,98],
  [117,118], [119,86], [119,118], [120,121], [120,122], [123,124], [123,99], [125,126], [125,83], [126,127],
  [74,128], [129,124], [129,128], [127,130], [130,121], [131,132], [131,122], [132,133], [133,134], [134,135],
  [135,87]
];

const FACEMESH_FILL = [
  [136,137], [136,138], [139,140], [139,141], [139,142], [139,143], [139,144], [145,146], [145,7], [145,147],
  [145,10], [145,148], [3,137], [3,146], [3,138], [3,149], [4,146], [4,149], [8,150], [8,147],
  [8,151], [8,152], [153,154], [153,38], [153,9], [153,155], [153,96], [153,156], [154,38], [154,48],
  [154,157], [154,158], [154,96], [154,106], [154,159], [14,158], [160,161], [160,162], [160,163], [161,164],
  [161,162], [161,165], [161,163], [166,167], [166,168], [166,169], [166,170], [166,171], [167,168], [167,170],
  [172,173], [172,174], [172,175], [172,176], [172,177], [178,137], [178,179], [178,140], [178,138], [178,180],
  [178,143], [181,182], [181,183], [181,184], [181,185], [181,186], [181,187], [26,188], [26,189], [190,191],
  [190,192], [190,193], [190,194], [191,195], [191,196], [191,193], [195,197], [195,198], [195,196], [199,200],
  [199,197], [199,201], [199,202], [199,198], [192,203], [192,194], [192,204], [205,206], [205,207], [205,208],
  [205,209], [206,210], [206,211], [206,208], [207,212], [207,209], [207,213], [212,213], [212,214], [212,215],
  [200,216], [200,217], [200,202], [200,198], [218,219], [218,220], [218,221], [218,175], [218,222], [218,223],
  [224,61], [224,225], [224,226], [224,227], [224,228], [224,60], [229,216], [229,230], [229,231], [229,226],
  [229,202], [232,233], [232,234], [232,235], [232,236], [232,237], [232,238], [164,239], [164,162], [239,240],
  [239,162], [239,241], [239,242], [240,242], [240,243], [240,244], [245,246], [245,247], [245,248], [245,249],
  [137,250], [137,179], [137,251], [137,252], [250,146], [250,253], [33,230], [33,231], [33,227], [33,214],
  [254,233], [254,255], [254,256], [254,257], [254,258], [254,259], [5,260], [5,261], [5,262], [5,263],
  [260,264], [260,265], [260,266], [260,261], [260,267], [268,234], [268,217], [268,269], [268,270], [268,271],
  [268,237], [146,253], [146,7], [146,148], [35,208], [35,209], [34,209], [34,213], [34,214], [27,188],
  [27,272], [38,273], [38,155], [38,211], [210,274], [210,211], [246,275], [246,248], [246,276], [39,277],
  [39,278], [279,280], [279,185], [279,262], [279,263], [182,280], [182,184], [182,185], [182,281], [282,283],
  [282,284], [282,285], [282,243], [44,188], [44,189], [44,272], [264,265], [264,266], [264,263], [264,281],
  [36,211], [36,208], [47,286], [49,286], [49,272], [49,157], [188,189], [188,272], [286,272], [286,46],
  [286,48], [286,157], [45,189], [45,225], [45,227], [189,225], [189,28], [162,241], [241,242], [242,244],
  [280,185], [280,263], [280,281], [283,284], [283,243], [283,244], [284,287], [284,285], [183,185], [183,288],
  [183,252], [183,186], [183,289], [173,290], [173,175], [168,169], [168,291], [168,292], [169,291], [293,287],
  [293,285], [293,291], [293,292], [287,285], [287,292], [294,295], [294,275], [294,238], [294,296], [59,297],
  [59,228], [298,140], [298,143], [299,184], [299,140], [299,295], [299,142], [299,187], [300,184], [300,266],
  [300,295], [300,236], [300,281], [184,281], [184,187], [233,234], [233,301], [233,256], [233,257], [233,235],
  [234,269], [234,302], [234,301], [234,237], [265,266], [37,272], [272,46], [247,290], [247,221], [247,249],
  [48,157], [157,15], [157,158], [15,158], [197,198], [216,303], [216,217], [216,270], [216,226], [216,202],
  [203,204], [203,304], [203,305], [203,306], [230,231], [230,214], [230,202], [230,215], [255,258], [255,307],
  [255,308], [255,259], [309,261], [309,288], [309,262], [303,270], [303,297], [303,226], [303,228], [217,269],
  [217,270], [217,198], [217,196], [269,302], [269,196], [269,193], [302,301], [302,193], [301,256], [301,193],
  [301,194], [301,204], [256,258], [256,204], [150,9], [150,308], [150,155], [150,147], [150,310], [270,297],
  [270,311], [270,277], [270,271], [231,226], [231,227], [179,140], [179,252], [179,312], [257,266], [257,235],
  [257,267], [257,259], [61,225], [258,308], [258,204], [258,304], [258,310], [266,235], [266,236], [266,267],
  [201,202], [201,215], [261,253], [261,313], [261,267], [261,251], [40,297], [40,277], [253,313], [253,251],
  [253,148], [314,65], [314,315], [314,66], [314,316], [314,317], [314,318], [65,315], [297,277], [297,228],
  [315,41], [315,317], [315,319], [315,278], [225,227], [225,28], [219,70], [219,320], [219,220], [219,72],
  [219,223], [140,312], [140,187], [235,236], [226,227], [226,228], [311,277], [311,271], [311,319], [311,278],
  [70,220], [70,321], [73,320], [66,316], [66,320], [158,159], [158,16], [71,321], [141,142], [141,144],
  [295,142], [295,236], [295,238], [185,288], [185,262], [9,155], [9,151], [9,156], [316,320], [316,322],
  [316,223], [316,318], [320,72], [320,223], [220,321], [220,323], [220,222], [41,278], [307,308], [307,147],
  [307,259], [307,148], [321,323], [321,74], [321,324], [277,278], [291,292], [290,221], [290,175], [243,244],
  [275,276], [275,296], [271,317], [271,237], [271,325], [271,319], [271,318], [308,147], [308,310], [273,274],
  [273,155], [273,211], [273,305], [273,306], [274,211], [274,305], [317,319], [317,318], [155,306], [155,310],
  [221,175], [221,249], [221,223], [7,326], [7,149], [147,10], [147,148], [10,326], [10,152], [313,267],
  [313,259], [313,148], [323,174], [323,222], [323,324], [323,327], [174,175], [174,222], [174,177], [174,327],
  [175,222], [248,249], [248,322], [248,276], [248,318], [236,238], [249,322], [249,223], [237,238], [237,325],
  [237,296], [238,296], [325,276], [325,318], [325,296], [267,259], [322,223], [322,318], [276,318], [276,296],
  [319,278], [259,148], [288,262], [288,251], [288,252], [262,263], [251,252], [211,208], [208,209], [209,213],
  [213,214], [214,215], [202,215], [228,60], [198,196], [196,193], [193,194], [194,204], [204,304], [304,306],
  [304,310], [263,281], [252,289], [252,312], [186,289], [186,312], [186,187], [289,312], [312,187], [305,306],
  [306,310], [326,149], [326,152], [326,328], [329,330], [329,331], [329,332], [329,333], [329,334], [329,335],
  [329,336], [84,337], [84,338], [339,340], [339,341], [339,342], [339,343], [340,344], [340,345], [340,342],
  [344,346], [344,347], [344,345], [348,349], [348,346], [348,350], [348,351], [348,347], [341,352], [341,343],
  [341,353], [354,355], [354,356], [354,357], [354,358], [355,359], [355,360], [355,357], [356,361], [356,358],
  [356,362], [361,362], [361,363], [361,364], [349,365], [349,366], [349,351], [349,347], [367,368], [367,324],
  [367,369], [367,177], [367,327], [367,370], [371,119], [371,372], [371,373], [371,374], [371,375], [371,118],
  [376,365], [376,377], [376,378], [376,373], [376,351], [379,380], [379,381], [379,382], [379,383], [379,384],
  [379,385], [165,386], [165,163], [386,387], [386,163], [386,388], [386,389], [387,389], [387,390], [387,391],
  [392,393], [392,394], [392,395], [392,396], [138,397], [138,180], [138,398], [138,399], [397,149], [397,400],
  [91,377], [91,378], [91,374], [91,363], [401,380], [401,402], [401,403], [401,404], [401,405], [401,406],
  [6,407], [6,408], [6,409], [6,410], [407,411], [407,412], [407,413], [407,408], [407,414], [415,381],
  [415,366], [415,416], [415,417], [415,418], [415,384], [149,400], [149,328], [93,357], [93,358], [92,358],
  [92,362], [92,363], [85,337], [85,419], [96,420], [96,156], [96,360], [359,421], [359,360], [393,422],
  [393,395], [393,423], [97,424], [97,425], [426,330], [426,427], [426,333], [426,409], [426,410], [330,427],
  [330,332], [330,333], [330,428], [429,430], [429,431], [429,432], [429,390], [102,337], [102,338], [102,419],
  [411,412], [411,413], [411,410], [411,428], [94,360], [94,357], [105,433], [107,433], [107,419], [107,159],
  [337,338], [337,419], [433,419], [433,104], [433,106], [433,159], [103,338], [103,372], [103,374], [338,372],
  [338,86], [163,388], [388,389], [389,391], [427,410], [427,428], [430,431], [430,390], [430,391], [431,434],
  [431,432], [331,333], [331,435], [331,399], [331,335], [176,436], [176,177], [170,171], [170,437], [170,438],
  [171,437], [439,434], [439,432], [439,437], [439,438], [434,432], [434,438], [440,441], [440,422], [440,385],
  [440,442], [117,443], [117,375], [444,332], [444,143], [444,441], [444,144], [444,336], [445,332], [445,413],
  [445,441], [445,383], [445,428], [332,428], [332,336], [380,381], [380,446], [380,403], [380,404], [380,382],
  [381,416], [381,447], [381,446], [381,384], [412,413], [95,419], [419,104], [394,436], [394,369], [394,396],
  [106,159], [159,16], [346,347], [365,448], [365,366], [365,417], [365,373], [365,351], [352,353], [352,449],
  [352,450], [352,451], [377,378], [377,363], [377,351], [377,364], [402,405], [402,452], [402,453], [402,406],
  [454,408], [454,435], [454,409], [448,417], [448,443], [448,373], [448,375], [366,416], [366,417], [366,347],
  [366,345], [416,447], [416,345], [416,342], [447,446], [447,342], [446,403], [446,342], [446,343], [446,353],
  [403,405], [403,353], [151,453], [151,156], [151,152], [151,455], [417,443], [417,456], [417,424], [417,418],
  [378,373], [378,374], [180,143], [180,399], [180,457], [404,413], [404,382], [404,414], [404,406], [119,372],
  [405,453], [405,353], [405,449], [405,455], [413,382], [413,383], [413,414], [350,351], [350,364], [408,400],
  [408,458], [408,414], [408,398], [98,443], [98,424], [400,458], [400,398], [400,328], [459,123], [459,460],
  [459,124], [459,461], [459,462], [459,463], [123,460], [443,424], [443,375], [460,99], [460,462], [460,464],
  [460,425], [372,374], [372,86], [368,74], [368,465], [368,324], [368,128], [368,370], [143,457], [143,336],
  [382,383], [373,374], [373,375], [456,424], [456,418], [456,464], [456,425], [74,324], [129,465], [124,461],
  [124,465], [441,144], [441,383], [441,385], [333,435], [333,409], [461,465], [461,466], [461,370], [461,463],
  [465,128], [465,370], [324,327], [99,425], [452,453], [452,152], [452,406], [452,328], [424,425], [437,438],
  [436,369], [436,177], [390,391], [422,423], [422,442], [418,462], [418,384], [418,467], [418,464], [418,463],
  [453,152], [453,455], [420,421], [420,156], [420,360], [420,450], [420,451], [421,360], [421,450], [462,464],
  [462,463], [156,451], [156,455], [369,177], [369,396], [369,370], [152,328], [458,414], [458,406], [458,328],
  [177,327], [395,396], [395,466], [395,423], [395,463], [383,385], [396,466], [396,370], [384,385], [384,467],
  [384,442], [385,442], [467,423], [467,463], [467,442], [414,406], [466,370], [466,463], [423,463], [423,442],
  [464,425], [406,328], [435,409], [435,398], [435,399], [409,410], [398,399], [360,357], [357,358], [358,362],
  [362,363], [363,364], [351,364], [375,118], [347,345], [345,342], [342,343], [343,353], [353,449], [449,451],
  [449,455], [410,428], [399,335], [399,457], [334,335], [334,457], [334,336], [335,457], [457,336], [450,451],
  [451,455]
];

// Pełny wariant Dense: kontury + fill. Długość = 1102.
const FACEMESH_DENSE = [...FACEMESH_CONTOURS, ...FACEMESH_FILL];

    const BS = BS_INDEX;

    // Lista krawedzi: najpierw fill (ciemniejszy), potem kontury (jasniejsze).
    // Identyczna kolejnosc co w faceBackground.js, dzieki czemu drawEdges
    // moze polegac na bicie isContour w bucket key.
    function buildEdgeList() {
      const result = [];
      for (let i = 0; i < FACEMESH_FILL.length; i++) {
        const a = FACEMESH_FILL[i][0];
        const b = FACEMESH_FILL[i][1];
        if (a < NUM_VERTICES && b < NUM_VERTICES) result.push([a, b, 0]);
      }
      for (let i = 0; i < FACEMESH_CONTOURS.length; i++) {
        const a = FACEMESH_CONTOURS[i][0];
        const b = FACEMESH_CONTOURS[i][1];
        if (a < NUM_VERTICES && b < NUM_VERTICES) result.push([a, b, 1]);
      }
      return result;
    }
    const EDGES = buildEdgeList();

    // Aproksymowane normale per-vertex: kierunek od centroidu do wierzcholka.
    // Sluzy do miekkiego tlumienia krawedzi tylnych w drawEdges (visibility).
    function buildBaseNormals() {
      const nx = new Float32Array(NUM_VERTICES);
      const ny = new Float32Array(NUM_VERTICES);
      const nz = new Float32Array(NUM_VERTICES);
      let ccx = 0, ccy = 0, ccz = 0;
      for (let i = 0; i < NUM_VERTICES; i++) {
        const j = i * 3;
        ccx += BASE_POSITIONS[j];
        ccy += BASE_POSITIONS[j + 1];
        ccz += BASE_POSITIONS[j + 2];
      }
      ccx /= NUM_VERTICES; ccy /= NUM_VERTICES; ccz /= NUM_VERTICES;
      for (let i = 0; i < NUM_VERTICES; i++) {
        const j = i * 3;
        const dx = BASE_POSITIONS[j] - ccx;
        const dy = BASE_POSITIONS[j + 1] - ccy;
        const dz = BASE_POSITIONS[j + 2] - ccz;
        const len = Math.sqrt(dx * dx + dy * dy + dz * dz);
        if (len > 1e-6) {
          nx[i] = dx / len; ny[i] = dy / len; nz[i] = dz / len;
        } else {
          nx[i] = 0; ny[i] = 0; nz[i] = 1;
        }
      }
      return { nx, ny, nz };
    }
    const BASE_NORMALS = buildBaseNormals();

    // Domyslne nachylenie glowy w dol (broda nizej) — identycznie jak login.
    const PITCH_BASE_OFFSET = -0.09;
    // Skala bazowa dobrana pod canvas 640x480 (kafelek kamery Teams).
    const FACE_SCALE_MUL = 0.32;

    // Stan renderera (analogon `state` z faceBackground.js).
    const faceState = {
      workVertices: new Float32Array(NUM_VERTICES * 3),
      projX: new Float32Array(NUM_VERTICES),
      projY: new Float32Array(NUM_VERTICES),
      projZ: new Float32Array(NUM_VERTICES),
      normalZ: new Float32Array(NUM_VERTICES),
      mimicry: {
        mouth_open: 0, smile: 0, frown: 0,
        blink_left: 0, blink_right: 0,
        eyebrow_left: 0, eyebrow_right: 0,
        cheek_puff: 0, angry: 0,
        vis_aa: 0, vis_oo: 0, vis_ee: 0, vis_mm: 0,
        vis_ff: 0, vis_ll: 0, vis_ss: 0, vis_ch: 0,
      },
      phase: 0,
      lastFrameMs: 0,
      blinkState: null,
      actions: [],
      nextBlinkAt: 1.5 + Math.random() * 2.0,
      nextSmileAt: 4.0 + Math.random() * 4.0,
      nextBrowSurpriseAt: 6.0 + Math.random() * 6.0,
      nextBrowAsymAt: 3.0 + Math.random() * 5.0,
      nextFrownAt: 10.0 + Math.random() * 8.0,
      nextYawnAt: 15.0 + Math.random() * 10.0,
      nextVisemeAt: 2.0 + Math.random() * 4.0,
      nextCheekAt: 12.0 + Math.random() * 10.0,
    };

    // workVertices = BASE + sum(weight_i * DELTA_i). Pomija blendshape'y
    // ponizej progu. Maska left/right pozwala niezaleznie sterowac strona.
    function applyBlendshapes(m) {
      const dst = faceState.workVertices;
      dst.set(BASE_POSITIONS);
      const WEIGHT_THRESHOLD = 1e-4;
      const apply = (bsIdx, weight, maskLeft, maskRight) => {
        if (bsIdx == null || bsIdx < 0) return;
        if (Math.abs(weight) <= WEIGHT_THRESHOLD) return;
        const deltas = BLENDSHAPE_DELTAS[bsIdx];
        if (!deltas) return;
        if (maskLeft) {
          for (let i = 0; i < NUM_VERTICES; i++) {
            const w = weight * maskLeft[i];
            if (w === 0) continue;
            const j = i * 3;
            dst[j]     += deltas[j]     * w;
            dst[j + 1] += deltas[j + 1] * w;
            dst[j + 2] += deltas[j + 2] * w;
          }
        } else if (maskRight) {
          for (let i = 0; i < NUM_VERTICES; i++) {
            const w = weight * maskRight[i];
            if (w === 0) continue;
            const j = i * 3;
            dst[j]     += deltas[j]     * w;
            dst[j + 1] += deltas[j + 1] * w;
            dst[j + 2] += deltas[j + 2] * w;
          }
        } else {
          for (let i = 0; i < NUM_VERTICES; i++) {
            const j = i * 3;
            dst[j]     += deltas[j]     * weight;
            dst[j + 1] += deltas[j + 1] * weight;
            dst[j + 2] += deltas[j + 2] * weight;
          }
        }
      };
      apply(BS.mouth_open, m.mouth_open, null, null);
      apply(BS.smile, m.smile, null, null);
      apply(BS.frown, m.frown, null, null);
      apply(BS.blink, m.blink_left, LEFT_MASK, null);
      apply(BS.blink, m.blink_right, null, RIGHT_MASK);
      apply(BS.brow_up, m.eyebrow_left, LEFT_MASK, null);
      apply(BS.brow_up, m.eyebrow_right, null, RIGHT_MASK);
      apply(BS.cheek_puff, m.cheek_puff, null, null);
      apply(BS.angry, m.angry, null, null);
      apply(BS.vis_aa, m.vis_aa, null, null);
      apply(BS.vis_oo, m.vis_oo, null, null);
      apply(BS.vis_ee, m.vis_ee, null, null);
      apply(BS.vis_mm, m.vis_mm, null, null);
      apply(BS.vis_ff, m.vis_ff, null, null);
      apply(BS.vis_ll, m.vis_ll, null, null);
      apply(BS.vis_ss, m.vis_ss, null, null);
      apply(BS.vis_ch, m.vis_ch, null, null);
    }

    // Projekcja 3D->2D: yaw (Y) -> pitch (X) -> perspektywa 1/(1.8 - z').
    // W tej samej petli rotujemy normale (bez perspektywy), zeby uniknac
    // drugiego przejscia po wierzcholkach.
    function project(cxp, cyp, scale, yaw, pitch) {
      const sinY = Math.sin(yaw);
      const cosY = Math.cos(yaw);
      const sinP = Math.sin(pitch);
      const cosP = Math.cos(pitch);
      const scalePersp = scale * 1.8;
      const src = faceState.workVertices;
      const px = faceState.projX;
      const py = faceState.projY;
      const pz = faceState.projZ;
      const nz = faceState.normalZ;
      const bnx = BASE_NORMALS.nx;
      const bny = BASE_NORMALS.ny;
      const bnz = BASE_NORMALS.nz;
      for (let i = 0; i < NUM_VERTICES; i++) {
        const j = i * 3;
        const x = src[j];
        const y = src[j + 1];
        const z = src[j + 2];
        const x1 = x * cosY + z * sinY;
        const z1 = -x * sinY + z * cosY;
        const y1 = y * cosP - z1 * sinP;
        const z2 = y * sinP + z1 * cosP;
        const depth = 1.8 - z2;
        const invDepth = depth > 0.1 ? 1.0 / depth : 1.0 / 0.1;
        px[i] = cxp + x1 * invDepth * scalePersp;
        py[i] = cyp + y1 * invDepth * scalePersp;
        pz[i] = z2;

        const nx0 = bnx[i];
        const ny0 = bny[i];
        const nz0 = bnz[i];
        const nz1r = -nx0 * sinY + nz0 * cosY;
        nz[i] = ny0 * sinP + nz1r * cosP;
      }
    }

    // Statyczna paleta — bot nie ma trybu shake (zarezerwowany do interakcji
    // userskich w login screen), wiec tint zwracamy stale jako bialy.
    function computeTintColor() {
      return { r: 255, g: 255, b: 255 };
    }

    // Bot rysuje na jednym canvas (MediaStreamTrackGenerator) bez glow canvas
    // (CSS filter:blur na osobnym layerze nie ma sensu, bo encoder pakuje
    // tylko ten jeden backbuffer). Stroke w jednym przejsciu z bucketami
    // alpha/lineWidth — identyczna paleta i krzywa co main canvas w faceBg.
    function drawEdges(ctx) {
      const px = faceState.projX;
      const py = faceState.projY;
      const pz = faceState.projZ;
      const nz = faceState.normalZ;
      const tint = computeTintColor();
      const buckets = new Map();
      for (let i = 0; i < EDGES.length; i++) {
        const e = EDGES[i];
        const a = e[0];
        const b = e[1];
        const isContour = e[2];
        const visibility = (nz[a] + nz[b]) * 0.5;
        let t = (visibility + 0.3) / 1.3;
        if (t < 0) t = 0; else if (t > 1) t = 1;
        const smooth = t * t * (3 - 2 * t);
        const visFade = 0.08 + smooth * 0.92;
        const avgZ = (pz[a] + pz[b]) * 0.5;
        let depthT = (avgZ + 0.5) * 1.3;
        if (depthT < 0) depthT = 0; else if (depthT > 1) depthT = 1;
        let alpha = (depthT * 0.55 + 0.45) * visFade;
        if (!isContour) alpha *= 0.5;
        if (alpha < 0.01) continue;
        const alphaBucket = Math.round(alpha * 19);
        const widthBucket = Math.round(depthT * 9);
        const key = (isContour << 9) | (widthBucket << 5) | alphaBucket;
        let arr = buckets.get(key);
        if (!arr) { arr = []; buckets.set(key, arr); }
        arr.push(i);
      }
      // Pass 1: glow canvas — ta sama geometria, ten sam tint, blur dochodzi
      // przez ctx.filter przy drawImage. Skia robi blur na CPU, ale dla
      // 640x480 i jednego drawImage per frame (30 fps) budzet ~2-3 ms.
      glowCtx.clearRect(0, 0, W, H);
      glowCtx.lineCap = 'butt';
      glowCtx.globalCompositeOperation = 'source-over';
      for (const [key, arr] of buckets) {
        const alphaBucket = key & 0x1f;
        const widthBucket = (key >> 5) & 0xf;
        const alpha = alphaBucket / 19;
        const depthT = widthBucket / 9;
        glowCtx.lineWidth = 1.4 + depthT * 0.5;
        glowCtx.strokeStyle = 'rgba(' + tint.r + ',' + tint.g + ',' + tint.b + ',' + alpha.toFixed(3) + ')';
        glowCtx.beginPath();
        for (let i = 0; i < arr.length; i++) {
          const e = EDGES[arr[i]];
          glowCtx.moveTo(px[e[0]], py[e[0]]);
          glowCtx.lineTo(px[e[1]], py[e[1]]);
        }
        glowCtx.stroke();
      }
      // Pass 2: sharp stroke do main ctx.
      ctx.lineCap = 'butt';
      ctx.globalCompositeOperation = 'source-over';
      for (const [key, arr] of buckets) {
        const alphaBucket = key & 0x1f;
        const widthBucket = (key >> 5) & 0xf;
        const alpha = alphaBucket / 19;
        const depthT = widthBucket / 9;
        ctx.lineWidth = 1.4 + depthT * 0.5;
        ctx.strokeStyle = 'rgba(' + tint.r + ',' + tint.g + ',' + tint.b + ',' + alpha.toFixed(3) + ')';
        ctx.beginPath();
        for (let i = 0; i < arr.length; i++) {
          const e = EDGES[arr[i]];
          const a = e[0];
          const b = e[1];
          ctx.moveTo(px[a], py[a]);
          ctx.lineTo(px[b], py[b]);
        }
        ctx.stroke();
      }
      // Pass 3: blurred glow kompozyt — 'lighter' addytywnie sumuje halo
      // wygladzajace przejscie front->back (login pass to mix-blend-mode
      // 'plus-lighter' na compositorze GPU; tu robimy CPU equivalent).
      ctx.save();
      ctx.globalCompositeOperation = 'lighter';
      ctx.filter = 'blur(8px) saturate(1.4)';
      ctx.drawImage(glowCanvas, 0, 0);
      ctx.filter = 'none';
      ctx.restore();
    }

    // Scheduler idle-akcji — port 1:1 z faceBackground.js. Kazda akcja
    // sumuje sie na mimicry przed applyBlendshapes.
    function easeInOut(t) {
      return t < 0.5 ? 2 * t * t : 1 - Math.pow(-2 * t + 2, 2) * 0.5;
    }
    function scheduleAction(now, opts) {
      faceState.actions.push({
        bsKey: opts.bsKey,
        side: opts.side || null,
        peakValue: opts.peakValue,
        t0: now,
        attack: opts.attack,
        hold: opts.hold,
        release: opts.release,
      });
    }
    function evalActions(now, m) {
      const actions = faceState.actions;
      for (let i = actions.length - 1; i >= 0; i--) {
        const a = actions[i];
        const local = now - a.t0;
        const total = a.attack + a.hold + a.release;
        if (local >= total) { actions.splice(i, 1); continue; }
        let v = 0;
        if (local < a.attack) {
          v = easeInOut(local / a.attack) * a.peakValue;
        } else if (local < a.attack + a.hold) {
          v = a.peakValue;
        } else {
          const releaseLocal = (local - a.attack - a.hold) / a.release;
          v = easeInOut(1 - releaseLocal) * a.peakValue;
        }
        if (a.bsKey === 'eyebrow') {
          if (a.side === 'left' || a.side === 'both') m.eyebrow_left += v;
          if (a.side === 'right' || a.side === 'both') m.eyebrow_right += v;
        } else {
          m[a.bsKey] += v;
        }
      }
    }

    // Idle pipeline: oddech + mrugniecia + 7 rodzin akcji (brew suprise/asym,
    // angry, ziewniecie, mikro-viseme, cheek puff, pol-usmieszki).
    function tickIdle() {
      const m = faceState.mimicry;
      const t = faceState.phase;
      m.mouth_open = 0; m.smile = 0; m.frown = 0;
      m.eyebrow_left = 0; m.eyebrow_right = 0;
      m.cheek_puff = 0; m.angry = 0;
      m.vis_aa = 0; m.vis_oo = 0; m.vis_ee = 0; m.vis_mm = 0;
      m.vis_ff = 0; m.vis_ll = 0; m.vis_ss = 0; m.vis_ch = 0;

      // Oddech jako pasywny offset na ustach.
      m.mouth_open = 0.05 + Math.sin(t * 0.8) * 0.02;

      // Mrugniecia: krzywa in/hold/out wewnatrz blinkState.
      if (faceState.blinkState === null && t >= faceState.nextBlinkAt) {
        faceState.blinkState = { phase: 'in', t0: t, duration: 0.08 };
      }
      if (faceState.blinkState) {
        const bs = faceState.blinkState;
        const local = t - bs.t0;
        let value = 0;
        if (bs.phase === 'in') {
          value = Math.min(local / bs.duration, 1);
          if (local >= bs.duration) {
            bs.phase = 'hold'; bs.t0 = t; bs.duration = 0.05;
          }
        } else if (bs.phase === 'hold') {
          value = 1;
          if (local >= bs.duration) {
            bs.phase = 'out'; bs.t0 = t; bs.duration = 0.12;
          }
        } else if (bs.phase === 'out') {
          value = Math.max(1 - local / bs.duration, 0);
          if (local >= bs.duration) {
            faceState.blinkState = null;
            faceState.nextBlinkAt = t + 3.5 + Math.random() * 2.0;
          }
        }
        m.blink_left = value;
        m.blink_right = value;
      } else {
        m.blink_left = 0;
        m.blink_right = 0;
      }

      if (t >= faceState.nextSmileAt) {
        const polarity = Math.random() < 0.7 ? 1 : -1;
        const peak = polarity > 0 ? 0.15 + Math.random() * 0.15 : -(0.1 + Math.random() * 0.15);
        scheduleAction(t, { bsKey: 'smile', peakValue: peak, attack: 0.3, hold: 0.6, release: 0.3 });
        faceState.nextSmileAt = t + 11.0 + Math.random() * 6.0;
      }
      if (t >= faceState.nextBrowSurpriseAt) {
        scheduleAction(t, { bsKey: 'eyebrow', side: 'both', peakValue: 0.6, attack: 0.2, hold: 0.4, release: 0.9 });
        if (Math.random() < 0.7) {
          scheduleAction(t, { bsKey: 'mouth_open', peakValue: 0.15, attack: 0.2, hold: 0.3, release: 0.5 });
        }
        faceState.nextBrowSurpriseAt = t + 14.0 + Math.random() * 8.0;
      }
      if (t >= faceState.nextBrowAsymAt) {
        const side = Math.random() < 0.5 ? 'left' : 'right';
        scheduleAction(t, { bsKey: 'eyebrow', side, peakValue: 0.45, attack: 0.25, hold: 0.4, release: 0.35 });
        faceState.nextBrowAsymAt = t + 9.0 + Math.random() * 7.0;
      }
      if (t >= faceState.nextFrownAt) {
        scheduleAction(t, { bsKey: 'angry', peakValue: 0.4, attack: 0.3, hold: 0.6, release: 0.3 });
        faceState.nextFrownAt = t + 18.0 + Math.random() * 12.0;
      }
      if (t >= faceState.nextYawnAt) {
        scheduleAction(t, { bsKey: 'mouth_open', peakValue: 0.4, attack: 0.5, hold: 0.3, release: 0.7 });
        scheduleAction(t, { bsKey: 'eyebrow', side: 'both', peakValue: 0.2, attack: 0.5, hold: 0.3, release: 0.7 });
        faceState.nextYawnAt = t + 25.0 + Math.random() * 15.0;
      }
      if (t >= faceState.nextVisemeAt) {
        const choices = ['vis_aa', 'vis_oo', 'vis_ee', 'vis_mm'];
        const key = choices[Math.floor(Math.random() * choices.length)];
        const peak = 0.3 + Math.random() * 0.2;
        scheduleAction(t, { bsKey: key, peakValue: peak, attack: 0.08, hold: 0.19, release: 0.08 });
        faceState.nextVisemeAt = t + 5.0 + Math.random() * 4.0;
      }
      if (t >= faceState.nextCheekAt) {
        scheduleAction(t, { bsKey: 'cheek_puff', peakValue: 0.3, attack: 0.2, hold: 0.3, release: 0.2 });
        faceState.nextCheekAt = t + 20.0 + Math.random() * 15.0;
      }

      evalActions(t, m);
    }

    // Pelna klatka: idle -> blendshape -> projekcja -> stroke.
    // Yaw/pitch wylacznie z idle oscillation (bot nie ma kursora ani
    // DeviceOrientationEvent — headless Xvfb).
    function renderFaceFrame(nowMs) {
      const dt = faceState.lastFrameMs > 0 ? (nowMs - faceState.lastFrameMs) / 1000 : 1 / 30;
      faceState.lastFrameMs = nowMs;
      faceState.phase += dt;
      tickIdle();
      applyBlendshapes(faceState.mimicry);

      // Czarne tlo identycznie jak ekran logowania (login.css ma ciemny grad
      // przez face-bg-root, ale tlo samego canvas to czern). Bot nie ma
      // gradientu DOM pod canvas, wiec malujemy bezposrednio.
      ctx.fillStyle = '#0a0b18';
      ctx.fillRect(0, 0, W, H);

      const baseScale = Math.min(W, H) * FACE_SCALE_MUL;
      const yawBase = Math.sin(faceState.phase * 0.15) * 0.15;
      const pitchBase = PITCH_BASE_OFFSET + Math.sin(faceState.phase * 0.1) * 0.08;
      // cy * 1.12 zeby twarz siedziala lekko nad srodkiem (broda ku dolowi
      // ramki) — proporcja jak na login (h * 0.56 wzgledem kontenera dla
      // pelnej wysokosci ekranu, tu kafelek 480 px wymaga mniejszego shiftu).
      project(W * 0.5, H * 0.56, baseScale, yawBase, pitchBase);
      drawEdges(ctx);
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
    try {
      setupVideoInjection();
    } catch (e) {
      console.warn('[tentaflow] setupVideoInjection blad', e);
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

    function tileDisplayName(tile) {
      // aria-label zawiera realna nazwe uczestnika (np. "Jan Kowalski, video, ...");
      // data-tid jest internal id Teams i nie nadaje sie do GUI.
      const al = tile.getAttribute('aria-label') || '';
      const trimmed = al.split(',')[0].trim();
      return trimmed || tile.getAttribute('data-tid') || '';
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
      return null;
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
        const tiles = document.querySelectorAll('[data-tid][data-stream-type]');
        const current = new Map();
        for (const tile of tiles) {
          const tid = tile.getAttribute('data-tid');
          if (!tid) continue;
          current.set(tid, tileDisplayName(tile));
        }
        for (const [tid, name] of current) {
          if (!knownTiles.has(tid)) {
            emit('participant_joined', { id: tid, name: name });
          }
        }
        for (const [tid, name] of knownTiles) {
          if (!current.has(tid)) {
            emit('participant_left', { id: tid, name: name });
          }
        }
        knownTiles = current;

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
      // Safety-net 1s — pokryje przegapiona mutacje w iframe / shadow root.
      // Nie polling 500ms, tylko brakujacy edge-case net.
      // UWAGA: NIE uzywamy registerInterval bo cleanupTentaflow() (WS close)
      // by to zniszczyl. Bridge przezywa rozlaczenia WS audio.
      setInterval(schedule, 1000);
    }
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
    // POLL_MS=50 daje 20Hz sampling — Chromium internal audioLevel stats
    // aktualizuja sie co ~20ms, wiec 50ms jest blisko realnego limitu tej
    // techniki. getStats() jest lokalny (~100-500us), 20Hz × kilka pc to
    // pomijalny CPU. SILENCE_HOLD_MS=300 — szybciej kasuje po koncu zdania,
    // dalej debounce'uje pauzy miedzy sylabami.
    const SPEAKER_START_LEVEL = 0.03;
    const SPEAKER_HOLD_LEVEL = 0.005;
    const SPEAKER_SILENCE_HOLD_MS = 300;
    const SPEAKER_POLL_MS = 50;
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
      // Wez pierwszy remote tile (nie nasz). data-tid w light-meetings == name.
      const tiles = document.querySelectorAll('[data-tid][data-stream-type]');
      const ourName = (window.__tentaflowBotName || '').toString();
      for (const t of tiles) {
        const name = t.getAttribute('data-tid') || '';
        // Teams dodaje " (Unverified)" / " (External)" — startsWith filtruje
        // i bota gdy ourName="DevBot" pasuje do "DevBot (Unverified)".
        if (name && !(ourName && name.indexOf(ourName) === 0)) {
          return name;
        }
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

    setInterval(async function () {
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
    }, SPEAKER_POLL_MS);
  }

  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', bootstrap);
  } else {
    bootstrap();
  }
})();
