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
//   * latest-wins: trzymamy bufor NAJNOWSZYCH ramek po `tsMs` (~3 s przy 25fps),
//     nigdy nie kolejkujemy w nieskonczonosc. Nowy chunk nadpisuje stan.
//   * pojedyncza petla rAF (NIE render-per-message): klatka rysowana jest co
//     najwyzej z czestotliwoscia odswiezania ekranu niezaleznie od 25fps na
//     drucie. Wiadomosci tylko aktualizuja bufor — rysuje rAF.
//   * synchronizacja PTS (media-time): gdy strumien niesie wspolna os czasu
//     (init-segment MSE ma `base_pts_ns`, a kazda ramka detekcji `pts_ns`),
//     bufor trzymamy po `ptsMs = (pts_ns - base_pts_ns)/1e6`, a cel rysowania to
//     `video.currentTime*1000` — overlay i wideo sa w tym samym media-time. Bez
//     tej osi (brak PTS) degradujemy do sciezki wall-clock (Date.now()).
//   * plynnosc 60fps: zamiast rysowac jedna "wybrana" ramke, w kazdej klatce rAF
//     budujemy mape `track_id -> box` w chwili `t=currentTime*1000` interpolujac
//     (lerp) miedzy sasiednimi detekcjami tego samego tracku albo ekstrapolujac
//     po predkosci z dwoch ostatnich detekcji (klamp MAX_EXTRAP_MS_KRAWEDZ)
//     gdy `t` wyprzedza najswiezsza detekcje tracku. Zgubione
//     tracki wygaszamy (fade-out) po TRACK_FADE_MS. Bez track_id — degradacja do
//     rysowania pojedynczej wybranej ramki bez interpolacji.
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

// Rozdziela tekst tablicy ADR z serwera na numer i opis towaru. Backend wysyla
// go w formacie "<kemler>/<un> <opis>" (opis pochodzi z gitignorowanej listy
// adr-list.json po stronie serwera — front NIE trzyma zadnych danych ADR).
// Kod ADR to "<kemler>/<un>" (bez spacji), a wszystko po pierwszej spacji to
// opis. Zwraca { kod, opis } (opis moze byc null gdy serwer go nie dolaczyl).
function rozdzielAdr(tekst) {
  const s = String(tekst).trim();
  const sp = s.indexOf(' ');
  if (sp < 0) return { kod: s, opis: null };
  const opis = s.slice(sp + 1).trim();
  return { kod: s.slice(0, sp), opis: opis.length > 0 ? opis : null };
}

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
// zeby nie wisialy nieaktualne detekcje gdy backend zamilknie. Luzny prog —
// wlasciwe wygaszanie starych ramek robi os media-time (MAX_WIEK_RAMKI_MS);
// ten wall-clockowy bezpiecznik lapie tylko realna cisze backendu, a nie
// chwilowe zrywy dostarczania WS (te przy ciasnym progu gasily caly overlay).
const WYGASZ_PO_MS = 1500;

// Okno czasu bufora ramek (ms) liczone WSTECZ od NAJNOWSZEJ ramki (po tsMs).
// Na WAN wideo potrafi byc kilka sekund za detekcjami — gdyby bufor trzymal
// tylko ~3 s historii, wszystkie ramki bylyby "z przyszlosci" wzgledem
// wyswietlanej klatki i overlay nie mialby czego narysowac. 20 s pokrywa
// realne opoznienia lacza z zapasem; starsze ramki sa odrzucane.
const BUFOR_OKNO_MS = 20000;

// Twardy limit bezpieczenstwa liczby ramek w buforze — chroni pamiec, gdyby
// backend slal ramki gesciej niz zakladane ~25fps (600 = ~24 s przy 25fps).
const BUFOR_MAX_RAMEK = 600;

// Maksymalne dopuszczalne WYPRZEDZENIE ramki detekcji wzgledem czasu wideo
// (media-time ramki > target + epsilon => ramka jest z PRZYSZLOSCI wzgledem
// wyswietlanej klatki i NIE wolno jej rysowac — boxy pojawialyby sie zanim
// obiekt wjedzie w kadr). 20ms = pol klatki wideo 25fps: `video.currentTime`
// kwantyzuje ~40ms, wieksza epsilon dawala systematyczne WYPRZEDZANIE boxow
// na ruchomych obiektach. TUNOWALNE bez restartu przez
// localStorage['tv_eps_przod_ms'].
const EPS_PRZOD_MS = 20;

// Maksymalny WIEK ramki detekcji wzgledem czasu wideo (target - media-time).
// Starsze ramki odrzucamy calkowicie — po zmianie sceny stare oznaczenia
// znikaja najpozniej po tym czasie. Ponizej progu pelna alpha do
// WIEK_FADE_OD_MS, potem liniowe wygaszanie do zera.
const MAX_WIEK_RAMKI_MS = 450;
const WIEK_FADE_OD_MS = 200;

// Maksymalna przerwa (ms) miedzy ramkami A i B, przez ktora jeszcze lerpujemy
// bbox tracku. Wieksza dziura = dropout detekcji — lerp przez nia ciagnalby
// box przez pol ekranu; zamiast tego trzymamy ostatnia realna pozycje (A).
const MAX_PRZERWA_LERP_MS = 300;

// Stale opoznienie (ms) miedzy PRZECHWYCENIEM klatki a jej pojawieniem sie na
// live-edge bufora MSE (enkod + mux + siec). Odejmowane od `Date.now()` przy
// estymacji czasu przechwycenia aktualnie wyswietlanej klatki. Wartosc TUNOWALNA
// — mozna nadpisac bez restartu przez localStorage['tv_sync_offset_ms'].
const SYNC_OFFSET_MS = 300;

// Okno ekstrapolacji pozycji boxa po predkosci vx/vy GDY target media-time
// wyprzedza najswiezsza detekcje tracku. USTAWIONE NA 0 — predykcja/ekstrapolacja
// w przyszlosc jest WYLACZONA: detektor obrabia KAZDA klatke wideo (~26fps), wiec
// nie ma potrzeby przewidywac pozycji, a ekstrapolacja po vx/vy dawala widoczny
// DRIFT boxow. Boxy pokazujemy WYLACZNIE na realnych pozycjach z obrobionych
// klatek (dobor ramki po PTS + interpolacja MIEDZY dwiema realnymi ramkami, gdzie
// oba konce sa realne). TUNOWALNE bez restartu przez localStorage['tv_max_extrap_ms'].
const MAX_EXTRAP_MS = 0;

// Okno MINIMALNEJ ekstrapolacji na SWIEZEJ KRAWEDZI (ms): gdy target media-time
// jest nowszy niz najnowsza detekcja tracku (brak ramki B), bbox przesuwamy po
// predkosci wyliczonej z dwoch ostatnich realnych detekcji, ale najwyzej o tyle
// ms. To NIE jest globalna predykcja — jedynie wypelnienie luki miedzy ostatnia
// obrobiona klatka a biezaca klatka wideo (~jedna ramka analizy przy 12fps),
// bez tego boxy CIAGNA SIE ZA ruchomym obiektem do 1/analysis_fps. Przy
// dt > okna trzymamy ostatnia realna pozycje (zero driftu). TUNOWALNE bez
// restartu przez localStorage['tv_extrap_krawedz_ms'].
const MAX_EXTRAP_MS_KRAWEDZ = 80;

