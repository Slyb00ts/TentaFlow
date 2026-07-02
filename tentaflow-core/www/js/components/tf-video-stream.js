// =============================================================================
// Plik: tf-video-stream.js
// Opis: Komponent <tf-video-stream stream-id="camera:cam_xxx" [label] [height-px]>
//       — kafelek live video oparty na MediaSource Extensions. Subskrybuje
//       strumien w StreamHub przez binary WS (Chunk B protocol), karmi fMP4
//       chunki do SourceBuffer dolaczonego do <video>. Cleanup w
//       disconnectedCallback wysyla StreamCloseRequest i zwalnia blob URL.
// Przyklad: <tf-video-stream stream-id="camera:cam_550e8400-e29b-41d4-a716-446655440000"
//             label="Wejscie · online" height-px="320"></tf-video-stream>
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';

// Domyslna wysokosc tile'a w px gdy atrybut nie ustawiony.
const DEFAULT_HEIGHT_PX = 320;

// Backoff kolejnych prob resubscribe po zerwaniu strumienia (lag / pad
// transportu / zamkniecie zrodla). Pierwsza proba szybko (UI nie miga bez
// powodu), kolejne coraz rzadziej; ostatnia wartosc powtarzana az do sukcesu.
const RESUBSCRIBE_BACKOFF_MS = [1000, 2000, 5000];

class TfVideoStream extends HTMLElement {
  static get observedAttributes() {
    return ['stream-id', 'label', 'height-px'];
  }

  constructor() {
    super();
    this._shadow = this.attachShadow({ mode: 'open' });
    this._video = null;
    this._labelEl = null;
    this._statusEl = null;
    this._mediaSource = null;
    this._sourceBuffer = null;
    this._blobUrl = null;
    this._appendQueue = [];
    this._appending = false;
    this._subscriptionUnsub = null;
    this._activeStreamId = null;
    this._resubscribeTimer = null;
    // Licznik prob resubscribe (indeks do RESUBSCRIBE_BACKOFF_MS) — zerowany
    // po udanym SubscribeResponse. Unsub listenera lifecycle WS ('open') —
    // pozwala wznowic subskrypcje natychmiast po powrocie transportu zamiast
    // czekac na kolejny tick backoffu.
    this._resubscribeAttempt = 0;
    this._lifecycleUnsub = null;
    this._disposed = false;
    // CorrelationId aktywnej subskrypcji — potrzebny zeby wyslac
    // StreamCloseRequest na tym samym id co oryginalny SubscribeRequest.
    this._activeCorrelationId = null;
    // Baza media-timeline (ns) z StreamSubscribeResponse — offset, ktory nakladka
    // detekcji odejmuje od `pts_ns` ramek, by kotwiczyc overlay na klatce wideo.
    // null gdy strumien nie ma wspolnej osi z detekcjami (LiDAR/audio/relay).
    this._basePtsNs = null;
  }

  // Udostepnia baze media-time (ns) dla nakladki detekcji. Nakladka czyta ten
  // getter przez `mediaBasePtsProvider` co klatke — sledzi zmiane przy resubscribe.
  get mediaBasePtsNs() {
    return this._basePtsNs;
  }

  connectedCallback() {
    this._disposed = false;
    if (!this._video) this._build();
    // Po realnym detach (_stopSubscription) listener dblclick zostaje zdjety —
    // przy ponownym podlaczeniu (bez _build) wpinamy go z powrotem.
    if (!this._onDblClick) {
      this._onDblClick = () => this._toggleFullscreen();
      this.addEventListener('dblclick', this._onDblClick);
    }
    this._applyAttributes();
    // The SDK reconciler can rip this element out of the DOM and re-insert it in
    // the same tick (disconnect→connect churn). If a deferred stop is pending
    // from such a disconnect, cancel it and KEEP the live subscription — do not
    // restart it (restarting would tear down + rebuild the backend mux branch,
    // which is exactly what left the tile stuck on "connecting").
    if (this._stopTimer) {
      clearTimeout(this._stopTimer);
      this._stopTimer = null;
      return;
    }
    this._startSubscription();
  }

