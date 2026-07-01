// =============================================================================
// Plik: vision-detections-overlay.js
// Opis: Nakladka <canvas> rysujaca detekcje NA ZYWO nad podgladem kamery
//       (tf-video-stream / <video>). Konsumuje BINARNY strumien detekcji przez
//       ApiBinary.subscribe('cameraDetectionsSubscribeRequest', { cameraId }) —
//       zero REST, zero raw WebSocket. Skaluje znormalizowane bbox 0..1 na
//       realny obszar wideo wewnatrz elementu (uwzglednia object-fit /
//       letterbox) i rysuje w pojedynczej petli requestAnimationFrame.
// Przyklad:
//   import { attachDetectionsOverlay } from '/js/modules/vision-detections-overlay.js';
//   const overlay = attachDetectionsOverlay({ video, cameraId: 'cam_...' });
//   // ... pozniej: overlay.destroy();
// =============================================================================
//
// PERFORMANCE (setki kamer @25fps, ale subskrybowane TYLKO widoczne kafelki):
//   * latest-wins: trzymamy maly bufor kilku NAJNOWSZYCH ramek po `tsMs`,
//     nigdy nie kolejkujemy w nieskonczonosc. Nowy chunk nadpisuje stan.
//   * pojedyncza petla rAF (NIE render-per-message): klatka rysowana jest co
//     najwyzej z czestotliwoscia odswiezania ekranu niezaleznie od 25fps na
//     drucie. Wiadomosci tylko aktualizuja bufor — rysuje rAF.
//   * synchronizacja PTS: mapujemy `video.currentTime` na czas sciany
//     (wall-clock ms) i rysujemy zestaw detekcji NAJBLIZSZY pokazywanej klatce
//     wideo, a nie temu co wlasnie przyszlo z sieci. Gdy mapowanie nie jest
//     dostepne (brak ruchu currentTime), rysujemy najnowsza ramke.
//   * teardown: na destroy()/zmianie strumienia wolamy unsubscribe, anulujemy
//     rAF, czyscimy bufory i ResizeObserver. Subskrybujemy tylko gdy dolaczeni.

import { ApiBinary } from '/js/protocol/api-binary-shim.js';

// Kolory ramek per klasa detekcji. Klasy nieznane dostaja kolor domyslny.
const KLASA_KOLORY = {
  tablica_adr: '#ff8c1a',
  tablica_rejestracyjna: '#3b82f6',
  nalepka_1: '#22c55e',
  nalepka_2: '#22c55e',
  nalepka_3: '#22c55e',
  nalepka_4: '#22c55e',
  nalepka_5: '#22c55e',
  nalepka_6: '#22c55e',
  nalepka_7: '#22c55e',
  nalepka_8: '#22c55e',
  nalepka_9: '#22c55e',
  znak_srodowiskowy: '#06b6d4',
  termometr: '#ef4444',
};
const KOLOR_DOMYSLNY = '#e5e7eb';

// Czerwony tint dla detekcji w stanie "uszkodzona" (nadpisuje kolor klasy).
const KOLOR_USZKODZONA = '#ef4444';

// Nalepki maja wspolny prefiks i wspolny kolor — dopasowanie po prefiksie
// gdy nie ma dokladnego klucza (np. nalepka_12).
function kolorDlaKlasy(klasa) {
  if (KLASA_KOLORY[klasa]) return KLASA_KOLORY[klasa];
  if (typeof klasa === 'string' && klasa.startsWith('nalepka')) return '#22c55e';
  return KOLOR_DOMYSLNY;
}

// Detekcja jest "uszkodzona" gdy jej `stan` zawiera ten token.
function jestUszkodzona(det) {
  return Array.isArray(det?.stan) && det.stan.some((s) => String(s) === 'uszkodzona');
}

// Po tylu ms bez nowej wiadomosci wygaszamy ostatnie ramki (czyscimy canvas),
// zeby nie wisialy nieaktualne detekcje gdy backend zamilknie.
const WYGASZ_PO_MS = 400;