// Po tylu ms bez detekcji tracku w/za targetem media-time uznajemy track za
// zgubiony: liniowo wygaszamy alpha do zera i pomijamy. Zapobiega "duchom" po
// obiektach, ktore znikly z kadru. MUSI byc > MAX_EXTRAP_MS (fade dopiero gdy
// track NAPRAWDE zgubiony): przy overshoot <= MAX_EXTRAP_MS alpha=1 (pelny box),
// gaszenie dopiero na odcinku (MAX_EXTRAP_MS, TRACK_FADE_MS].
//
// 450ms — spojne z MAX_WIEK_RAMKI_MS: zadne oznaczenie nie zyje dluzej niz
// ~pol sekundy za wideo. TUNOWALNE bez restartu przez
// localStorage['tv_track_fade_ms'] (klampowane, by zawsze > MAX_EXTRAP_MS).
const TRACK_FADE_MS = 450;

// Backoff kolejnych prob ponownej subskrypcji detekcji po zerwaniu strumienia
// (pad transportu WS / koniec strumienia po stronie serwera). Ostatnia wartosc
// powtarzana az do sukcesu; licznik zerowany po udanej subskrypcji.
const RESUBSCRIBE_BACKOFF_MS = [1000, 2000, 5000];

// Po tylu ms bez odswiezenia usuwamy wpis z cache stanu/OCR per track_id
// (trackMeta), zeby mapa nie rosla po zniknieciu obiektu z kadru. 10s pokrywa
// typowa dziure miedzy wzbogaceniami fazy-2 z zapasem.
const TRACK_META_TTL_MS = 10000;

// Minimalny score odczytu OCR tablicy (`tekst`) dopuszczajacy go DO GLOSOWANIA
// czasowego per track_id. Nizsze odczyty (slaba detekcja, krawedz kadru, ruch)
// daja smieci ("EICS", "182O") i sa ignorowane — nie licza sie na zwyciezce.
// Struktura glosow (ocrGlosy) uzywa tego samego TTL co trackMeta.
const OCR_MIN_SCORE = 0.75;

// Konwersja pola u64 (BigInt | number | null/undefined) z dekodera na Number ns
// albo null. pts_ns/base_pts_ns to nanosekundy media-timeline (~1e11..1e12) —
// mieszcza sie bezpiecznie w Number (2^53).
function ptsToNumber(v) {
  if (v == null) return null;
  const n = typeof v === 'bigint' ? Number(v) : Number(v);
  return Number.isFinite(n) ? n : null;
}

// Interpolacja liniowa calego bboxa [x,y,w,h] miedzy A i B wspolczynnikiem a.
function lerpBbox(a, b, alpha) {
  return [
    a[0] + (b[0] - a[0]) * alpha,
    a[1] + (b[1] - a[1]) * alpha,
    a[2] + (b[2] - a[2]) * alpha,
    a[3] + (b[3] - a[3]) * alpha,
  ];
}

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

// Aktualne okno ekstrapolacji (ms): nadpisanie z localStorage lub MAX_EXTRAP_MS.
function maxExtrapMs() {
  try {
    const raw = localStorage.getItem('tv_max_extrap_ms');
    if (raw != null) {
      const v = Number(raw);
      if (Number.isFinite(v) && v >= 0) return v;
    }
  } catch { /* ignore */ }
  return MAX_EXTRAP_MS;
}

// Aktualna epsilon wyprzedzenia ramki (ms): nadpisanie z localStorage lub
// EPS_PRZOD_MS.
function epsPrzodMs() {
  try {
    const raw = localStorage.getItem('tv_eps_przod_ms');
    if (raw != null) {
      const v = Number(raw);
      if (Number.isFinite(v) && v >= 0) return v;
    }
  } catch { /* ignore */ }
  return EPS_PRZOD_MS;
}

// Aktualne okno ekstrapolacji na swiezej krawedzi (ms): nadpisanie z
// localStorage lub MAX_EXTRAP_MS_KRAWEDZ.
function extrapKrawedzMs() {
  try {
    const raw = localStorage.getItem('tv_extrap_krawedz_ms');
    if (raw != null) {
      const v = Number(raw);
      if (Number.isFinite(v) && v >= 0) return v;
    }
  } catch { /* ignore */ }
  return MAX_EXTRAP_MS_KRAWEDZ;
}

// Aktualny prog wygaszania zgubionego tracku (ms). Klampowany tak, by ZAWSZE
// byl wiekszy od okna ekstrapolacji (inaczej dzielenie w fade daloby <=0 albo
// ujemna alpha juz na starcie okna) — fix CR-001.
function trackFadeMs() {
  const maxE = maxExtrapMs();
  let v = TRACK_FADE_MS;
  try {
    const raw = localStorage.getItem('tv_track_fade_ms');
    if (raw != null) {
      const n = Number(raw);
      if (Number.isFinite(n)) v = n;
    }
  } catch { /* ignore */ }
  return Math.max(v, maxE + 1);
}

// =============================================================================
// Publiczne API
// =============================================================================

// Tworzy nakladke nad podanym elementem <video> i zwraca uchwyt z destroy().
// `video`         — element <video> LUB host (tf-video-stream) — wtedy szukamy
//                   <video> w jego shadow DOM przez `videoResolver`.
// `cameraId`      — identyfikator kamery (`cam_<uuid v4>`) do subskrypcji binarnej.
// `videoResolver` — opcjonalna funkcja zwracajaca realny <video> (shadow DOM).
// `mediaBasePtsProvider` — opcjonalna funkcja zwracajaca `base_pts_ns` strumienia
//                   (ns, z init-segmentu MSE) lub null. Gdy dostepna razem z
//                   `pts_ns` ramek — overlay przechodzi na precyzyjny tryb
//                   media-time. Alternatywa: setter overlay.setMediaBasePts(ns).
export function attachDetectionsOverlay(opts) {
  return new DetectionsOverlay(opts);
}

// =============================================================================
// Implementacja
// =============================================================================

