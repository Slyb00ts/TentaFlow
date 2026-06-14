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

    // Mapowanie video.currentTime (sekundy mediów) na czas sciany (ms unix) ramek
    // detekcji. Ustawiane przy pierwszej widzianej parze (currentTime, tsMs);
    // pozwala wybrac detekcje dla AKTUALNIE WYSWIETLANEJ klatki MSE.
    this.ptsAnchorMediaS = null;
    this.ptsAnchorWallMs = null;

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
  pushFrame(tsMs, items) {
    const frame = { tsMs, items };
    const buf = this.frames;
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

  // Wybiera zestaw detekcji najblizszy czasowo do klatki pokazywanej przez
  // <video>. Mapujemy `video.currentTime` (sekundy mediów) na czas sciany przez
  // kotwice (currentTime, tsMs) zlapana przy pierwszej parze. Gdy currentTime
  // nie postepuje (brak realnego dekodowania MSE) lub kotwicy brak — degradujemy
  // HONESTNIE do najnowszej ramki.
  selectFrame() {
    const buf = this.frames;
    if (buf.length === 0) return null;
    const latest = buf[buf.length - 1];

    const v = this.videoEl();
    const ct = v ? v.currentTime : 0;
    const hasPlayback = v && Number.isFinite(ct) && ct > 0 && (v.readyState || 0) > 0;
    if (!hasPlayback) return latest;

    // Ustaw/odswiez kotwice currentTime<->tsMs przy najnowszej ramce. To trzyma
    // mapowanie aktualnym mimo dryfu zegarow (MSE bufer rosnie, sieci jitter).
    if (this.ptsAnchorMediaS == null) {
      this.ptsAnchorMediaS = ct;
      this.ptsAnchorWallMs = latest.tsMs;
    }
    // Docelowy czas sciany dla aktualnie wyswietlanej klatki.
    const targetWallMs = this.ptsAnchorWallMs + (ct - this.ptsAnchorMediaS) * 1000;

    // Re-kotwiczenie: gdy kotwica zestarzala sie (target daleko poza buforem),
    // przeskocz do najnowszej ramki i ustaw kotwice od nowa — unika trwalego
    // dryfu, gdy MSE chwilowo stoi a potem nadrabia.
    if (targetWallMs > latest.tsMs + 2000 || targetWallMs < buf[0].tsMs - 2000) {
      this.ptsAnchorMediaS = ct;
      this.ptsAnchorWallMs = latest.tsMs;
      return latest;
    }

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
    if (!frame || !frame.items.length) return;

    const area = this.videoContentRect();
    const dpr = window.devicePixelRatio || 1;
    const lineW = Math.max(2, Math.round(2 * dpr));
    const fontPx = Math.max(11, Math.round(13 * dpr));
    ctx.lineWidth = lineW;
    ctx.font = `${fontPx}px var(--font-mono, monospace)`;
    ctx.textBaseline = 'bottom';

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
      ctx.strokeStyle = kolor;
      ctx.strokeRect(x, y, w, h);

      const etykieta = budujEtykiete(det);
      if (etykieta) this.drawLabel(ctx, etykieta, x, y, kolor, fontPx, dpr);
    }
  }

  // Rysuje etykiete z tlem nad lewym-gornym rogiem ramki (lub pod nim, gdy
  // u gory brak miejsca), zeby tekst byl czytelny na dowolnym obrazie.
  drawLabel(ctx, text, x, y, kolor, fontPx, dpr) {
    const padX = Math.round(5 * dpr);
    const padY = Math.round(3 * dpr);
    const textW = ctx.measureText(text).width;
    const boxW = textW + padX * 2;
    const boxH = fontPx + padY * 2;
    let labelY = y - boxH;
    if (labelY < 0) labelY = y; // brak miejsca u gory — rysuj wewnatrz ramki
    let labelX = x;
    if (labelX + boxW > this.canvas.width) labelX = this.canvas.width - boxW;
    if (labelX < 0) labelX = 0;

    ctx.fillStyle = kolor;
    ctx.fillRect(labelX, labelY, boxW, boxH);
    ctx.fillStyle = '#0a0a14';
    ctx.fillText(text, labelX + padX, labelY + boxH - padY);
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
    if (this.canvas.parentElement) this.canvas.parentElement.removeChild(this.canvas);
  }

  // Alias zachowany dla wywolan starego API (renderer wola dispose()).
  dispose() {
    this.destroy();
  }
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