// Ile NAJNOWSZYCH ramek trzymamy w buforze PTS-sync. Przy 25fps to ~0.6 s
// historii — wystarcza, by dopasowac detekcje do opoznionej klatki MSE bez
// rosnacej pamieci. Starsze ramki sa odrzucane (latest-wins).
const BUFOR_MAX_RAMEK = 16;

// Stale opoznienie (ms) miedzy PRZECHWYCENIEM klatki a jej pojawieniem sie na
// live-edge bufora MSE (enkod + mux + siec). Odejmowane od `Date.now()` przy
// estymacji czasu przechwycenia aktualnie wyswietlanej klatki. Wartosc TUNOWALNA
// — mozna nadpisac bez restartu przez localStorage['tv_sync_offset_ms'].
const SYNC_OFFSET_MS = 300;

// Zwraca aktualnie obowiazujacy offset synchronizacji: nadpisanie z localStorage
// (do tuningu na zywo) lub stala domyslna SYNC_OFFSET_MS.
function syncOffsetMs() {
  try {
    const raw = localStorage.getItem('tv_sync_offset_ms');
    if (raw != null) {
      const v = Number(raw);
      if (Number.isFinite(v)) return v;
    }
  } catch { /* ignore */ }
  return SYNC_OFFSET_MS;
}

// =============================================================================
// Publiczne API
// =============================================================================

// Tworzy nakladke nad podanym elementem <video> i zwraca uchwyt z destroy().
// `video`         — element <video> LUB host (tf-video-stream) — wtedy szukamy
//                   <video> w jego shadow DOM przez `videoResolver`.
// `cameraId`      — identyfikator kamery (`cam_<uuid v4>`) do subskrypcji binarnej.
// `videoResolver` — opcjonalna funkcja zwracajaca realny <video> (shadow DOM).
export function attachDetectionsOverlay(opts) {
  return new DetectionsOverlay(opts);
}

// =============================================================================
// Implementacja
// =============================================================================

class DetectionsOverlay {
  constructor({ video, cameraId, videoResolver = null }) {
    if (!video) throw new Error('attachDetectionsOverlay: brak elementu video');
    if (!cameraId) throw new Error('attachDetectionsOverlay: brak cameraId');

    this.hostEl = video;
    this.cameraId = String(cameraId);
    // tf-video-stream trzyma <video> w shadow DOM — resolver pozwala
    // znalezc realny element do pomiaru object-fit i wymiarow klatki.
    this.videoResolver = videoResolver;

    this.canvas = document.createElement('canvas');
    this.canvas.className = 'vision-detections-overlay';
    this.canvas.style.position = 'absolute';
    this.canvas.style.left = '0';
    this.canvas.style.top = '0';
    this.canvas.style.zIndex = '20';
    this.canvas.style.pointerEvents = 'none';
    this.ctx = this.canvas.getContext('2d');

    // Bufor NAJNOWSZYCH ramek posortowany rosnaco po tsMs (latest-wins).
    // Kazdy element: { tsMs, items }. Przy braku PTS-sync rysujemy ostatni.
    this.frames = [];
    this.lastMessageAt = 0;
    this.disposed = false;
    this.unsub = null;
    // Sygnalizuje disposal w trakcie oczekiwania na ApiBinary.subscribe(),
    // zeby pozno-rozwiazany unsubscribe nie wyciekl gdy panel zniknie.
    this.pending = { disposed: false };
    this.rafId = null;

    // Uchwyt do pomiaru z zewnatrz (np. Playwright): pozwala odczytac `frames`,
    // videoContentRect() i wynik selectFrame() bez rebuildu.
    this.hostEl.__detOverlay = this;

    this.boundResize = () => this.resizeCanvas();
    this.resizeObserver = typeof ResizeObserver !== 'undefined'
      ? new ResizeObserver(this.boundResize)
      : null;

    this.mountCanvas();
    this.resizeCanvas();
    this.connect();
    this.startRenderLoop();
  }