class DetectionsOverlay {
  constructor({ video, cameraId, videoResolver = null, mediaBasePtsProvider = null }) {
    if (!video) throw new Error('attachDetectionsOverlay: brak elementu video');
    if (!cameraId) throw new Error('attachDetectionsOverlay: brak cameraId');

    this.hostEl = video;
    this.cameraId = String(cameraId);
    // tf-video-stream trzyma <video> w shadow DOM — resolver pozwala
    // znalezc realny element do pomiaru object-fit i wymiarow klatki.
    this.videoResolver = videoResolver;
    // Zrodlo `base_pts_ns` (ns) do rebazowania osi media-time. Provider (czytany
    // co ramke — sledzi zmiany bazy przy resubscribe) ma pierwszenstwo przed
    // wartoscia ustawiana recznie przez setMediaBasePts().
    this.mediaBasePtsProvider = typeof mediaBasePtsProvider === 'function' ? mediaBasePtsProvider : null;
    this.mediaBasePtsManual = null;

    this.canvas = document.createElement('canvas');
    this.canvas.className = 'vision-detections-overlay';
    this.canvas.style.position = 'absolute';
    this.canvas.style.left = '0';
    this.canvas.style.top = '0';
    this.canvas.style.zIndex = '20';
    this.canvas.style.pointerEvents = 'none';
    this.ctx = this.canvas.getContext('2d');

    // Bufor NAJNOWSZYCH ramek posortowany rosnaco po tsMs (latest-wins).
    // Kazdy element: { tsMs, ptsNs, items }. `ptsNs` (ns media-timeline lub null)
    // uzywany w trybie media-time; `tsMs` (wall-clock) w trybie awaryjnym.
    this.frames = [];
    // Kliencki cache OSTATNIEGO ZNANEGO niepustego `stan`/`tekst` (OCR) per
    // track_id. Serwer wzbogaca detekcje w fazie-2 z opoznieniem, wiec
    // najswiezsza ramka tracku (faza-1) miewa pusty stan; z cache uzupelniamy
    // etykiete, by nie migotala miedzy "(czysta)" a brakiem stanu.
    // Klucz: track_id (Number). Wartosc: { stan: Array|null, tekst: String|null, at: ms }.
    this.trackMeta = new Map();
    // Glosowanie czasowe na odczyt OCR tablicy per track_id: zliczamy wystapienia
    // stringow `tekst` (tylko score>=OCR_MIN_SCORE), by przy renderze pokazac
    // NAJSPOJNIEJSZY odczyt zamiast ostatniego losowego (eliminacja migania/smieci).
    // Klucz: track_id (Number). Wartosc: { glosy: Map<tekst, {count, maxScore}>, at: ms }.
    this.ocrGlosy = new Map();
    // Stabilna KOLEJNOSC klas nalepek/znakow w linii 2 paska (bez stalego slotu).
    // Kazdej klasie przy PIERWSZYM pojawieniu nadajemy rosnacy `seq`; sortowanie
    // obecnych klas po `seq` daje stabilna kolejnosc (nowa klasa dochodzi na
    // koniec, nie przestawia istniejacych). Nalepki pakujemy potem OD LEWEJ
    // (sloty 0,1,2 bez dziur). Wpis wygasa po TRACK_META_TTL_MS nieobecnosci.
    // Klucz: klasa (String). Wartosc: { seq: Number, at: ms }.
    this.klasaSeq = new Map();
    // Monotoniczny licznik nadawany kolejnym NOWYM klasom (zrodlo `seq`).
    this.klasaSeqLicznik = 0;
    // Ostatnia baza PTS (ns) widziana od providera. Zmiana bazy (resubscribe
    // wideo) uniewaznia bufor — stare ramki liczylyby sie wg zlej osi.
    this.lastBasePtsNs = null;
    // Ostatni widziany element <video> — zmiana referencji oznacza przelaczenie
    // strumienia/kamery (reattach), po ktorym stare oznaczenia musza zniknac.
    this.lastVideoEl = null;
    this.lastMessageAt = 0;
    this.disposed = false;
    this.unsub = null;
    // Sygnalizuje disposal w trakcie oczekiwania na ApiBinary.subscribe(),
    // zeby pozno-rozwiazany unsubscribe nie wyciekl gdy panel zniknie.
    this.pending = { disposed: false };
    this.rafId = null;
    // Guard przed podwojna subskrypcja: `connecting` = subscribe w locie,
    // `unsub` != null = subskrypcja aktywna. Resubscribe po zerwaniu strumienia
    // idzie przez scheduleResubscribe() z backoffem + budzeniem na 'open' WS.
    this.connecting = false;
    this.resubscribeAttempt = 0;
    this.resubscribeTimer = null;
    this.lifecycleUnsub = null;

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

  // Ustawia recznie baze media-time (`base_pts_ns`, ns) — alternatywa dla
  // providera przekazanego w opts. Wartosc uzyta gdy providera brak.
  setMediaBasePts(ns) {
    this.mediaBasePtsManual = ptsToNumber(ns);
  }

  // Zwraca aktualna baze media-time (ns) jako Number albo null. Provider ma
  // pierwszenstwo (sledzi zmiany przy resubscribe strumienia MSE).
  mediaBasePtsNs() {
    if (this.mediaBasePtsProvider) {
      try { return ptsToNumber(this.mediaBasePtsProvider()); } catch { return null; }
    }
    return this.mediaBasePtsManual;
  }

  // Dopasowuje rozmiar canvasa (CSS px) do boxu hosta oraz bufor pikseli do DPR.
  resizeCanvas() {
    const rect = this.hostEl.getBoundingClientRect();
    const parent = this.canvas.parentElement;
    // Zapamietana pozycja hosta w viewporcie — draw() porownuje ja co klatke
    // i przelicza offset, gdy host przesunal sie BEZ zmiany rozmiaru
    // (ResizeObserver takiego ruchu nie zglasza).
    this.lastHostLeft = rect.left;
    this.lastHostTop = rect.top;
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
    // Guard przed podwojna subskrypcja (timer backoffu + budzenie 'open' moga
    // wystrzelic blisko siebie): nic nie rob gdy subscribe w locie lub aktywny.
    if (this.connecting || this.unsub) return;
    this.connecting = true;
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
        this.connecting = false;
        if (this.pending.disposed || this.disposed) {
          try { unsub(); } catch { /* ignore */ }
          return;
        }
        this.unsub = unsub;
        this.resubscribeAttempt = 0;
      })
      .catch((err) => {
        this.connecting = false;
        console.warn('[vision-detections-overlay] subscribe failed:', err?.message ?? err);
        // Nieudana subskrypcja (np. WS w trakcie reconnectu) — probuj ponownie
        // z backoffem, az sie uda.
        this.scheduleResubscribe();
      });
  }

  // Planuje ponowna subskrypcje detekcji po zerwaniu strumienia. Backoff wg
  // RESUBSCRIBE_BACKOFF_MS; dodatkowo gdy WS lezy — jednorazowy listener
  // lifecycle 'open' wznawia subskrypcje od razu po powrocie transportu,
  // bez czekania na kolejny tick timera.
  scheduleResubscribe() {
    if (this.disposed) return;
    if (this.resubscribeTimer != null) return;
    const delay =
      RESUBSCRIBE_BACKOFF_MS[Math.min(this.resubscribeAttempt, RESUBSCRIBE_BACKOFF_MS.length - 1)];
    this.resubscribeAttempt += 1;
    this.resubscribeTimer = setTimeout(() => {
      this.resubscribeTimer = null;
      if (this.disposed) return;
      this.connect();
    }, delay);
    if (!ApiBinary.isConnected() && !this.lifecycleUnsub) {
      this.lifecycleUnsub = ApiBinary.onLifecycle((ev) => {
        if (ev.type !== 'open') return;
        if (this.lifecycleUnsub) {
          this.lifecycleUnsub();
          this.lifecycleUnsub = null;
        }
        if (this.disposed) return;
        if (this.resubscribeTimer != null) {
          clearTimeout(this.resubscribeTimer);
          this.resubscribeTimer = null;
        }
        this.connect();
      });
    }
  }

  onChunk(body) {
    if (this.disposed) return;
    if (!body || body.variant !== 'CameraDetectionsFrame') return;
    // Wiadomosci dla innej kamery ignorujemy (jeden strumien = jedna kamera,
    // ale strzezemy sie przed niespojnoscia po stronie backendu).
    const cam = body.cameraId ?? body.camera_id;
    if (cam != null && String(cam) !== this.cameraId) return;

    const tsMs = Number(body.tsMs ?? body.ts_ms ?? 0);
    const ptsNs = ptsToNumber(body.pts_ns ?? body.ptsNs);
    const items = Array.isArray(body.items) ? body.items : [];
    // Calosciowy czas obrobki ramki po stronie backendu (detekcja+OCR+stan).
    // Pole opcjonalne — backend zaczyna je slac pozniej; do tego czasu null.
    const rawProc = body.proc_ms ?? body.procMs;
    const procMs = Number.isFinite(Number(rawProc)) ? Number(rawProc) : null;
    this.pushFrame(tsMs, ptsNs, items, procMs);
    this.lastMessageAt = performance.now();
  }

  // Wstawia ramke do bufora utrzymujac porzadek rosnacy po tsMs. Ewikcja:
  // ramki starsze niz BUFOR_OKNO_MS wzgledem NAJNOWSZEJ ramki (po tsMs) oraz
  // twardy limit BUFOR_MAX_RAMEK sztuk (najstarsze odrzucamy). Najczestsza
  // sciezka (ramki przychodza chronologicznie) to push na koniec + shift.
  //
  // FAZA 2 (wzbogacenie po stronie backendu) publikuje ten sam tsMs co FAZA 1
  // (surowe boxy). Zamiast duplikowac ramke AKTUALIZUJEMY istniejaca po tsMs, by
  // overlay podmienil surowe boxy na wzbogacone etykiety (stan/OCR) dla tej samej
  // klatki — inaczej selectFrame trzymalby sie wersji surowej (FAZA 1).
  pushFrame(tsMs, ptsNs, items, procMs = null) {
    // Zbierz ostatni znany niepusty stan/OCR per track_id z KAZDEJ przychodzacej
    // ramki (takze wzbogaconej fazy-2, ktora nadpisuje istniejacy tsMs) — inaczej
    // przy renderze bralibysmy metadane tylko z najswiezszej ramki (czesto surowej).
    this.zapiszTrackMeta(items);
    // Zbierz glosy OCR tablic (score>=OCR_MIN_SCORE) z KAZDEJ ramki (takze
    // wzbogaconej fazy-2) — glosowanie musi widziec wszystkie odczyty tracku.
    this.zapiszGlosyOcr(items);

    const buf = this.frames;
    for (let i = buf.length - 1; i >= 0; i--) {
      if (buf[i].tsMs === tsMs) {
        buf[i].items = items;
        // Nie nadpisuj znanego ptsNs nullem — wzbogacona ramka fazy-2 bez PTS
        // kasowalaby os media-time ramki i wypadalaby z doboru po czasie.
        if (ptsNs != null) buf[i].ptsNs = ptsNs;
        if (procMs != null) buf[i].procMs = procMs;
        return;
      }
      // Bufor posortowany rosnaco — gdy zejdziemy ponizej tsMs, dopasowania nie ma.
      if (buf[i].tsMs < tsMs) break;
    }
    const frame = { tsMs, ptsNs, items, procMs };
    if (buf.length === 0 || tsMs >= buf[buf.length - 1].tsMs) {
      buf.push(frame);
    } else {
      // Ramka spoza kolejnosci (rzadkie) — wstaw na wlasciwa pozycje.
      let i = buf.length - 1;
      while (i >= 0 && buf[i].tsMs > tsMs) i--;
      buf.splice(i + 1, 0, frame);
    }
    const prog = buf[buf.length - 1].tsMs - BUFOR_OKNO_MS;
    while (buf.length > 0 && buf[0].tsMs < prog) buf.shift();
    while (buf.length > BUFOR_MAX_RAMEK) buf.shift();
  }

  // Aktualizuje cache stanu/OCR per track_id na podstawie itemow ramki. Zapisuje
  // WYLACZNIE niepuste wartosci (pusty stan fazy-1 nie kasuje wczesniej znanego
  // stanu). Przy okazji usuwa wpisy starsze niz TRACK_META_TTL_MS (ewikcja).
  zapiszTrackMeta(items) {
    const now = performance.now();
    if (Array.isArray(items)) {
      for (const it of items) {
        const id = it?.track_id ?? it?.trackId ?? 0;
        if (!(id > 0)) continue;
        const stanNiepusty = Array.isArray(it?.stan) && it.stan.length > 0;
        const tekstNiepusty = it?.tekst != null && String(it.tekst).length > 0;
        if (!stanNiepusty && !tekstNiepusty) continue;
        let e = this.trackMeta.get(id);
        if (!e) { e = { stan: null, tekst: null, at: now }; this.trackMeta.set(id, e); }
        if (stanNiepusty) e.stan = it.stan;
        if (tekstNiepusty) e.tekst = String(it.tekst);
        e.at = now;
      }
    }
    // Ewikcja przeterminowanych wpisow (obiekt dawno zniknal z kadru).
    for (const [id, e] of this.trackMeta) {
      if (now - e.at > TRACK_META_TTL_MS) this.trackMeta.delete(id);
    }
  }

  // Zlicza glosy na odczyt OCR (`tekst`) dla detekcji klasy tablica_rejestracyjna
  // per track_id. Do glosowania trafiaja WYLACZNIE odczyty o score>=OCR_MIN_SCORE
  // i niepustym tekscie (slabe odczyty daja smieci — ignorujemy je). Przy okazji
  // ewiktuje wpisy starsze niz TRACK_META_TTL_MS (jak trackMeta).
  zapiszGlosyOcr(items) {
    const now = performance.now();
    if (Array.isArray(items)) {
      for (const it of items) {
        if (it?.klasa !== 'tablica_rejestracyjna') continue;
        const id = it?.track_id ?? it?.trackId ?? 0;
        if (!(id > 0)) continue;
        const tekst = it?.tekst != null ? String(it.tekst) : '';
        if (tekst.length === 0) continue;
        const score = Number.isFinite(it?.score) ? it.score : 0;
        if (score < OCR_MIN_SCORE) continue;
        let wpis = this.ocrGlosy.get(id);
        if (!wpis) { wpis = { glosy: new Map(), at: now }; this.ocrGlosy.set(id, wpis); }
        let g = wpis.glosy.get(tekst);
        if (!g) { g = { count: 0, maxScore: 0 }; wpis.glosy.set(tekst, g); }
        g.count += 1;
        if (score > g.maxScore) g.maxScore = score;
        wpis.at = now;
      }
    }
    // Ewikcja przeterminowanych glosow (track dawno zniknal z kadru).
    for (const [id, wpis] of this.ocrGlosy) {
      if (now - wpis.at > TRACK_META_TTL_MS) this.ocrGlosy.delete(id);
    }
  }

  // Zwraca zwycieski odczyt OCR dla track_id: string o najwyzszym `count`, a przy
  // remisie o wyzszym `maxScore`. null gdy brak jakiegokolwiek glosu (>=OCR_MIN_SCORE).
  zwyciezcaOcr(trackId) {
    const wpis = this.ocrGlosy.get(trackId);
    if (!wpis || wpis.glosy.size === 0) return null;
    let best = null;
    let bestCount = -1;
    let bestScore = -1;
    for (const [tekst, g] of wpis.glosy) {
      if (g.count > bestCount || (g.count === bestCount && g.maxScore > bestScore)) {
        best = tekst;
        bestCount = g.count;
        bestScore = g.maxScore;
      }
    }
    return best;
  }

  // Przygotowuje detekcje pod ETYKIETE: dla tablic rejestracyjnych podmienia
  // `tekst` na zwyciezce glosowania czasowego (najspojniejszy odczyt) zamiast
  // surowego, potencjalnie smieciowego odczytu z biezacej ramki. Brak zwyciezcy
  // (zaden odczyt >=OCR_MIN_SCORE) -> null, czyli etykieta bez tekstu (wolimy
  // brak nad smieci). Zwraca plytka kopie (nie mutuje itemu z bufora); pozostale
  // klasy zwraca bez zmian.
  detDoEtykiety(det) {
    if (det?.klasa !== 'tablica_rejestracyjna') return det;
    const id = det?.track_id ?? det?.trackId ?? 0;
    const zwyciezca = id > 0 ? this.zwyciezcaOcr(id) : null;
    return { ...det, tekst: zwyciezca };
  }

  // Uzupelnia metadane detekcji o ostatni znany stan/OCR z cache, GDY biezaca
  // ramka ma je puste. Zwraca plytka kopie (nie mutuje itemu z bufora) lub
  // oryginal, gdy nic do uzupelnienia. Detekcje bez track_id zwraca bez zmian.
  wzbogacDet(det) {
    const id = det?.track_id ?? det?.trackId ?? 0;
    if (!(id > 0)) return det;
    const meta = this.trackMeta.get(id);
    if (!meta) return det;
    const stanPusty = !Array.isArray(det?.stan) || det.stan.length === 0;
    const tekstPusty = det?.tekst == null || String(det.tekst).length === 0;
    const uzupelnijStan = stanPusty && Array.isArray(meta.stan) && meta.stan.length > 0;
    const uzupelnijTekst = tekstPusty && meta.tekst != null && meta.tekst.length > 0;
    if (!uzupelnijStan && !uzupelnijTekst) return det;
    const wynik = { ...det };
    if (uzupelnijStan) wynik.stan = meta.stan;
    if (uzupelnijTekst) wynik.tekst = meta.tekst;
    return wynik;
  }

  onEnd(body) {
    if (this.disposed) return;
    const reason = String(body?.reason ?? '');
    if (reason && reason !== 'client_request') {
      console.warn('[vision-detections-overlay] stream ended:', reason);
    }
    // Strumien zakonczony (takze syntetyczny `transport_closed` po padzie WS) —
    // klient juz usunal listener, wiec porzucamy uchwyt unsub bez wolania go
    // (wyslaloby zbedny StreamCloseRequest na martwym correlation_id).
    this.unsub = null;
    // Czyscimy caly stan (nie tylko bufor ramek), zeby po ponownym starcie
    // nie ozyly stare etykiety/glosy OCR.
    this.resetStanKamery();
    // Koniec z inicjatywy serwera/transportu — subskrybuj ponownie z backoffem,
    // zeby oznaczenia same wrocily po reconnect. `client_request` = nasze
    // wlasne zamkniecie, bez wznawiania.
    if (reason !== 'client_request') {
      this.scheduleResubscribe();
    }
  }

  onError(err) {
    if (this.disposed) return;
    console.warn('[vision-detections-overlay] protocol error:', err?.message ?? err);
    // Blad protokolu konczy strumien po naszej stronie — zamknij subskrypcje
    // (unsub zdejmuje listener i sle StreamCloseRequest), wyczysc stan i
    // sprobuj zasubskrybowac od nowa z backoffem.
    if (this.unsub) {
      try { this.unsub(); } catch { /* ignore */ }
      this.unsub = null;
    }
    this.resetStanKamery();
    this.scheduleResubscribe();
  }

  // Czysci CALY stan zwiazany z aktualnie ogladana kamera/strumieniem: bufor
  // ramek, cache stanu/OCR per track, glosy OCR i kolejnosc klas paska. Wywolywane
  // przy przelaczeniu ujecia (zmiana bazy PTS lub elementu <video>) oraz gdy
  // serwer zakonczy strumien — bez tego stare oznaczenia wisza kilka klatek.
  resetStanKamery() {
    this.frames.length = 0;
    this.trackMeta.clear();
    this.ocrGlosy.clear();
    this.klasaSeq.clear();
    this.lastMessageAt = 0;
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

  // Calosciowy czas obrobki (ms) z NAJSWIEZSZEJ ramki detekcji. null gdy brak
  // ramek lub gdy backend jeszcze nie sle pola `proc_ms`. Badge kafelka czyta to
  // przez element.__detOverlay.lastProcMs i pokazuje np. "18 ms" / "—".
  get lastProcMs() {
    if (this.frames.length === 0) return null;
    const v = this.frames[this.frames.length - 1].procMs;
    return v == null ? null : v;
  }

  // Wybiera zestaw detekcji dla klatki pokazywanej przez <video> w trybie
  // wall-clock. Estymujemy per-klatka realny czas przechwycenia wyswietlanej
  // klatki (targetCaptureWallMs) i bierzemy NAJNOWSZA ramke o tsMs <= cel +
  // EPS_PRZOD_MS (zakaz ramek z przyszlosci) i wieku <= MAX_WIEK_RAMKI_MS
  // (zakaz przestarzalych) — te same bramki co w trybie PTS. Zwraca null gdy
  // brak playbacku/bufora albo zadna ramka nie pasuje.
  selectFrame() {
    const buf = this.frames;
    if (buf.length === 0) return null;

    const targetWallMs = this.targetCaptureWallMs();
    if (targetWallMs == null) return null;

    const eps = epsPrzodMs();
    for (let i = buf.length - 1; i >= 0; i--) {
      if (buf[i].tsMs > targetWallMs + eps) continue;
      if (targetWallMs - buf[i].tsMs > MAX_WIEK_RAMKI_MS) return null;
      return buf[i];
    }
    return null;
  }

  // =============================================================================
  // Interpolacja media-time per track_id (Faza B)
  // =============================================================================

  // Czas przechwycenia ramki w osi media (ms): (ptsNs - base)/1e6. null gdy ramka
  // nie ma ptsNs LUB wynik jest niefinity/ujemny (pts sprzed bazy = zla baza po
  // resubscribe albo smieciowy pakiet — takiej ramki nie wolno dopasowywac).
  // `base` przekazywany, by nie czytac providera per-ramka.
  frameCaptureMs(frame, base) {
    if (frame.ptsNs == null || base == null) return null;
    const ms = (frame.ptsNs - base) / 1e6;
    if (!Number.isFinite(ms) || ms < 0) return null;
    return ms;
  }

  // Wybiera NAJNOWSZA ramke o media-time <= t + EPS_PRZOD_MS (nigdy z
  // przyszlosci) i wieku <= MAX_WIEK_RAMKI_MS (nigdy przestarzala). Skan od
  // konca bufora (rosnie po tsMs, wiec media-time tez ~rosnie) — pierwsza
  // pasujaca jest najnowsza dopuszczalna; ramki bez media-time pomijamy.
  // Zwraca { frame, wiekMs } albo null gdy zadna nie pasuje.
  wybierzNajlepszaRamke(buf, base, t) {
    const eps = epsPrzodMs();
    for (let i = buf.length - 1; i >= 0; i--) {
      const c = this.frameCaptureMs(buf[i], base);
      if (c == null) continue;
      if (c > t + eps) continue;
      const wiekMs = t - c;
      if (wiekMs > MAX_WIEK_RAMKI_MS) return null;
      return { frame: buf[i], wiekMs };
    }
    return null;
  }

  // Alpha dla ramki o podanym wieku: pelna do WIEK_FADE_OD_MS, potem liniowe
  // wygaszanie do zera przy MAX_WIEK_RAMKI_MS.
  alphaDlaWieku(wiekMs) {
    if (wiekMs <= WIEK_FADE_OD_MS) return 1;
    return Math.max(0, 1 - (wiekMs - WIEK_FADE_OD_MS) / (MAX_WIEK_RAMKI_MS - WIEK_FADE_OD_MS));
  }

  // Opakowuje surowe detekcje ramki w liste renderowa {det, bbox, alpha} bez
  // interpolacji (tryb awaryjny / brak trackingu).
  itemsAsRender(frame, alpha = 1) {
    if (!frame || alpha <= 0) return [];
    const out = [];
    for (const it of frame.items) {
      if (Array.isArray(it?.bbox) && it.bbox.length >= 4) {
        // Uzupelnij stan/OCR z cache, gdy wybrana ramka ma je puste (dot.
        // itemow z track_id; bez track_id wzbogacDet zwraca oryginal).
        out.push({ det: this.wzbogacDet(it), bbox: it.bbox, alpha });
      }
    }
    return out;
  }

  // Buduje liste detekcji do narysowania w BIEZACEJ klatce rAF.
  // Zwraca tablice { det, bbox, alpha }, gdzie:
  //   * `det`   — metadane (klasa/score/stan/tekst) z ramki wybranej dla obrazu,
  //   * `bbox`  — [x,y,w,h] znormalizowane, zinterpolowane/ekstrapolowane na `t`,
  //   * `alpha` — 0..1 (wygaszanie zgubionych trackow).
  //
  // Tryby (degradacja):
  //   1) media-time + tracki  → interpolacja/ekstrapolacja per track_id,
  //   2) media-time bez track  → ramka o ptsMs najblizszym `t` (bez interpolacji),
  //   3) brak PTS/base         → sciezka wall-clock (selectFrame), jak dotad.
  computeRenderList() {
    const base = this.mediaBasePtsNs();
    // Zmiana bazy PTS (resubscribe wideo) oznacza przelaczenie strumienia —
    // ramki sprzed zmiany liczylyby sie wg nowej osi. Czyscimy CALY stan kamery,
    // by stare oznaczenia poprzedniego ujecia nie wisialy nad nowym.
    if (base != null && this.lastBasePtsNs !== base) {
      this.resetStanKamery();
      this.lastBasePtsNs = base;
    }

    // Zmiana samego elementu <video> (reattach kafelka) — takze przelaczenie
    // ujecia. Resetujemy stan nawet gdy strumien nie niesie osi PTS.
    const vNow = this.videoEl();
    if (vNow && this.lastVideoEl && vNow !== this.lastVideoEl) {
      this.resetStanKamery();
    }
    if (vNow) this.lastVideoEl = vNow;

    const buf = this.frames;
    if (buf.length === 0) return [];

    // Tryb PTS aktywny gdy znamy baze i JAKAKOLWIEK ramka bufora ma ptsNs —
    // pojedyncza ramka bez PTS nie przelacza calego overlay na wall-clock
    // (miganie miedzy dwiema osiami czasu). Ramki bez ptsNs sa po prostu
    // pomijane przy dopasowaniu (frameCaptureMs zwraca dla nich null).
    const havePts = base != null && buf.some((f) => f.ptsNs != null);
    if (!havePts) {
      // TRYB AWARYJNY (brak wspolnej osi PTS): wall-clock jak wczesniej.
      return this.itemsAsRender(this.selectFrame());
    }

    // Cel rysowania = pozycja playheadu w osi media (ms). Overlay i wideo
    // startuja od ~0 w tej samej osi, wiec nie potrzeba offsetu sciany.
    const v = this.videoEl();
    const ct = v ? v.currentTime : NaN;
    const t = Number.isFinite(ct) && ct > 0 ? ct * 1000 : null;
    if (t == null) {
      // Brak playbacku (readyState 0 / currentTime 0) — nie rysuj nic;
      // detekcje na zamrozonym/czarnym obrazie nie odpowiadaja klatce.
      return [];
    }

    // Czy w buforze sa realne tracki (track_id > 0)?
    let haveTracks = false;
    for (const f of buf) {
      for (const it of f.items) {
        if ((it?.track_id ?? it?.trackId ?? 0) > 0) { haveTracks = true; break; }
      }
      if (haveTracks) break;
    }

    if (!haveTracks) {
      // Bez trackingu (stary serwer / stub) — NAJNOWSZA ramka nie nowsza niz
      // t + EPS_PRZOD_MS (zakaz rysowania przyszlosci) i nie starsza niz
      // MAX_WIEK_RAMKI_MS (stare oznaczenia znikaja), wygaszana z wiekiem.
      const sel = this.wybierzNajlepszaRamke(buf, base, t);
      if (!sel) return [];
      return this.itemsAsRender(sel.frame, this.alphaDlaWieku(sel.wiekMs));
    }

    return this.interpolujTracki(buf, base, t);
  }

  // Rdzen Fazy B: dla kazdego track_id znajduje ramke A (ostatnia z ptsMs <= t) i
  // B (pierwsza z ptsMs >= t), po czym lerpuje bbox (bracketing) albo ekstrapoluje
  // po vx/vy gdy `t` wyprzedza najswiezsza detekcje. Metadane bierze z ramki
  // wybranej dla obrazu (A, a bez niej B). Zgubione tracki wygasza po TRACK_FADE_MS.
  interpolujTracki(buf, base, t) {
    // Progi czytane raz na klatke (tunowalne przez localStorage).
    const maxE = maxExtrapMs();
    const fadeMs = trackFadeMs();
    const eps = epsPrzodMs();
    const extrapKrawedz = extrapKrawedzMs();
    // track_id -> { A:{it,ms}, Aprev:{it,ms}, B:{it,ms} }
    const tracks = new Map();
    for (const f of buf) {
      const c = this.frameCaptureMs(f, base);
      if (c == null) continue;
      for (const it of f.items) {
        const id = it?.track_id ?? it?.trackId ?? 0;
        if (id <= 0) continue;
        if (!Array.isArray(it.bbox) || it.bbox.length < 4) continue;
        let e = tracks.get(id);
        if (!e) { e = { A: null, Aprev: null, B: null }; tracks.set(id, e); }
        // Bracketing po WARTOSCI ptsMs (nie po kolejnosci iteracji — bufor jest
        // sortowany po tsMs, a PTS moga przyjsc out-of-order):
        //   A = ramka o maksymalnym ms <= t, Aprev = druga najnowsza <= t
        //   (do estymacji predkosci na krawedzi), B = o minimalnym ms >= t.
        if (c <= t) {
          if (e.A == null || c > e.A.ms) { e.Aprev = e.A; e.A = { it, ms: c }; }
          else if (c < e.A.ms && (e.Aprev == null || c > e.Aprev.ms)) e.Aprev = { it, ms: c };
        }
        if (c >= t && (e.B == null || c < e.B.ms)) e.B = { it, ms: c };
      }
    }

    const out = [];
    for (const e of tracks.values()) {
      // Metadane z ramki wybranej dla obrazu (A, a bez niej B) — nie z
      // najswiezszej, ktora moze wyprzedzac wyswietlana klatke (etykieta
      // pokazywalaby stan/OCR zanim obraz go dogoni). Pusty stan/OCR (faza-1)
      // uzupelniamy z cache ostatnim znanym niepustym stanem/tekstem.
      const det = this.wzbogacDet((e.A ?? e.B).it);
      let bbox;
      let alpha = 1;
      // Lerp tylko przez ciagly odcinek detekcji (przerwa <= MAX_PRZERWA_LERP_MS)
      // — przez dropout nie ciagniemy boxa, tylko trzymamy ostatnia realna
      // pozycje A (galaz nizej).
      const spanAB = e.A && e.B ? e.B.ms - e.A.ms : Infinity;
      if (e.A && e.B && spanAB <= MAX_PRZERWA_LERP_MS) {
        bbox = spanAB > 0
          ? lerpBbox(e.A.it.bbox, e.B.it.bbox, (t - e.A.ms) / spanAB)
          : e.A.it.bbox.slice();
      } else if (e.A) {
        // `t` za najswiezsza detekcja tracku (brak ramki B — swieza krawedz).
        // MINIMALNA ekstrapolacja: przesuwamy bbox po predkosci z dwoch ostatnich
        // realnych detekcji, twardo klampowanej do okna extrapKrawedz — to tylko
        // wypelnienie luki miedzy ostatnia obrobiona klatka a biezaca klatka
        // wideo, NIE globalna predykcja. Bez tego boxy ciagna sie ZA ruchomym
        // obiektem do 1/analysis_fps. Przy dt > okna trzymamy ostatnia realna
        // pozycje (zero driftu). Gdy detekcja tracku milczy dluzej niz okno
        // fade — wygaszamy go, by nie wisial "duch" po obiekcie z kadru.
        const overshoot = t - e.A.ms;
        bbox = e.A.it.bbox.slice();
        const spanPrev = e.Aprev ? e.A.ms - e.Aprev.ms : 0;
        if (overshoot <= extrapKrawedz && spanPrev > 0 && spanPrev <= MAX_PRZERWA_LERP_MS) {
          const vx = (e.A.it.bbox[0] - e.Aprev.it.bbox[0]) / spanPrev;
          const vy = (e.A.it.bbox[1] - e.Aprev.it.bbox[1]) / spanPrev;
          bbox[0] += vx * overshoot;
          bbox[1] += vy * overshoot;
        }
        if (overshoot > maxE) {
          alpha = 1 - (overshoot - maxE) / (fadeMs - maxE);
        }
      } else if (e.B) {
        // Track istnieje WYLACZNIE w przyszlosci wzgledem wyswietlanej klatki
        // (wideo buforuje za live). Rysowanie go wyprzedzaloby obraz — boxy
        // pojawialyby sie zanim obiekt wjedzie w kadr. Dopuszczamy jedynie
        // mala epsilon (kwantyzacja klatek), reszte pomijamy.
        if (e.B.ms - t > eps) continue;
        bbox = e.B.it.bbox.slice();
      } else {
        continue;
      }
      if (alpha <= 0) continue; // track zgubiony — nie rysuj
      out.push({ det, bbox, alpha });
    }

    // Tryb mieszany: detekcje bez track_id (id<=0) nie sa interpolowane, ale
    // musza byc widoczne. Dorysowujemy je jako statyczne boxy z ramki
    // DOPASOWANEJ do czasu wideo (nigdy z przyszlosci, wygaszane z wiekiem)
    // — nie z najswiezszej, ktora wyprzedza wyswietlana klatke.
    const sel = this.wybierzNajlepszaRamke(buf, base, t);
    if (sel) {
      const alphaRamki = this.alphaDlaWieku(sel.wiekMs);
      if (alphaRamki > 0) {
        for (const it of sel.frame.items) {
          const id = it?.track_id ?? it?.trackId ?? 0;
          if (id > 0) continue;
          if (!Array.isArray(it.bbox) || it.bbox.length < 4) continue;
          out.push({ det: it, bbox: it.bbox, alpha: alphaRamki });
        }
      }
    }
    return out;
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
    // Host mogl sie przesunac bez zmiany rozmiaru (np. przemeblowanie layoutu)
    // — ResizeObserver tego nie widzi, wiec pozycje sprawdzamy per klatke.
    const hrect = this.hostEl.getBoundingClientRect();
    if (hrect.left !== this.lastHostLeft || hrect.top !== this.lastHostTop) {
      this.resizeCanvas();
    }

    const ctx = this.ctx;
    ctx.clearRect(0, 0, this.canvas.width, this.canvas.height);

    // Debug HUD (do tuningu synchronizacji) — rysowany zawsze gdy wlaczony,
    // PRZED bramkami wygaszania, zeby diagnozowac takze stan "nic nie rysuje"
    // (pusty bufor, cisza backendu, brak dopasowanej ramki).
    this.drawDebugHud();

    // Wygaszanie: po WYGASZ_PO_MS bez nowej wiadomosci nie rysujemy nic.
    if (this.frames.length === 0) return;
    if (this.lastMessageAt && performance.now() - this.lastMessageAt > WYGASZ_PO_MS) {
      return;
    }

    // Lista {det, bbox, alpha} juz zinterpolowana/wybrana pod biezacy media-time.
    const lista = this.computeRenderList();

    if (!lista.length) return;

    const area = this.videoContentRect();
    const dpr = window.devicePixelRatio || 1;
    // Ramka: grubsza dla czytelnosci na obrazie (skalowana z DPR).
    const lineW = Math.max(2, Math.round(2.5 * dpr));

    // Rysuj WYLACZNIE kolorowe ramki detekcji (bez etykiet/pigulek). Lekki cien
    // poprawia czytelnosc cienkiej ramki na jasnym/ruchomym tle wideo.
    ctx.save();
    ctx.lineJoin = 'round';
    ctx.shadowColor = 'rgba(0, 0, 0, 0.55)';
    ctx.shadowBlur = Math.round(2 * dpr);
    for (const r of lista) {
      const det = r.det;
      const bbox = r.bbox;
      if (!Array.isArray(bbox) || bbox.length < 4) continue;
      const [nx, ny, nw, nh] = bbox;
      // Znormalizowane 0..1 -> piksele realnego obszaru wideo (area).
      const x = area.x + nx * area.width;
      const y = area.y + ny * area.height;
      const w = nw * area.width;
      const h = nh * area.height;

      const alpha = r.alpha == null ? 1 : r.alpha;
      const kolor = jestUszkodzona(det) ? KOLOR_USZKODZONA : kolorDlaKlasy(det.klasa);
      ctx.lineWidth = lineW;
      ctx.strokeStyle = kolor;
      ctx.globalAlpha = alpha;
      ctx.strokeRect(x, y, w, h);
    }
    ctx.globalAlpha = 1;
    ctx.restore();

    // Dwuliniowy pasek podsumowania u dolu obszaru wideo (nakladka na wideo).
    this.rysujPasek(ctx, lista, area, dpr);
  }

  // Rysuje na canvasie dwuliniowy pasek u dolu obszaru wideo o STALYM ukladzie
  // (tekst stoi w miejscu — zmienia sie tylko wartosc, nie pozycja):
  //   Linia 1: dwie stale kolumny "Rejestracja: <nr>" (lewa polowa) oraz
  //            "ADR: <nr> (<opis wg UN>)" (prawa polowa). Wartosci startuja w
  //            stalym offsetcie X (za etykieta), wiec pojawienie/zniknniecie
  //            wartosci nie przesuwa tekstu.
  //   Linia 2: DOKLADNIE 3 rowne kolumny (sloty 0,1,2) na nalepki/znaki, kazdy
  //            "<klasa> (<stan>)" albo "—". Kazda klasa najwyzej RAZ (dedup).
  //            Obecne klasy PAKUJEMY OD LEWEJ (bez dziur z lewej) w stabilnej
  //            kolejnosci wg `seq` pierwszego pojawienia (mapa klasaSeq z TTL —
  //            nowa nalepka dochodzi na koniec, istniejace nie przeskakuja).
  // Rysuje na canvasie (overlay jest czysto canvasowy — brak osobnego DOM
  // overlay), wiec pasek jest spojny z rysowaniem boxow.
  rysujPasek(ctx, lista, area, dpr) {
    let rej = null;
    let adr = null;
    // Unikalne klasy nalepek/znakow w kadrze: klasa -> { label, score }.
    // Dedup po samej KLASIE — wiele instancji tej samej klasy zwija sie do
    // jednej (wybieramy wpis o najwyzszym score).
    const uniq = new Map();
    for (const r of lista) {
      const det = r.det;
      const klasa = det?.klasa;
      if (!klasa) continue;
      if (klasa === 'tablica_rejestracyjna') {
        // Zwyciezca glosowania OCR (najspojniejszy odczyt per track_id).
        const t = this.detDoEtykiety(det)?.tekst;
        if (t != null && String(t).length > 0 && rej == null) rej = String(t);
      } else if (klasa === 'tablica_adr') {
        // Backend OCR-uje ADR i przysyla "<kemler>/<un> <opis>" (opis z listy
        // po stronie serwera). Pole bywa puste, gdy odczyt sie nie dopasowal.
        if (det?.tekst != null && String(det.tekst).length > 0 && adr == null) {
          adr = String(det.tekst);
        }
      } else if (klasa.startsWith('nalepka') || klasa === 'znak_srodowiskowy' || klasa === 'termometr') {
        const score = det?.score ?? det?.confidence ?? 0;
        const prev = uniq.get(klasa);
        if (prev && score <= prev.score) continue;
        const stan = Array.isArray(det?.stan) && det.stan.length > 0 ? det.stan.join(', ') : null;
        uniq.set(klasa, { label: stan ? `${klasa} (${stan})` : klasa, score });
      }
    }

    // Stabilna kolejnosc bez stalego slotu: kazdej klasie przy PIERWSZYM
    // pojawieniu nadajemy rosnacy `seq`; TTL usuwa wpisy klas nieobecnych dluzej
    // niz TRACK_META_TTL_MS (przy ponownym pojawieniu dostana nowy, wyzszy seq).
    const now = performance.now();
    for (const [k, e] of this.klasaSeq) {
      if (now - e.at > TRACK_META_TTL_MS) this.klasaSeq.delete(k);
    }
    for (const k of uniq.keys()) {
      let e = this.klasaSeq.get(k);
      if (!e) { e = { seq: this.klasaSeqLicznik++, at: now }; this.klasaSeq.set(k, e); }
      else e.at = now;
    }
    // Posortuj OBECNE klasy po `seq` (rosnaco) — stabilna kolejnosc bez przeskokow.
    const obecne = [...uniq.keys()].sort(
      (a, b) => this.klasaSeq.get(a).seq - this.klasaSeq.get(b).seq,
    );
    // Pakuj OD LEWEJ do slotow 0,1,2 (bez dziur); brakujace sloty z PRAWEJ = "—".
    const sloty = ['—', '—', '—'];
    for (let s = 0; s < 3 && s < obecne.length; s++) {
      sloty[s] = uniq.get(obecne[s]).label;
    }

    // Wartosc ADR: numer + opis rozdzielone z tekstu przyslanego przez serwer
    // ("<kemler>/<un> <opis>"). Opis pokazujemy w nawiasie za numerem; brak opisu
    // -> sam numer. Zadne dane ADR nie sa trzymane po stronie frontu.
    let adrVal = '—';
    if (adr != null) {
      const { kod, opis } = rozdzielAdr(adr);
      adrVal = opis ? `${kod} (${opis})` : kod;
    }

    const fontPx = Math.round(13 * dpr);
    const padX = Math.round(12 * dpr);
    const padY = Math.round(8 * dpr);
    const lineH = Math.round(fontPx * 1.35);
    // Pasek ma ZAWSZE 2 linie (linia 2 to stale 3 sloty) — stala wysokosc.
    const barH = padY * 2 + 2 * lineH;
    const barX = area.x;
    const barW = area.width;
    const barY = area.y + area.height - barH;

    ctx.save();
    ctx.font = `600 ${fontPx}px system-ui, -apple-system, "Segoe UI", Roboto, sans-serif`;
    ctx.textBaseline = 'top';
    // Polprzezroczyste ciemne tlo dla czytelnosci bialego tekstu na wideo.
    ctx.fillStyle = 'rgba(0, 0, 0, 0.6)';
    ctx.fillRect(barX, barY, barW, barH);
    ctx.fillStyle = '#ffffff';

    // --- Linia 1: dwie stale kolumny (Rejestracja | ADR) ---
    // Wartosci startuja w stalym offsetcie liczonym z szerokosci STALEJ etykiety
    // (spacja gwarantuje odstep), wiec nie przesuwaja sie przy zmianie tresci.
    const tyLinia1 = barY + padY;
    const kolRejX = barX + padX;
    const etRej = 'Rejestracja: ';
    ctx.fillText('Rejestracja:', kolRejX, tyLinia1);
    ctx.fillText(rej ?? '—', kolRejX + ctx.measureText(etRej).width, tyLinia1);

    const kolAdrX = barX + Math.round(barW * 0.5);
    const etAdr = 'ADR: ';
    ctx.fillText('ADR:', kolAdrX, tyLinia1);
    ctx.fillText(adrVal, kolAdrX + ctx.measureText(etAdr).width, tyLinia1);

    // --- Linia 2: 3 stale sloty o rownej szerokosci ---
    const tyLinia2 = tyLinia1 + lineH;
    const slotW = barW / 3;
    for (let s = 0; s < 3; s++) {
      ctx.fillText(sloty[s], barX + s * slotW + padX, tyLinia2);
    }
    ctx.restore();
  }

  // Rysuje diagnostyczny HUD w rogu canvasu (male mono) gdy wlaczony flaga
  // localStorage['tv_overlay_debug']==='1'. Pokazuje pelny tor czasu media-time:
  // target (currentTime), media-time wybranej ramki, delte, rozmiar bufora oraz
  // ostrzezenie "BASE?!" gdy najblizsza ramka odstaje od targetu o >1 s (sygnal
  // zlej bazy PTS). Dodatkowo czasy wall-clock do tarowania SYNC_OFFSET_MS.
  drawDebugHud() {
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

    // Tor media-time: target, wybrana ramka (ta sama logika co render), delta,
    // odchylka NAJBLIZSZEJ ramki (dowolnej — takze z przyszlosci) od targetu.
    const base = this.mediaBasePtsNs();
    const t = ct > 0 ? ct * 1000 : null;
    let selMs = null;
    let selWiek = null;
    let nearDiff = null;
    if (base != null && t != null) {
      const sel = this.wybierzNajlepszaRamke(this.frames, base, t);
      if (sel) {
        selMs = this.frameCaptureMs(sel.frame, base);
        selWiek = sel.wiekMs;
      }
      for (const f of this.frames) {
        const c = this.frameCaptureMs(f, base);
        if (c == null) continue;
        const d = Math.abs(c - t);
        if (nearDiff == null || d < nearDiff) nearDiff = d;
      }
    }

    const fmt = (x, unit = '') => (x == null ? '—' : `${Math.round(x)}${unit}`);
    const linie = [
      `ct=${ct.toFixed(3)}s base=${base == null ? '—' : 'ok'}`,
      `t=${fmt(t, 'ms')} sel=${fmt(selMs, 'ms')}`,
      `Δsel=${fmt(selWiek, 'ms')} near=${fmt(nearDiff, 'ms')}`,
      `buf=${this.frames.length}/${BUFOR_MAX_RAMEK}`,
      `bufEnd=${bufEnd == null ? '—' : bufEnd.toFixed(3) + 's'} lat=${fmt(videoLatMs, 'ms')}`,
      `detAge=${fmt(detAge, 'ms')} off=${fmt(syncOffsetMs(), 'ms')}`,
    ];
    if (nearDiff != null && nearDiff > 1000) {
      linie.push(`BASE?! near>1s`);
    }

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

  // =============================================================================
  // Sprzatanie
  // =============================================================================

  destroy() {
    if (this.disposed) return;
    this.disposed = true;
    this.pending.disposed = true;
    if (this.rafId != null) cancelAnimationFrame(this.rafId);
    this.rafId = null;
    if (this.resubscribeTimer != null) {
      clearTimeout(this.resubscribeTimer);
      this.resubscribeTimer = null;
    }
    if (this.lifecycleUnsub) {
      this.lifecycleUnsub();
      this.lifecycleUnsub = null;
    }
    if (this.resizeObserver) this.resizeObserver.disconnect();
    else window.removeEventListener('resize', this.boundResize);
    if (this.unsub) {
      try { this.unsub(); } catch { /* ignore */ }
      this.unsub = null;
    }
    this.frames.length = 0;
    this.trackMeta.clear();
    this.klasaSeq.clear();
    this.ocrGlosy.clear();
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