  disconnectedCallback() {
    this._disposed = true;
    // Defer the unsubscribe: a reconcile-driven disconnect is usually followed by
    // an immediate reconnect (same element) or a replacement tile subscribing to
    // the same stream. Holding the subscription open for a short grace period
    // keeps the backend mux branch attached across the churn, so frames keep
    // flowing instead of the branch detaching and re-attaching empty.
    if (this._stopTimer) return;
    this._stopTimer = setTimeout(() => {
      this._stopTimer = null;
      this._stopSubscription('detached (deferred)');
    }, 1000);
  }

  attributeChangedCallback(name, oldValue, newValue) {
    if (!this._video) return;
    if (oldValue === newValue) return;
    if (name === 'stream-id') {
      // Restart pelnego pipeline'u: nowe MediaSource + nowa subskrypcja.
      this._stopSubscription('stream-id changed');
      this._applyAttributes();
      if (this.isConnected && !this._disposed) {
        this._startSubscription();
      }
      return;
    }
    this._applyAttributes();
  }

  _build() {
    const style = document.createElement('style');
    style.textContent = `
      :host { display: block; position: relative; height: var(--tf-video-stream-height, ${DEFAULT_HEIGHT_PX}px); background: #0a0a14; border-radius: 8px; overflow: hidden; }
      video { width: 100%; height: 100%; object-fit: cover; background: #000; display: block; }
      .label { position: absolute; top: 8px; left: 8px; background: rgba(0,0,0,0.7); color: #fff; padding: 4px 10px; border-radius: 4px; font: 12px var(--font-mono, monospace); pointer-events: none; }
      .label[hidden] { display: none; }
      .status { position: absolute; inset: 0; display: flex; align-items: center; justify-content: center; color: var(--text-muted, #999); font-style: italic; pointer-events: none; padding: 0 12px; text-align: center; }
      .status[hidden] { display: none; }
    `;
    this._video = document.createElement('video');
    this._video.autoplay = true;
    this._video.muted = true;
    this._video.playsInline = true;
    this._video.setAttribute('playsinline', '');

    // Podwojny klik -> fullscreen tej kamery. Fullscreen bierzemy na kontenerze
    // kafelka (rodzic hosta), zeby nakladka detekcji <canvas> — dolaczana jako
    // rodzenstwo w tym samym kontenerze — pozostala widoczna na pelnym ekranie.
    this._onDblClick = () => this._toggleFullscreen();
    this.addEventListener('dblclick', this._onDblClick);

    this._labelEl = document.createElement('div');
    this._labelEl.className = 'label';
    this._labelEl.hidden = true;

    this._statusEl = document.createElement('div');
    this._statusEl.className = 'status';
    this._statusEl.textContent = 'Łączenie ze strumieniem…';

    this._shadow.append(style, this._video, this._labelEl, this._statusEl);
  }

  _applyAttributes() {
    const label = this.getAttribute('label') ?? '';
    if (label.length > 0) {
      this._labelEl.textContent = label;
      this._labelEl.hidden = false;
    } else {
      this._labelEl.textContent = '';
      this._labelEl.hidden = true;
    }
    const heightRaw = Number(this.getAttribute('height-px'));
    const height =
      Number.isFinite(heightRaw) && heightRaw > 0 ? Math.floor(heightRaw) : DEFAULT_HEIGHT_PX;
    this.style.setProperty('--tf-video-stream-height', `${height}px`);
  }

  // Przelacza fullscreen dla kafelka kamery. Wchodzimy na kontenerze kafelka
  // (rodzic hosta) — obejmuje on <video> i nakladke detekcji. Ponowny dblclick
  // (lub Esc obslugiwany natywnie przez przegladarke) wychodzi z fullscreena.
  _toggleFullscreen() {
    const container = this.parentElement || this;
    const active = document.fullscreenElement;
    if (active && (active === container || active === this || active.contains(this))) {
      if (document.exitFullscreen) document.exitFullscreen().catch(() => {});
      return;
    }
    if (typeof container.requestFullscreen === 'function') {
      container.requestFullscreen().catch((e) => {
        console.warn('[tf-video-stream] fullscreen failed:', e?.message ?? e);
      });
    }
  }

  _setStatus(text) {
    if (!this._statusEl) return;
    if (typeof text === 'string' && text.length > 0) {
      this._statusEl.textContent = text;
      this._statusEl.hidden = false;
    } else {
      this._statusEl.hidden = true;
    }
  }