  // Wstawia canvas do kontenera tak, by lezal dokladnie nad elementem video.
  // Wymaga, by rodzic byl positioned — gdy nie jest, ustawiamy `relative`
  // (tf-video-stream :host juz jest `position: relative`).
  mountCanvas() {
    const parent = this.hostEl.parentElement || this.hostEl;
    const cs = getComputedStyle(parent);
    if (cs.position === 'static') parent.style.position = 'relative';
    parent.appendChild(this.canvas);

    if (this.resizeObserver) {
      this.resizeObserver.observe(this.hostEl);
      this.resizeObserver.observe(parent);
    } else {
      window.addEventListener('resize', this.boundResize);
    }
  }

  // Zwraca realny element <video> (rozwiazany takze z shadow DOM tf-video-stream).
  videoEl() {
    if (this.videoResolver) {
      const v = this.videoResolver();
      if (v) return v;
    }
    if (this.hostEl.tagName === 'VIDEO') return this.hostEl;
    if (this.hostEl.shadowRoot) {
      const v = this.hostEl.shadowRoot.querySelector('video');
      if (v) return v;
    }
    return this.hostEl.querySelector?.('video') || null;
  }

  // Dopasowuje rozmiar canvasa (CSS px) do boxu hosta oraz bufor pikseli do DPR.
  resizeCanvas() {
    const rect = this.hostEl.getBoundingClientRect();
    const parent = this.canvas.parentElement;
    // Pozycja canvasa wzgledem positioned-rodzica — host moze byc wsuniety
    // (padding/border rodzica), wiec liczymy offset, nie zakladamy (0,0).
    if (parent) {
      const prect = parent.getBoundingClientRect();
      this.canvas.style.left = `${rect.left - prect.left}px`;
      this.canvas.style.top = `${rect.top - prect.top}px`;
    }
    const dpr = window.devicePixelRatio || 1;
    const w = Math.max(1, Math.round(rect.width));
    const h = Math.max(1, Math.round(rect.height));
    this.canvas.style.width = `${w}px`;
    this.canvas.style.height = `${h}px`;
    this.canvas.width = Math.max(1, Math.round(w * dpr));
    this.canvas.height = Math.max(1, Math.round(h * dpr));
  }

  // Liczy realny prostokat klatki wideo wewnatrz elementu z uwzglednieniem
  // object-fit. Zwraca { x, y, width, height } w pikselach BUFORA canvasa
  // (czyli juz pomnozone przez DPR). Dla `cover` klatka wypelnia caly box i
  // jest przycinana; dla `contain` powstaje letterbox (pasy). Bez znanych
  // wymiarow klatki zwracamy caly box.
  videoContentRect() {
    const dpr = window.devicePixelRatio || 1;
    const boxW = this.canvas.width;
    const boxH = this.canvas.height;
    const v = this.videoEl();
    const vw = v?.videoWidth || 0;
    const vh = v?.videoHeight || 0;
    if (!vw || !vh) return { x: 0, y: 0, width: boxW, height: boxH };

    const fit = v ? (getComputedStyle(v).objectFit || 'contain') : 'contain';
    const videoRatio = vw / vh;
    const boxRatio = boxW / boxH;

    if (fit === 'fill') {
      return { x: 0, y: 0, width: boxW, height: boxH };
    }
    if (fit === 'none') {
      // Klatka w rozmiarze natywnym (px CSS * DPR), wycentrowana.
      const w = vw * dpr;
      const h = vh * dpr;
      return { x: (boxW - w) / 2, y: (boxH - h) / 2, width: w, height: h };
    }

    // `cover`: skalujemy tak, by wypelnic box (krotszy bok dociety).
    // `contain`: skalujemy tak, by zmiescic caly obraz (letterbox).
    const coverMode = fit === 'cover';
    const fillByWidth = coverMode ? boxRatio > videoRatio : boxRatio < videoRatio;
    if (fillByWidth) {
      const width = boxW;
      const height = width / videoRatio;
      return { x: 0, y: (boxH - height) / 2, width, height };
    }
    const height = boxH;
    const width = height * videoRatio;
    return { x: (boxW - width) / 2, y: 0, width, height };
  }

  // =============================================================================
  // Binarna subskrypcja detekcji
  // =============================================================================

