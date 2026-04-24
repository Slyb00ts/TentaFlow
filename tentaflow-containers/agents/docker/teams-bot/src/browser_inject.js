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

  // Lista aktywnych interwalow — posprzatamy je w cleanupTentaflow() gdy WS sie zamknie.
  const __tfIntervals = [];
  function registerInterval(id) {
    __tfIntervals.push(id);
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
        const combined = new MediaStream();
        if (constraints.audio && micGenerator) combined.addTrack(micGenerator);
        if (constraints.video && videoGenerator) combined.addTrack(videoGenerator);
        if (combined.getTracks().length > 0) return combined;
        return origGum(constraints);
      } catch (e) {
        console.warn('[tentaflow] getUserMedia patch blad, fallback na oryginalny', e);
        return origGum(constraints);
      }
    };
    if (window.__tentaflowBridge) window.__tentaflowBridge.micSetupDone = true;
  }

  // --------------------------------------------------------------------------
  // Roster scraping — lista uczestnikow spotkania
  // Teams renderuje panel uczestnikow w [data-tid="roster-panel"]. Gdy panel
  // jest zamkniety korzystamy z aria-label kafelkow video jako fallback.
  // --------------------------------------------------------------------------
  function getRosterList() {
    const seen = new Map();
    const rosterItems = document.querySelectorAll(
      '[data-tid="roster-panel"] li[role="option"], [data-tid="roster-panel"] [role="treeitem"]'
    );
    rosterItems.forEach((item) => {
      // Tylko dedykowane selektory — item.textContent zgarnialby caly widget
      // z roli, statusem, odznakami itd., co psuje liste i metadata STT.
      const nameEl = item.querySelector('.ts-tooltip-trigger, [data-tid="roster-participant-name"]');
      if (!nameEl) return;
      const name = (nameEl.textContent || '').trim();
      if (!name) return;
      const statusEl = item.querySelector('[data-tid="roster-participant-status"]');
      const status = statusEl ? (statusEl.textContent || '').trim() : 'present';
      if (!seen.has(name)) seen.set(name, { name, status });
    });
    if (seen.size === 0) {
      // Fallback: kafelki video (gdy panel roster zamkniety)
      document.querySelectorAll('[aria-label^="Video of"], [aria-label^="Video tile"]').forEach((tile) => {
        const label = tile.getAttribute('aria-label') || '';
        const m = label.match(/Video (?:of|tile, )\s*(.+?)(?:,|$)/i);
        if (m && m[1]) {
          const name = m[1].trim();
          if (!seen.has(name)) seen.set(name, { name, status: 'present' });
        }
      });
    }
    return Array.from(seen.values());
  }

  function sendRoster() {
    if (!ws || ws.readyState !== WebSocket.OPEN) return;
    try {
      const roster = getRosterList();
      const json = JSON.stringify(roster);
      const payload = new TextEncoder().encode(json);
      const buf = new ArrayBuffer(1 + payload.byteLength);
      const u8 = new Uint8Array(buf);
      u8[0] = 0x03;
      u8.set(payload, 1);
      ws.send(buf);
    } catch (e) {
      console.warn('[tentaflow] sendRoster blad:', e);
    }
  }

  // --------------------------------------------------------------------------
  // Active speaker tracking — kto aktualnie mowi
  // Teams oznacza aktywnego mowce klasa active-speaker na kafelku wideo oraz
  // atrybutem [data-is-presenter="true"]. Dostepny tez aria-label "... is speaking".
  // --------------------------------------------------------------------------
  function getActiveSpeaker() {
    const presenter = document.querySelector('[data-is-presenter="true"]');
    if (presenter) {
      const label = presenter.getAttribute('aria-label') || presenter.textContent || '';
      const m = label.match(/^(.+?)(?:,|$)/);
      if (m && m[1]) return m[1].trim();
    }
    const active = document.querySelector('.active-speaker, [data-tid="active-speaker"]');
    if (active) {
      const nameEl = active.querySelector('.ts-tooltip-trigger, [data-tid="participant-name"]');
      if (nameEl) {
        const name = (nameEl.textContent || '').trim();
        if (name) return name;
      }
      const label = active.getAttribute('aria-label') || '';
      const m = label.match(/^(.+?)(?:,|$)/);
      if (m && m[1]) return m[1].trim();
    }
    const speakingEl = document.querySelector('[aria-label*="is speaking"]');
    if (speakingEl) {
      const label = speakingEl.getAttribute('aria-label') || '';
      const m = label.match(/^(.+?)\s+is speaking/i);
      if (m && m[1]) return m[1].trim();
    }
    return null;
  }

  let lastActiveSpeaker = null;
  function sendActiveSpeakerIfChanged() {
    if (!ws || ws.readyState !== WebSocket.OPEN) return;
    try {
      const current = getActiveSpeaker();
      if (current === lastActiveSpeaker) return;
      lastActiveSpeaker = current;
      const payload = current ? new TextEncoder().encode(current) : new Uint8Array(0);
      const buf = new ArrayBuffer(1 + payload.byteLength);
      const u8 = new Uint8Array(buf);
      u8[0] = 0x04;
      if (payload.byteLength > 0) u8.set(payload, 1);
      ws.send(buf);
    } catch (e) {
      console.warn('[tentaflow] sendActiveSpeakerIfChanged blad:', e);
    }
  }

  // --------------------------------------------------------------------------
  // Video injection — kamerka bota (avatar 640x480 @ 30fps)
  // --------------------------------------------------------------------------
  let videoGenerator = null;
  let videoWriter = null;
  function setupVideoInjection() {
    if (window.__tentaflowBridge && window.__tentaflowBridge.videoSetupDone) return;
    if (typeof MediaStreamTrackGenerator === 'undefined' ||
        typeof OffscreenCanvas === 'undefined' ||
        typeof VideoFrame === 'undefined') {
      console.warn('[tentaflow] MSTG/OffscreenCanvas/VideoFrame niedostepne — video injection wylaczone');
      videoGenerator = null;
      window.__tentaflowVideoAvailable = false;
      return;
    }
    try {
      videoGenerator = new MediaStreamTrackGenerator({ kind: 'video' });
      videoWriter = videoGenerator.writable.getWriter();
    } catch (e) {
      console.warn('[tentaflow] Blad tworzenia video generator', e);
      videoGenerator = null;
      window.__tentaflowVideoAvailable = false;
      return;
    }
    const W = 640, H = 480;
    const canvas = new OffscreenCanvas(W, H);
    const ctx = canvas.getContext('2d');
    let baseTs = 0;
    // 30 FPS so the rotating ring looks smooth as Teams compresses the tile.
    // VideoFrame objects are cheap to produce with OffscreenCanvas and Chromium
    // reuses the underlying GPU buffer, so 30 fps costs little over the 1 fps
    // static avatar we had before.
    const FPS = 30;
    const frameIntervalUs = Math.round(1_000_000 / FPS);
    const TAU = Math.PI * 2;
    const cx = W / 2, cy = H / 2;
    const ringOuter = 140, ringInner = 116;
    const accent = '#7c5cff';
    const accentSoft = 'rgba(99,102,241,0.20)';
    const dotColor = '#9b87ff';
    const label = 'TENTAFLOW';
    let t0 = performance.now();
    const drawAndWrite = async () => {
      let bitmap = null;
      let frame = null;
      try {
        const t = (performance.now() - t0) / 1000;
        // Deep navy gradient backdrop — same palette as the login screen.
        const grad = ctx.createRadialGradient(cx, cy, 40, cx, cy, 380);
        grad.addColorStop(0, '#171a2e');
        grad.addColorStop(1, '#0a0b18');
        ctx.fillStyle = grad;
        ctx.fillRect(0, 0, W, H);

        // Soft pulsing halo behind the ring.
        const pulse = 0.5 + 0.5 * Math.sin(t * 1.4);
        ctx.beginPath();
        ctx.fillStyle = `rgba(124,92,255,${0.06 + 0.05 * pulse})`;
        ctx.arc(cx, cy - 10, 190 + pulse * 8, 0, TAU);
        ctx.fill();

        // Base track of the ring (dim).
        ctx.beginPath();
        ctx.lineWidth = ringOuter - ringInner;
        ctx.strokeStyle = accentSoft;
        ctx.arc(cx, cy - 10, (ringOuter + ringInner) / 2, 0, TAU);
        ctx.stroke();

        // Rotating highlighted arc — same feel as the login spinner.
        const angle = (t * 2.6) % TAU;
        ctx.beginPath();
        ctx.lineCap = 'round';
        ctx.strokeStyle = accent;
        ctx.arc(cx, cy - 10, (ringOuter + ringInner) / 2, angle - 1.2, angle);
        ctx.stroke();

        // Orbiting dot to add life at slower tempo.
        const dotAngle = angle * 1.3;
        const dx = cx + Math.cos(dotAngle) * (ringOuter + 14);
        const dy = (cy - 10) + Math.sin(dotAngle) * (ringOuter + 14);
        ctx.beginPath();
        ctx.fillStyle = dotColor;
        ctx.arc(dx, dy, 6, 0, TAU);
        ctx.fill();

        // Center mark — letter T for brand recognition. A wireframe face would
        // be a nicer port of faceBackground.js, but would require shipping the
        // face-data / face-edges datasets into the bot container.
        ctx.fillStyle = '#f5f5ff';
        ctx.font = 'bold 96px "Segoe UI", system-ui, sans-serif';
        ctx.textAlign = 'center';
        ctx.textBaseline = 'middle';
        ctx.fillText('T', cx, cy - 14);

        // Label row underneath.
        ctx.fillStyle = 'rgba(220, 220, 255, 0.72)';
        ctx.font = '600 22px "Segoe UI", system-ui, sans-serif';
        ctx.letterSpacing = '4px';
        ctx.fillText(label, cx, cy + 120);

        // Animated three-dot activity row.
        const dotsY = cy + 150;
        for (let i = 0; i < 3; i++) {
          const phase = (t * 2 - i * 0.35) % 1.2;
          const alpha = phase < 1 ? Math.sin(phase * Math.PI) : 0;
          ctx.beginPath();
          ctx.fillStyle = `rgba(155, 135, 255, ${0.25 + 0.6 * alpha})`;
          ctx.arc(cx - 20 + i * 20, dotsY, 3.5, 0, TAU);
          ctx.fill();
        }

        bitmap = canvas.transferToImageBitmap();
        frame = new VideoFrame(bitmap, { timestamp: baseTs });
        baseTs += frameIntervalUs;
        await videoWriter.write(frame);
      } catch (e) {
        cleanupTentaflow();
      } finally {
        if (frame) { try { frame.close(); } catch (_) {} }
        if (bitmap && bitmap.close) { try { bitmap.close(); } catch (_) {} }
      }
    };
    registerInterval(setInterval(drawAndWrite, Math.round(1000 / FPS)));
    window.__tentaflowVideoAvailable = true;
    if (window.__tentaflowBridge) window.__tentaflowBridge.videoSetupDone = true;
    console.log('[tentaflow] Video injection zainicjalizowane (' + W + 'x' + H + ' @ ' + FPS + 'fps)');
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
    if (window.__tentaflowBridge && window.__tentaflowBridge.setupDone) {
      console.log('[tentaflow] bootstrap juz wykonany — pomijam');
      return;
    }
    // WAZNE: hook RTCPeerConnection musi byc ZANIM Teams stworzy pc.
    // Tutaj jestesmy w evaluate_on_new_document, przed jakimkolwiek JS strony,
    // wiec jestesmy bezpieczni.
    try {
      hookRTCPeerConnection();
    } catch (e) {
      console.warn('[tentaflow] hookRTCPeerConnection blad', e);
    }
    // Video setup PRZED hookRTCPeerConnection uzyciem, zeby flag
    // __tentaflowVideoAvailable byl ustawiony zanim Rust odpyta o niego w prejoin.
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
    // Polling roster (3s) i active-speaker (500ms)
    registerInterval(setInterval(sendRoster, 3000));
    registerInterval(setInterval(sendActiveSpeakerIfChanged, 500));
    if (window.__tentaflowBridge) window.__tentaflowBridge.setupDone = true;
    console.log('[tentaflow] Bridge audio zainicjalizowany');
  }

  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', bootstrap);
  } else {
    bootstrap();
  }
})();