  _startSubscription() {
    const streamId = this.getAttribute('stream-id') ?? '';
    if (!streamId) {
      this._setStatus('Brak identyfikatora strumienia.');
      return;
    }
    if (typeof window.MediaSource === 'undefined') {
      this._setStatus('Przeglądarka nie wspiera MediaSource Extensions.');
      return;
    }
    this._activeStreamId = streamId;
    this._basePtsNs = null;
    this._setStatus('Łączenie ze strumieniem…');

    const mediaSource = new MediaSource();
    this._mediaSource = mediaSource;
    this._blobUrl = URL.createObjectURL(mediaSource);
    this._video.src = this._blobUrl;
    // Wlasciwy SourceBuffer tworzymy dopiero po `sourceopen` + przyjsciu
    // SubscribeResponse z `mime_type` — MediaSource nie pozwala dodac bufora
    // przed otwarciem, a serwer zna MIME dopiero po hub.subscribe().
    mediaSource.addEventListener(
      'sourceopen',
      () => {
        this._mediaSourceReady = true;
        this._tryCreateSourceBuffer();
      },
      { once: true },
    );

    this._openSubscription(streamId);
  }

  _openSubscription(streamId) {
    // Flaga sygnalizujaca disposal w trakcie ApiBinary.subscribe — bez tego
    // pozno-rozwiazany unsub() wycieklby gdyby user zamknal panel zanim WS
    // sie podlaczyl. Trzymamy lokalnie zeby unikac wyscigow miedzy startami.
    const pending = { disposed: false };
    this._subscriptionUnsub = () => {
      pending.disposed = true;
    };
    ApiBinary.subscribe(
      'streamSubscribeRequest',
      { streamId },
      {
        onChunk: (body) => this._onSubscriptionChunk(body),
        onEnd: (body) => this._onSubscriptionEnd(body),
        onError: (err) => this._onSubscriptionError(err),
      },
    )
      .then((unsub) => {
        if (pending.disposed || this._disposed) {
          try {
            unsub();
          } catch (e) {
            /* ignore */
          }
          return;
        }
        this._subscriptionUnsub = () => {
          try {
            unsub();
          } catch (e) {
            console.warn('[tf-video-stream] unsub threw:', e);
          }
        };
      })
      .catch((err) => {
        console.warn('[tf-video-stream] subscribe failed:', err?.message ?? err);
        this._setStatus('Nie udało się otworzyć strumienia.');
        // Subskrypcja nie doszla do skutku (np. WS w trakcie reconnectu) —
        // sprzatnij pipeline i probuj ponownie z backoffem, az sie uda.
        this._resetMediaPipeline();
        this._scheduleResubscribe();
      });
  }

  _onSubscriptionChunk(body) {
    if (this._disposed) return;
    if (!body || typeof body !== 'object') return;
    if (body.variant === 'StreamSubscribeResponse') {
      this._resubscribeAttempt = 0;
      this._mime = String(body.mime_type ?? body.mimeType ?? '');
      const base = body.base_pts_ns ?? body.basePtsNs;
      this._basePtsNs = base == null ? null : (typeof base === 'bigint' ? Number(base) : Number(base));
      if (!Number.isFinite(this._basePtsNs)) this._basePtsNs = null;
      this._tryCreateSourceBuffer();
      return;
    }
    if (body.variant === 'StreamFrame') {
      const data = body.data;
      if (!(data instanceof Uint8Array) || data.byteLength === 0) return;
      // Kopia bytow — wasm pamiec moze byc reuse'owana po powrocie do
      // dispatch loopa, a appendBuffer jest async.
      const buf = data.slice();
      this._enqueueAppend(buf);
      this._setStatus('');
      return;
    }
  }

  _onSubscriptionEnd(body) {
    if (this._disposed) return;
    const reason = String(body?.reason ?? '');
    // Zerwania wymagajace pelnego restartu pipeline'u + resubscribe:
    //   subscriber_lagged   — serwer ubil subskrypcje (klient nie nadazal),
    //   transport_closed    — syntetyczny koniec po padzie WS (po reconnect
    //                          serwer nie zna starych subskrypcji per-socket),
    //   source_unregistered — zrodlo zniklo (np. restart kamery); po powrocie
    //                          kafelek ma sam wrocic do zycia.
    if (
      reason === 'subscriber_lagged' ||
      reason === 'transport_closed' ||
      reason === 'source_unregistered'
    ) {
      console.warn(`[tf-video-stream] strumien zerwany (${reason}), resubscribe`);
      this._resetMediaPipeline();
      this._scheduleResubscribe();
      return;
    }
    if (reason === 'client_request' || reason === '') {
      // Spowodowane wlasnym close() — bez komunikatu.
      return;
    }
    this._setStatus(`Strumień zakończony: ${reason}`);
  }