  connect() {
    if (this.disposed) return;
    // Lokalny disposed-guard rozwiazywany asynchronicznie przez ApiBinary.subscribe.
    ApiBinary.subscribe(
      'cameraDetectionsSubscribeRequest',
      { cameraId: this.cameraId },
      {
        onChunk: (body) => this.onChunk(body),
        onEnd: (body) => this.onEnd(body),
        onError: (err) => this.onError(err),
      },
    )
      .then((unsub) => {
        if (this.pending.disposed || this.disposed) {
          try { unsub(); } catch { /* ignore */ }
          return;
        }
        this.unsub = unsub;
      })
      .catch((err) => {
        console.warn('[vision-detections-overlay] subscribe failed:', err?.message ?? err);
      });
  }

  onChunk(body) {
    if (this.disposed) return;
    if (!body || body.variant !== 'CameraDetectionsFrame') return;
    // Wiadomosci dla innej kamery ignorujemy (jeden strumien = jedna kamera,
    // ale strzezemy sie przed niespojnoscia po stronie backendu).
    const cam = body.cameraId ?? body.camera_id;
    if (cam != null && String(cam) !== this.cameraId) return;

    const tsMs = Number(body.tsMs ?? body.ts_ms ?? 0);
    const items = Array.isArray(body.items) ? body.items : [];
    this.pushFrame(tsMs, items);
    this.lastMessageAt = performance.now();
  }

  // Wstawia ramke do bufora utrzymujac porzadek rosnacy po tsMs i limit
  // BUFOR_MAX_RAMEK (latest-wins — najstarsze odrzucamy). Najczestsza sciezka
  // (ramki przychodza chronologicznie) to push na koniec + ewentualny shift.
  //
  // FAZA 2 (wzbogacenie po stronie backendu) publikuje ten sam tsMs co FAZA 1
  // (surowe boxy). Zamiast duplikowac ramke AKTUALIZUJEMY istniejaca po tsMs, by
  // overlay podmienil surowe boxy na wzbogacone etykiety (stan/OCR) dla tej samej
  // klatki — inaczej selectFrame trzymalby sie wersji surowej (FAZA 1).
  pushFrame(tsMs, items) {
    const buf = this.frames;
    for (let i = buf.length - 1; i >= 0; i--) {
      if (buf[i].tsMs === tsMs) { buf[i].items = items; return; }
      // Bufor posortowany rosnaco — gdy zejdziemy ponizej tsMs, dopasowania nie ma.
      if (buf[i].tsMs < tsMs) break;
    }
    const frame = { tsMs, items };
    if (buf.length === 0 || tsMs >= buf[buf.length - 1].tsMs) {
      buf.push(frame);
    } else {
      // Ramka spoza kolejnosci (rzadkie) — wstaw na wlasciwa pozycje.
      let i = buf.length - 1;
      while (i >= 0 && buf[i].tsMs > tsMs) i--;
      buf.splice(i + 1, 0, frame);
    }
    while (buf.length > BUFOR_MAX_RAMEK) buf.shift();
  }

  onEnd(body) {
    if (this.disposed) return;
    const reason = String(body?.reason ?? '');
    if (reason && reason !== 'client_request') {
      console.warn('[vision-detections-overlay] stream ended:', reason);
    }
    // Strumien zakonczony przez serwer — przestajemy rysowac stare ramki.
    this.frames.length = 0;
  }

  onError(err) {
    if (this.disposed) return;
    console.warn('[vision-detections-overlay] protocol error:', err?.message ?? err);
  }

  // =============================================================================
  // Synchronizacja PTS — wybor ramki dla AKTUALNIE WYSWIETLANEJ klatki wideo
  // =============================================================================

  // Estymuje realny czas sciany (ms unix) PRZECHWYCENIA klatki, ktora <video>
  // pokazuje w tej chwili. Playhead siedzi `videoLatencyMs` za live-edge bufora
  // MSE, a live-edge sam jest opozniony wzgledem przechwycenia o staly
  // SYNC_OFFSET_MS (enkod/mux/siec). Zwraca null gdy brak playbacku/bufora.
  targetCaptureWallMs() {
    const v = this.videoEl();
    if (!v) return null;
    const ct = v.currentTime;
    if (!Number.isFinite(ct) || ct <= 0 || (v.readyState || 0) === 0) return null;

    const buffered = v.buffered;
    if (!buffered || buffered.length === 0) return null;
    const bufEnd = buffered.end(buffered.length - 1);
    if (!Number.isFinite(bufEnd)) return null;

    // Jak daleko playhead jest za live-edge (opoznienie odtwarzania MSE).
    const videoLatencyMs = (bufEnd - ct) * 1000;
    return Date.now() - videoLatencyMs - syncOffsetMs();
  }

