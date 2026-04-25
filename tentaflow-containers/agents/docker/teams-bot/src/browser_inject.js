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
  let videoCanvas = null;
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
    const W = 640, H = 480;
    // captureStream samples whatever the compositor draws for this canvas.
    // A canvas that is never attached to the document never gets composited,
    // so the resulting MediaStreamTrack stays live-but-muted forever — that
    // is exactly the symptom we hit, Teams renders a black tile while the
    // track reports muted=true. Append the canvas off-screen at 1x1 so it
    // counts as part of the rendered tree without taking visible space.
    const canvas = document.createElement('canvas');
    canvas.width = W;
    canvas.height = H;
    canvas.style.cssText =
      'position:fixed;left:-99999px;top:-99999px;width:1px;height:1px;' +
      'pointer-events:none;opacity:0.001;';
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
    let stream = null;
    try {
      stream = canvas.captureStream(30);
      const tracks = stream.getVideoTracks();
      if (!tracks.length) throw new Error('captureStream returned no video tracks');
      videoGenerator = tracks[0];
      try { videoGenerator.contentHint = 'motion'; } catch (_) {}
      // Diagnostic shapshot the moment the stream is built — reveals whether
      // captureStream produces a unmuted track at all (ie. whether the
      // compositor is wired) before the meeting page even sees us.
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

    // ----- Wireframe face mesh data ------------------------------------------
    // Vendored subset of `tentaflow-core/www/js/data/face-data.js` and
    // `face-edges.js` (Head_5 / piotr.bin export — see those files for license
    // and regeneration tooling). We only inline what's needed for a static
    // mesh: 486 base positions and the 125 contour edges. Blendshape deltas,
    // left/right masks and the dense fill are intentionally dropped.
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
    // MediaPipe contours subset (mouth, eyes, brows, oval, simplified nose).
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

    // Pre-allocated projected coordinates so we don't churn arrays per frame.
    const projX = new Float32Array(NUM_VERTICES);
    const projY = new Float32Array(NUM_VERTICES);

    // 3D rotation (yaw around Y, pitch around X) + perspective projection.
    // Same convention as faceBackground.js: depth = 1.8 - z'. Mesh y points up
    // in source data, so we flip when writing to canvas.
    function projectMesh(yaw, pitch, scale) {
      const sinY = Math.sin(yaw);
      const cosY = Math.cos(yaw);
      const sinP = Math.sin(pitch);
      const cosP = Math.cos(pitch);
      const scalePersp = scale * 1.8;
      for (let i = 0; i < NUM_VERTICES; i++) {
        const j = i * 3;
        const x = BASE_POSITIONS[j];
        const y = BASE_POSITIONS[j + 1];
        const z = BASE_POSITIONS[j + 2];
        const x1 = x * cosY + z * sinY;
        const z1 = -x * sinY + z * cosY;
        const y1 = y * cosP - z1 * sinP;
        const z2 = y * sinP + z1 * cosP;
        const depth = 1.8 - z2;
        const inv = depth > 0.1 ? 1.0 / depth : 1.0 / 0.1;
        projX[i] = cx + x1 * inv * scalePersp;
        // Flip Y so source-up renders to canvas-up. Slight upward bias keeps
        // the chin from sitting at the bottom of the 480px frame.
        projY[i] = cy - y1 * inv * scalePersp - 8;
      }
    }

    let t0 = performance.now();
    // Blink schedule: trigger on a randomized cadence and decay over ~140ms.
    let nextBlinkAt = 3.0;
    let blinkPhase = 0;

    const drawAndWrite = () => {
      try {
        const t = (performance.now() - t0) / 1000;
        // Deep navy gradient backdrop — same palette as the login screen.
        const grad = ctx.createRadialGradient(cx, cy, 40, cx, cy, 380);
        grad.addColorStop(0, '#171a2e');
        grad.addColorStop(1, '#0a0b18');
        ctx.fillStyle = grad;
        ctx.fillRect(0, 0, W, H);

        // Soft pulsing halo behind the mesh.
        const pulse = 0.5 + 0.5 * Math.sin(t * 1.4);
        ctx.beginPath();
        ctx.fillStyle = `rgba(124,92,255,${0.06 + 0.05 * pulse})`;
        ctx.arc(cx, cy - 10, 190 + pulse * 8, 0, TAU);
        ctx.fill();

        // Idle motion: gentle yaw oscillation + breathing pitch.
        const yaw = Math.sin(t * 0.4) * 0.12;
        const pitchBase = -0.04 + Math.sin(t * 0.6) * 0.03;

        // Blink: short symmetric pitch nudge so the brow line dips. Crude but
        // we don't have eye-vertex indices broken out, so we avoid touching
        // geometry and just let the halo + a subtle pitch pulse sell it.
        if (t >= nextBlinkAt && blinkPhase === 0) {
          blinkPhase = t;
        }
        let blinkBoost = 0;
        if (blinkPhase > 0) {
          const dt = t - blinkPhase;
          if (dt > 0.14) {
            blinkPhase = 0;
            nextBlinkAt = t + 3.5 + Math.random() * 2.0;
          } else {
            // Triangle curve peaking at 70ms.
            blinkBoost = dt < 0.07 ? dt / 0.07 : (0.14 - dt) / 0.07;
          }
        }
        const pitch = pitchBase + blinkBoost * 0.05;

        projectMesh(yaw, pitch, 150);

        // Build the contour path once — we re-stroke twice (glow + line).
        ctx.beginPath();
        for (let i = 0; i < FACEMESH_CONTOURS.length; i++) {
          const [a, b] = FACEMESH_CONTOURS[i];
          ctx.moveTo(projX[a], projY[a]);
          ctx.lineTo(projX[b], projY[b]);
        }
        // Glow pass.
        ctx.lineWidth = 4;
        ctx.strokeStyle = `rgba(155, 135, 255, ${0.18 + 0.08 * pulse})`;
        ctx.stroke();
        // Sharp pass.
        ctx.lineWidth = 1.2;
        ctx.strokeStyle = 'rgba(180, 155, 255, 0.78)';
        ctx.stroke();

        // Wordmark underneath so the tile is unmistakably TentaFlow.
        ctx.fillStyle = 'rgba(220, 220, 255, 0.72)';
        ctx.font = '600 22px "Segoe UI", system-ui, sans-serif';
        ctx.textAlign = 'center';
        ctx.textBaseline = 'middle';
        ctx.fillText(label, cx, cy + 180);

        // Animated three-dot activity row.
        const dotsY = cy + 210;
        for (let i = 0; i < 3; i++) {
          const phase = (t * 2 - i * 0.35) % 1.2;
          const alpha = phase < 1 ? Math.sin(phase * Math.PI) : 0;
          ctx.beginPath();
          ctx.fillStyle = `rgba(155, 135, 255, ${0.25 + 0.6 * alpha})`;
          ctx.arc(cx - 20 + i * 20, dotsY, 3.5, 0, TAU);
          ctx.fill();
        }

        // captureStream picks up whatever is on the canvas at its own pace;
        // we just need to leave the backbuffer freshly painted. No frame
        // object, no writer, no manual timestamps.
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