  _onSubscriptionError(err) {
    if (this._disposed) return;
    const message = err?.message ?? String(err ?? '');
    console.warn('[tf-video-stream] protocol error:', message);
    this._setStatus('Błąd strumienia.');
  }

  _tryCreateSourceBuffer() {
    if (this._sourceBuffer) return;
    if (!this._mediaSource || this._mediaSource.readyState !== 'open') return;
    if (!this._mime) return;
    if (!('isTypeSupported' in MediaSource) || !MediaSource.isTypeSupported(this._mime)) {
      this._setStatus(`Format nie wspierany: ${this._mime}`);
      return;
    }
    let sourceBuffer;
    try {
      sourceBuffer = this._mediaSource.addSourceBuffer(this._mime);
    } catch (e) {
      console.error('[tf-video-stream] addSourceBuffer failed:', e);
      this._setStatus('Nie udało się utworzyć bufora video.');
      return;
    }
    sourceBuffer.mode = 'segments';
    sourceBuffer.addEventListener('updateend', () => {
      this._appending = false;
      this._maybeTrimBuffer();
      this._syncPlayhead();
      this._drainAppendQueue();
    });
    sourceBuffer.addEventListener('error', (e) => {
      console.warn('[tf-video-stream] sourceBuffer error:', e);
    });
    this._sourceBuffer = sourceBuffer;
    this._drainAppendQueue();
  }

  _enqueueAppend(bytes) {
    this._appendQueue.push(bytes);
    this._drainAppendQueue();
  }

  _drainAppendQueue() {
    if (this._appending) return;
    if (!this._sourceBuffer) return;
    // SourceBuffer moze zostac usuniety z MediaSource gdy MS przeszedl
    // do stanu 'closed'/'ended' po wczesniejszym appendBuffer error.
    // Probowanie operacji na takim sourceBuffer rzuca InvalidStateError
    // ("SourceBuffer has been removed from the parent media source").
    if (!this._mediaSource || this._mediaSource.readyState !== 'open') return;
    if (this._sourceBuffer.updating) return;
    if (this._appendQueue.length === 0) return;
    const bytes = this._appendQueue.shift();
    this._appending = true;
    try {
      this._sourceBuffer.appendBuffer(bytes);
    } catch (e) {
      this._appending = false;
      console.warn('[tf-video-stream] appendBuffer failed:', e?.name, e?.message);
      // QuotaExceeded — agresywnie obetnij bufor i sproboj jeszcze raz.
      if (e?.name === 'QuotaExceededError') {
        this._forceTrimBuffer();
        // Wrzuc bajt z powrotem na poczatek kolejki — sproboj po updateend
        // wywolanym przez remove().
        this._appendQueue.unshift(bytes);
        return;
      }
      // Inne bledy (np. zly init segment) — restart pipeline'u.
      this._resetMediaPipeline();
      this._scheduleResubscribe();
    }
  }