  // Wybiera zestaw detekcji najblizszy czasowo do klatki pokazywanej przez
  // <video>. Estymujemy per-klatka realny czas przechwycenia wyswietlanej klatki
  // (targetCaptureWallMs) i wybieramy detekcje o tsMs najblizszym temu czasowi.
  // Gdy brak playbacku/bufora — degradujemy HONESTNIE do najnowszej ramki.
  selectFrame() {
    const buf = this.frames;
    if (buf.length === 0) return null;
    const latest = buf[buf.length - 1];

    const targetWallMs = this.targetCaptureWallMs();
    if (targetWallMs == null) return latest;

    // Wybierz ramke o tsMs najblizszym targetWallMs.
    let best = buf[0];
    let bestDiff = Math.abs(best.tsMs - targetWallMs);
    for (let i = 1; i < buf.length; i++) {
      const d = Math.abs(buf[i].tsMs - targetWallMs);
      if (d < bestDiff) { best = buf[i]; bestDiff = d; }
    }
    return best;
  }

  // =============================================================================
  // Rysowanie (pojedyncza petla rAF)
  // =============================================================================

  startRenderLoop() {
    const tick = () => {
      if (this.disposed) return;
      this.draw();
      this.rafId = requestAnimationFrame(tick);
    };
    this.rafId = requestAnimationFrame(tick);
  }