  /// Keeps the playhead on the live edge. A live RTSP camera's fMP4 carries the
  /// camera's own PTS timeline (often starting hundreds of seconds in), so the
  /// element's initial currentTime=0 lands in an unbuffered gap and playback
  /// never starts (stays paused at 0). Seek into the buffered range whenever the
  /// playhead is outside it or has drifted too far behind, then resume playback.
  _syncPlayhead() {
    const v = this._video;
    if (!v) return;
    let ranges;
    try {
      ranges = this._sourceBuffer && this._sourceBuffer.buffered;
    } catch (e) {
      return;
    }
    if (!ranges || ranges.length === 0) return;
    const start = ranges.start(0);
    const end = ranges.end(ranges.length - 1);
    // Hold a cushion behind the live edge instead of riding it. Seeking to
    // `end - tiny` left the playhead with ~0.07s buffered ahead, so any network
    // hiccup underran the decoder and stalled. A ~1s cushion keeps a few
    // fragments queued ahead of the playhead; the camera's wall-clock latency
    // stays low while the player no longer starves on jitter.
    // Jitter buffer ahead of the playhead — NOT network latency (a LAN robot is
    // ~3-4ms). It only needs to cover a couple of fMP4 fragments so the decoder
    // never starves between fragments. With a clean continuous server timeline
    // (h264timestamper + param-only AUs dropped) a small cushion suffices.
    const TARGET_LATENCY_SECS = 0.1;
    // Twarda granica dryfu — powyzej NIE czekamy, tylko przeskakujemy na
    // live-edge (bufor odjechal za daleko po dluzszym stallu / karcie w tle).
    const HARD_SNAP_SECS = 0.5;
    // Histereza lekkiego przyspieszenia (catch-up bez skokow): wlaczamy
    // playbackRate>1 gdy playhead za daleko za live-edge, wracamy do 1.0 po
    // dogonieniu. Zakres < HARD_SNAP zeby najpierw probowac plynnie.
    const CATCHUP_ON_SECS = 0.25;
    const CATCHUP_OFF_SECS = 0.15;
    const CATCHUP_RATE = 1.05;
    // Only build the initial cushion before first play: wait until enough is
    // buffered so playback starts with room ahead rather than at the edge.
    if (v.paused && end - start < TARGET_LATENCY_SECS && v.currentTime <= start) {
      return;
    }
    const behind = end - v.currentTime;
    if (v.currentTime < start || behind > HARD_SNAP_SECS) {
      // Przeskok na live-edge — bez czekania na plynne nadgonienie.
      try {
        v.currentTime = Math.max(start, end - TARGET_LATENCY_SECS);
      } catch (e) {
        // Not seekable yet — retry on the next updateend.
      }
      if (v.playbackRate !== 1.0) v.playbackRate = 1.0;
    } else if (behind > CATCHUP_ON_SECS) {
      // Lekkie przyspieszenie, zeby plynnie dogonic live-edge bez skoku.
      if (v.playbackRate !== CATCHUP_RATE) v.playbackRate = CATCHUP_RATE;
    } else if (behind <= CATCHUP_OFF_SECS && v.playbackRate !== 1.0) {
      // Dogonione — powrot do normalnej predkosci.
      v.playbackRate = 1.0;
    }
    if (v.paused) {
      const pr = v.play();
      if (pr && typeof pr.catch === 'function') pr.catch(() => {});
    }
  }

  _maybeTrimBuffer() {
    if (!this._sourceBuffer || this._sourceBuffer.updating) return;
    // Sprawdz czy MediaSource jest dalej otwarte — czytanie `.buffered` z
    // sourceBuffer usunietego z MS rzuca InvalidStateError.
    if (!this._mediaSource || this._mediaSource.readyState !== 'open') return;
    let ranges;
    try {
      ranges = this._sourceBuffer.buffered;
    } catch (e) {
      // SourceBuffer usuniety z parent MS — restart pipeline'u.
      return;
    }
    if (ranges.length === 0) return;
    const start = ranges.start(0);
    // Trzymamy tylko ~2 s historii przed playheadem — bufor nie rosnie, a
    // pamiec MSE zostaje mala na dlugo zyjacych kafelkach. Przycinamy regularnie
    // (na kazdym updateend), a nie dopiero przy 30 s okna.
    const v = this._video;
    const ct = v ? v.currentTime : 0;
    const removeUntil = ct - 2;
    if (!(removeUntil > start + 0.1)) return;
    try {
      this._sourceBuffer.remove(start, removeUntil);
    } catch (e) {
      console.warn('[tf-video-stream] remove(buffered) failed:', e?.message);
    }
  }

  _forceTrimBuffer() {
    if (!this._sourceBuffer || this._sourceBuffer.updating) return;
    const ranges = this._sourceBuffer.buffered;
    if (ranges.length === 0) return;
    const start = ranges.start(0);
    const end = ranges.end(ranges.length - 1);
    // Zostaw tylko ostatnie 5 sekund — agresywne odzyskanie pamieci.
    const removeUntil = Math.max(start, end - 5);
    if (removeUntil <= start) return;
    try {
      this._sourceBuffer.remove(start, removeUntil);
    } catch (e) {
      console.warn('[tf-video-stream] force trim failed:', e?.message);
    }
  }