  draw() {
    const ctx = this.ctx;
    ctx.clearRect(0, 0, this.canvas.width, this.canvas.height);

    // Wygaszanie: po WYGASZ_PO_MS bez nowej wiadomosci nie rysujemy nic.
    if (this.frames.length === 0) return;
    if (this.lastMessageAt && performance.now() - this.lastMessageAt > WYGASZ_PO_MS) {
      return;
    }

    const frame = this.selectFrame();

    // Debug HUD (do tuningu SYNC_OFFSET_MS) — rysowany zawsze gdy wlaczony,
    // niezaleznie od tego czy wybrana ramka ma detekcje.
    this.drawDebugHud(frame);

    if (!frame || !frame.items.length) return;

    const area = this.videoContentRect();
    const dpr = window.devicePixelRatio || 1;
    // Ramka: grubsza dla czytelnosci na obrazie (skalowana z DPR).
    const lineW = Math.max(2, Math.round(2.5 * dpr));
    // Etykieta: pogrubiony font sans ~15px (min 13px), NIE mono — czytelniejszy
    // na jasnych kolorach pigulek. Canvas nie rozwija var(--font-*), wiec podajemy
    // konkretny stack systemowy.
    const fontPx = Math.max(Math.round(13 * dpr), Math.round(15 * dpr));
    ctx.font = `700 ${fontPx}px system-ui, -apple-system, "Segoe UI", Roboto, sans-serif`;
    ctx.textBaseline = 'bottom';

    const padX = Math.round(7 * dpr);
    const padY = Math.round(4 * dpr);
    const gap = Math.round(2 * dpr);
    // Minimalny odstep miedzy pigulkami przy rozsuwaniu (declutter).
    const odstep = Math.round(3 * dpr);

    // ETAP 1: rysuj same ramki boxow i zbierz preferowane prostokaty etykiet.
    // Etykiety NIE sa jeszcze rysowane — najpierw rozsuwamy je (ETAP 2), by nie
    // nachodzily na siebie na gestej scenie.
    const etykiety = [];
    for (const det of frame.items) {
      const bbox = det?.bbox;
      if (!Array.isArray(bbox) || bbox.length < 4) continue;
      const [nx, ny, nw, nh] = bbox;
      // Znormalizowane 0..1 -> piksele realnego obszaru wideo (area).
      const x = area.x + nx * area.width;
      const y = area.y + ny * area.height;
      const w = nw * area.width;
      const h = nh * area.height;

      const kolor = jestUszkodzona(det) ? KOLOR_USZKODZONA : kolorDlaKlasy(det.klasa);
      ctx.lineWidth = lineW;
      ctx.strokeStyle = kolor;
      ctx.strokeRect(x, y, w, h);

      const text = budujEtykiete(det);
      if (!text) continue;

      const textW = ctx.measureText(text).width;
      const labelW = textW + padX * 2;
      const labelH = fontPx + padY * 2;
      // Preferowana pozycja: nad lewym-gornym rogiem boxa (jak dotychczas),
      // dociagnieta do krawedzi canvasu.
      let prefY = y - labelH - gap;
      if (prefY < 0) prefY = y; // brak miejsca u gory — wewnatrz boxa
      let prefX = x;
      if (prefX + labelW > this.canvas.width) prefX = this.canvas.width - labelW;
      if (prefX < 0) prefX = 0;

      const score = Number.isFinite(det?.score) ? det.score : 0;
      etykiety.push({
        text,
        kolor,
        labelW,
        labelH,
        prefX,
        prefY,
        // Aktualna (rozsunieta) pozycja — nadpisywana w ulozEtykiety().
        x: prefX,
        y: prefY,
        // Gorna krawedz boxa (punkt zaczepienia linii-lacznika).
        boxTop: y,
        boxCenterX: x + w / 2,
        // Priorytet ukladania: najpierw score, box area jako rozstrzygniecie.
        priorytet: score * 1e7 + w * h,
        moved: false,
      });
    }

    // ETAP 2: greedy declutter — rozsun kolidujace pigulki.
    this.ulozEtykiety(etykiety, odstep);

    // ETAP 3a: linie-laczniki (pod pigulkami) dla etykiet odsunietych od boxa.
    for (const e of etykiety) {
      if (e.moved) this.drawLeader(ctx, e, dpr);
    }
    // ETAP 3b: pigulki na wierzchu (zakrywaja punkty zaczepienia lacznikow).
    for (const e of etykiety) {
      this.drawLabel(ctx, e, padX, padY, dpr);
    }
  }

  // Rysuje diagnostyczny HUD w rogu canvasu (male mono) gdy wlaczony flaga
  // localStorage['tv_overlay_debug']==='1'. Pokazuje czasy potrzebne do
  // wytarowania SYNC_OFFSET_MS: currentTime, live-edge bufora, opoznienie wideo,
  // wiek najnowszej i wybranej detekcji oraz odchylke od celu.
  drawDebugHud(selected) {
    let on = false;
    try { on = localStorage.getItem('tv_overlay_debug') === '1'; } catch { /* ignore */ }
    if (!on) return;

    const ctx = this.ctx;
    const v = this.videoEl();
    const now = Date.now();
    const ct = v && Number.isFinite(v.currentTime) ? v.currentTime : 0;
    let bufEnd = null;
    if (v && v.buffered && v.buffered.length > 0) {
      bufEnd = v.buffered.end(v.buffered.length - 1);
    }
    const videoLatMs = bufEnd != null ? (bufEnd - ct) * 1000 : null;
    const latest = this.frames.length ? this.frames[this.frames.length - 1] : null;
    const detAge = latest ? now - latest.tsMs : null;
    const targetWallMs = this.targetCaptureWallMs();
    const selAge = selected ? now - selected.tsMs : null;
    const deltaTarget = (targetWallMs != null && selected) ? targetWallMs - selected.tsMs : null;

    const fmt = (x, unit = '') => (x == null ? '—' : `${Math.round(x)}${unit}`);
    const linie = [
      `ct=${ct.toFixed(3)}s`,
      `bufEnd=${bufEnd == null ? '—' : bufEnd.toFixed(3) + 's'}`,
      `videoLat=${fmt(videoLatMs, 'ms')}`,
      `detAge=${fmt(detAge, 'ms')}`,
      `offset=${fmt(syncOffsetMs(), 'ms')}`,
      `selAge=${fmt(selAge, 'ms')}`,
      `Δtarget=${fmt(deltaTarget, 'ms')}`,
    ];

    const dpr = window.devicePixelRatio || 1;
    const fontPx = Math.round(11 * dpr);
    ctx.save();
    ctx.font = `${fontPx}px ui-monospace, SFMono-Regular, Menlo, Consolas, monospace`;
    ctx.textBaseline = 'top';
    const lineH = Math.round(fontPx * 1.35);
    let maxW = 0;
    for (const t of linie) maxW = Math.max(maxW, ctx.measureText(t).width);
    const pad = Math.round(6 * dpr);
    const boxW = maxW + pad * 2;
    const boxH = linie.length * lineH + pad * 2;
    const bx = Math.round(6 * dpr);
    const by = Math.round(6 * dpr);
    ctx.fillStyle = 'rgba(0,0,0,0.65)';
    ctx.fillRect(bx, by, boxW, boxH);
    ctx.fillStyle = '#7CFC7C';
    for (let i = 0; i < linie.length; i++) {
      ctx.fillText(linie[i], bx + pad, by + pad + i * lineH);
    }
    ctx.restore();
  }

  // Greedy declutter: uklada etykiety tak, by ich pigulki na siebie nie nachodzily.
  // Wyzszy priorytet (score, potem box area) zajmuje preferowane miejsce pierwszy;
  // mniej istotne ustepuja i sa przesuwane. Zlozonosc O(n^2) — akceptowalna dla
  // kilkunastu etykiet na klatke.
  ulozEtykiety(etykiety, odstep) {
    const kolejnosc = etykiety.slice().sort((a, b) => b.priorytet - a.priorytet);
    const ulozone = [];
    const cw = this.canvas.width;
    const ch = this.canvas.height;
    for (const e of kolejnosc) {
      const poz = this.znajdzWolneMiejsce(e, ulozone, odstep, cw, ch);
      e.x = poz.x;
      e.y = poz.y;
      e.moved = Math.abs(e.x - e.prefX) > 1 || Math.abs(e.y - e.prefY) > 1;
      ulozone.push(e);
    }
  }

  // Szuka najblizszej wolnej pozycji dla etykiety: najpierw preferowana, potem
  // w dol, w gore (kolumnami co wysokosc+odstep), na koncu przesuniecie w bok.
  // Kandydaci sa klampowani do obszaru canvasu. Gdy nic nie znaleziono —
  // zwraca preferowana (dopuszczamy nachodzenie jako ostatecznosc).
  znajdzWolneMiejsce(e, ulozone, odstep, cw, ch) {
    const krok = e.labelH + odstep;
    const maxK = 24;
    const boki = [0, e.labelW + odstep, -(e.labelW + odstep)];
    for (const dx of boki) {
      // Dla kazdej kolumny probujemy preferowana wysokosc, potem coraz dalej
      // w dol i w gore naprzemiennie (najblizsze wolne miejsce wygrywa).
      for (let k = 0; k <= maxK; k++) {
        const przesuniecia = k === 0 ? [0] : [k * krok, -k * krok];
        for (const dy of przesuniecia) {
          let x = e.prefX + dx;
          let y = e.prefY + dy;
          x = Math.max(0, Math.min(x, cw - e.labelW));
          y = Math.max(0, Math.min(y, ch - e.labelH));
          if (!kolidujeZ(x, y, e.labelW, e.labelH, ulozone, odstep)) {
            return { x, y };
          }
        }
      }
    }
    return { x: e.prefX, y: e.prefY };
  }