  _resetMediaPipeline() {
    this._appendQueue.length = 0;
    this._appending = false;
    this._sourceBuffer = null;
    this._mime = null;
    this._basePtsNs = null;
    this._mediaSourceReady = false;
    if (this._mediaSource) {
      try {
        if (this._mediaSource.readyState === 'open') this._mediaSource.endOfStream();
      } catch (e) {
        /* ignore */
      }
      this._mediaSource = null;
    }
    if (this._blobUrl) {
      URL.revokeObjectURL(this._blobUrl);
      this._blobUrl = null;
    }
    if (this._video) {
      this._video.removeAttribute('src');
      try {
        this._video.load();
      } catch (e) {
        /* ignore */
      }
    }
    if (this._subscriptionUnsub) {
      try {
        this._subscriptionUnsub();
      } catch (e) {
        /* ignore */
      }
      this._subscriptionUnsub = null;
    }
  }

  _scheduleResubscribe() {
    if (this._disposed) return;
    if (this._resubscribeTimer != null) return;
    this._setStatus('Łączenie ponownie…');
    const delay =
      RESUBSCRIBE_BACKOFF_MS[Math.min(this._resubscribeAttempt, RESUBSCRIBE_BACKOFF_MS.length - 1)];
    this._resubscribeAttempt += 1;
    this._resubscribeTimer = setTimeout(() => {
      this._resubscribeTimer = null;
      if (this._disposed || !this.isConnected) return;
      this._startSubscription();
    }, delay);
    // Gdy WS lezy, nie czekaj slepo na tick backoffu — jednorazowy listener
    // lifecycle 'open' wznawia subskrypcje od razu po powrocie transportu.
    if (!ApiBinary.isConnected() && !this._lifecycleUnsub) {
      this._lifecycleUnsub = ApiBinary.onLifecycle((ev) => {
        if (ev.type !== 'open') return;
        if (this._lifecycleUnsub) {
          this._lifecycleUnsub();
          this._lifecycleUnsub = null;
        }
        if (this._disposed || !this.isConnected) return;
        if (this._resubscribeTimer != null) {
          clearTimeout(this._resubscribeTimer);
          this._resubscribeTimer = null;
        }
        this._startSubscription();
      });
    }
  }

  _stopSubscription(_reason) {
    if (this._stopTimer) {
      clearTimeout(this._stopTimer);
      this._stopTimer = null;
    }
    if (this._resubscribeTimer != null) {
      clearTimeout(this._resubscribeTimer);
      this._resubscribeTimer = null;
    }
    if (this._lifecycleUnsub) {
      this._lifecycleUnsub();
      this._lifecycleUnsub = null;
    }
    this._resubscribeAttempt = 0;
    // ApiBinary.subscribe usuwa server-side state przez StreamEnd; bezposrednio
    // zamykajac listener prosto unsubscribe'ujemy. Serwer wykryje rozlaczony
    // socket lub klient moze opcjonalnie wyslac StreamCloseRequest — robimy
    // to gdy znamy stream-id, zeby serwer od razu zwolnil slot quoty.
    const streamId = this._activeStreamId;
    if (this._subscriptionUnsub) {
      try {
        this._subscriptionUnsub();
      } catch (e) {
        /* ignore */
      }
      this._subscriptionUnsub = null;
    }
    if (streamId) {
      // Best-effort cleanup po stronie serwera. Nieoczekiwane bledy ignorujemy
      // — server tak czy inaczej zwolni subscription po EOF socketu.
      ApiBinary.action('streamCloseRequest', { streamId }).catch(() => {
        /* ignore */
      });
    }
    this._resetMediaPipeline();
    this._activeStreamId = null;
    // Cleanup listenera fullscreena i wyjscie z fullscreena gdy kafelek znika.
    if (this._onDblClick) {
      this.removeEventListener('dblclick', this._onDblClick);
      this._onDblClick = null;
    }
    const active = document.fullscreenElement;
    if (active && (active === this || active.contains(this)) && document.exitFullscreen) {
      document.exitFullscreen().catch(() => {});
    }
  }
}

if (!customElements.get('tf-video-stream')) {
  customElements.define('tf-video-stream', TfVideoStream);
}

export { TfVideoStream };