  // Rysuje cienka linie-lacznik w kolorze klasy od gornej krawedzi boxa do
  // najblizszej krawedzi pigulki — sygnalizuje, ktorej detekcji dotyczy
  // odsunieta etykieta.
  drawLeader(ctx, e, dpr) {
    const bx = e.boxCenterX;
    const by = e.boxTop;
    // Punkt na pigulce zwrocony ku boxowi (gorna/dolna krawedz lub srodek).
    let ty;
    if (e.y + e.labelH <= by) ty = e.y + e.labelH;
    else if (e.y >= by) ty = e.y;
    else ty = e.y + e.labelH / 2;
    const tx = Math.max(e.x, Math.min(bx, e.x + e.labelW));

    ctx.beginPath();
    ctx.moveTo(bx, by);
    ctx.lineTo(tx, ty);
    ctx.lineWidth = Math.max(1, Math.round(1 * dpr));
    ctx.strokeStyle = e.kolor;
    ctx.globalAlpha = 0.85;
    ctx.stroke();
    ctx.globalAlpha = 1;
  }

  // Rysuje etykiete jako pelna, nieprzezroczysta pigulke w kolorze klasy z
  // zaokraglonymi rogami i BIALYM pogrubionym tekstem (z ciemna obwodka dla
  // kontrastu na jasnych kolorach). Pozycja (e.x, e.y) jest juz rozsunieta
  // przez declutter.
  drawLabel(ctx, e, padX, padY, dpr) {
    const radius = Math.round(4 * dpr);

    // Pigulka w kolorze klasy z zaokraglonymi rogami (fallback na prostokat,
    // gdy przegladarka nie ma roundRect).
    ctx.fillStyle = e.kolor;
    ctx.beginPath();
    if (typeof ctx.roundRect === 'function') {
      ctx.roundRect(e.x, e.y, e.labelW, e.labelH, radius);
    } else {
      ctx.rect(e.x, e.y, e.labelW, e.labelH);
    }
    ctx.fill();

    // Bialy pogrubiony tekst z ciemna obwodka — czytelny na kazdym kolorze pigulki.
    const tx = e.x + padX;
    const ty = e.y + e.labelH - padY;
    ctx.lineJoin = 'round';
    ctx.lineWidth = Math.max(2, Math.round(2 * dpr));
    ctx.strokeStyle = 'rgba(0,0,0,0.55)';
    ctx.strokeText(e.text, tx, ty);
    ctx.fillStyle = '#fff';
    ctx.fillText(e.text, tx, ty);
  }

  // =============================================================================
  // Sprzatanie
  // =============================================================================

  destroy() {
    if (this.disposed) return;
    this.disposed = true;
    this.pending.disposed = true;
    if (this.rafId != null) cancelAnimationFrame(this.rafId);
    this.rafId = null;
    if (this.resizeObserver) this.resizeObserver.disconnect();
    else window.removeEventListener('resize', this.boundResize);
    if (this.unsub) {
      try { this.unsub(); } catch { /* ignore */ }
      this.unsub = null;
    }
    this.frames.length = 0;
    if (this.hostEl && this.hostEl.__detOverlay === this) {
      delete this.hostEl.__detOverlay;
    }
    if (this.canvas.parentElement) this.canvas.parentElement.removeChild(this.canvas);
  }

  // Alias zachowany dla wywolan starego API (renderer wola dispose()).
  dispose() {
    this.destroy();
  }
}

// Test kolizji prostokata (x,y,w,h) z juz ulozonymi pigulkami, z marginesem
// `odstep` z kazdej strony. AABB — szybkie, bez alokacji.
function kolidujeZ(x, y, w, h, ulozone, odstep) {
  for (const o of ulozone) {
    if (
      x < o.x + o.labelW + odstep &&
      x + w + odstep > o.x &&
      y < o.y + o.labelH + odstep &&
      y + h + odstep > o.y
    ) {
      return true;
    }
  }
  return false;
}

// Buduje etykiete: `klasa` + opcjonalnie ` "tekst"` + opcjonalnie ` (stan...)`.
function budujEtykiete(det) {
  const czesci = [];
  if (det?.klasa) czesci.push(String(det.klasa));
  if (det?.score != null && Number.isFinite(det.score)) {
    czesci.push(`${Math.round(det.score * 100)}%`);
  }
  if (det?.tekst != null && String(det.tekst).length > 0) {
    czesci.push(`"${det.tekst}"`);
  }
  if (Array.isArray(det?.stan) && det.stan.length > 0) {
    czesci.push(`(${det.stan.join(', ')})`);
  }
  return czesci.join(' ');
}
