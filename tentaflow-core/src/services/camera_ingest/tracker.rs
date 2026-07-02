// =============================================================================
// Plik: services/camera_ingest/tracker.rs
// Opis: Lekki tracker IOU (bez Kalmana) nadajacy detekcjom stabilne `track_id`
//       oraz prędkość srodka boxa (vx, vy). Stan trzymany per kamera.
// Przyklad:
//   tracker::update("cam_x", &mut dets, Some(pts_ns));
//   // po wywolaniu kazda detekcja ma track_id oraz vx/vy (jesli byla baza czasu)
// =============================================================================
//
// Dopasowanie greedy po macierzy IOU (detekcje × tracki), malejaco. Asocjacja
// jest PREDYKCYJNA (styl SORT, bez pelnego Kalmana): przed liczeniem IOU kazdy
// track jest ekstrapolowany po swojej prędkości (vx, vy) o czas dt do biezacej
// klatki, a detekcje dopasowywane sa do POZYCJI PRZEWIDZIANEJ, nie ostatniej
// znanej. Dzieki temu szybko jadacy obiekt (cysterna przez kadr), ktory miedzy
// klatkami przeskakuje >30% kadru, nadal pokrywa sie ze swoja predykcja i trzyma
// ten sam track_id. Gdy IOU jest niskie, ale srodek detekcji lezy blisko srodka
// przewidzianego, para i tak jest kandydatem (kryterium odleglosci). Prędkość
// liczona ze srodkow boxow i delty PTS (media-timeline, ns) od REALNEJ ostatniej
// pozycji tracku (nie od predykcji), z lekkim wygladzeniem EMA; brak PTS =>
// prędkość pomijana (0), ale identyfikatory dalej sa nadawane. Boxy w formacie
// [x, y, w, h] znormalizowanym 0..1 (identycznie jak `Detection.bbox`).

#![cfg(feature = "inference-vision-gpu")]

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use crate::services::detection_bus::Detection;

/// Prog IOU (wzgledem PRZEWIDZIANEGO boxa tracku) uznania pary za silne
/// dopasowanie. Obnizony pod szybki ruch — przy duzej prędkości predykcja nie jest
/// idealna i pokrycie bywa niepelne. Pary ponizej progu, ale z bliskim srodkiem,
/// nadal sa kandydatami (kryterium odleglosci srodkow, patrz `MAX_CENTER_JUMP`).
const IOU_THRESHOLD: f32 = 0.2;

/// Liczba kolejnych klatek bez dopasowania, po ktorej track jest usuwany. Szybki
/// obiekt po wyjezdzie z kadru ma szybko znikac (inaczej ekstrapolacja klienta
/// rysuje „duchy"), dlatego krotki bufor 3 klatek (~0.3 s @10fps).
const MAX_MISSES: u32 = 3;

/// Maksymalna dopuszczalna odleglosc srodka detekcji od srodka PRZEWIDZIANEGO boxa
/// tracku dla uznania pary za kandydata. Poniewaz predykcja zniosla juz wiekszosc
/// skoku szybkiego obiektu, residuum jest male; prog trzymamy lekko podniesiony,
/// by pierwsze dopasowanie (gdy prędkość jeszcze nieznana, predykcja = ostatnia
/// pozycja) tez zdazylo zlapac obiekt i zbootstrapowac prędkość. Bramka chroni
/// przed przeskokiem id przy krzyzowaniu sie obiektow tej samej klasy.
const MAX_CENTER_JUMP: f32 = 0.35;

/// Dolny prog dt (s) przy liczeniu prędkości. Ponizej — zdublowane lub bardzo
/// bliskie PTS daja mikroskopijne dt i eksplozje vx/vy; w takim przypadku
/// zostawiamy poprzednia prędkość tracku zamiast liczyc ja od nowa.
const MIN_DT_S: f32 = 0.02;

/// Waga nowego pomiaru prędkości przy wygladzaniu EMA (`vx = W*vx_new + (1-W)*vx`).
/// Tlumi szum detekcji, by ekstrapolacja klienta nie skakala, zachowujac przy tym
/// szybka reakcje na realna zmiane prędkości.
const VEL_EMA_WEIGHT: f32 = 0.6;

/// Pojedynczy sledzony obiekt.
struct Track {
    id: u32,
    /// Klasa obiektu (np. „tablica", „nalepka"). Bramka dopasowania: detekcja
    /// moze trafic tylko do tracku tej samej klasy, by track_id nie przeskakiwal
    /// miedzy roznymi klasami na tym samym pojezdzie.
    klasa: String,
    /// Ostatni box [x, y, w, h] znormalizowany.
    bbox: [f32; 4],
    /// PTS ostatniej obserwacji (ns) — baza do liczenia dt.
    last_pts_ns: Option<u64>,
    /// Pozycja tracku (box) w chwili `last_pts_ns` — REFERENCJA do liczenia
    /// prędkości. Trzymana osobno od `bbox`, bo `bbox` aktualizujemy każdą klatką
    /// (także bez PTS lub z nierosnącym PTS), a prędkość musi liczyć się względem
    /// pozycji o znanym, spójnym czasie — inaczej dt i przesunięcie pochodziłyby z
    /// różnych klatek i psuly predykcje szybkich obiektow.
    vel_ref_bbox: [f32; 4],
    vx: f32,
    vy: f32,
    /// Czy prędkość byla juz choc raz zmierzona. Pierwszy pomiar ustawiamy wprost,
    /// kolejne wygladzamy EMA — inaczej start od zera „zjadalby" pierwsza prędkość.
    has_vel: bool,
    /// Liczba kolejnych klatek bez dopasowania.
    misses: u32,
}

/// Stan trackera jednej kamery.
struct CameraTracker {
    next_id: u32,
    tracks: Vec<Track>,
}

impl CameraTracker {
    fn new() -> Self {
        Self {
            // Zaczynamy od 1 — 0 rezerwujemy jako "brak przypisania" w `Detection`.
            next_id: 1,
            tracks: Vec::new(),
        }
    }
}

/// Rejestr trackerow per camera_id. Proces-wide singleton (wzor jak `cold_state`).
fn tracker_state() -> &'static Mutex<HashMap<String, CameraTracker>> {
    static S: OnceLock<Mutex<HashMap<String, CameraTracker>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Srodek boxa [x, y, w, h] → (cx, cy).
#[inline]
fn center(bbox: &[f32; 4]) -> (f32, f32) {
    (bbox[0] + bbox[2] * 0.5, bbox[1] + bbox[3] * 0.5)
}

/// Przewiduje box tracku po czasie `dt` (s), przesuwajac srodek o `vx*dt, vy*dt`
/// i zachowujac rozmiar. Srodek jest miekko przyciety do [0, 1], by ekstrapolacja
/// obiektu wyjezdzajacego z kadru nie „uciekala" w nieskonczonosc. Przy `dt == 0`
/// (brak bazy czasu) zwraca box bez zmian — asocjacja dziala wtedy jak zwykle IOU.
#[inline]
fn predict_bbox(bbox: &[f32; 4], vx: f32, vy: f32, dt: f32) -> [f32; 4] {
    let (cx, cy) = center(bbox);
    let ncx = (cx + vx * dt).clamp(0.0, 1.0);
    let ncy = (cy + vy * dt).clamp(0.0, 1.0);
    [ncx - bbox[2] * 0.5, ncy - bbox[3] * 0.5, bbox[2], bbox[3]]
}

/// IOU dwoch boxow w formacie [x, y, w, h]. Zwraca 0 dla zdegenerowanych boxow.
fn iou(a: &[f32; 4], b: &[f32; 4]) -> f32 {
    let (ax0, ay0, aw, ah) = (a[0], a[1], a[2], a[3]);
    let (bx0, by0, bw, bh) = (b[0], b[1], b[2], b[3]);
    if aw <= 0.0 || ah <= 0.0 || bw <= 0.0 || bh <= 0.0 {
        return 0.0;
    }
    let ax1 = ax0 + aw;
    let ay1 = ay0 + ah;
    let bx1 = bx0 + bw;
    let by1 = by0 + bh;

    let ix0 = ax0.max(bx0);
    let iy0 = ay0.max(by0);
    let ix1 = ax1.min(bx1);
    let iy1 = ay1.min(by1);

    let iw = (ix1 - ix0).max(0.0);
    let ih = (iy1 - iy0).max(0.0);
    let inter = iw * ih;
    if inter <= 0.0 {
        return 0.0;
    }
    let union = aw * ah + bw * bh - inter;
    if union <= 0.0 {
        return 0.0;
    }
    inter / union
}

/// Aktualizuje tracker kamery detekcjami biezacej klatki. Po wywolaniu kazda
/// detekcja w `dets` ma nadany `track_id`, a jesli dostepna byla baza czasu
/// (delta PTS), rowniez `vx`/`vy` (jednostki znormalizowane/s). Detekcje bez
/// dopasowania dostaja nowy identyfikator; tracki bez dopasowania rosna licznik
/// `misses` i sa usuwane po `MAX_MISSES`.
pub fn update(camera_id: &str, dets: &mut [Detection], pts_ns: Option<u64>) {
    let mut state = tracker_state().lock().unwrap_or_else(|e| e.into_inner());
    let cam = state
        .entry(camera_id.to_string())
        .or_insert_with(CameraTracker::new);

    let n_det = dets.len();
    let n_trk = cam.tracks.len();

    // Ktora detekcja/track juz dopasowana.
    let mut det_matched = vec![false; n_det];
    let mut trk_matched = vec![false; n_trk];
    // Mapowanie detekcja → indeks tracku (po greedy).
    let mut det_to_trk: Vec<Option<usize>> = vec![None; n_det];

    // Przewidziany box kazdego tracku na biezaca klatke (predykcja po prędkości).
    // Asocjacja liczona wzgledem TYCH boxow, nie ostatnich znanych — szybki obiekt,
    // ktory „uciekl", nadal pokrywa sie ze swoja predykcja.
    let pred_boxes: Vec<[f32; 4]> = cam
        .tracks
        .iter()
        .map(|trk| {
            let dt = match (trk.last_pts_ns, pts_ns) {
                (Some(prev), Some(now)) if now > prev => (now - prev) as f32 / 1_000_000_000.0,
                _ => 0.0,
            };
            predict_bbox(&trk.bbox, trk.vx, trk.vy, dt)
        })
        .collect();

    // Zbuduj kandydatow (det, trk, score) i sortuj malejaco po score. Pomijamy pary
    // o roznej klasie (bramka klasy) oraz pary, w ktorych srodek detekcji lezy dalej
    // niz `MAX_CENTER_JUMP` od srodka PRZEWIDZIANEGO boxa (bramka ruchu) — chroni
    // przed przeskokiem id. Silne dopasowanie (IOU >= prog) dostaje score = IOU;
    // slabe IOU, ale bliski srodek, dostaje score proporcjonalny do bliskosci,
    // zawsze ponizej progu IOU — dzieki temu realne pokrycia maja pierwszenstwo,
    // a asocjacja po samej odleglosci sluzy jako uzupelnienie przy szybkim ruchu.
    let mut pairs: Vec<(usize, usize, f32)> = Vec::with_capacity(n_det.max(n_trk));
    for (di, det) in dets.iter().enumerate() {
        for (ti, trk) in cam.tracks.iter().enumerate() {
            if det.klasa != trk.klasa {
                continue;
            }
            let pred = &pred_boxes[ti];
            let (dcx, dcy) = center(&det.bbox);
            let (pcx, pcy) = center(pred);
            let dist = ((dcx - pcx).powi(2) + (dcy - pcy).powi(2)).sqrt();
            if dist > MAX_CENTER_JUMP {
                continue;
            }
            let score_iou = iou(&det.bbox, pred);
            let score = if score_iou >= IOU_THRESHOLD {
                score_iou
            } else {
                IOU_THRESHOLD * (1.0 - dist / MAX_CENTER_JUMP)
            };
            pairs.push((di, ti, score));
        }
    }
    pairs.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));

    // Greedy: bierz najlepsze pary, pomijajac juz zajete detekcje/tracki.
    for (di, ti, _score) in pairs {
        if det_matched[di] || trk_matched[ti] {
            continue;
        }
        det_matched[di] = true;
        trk_matched[ti] = true;
        det_to_trk[di] = Some(ti);
    }

    // Dopasowane: policz prędkość, zaktualizuj box + PTS, zeruj misses.
    for (di, det) in dets.iter_mut().enumerate() {
        match det_to_trk[di] {
            Some(ti) => {
                let trk = &mut cam.tracks[ti];
                let (cx, cy) = center(&det.bbox);
                // Prędkość liczona względem pozycji o OSTATNIM znanym PTS
                // (`vel_ref_bbox`), spójnej z `last_pts_ns` — nie względem `bbox`,
                // który mógł zostać nadpisany klatką bez PTS lub z nierosnącym PTS.
                let (pcx, pcy) = center(&trk.vel_ref_bbox);
                // dt z PTS (ns → s). Brak bazy czasu lub nierosnace PTS => pomijamy
                // prędkość (zostaje poprzednia z tracku), ale przypisanie trwa.
                if let (Some(prev), Some(now)) = (trk.last_pts_ns, pts_ns) {
                    if now > prev {
                        let dt = (now - prev) as f32 / 1_000_000_000.0;
                        if dt >= MIN_DT_S {
                            let vx_new = (cx - pcx) / dt;
                            let vy_new = (cy - pcy) / dt;
                            if trk.has_vel {
                                // Wygladzenie EMA tlumiace szum detekcji.
                                trk.vx = VEL_EMA_WEIGHT * vx_new + (1.0 - VEL_EMA_WEIGHT) * trk.vx;
                                trk.vy = VEL_EMA_WEIGHT * vy_new + (1.0 - VEL_EMA_WEIGHT) * trk.vy;
                            } else {
                                // Pierwszy pomiar — ustawiamy wprost (brak historii).
                                trk.vx = vx_new;
                                trk.vy = vy_new;
                                trk.has_vel = true;
                            }
                        }
                    }
                }
                trk.klasa = det.klasa.clone();
                // `bbox` (nowa pozycja z detekcji) aktualizujemy zawsze — sluzy do
                // predykcji asocjacji nastepnej klatki.
                trk.bbox = det.bbox;
                // `last_pts_ns` i sprzezony z nim `vel_ref_bbox` przesuwamy TYLKO gdy
                // nowy PTS istnieje i jest monotoniczny (>= poprzedniego). Przy
                // braku/nierosnacym PTS referencja predkosci zostaje bez zmian, spojna
                // czasowo z zapamietana pozycja uzyta do liczenia predkosci.
                if let Some(now) = pts_ns {
                    let monotonic = trk.last_pts_ns.map_or(true, |prev| now >= prev);
                    if monotonic {
                        trk.last_pts_ns = Some(now);
                        trk.vel_ref_bbox = det.bbox;
                    }
                }
                trk.misses = 0;

                det.track_id = trk.id;
                det.vx = trk.vx;
                det.vy = trk.vy;
            }
            None => {
                // Niedopasowana detekcja → nowy track.
                let id = cam.next_id;
                cam.next_id = cam.next_id.wrapping_add(1);
                cam.tracks.push(Track {
                    id,
                    klasa: det.klasa.clone(),
                    bbox: det.bbox,
                    last_pts_ns: pts_ns,
                    vel_ref_bbox: det.bbox,
                    vx: 0.0,
                    vy: 0.0,
                    has_vel: false,
                    misses: 0,
                });
                det.track_id = id;
                det.vx = 0.0;
                det.vy = 0.0;
            }
        }
    }

    // Niedopasowane tracki (tylko te istniejace na wejsciu; nowo dodane sa swieze).
    for ti in 0..n_trk {
        if !trk_matched[ti] {
            cam.tracks[ti].misses += 1;
        }
    }
    cam.tracks.retain(|t| t.misses <= MAX_MISSES);

    // Wpisu kamery NIE usuwamy przy pustym `tracks`. Reset licznika `next_id` (start
    // od 1) przy odtworzeniu wpisu powodowal REUZYCIE track_id po pustym kadrze miedzy
    // pojazdami (czeste na bramie ADR). Cache wzbogacania klucza po (camera_id,
    // track_id) przypisywalby wtedy NOWEMU obiektowi stan/rejestracje POPRZEDNIEGO.
    // Zostawiamy wpis `CameraTracker` (z zachowanym `next_id`) — `track_id` rosnie
    // monotonicznie przez cala sesje kamery. Pusty wpis to jeden maly rekord per
    // kamera (ograniczony liczba kamer), usuwany dopiero przy realnym teardownie:
    // `remove(camera_id)` oraz `clear()`.
}

/// Usuwa stan trackera pojedynczej kamery. Wolane przy usuwaniu kamery, by nie
/// zostawiac martwego stanu (tracki, licznik id) w procesowym rejestrze.
pub fn remove(camera_id: &str) {
    let mut state = tracker_state().lock().unwrap_or_else(|e| e.into_inner());
    state.remove(camera_id);
}

/// Czysci stan trackera wszystkich kamer. Wolane przy globalnym drainie warstwy
/// analizy, gdy odpinane sa wszystkie kamery naraz.
pub fn clear() {
    let mut state = tracker_state().lock().unwrap_or_else(|e| e.into_inner());
    state.clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn det(bbox: [f32; 4]) -> Detection {
        Detection {
            klasa: "obiekt".into(),
            bbox,
            score: 0.9,
            stan: Vec::new(),
            tekst: None,
            track_id: 0,
            vx: 0.,
            vy: 0.,
        }
    }

    #[test]
    fn iou_pelne_pokrycie_daje_jeden() {
        let a = [0.0, 0.0, 0.2, 0.2];
        assert!((iou(&a, &a) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn iou_rozlaczne_daje_zero() {
        let a = [0.0, 0.0, 0.1, 0.1];
        let b = [0.5, 0.5, 0.1, 0.1];
        assert_eq!(iou(&a, &b), 0.0);
    }

    #[test]
    fn stabilny_id_miedzy_klatkami() {
        let cam = "trk-test-stable";
        let mut f1 = vec![det([0.10, 0.10, 0.20, 0.20])];
        update(cam, &mut f1, Some(0));
        let id1 = f1[0].track_id;
        assert!(id1 > 0);

        // Nieznaczny ruch — nadal wysokie IOU, ten sam id.
        let mut f2 = vec![det([0.11, 0.10, 0.20, 0.20])];
        update(cam, &mut f2, Some(1_000_000_000));
        assert_eq!(f2[0].track_id, id1);
    }

    #[test]
    fn nowy_obiekt_dostaje_nowy_id() {
        let cam = "trk-test-new";
        let mut f1 = vec![det([0.10, 0.10, 0.10, 0.10])];
        update(cam, &mut f1, Some(0));
        let id1 = f1[0].track_id;

        // Zupelnie inne polozenie — brak dopasowania, nowy id.
        let mut f2 = vec![det([0.70, 0.70, 0.10, 0.10])];
        update(cam, &mut f2, Some(1_000_000_000));
        assert_ne!(f2[0].track_id, id1);
    }

    #[test]
    fn predkosc_z_delty_pts() {
        let cam = "trk-test-vel";
        let mut f1 = vec![det([0.10, 0.20, 0.20, 0.20])];
        update(cam, &mut f1, Some(0));
        // Srodek startowy: (0.20, 0.30).

        // Po 1 s box przesuniety o +0.10 w x → cx 0.30, vx ≈ 0.10/s.
        let mut f2 = vec![det([0.20, 0.20, 0.20, 0.20])];
        update(cam, &mut f2, Some(1_000_000_000));
        assert!((f2[0].vx - 0.10).abs() < 1e-3, "vx={}", f2[0].vx);
        assert!(f2[0].vy.abs() < 1e-3, "vy={}", f2[0].vy);
    }

    #[test]
    fn brak_pts_bez_predkosci() {
        let cam = "trk-test-nopts";
        let mut f1 = vec![det([0.10, 0.20, 0.20, 0.20])];
        update(cam, &mut f1, None);
        let mut f2 = vec![det([0.20, 0.20, 0.20, 0.20])];
        update(cam, &mut f2, None);
        // Bez bazy czasu prędkość zostaje zerowa, ale id sie zachowuje.
        assert_eq!(f2[0].vx, 0.0);
        assert_eq!(f2[0].vy, 0.0);
        assert_eq!(f2[0].track_id, f1[0].track_id);
    }

    #[test]
    fn szybki_obiekt_zachowuje_id_dzieki_predykcji() {
        let cam = "trk-test-fast";
        // Klatka 1: obiekt wchodzi w kadr z lewej.
        let mut f1 = vec![det([0.00, 0.40, 0.10, 0.10])];
        update(cam, &mut f1, Some(0));
        let id1 = f1[0].track_id;
        assert!(id1 > 0);

        // Klatka 2 (@10fps, +0.1 s): skok srodka o +0.30 w x. Prędkość jeszcze
        // nieznana (predykcja = ostatnia pozycja), ale bramka odleglosci lapie
        // obiekt i bootstrapuje prędkość vx = 0.30 / 0.1 = 3.0.
        let mut f2 = vec![det([0.30, 0.40, 0.10, 0.10])];
        update(cam, &mut f2, Some(100_000_000));
        assert_eq!(f2[0].track_id, id1, "asocjacja po odleglosci powinna trzymac id");
        assert!(f2[0].vx > 0.0, "prędkość powinna byc zbootstrapowana, vx={}", f2[0].vx);

        // Klatka 3 (+0.1 s): kolejny skok o +0.30. Teraz predykcja przesuwa box
        // tracku o vx*dt ≈ 0.30 — srodek przewidziany pokrywa sie z detekcja, wiec
        // mimo skoku ~0.30/klatke id musi zostac zachowany.
        let mut f3 = vec![det([0.60, 0.40, 0.10, 0.10])];
        update(cam, &mut f3, Some(200_000_000));
        assert_eq!(
            f3[0].track_id, id1,
            "predykcja powinna utrzymac id na szybkim ruchu"
        );
        assert!(f3[0].vx > 0.0, "vx powinna pozostac dodatnia, vx={}", f3[0].vx);
    }

    #[test]
    fn track_id_monotoniczny_po_oproznieniu_kadru() {
        let cam = "trk-test-monotonic";
        // Pojazd A: kilka klatek, by ustabilizowac track.
        let mut fa = vec![det([0.10, 0.10, 0.20, 0.20])];
        update(cam, &mut fa, Some(0));
        let id_a = fa[0].track_id;
        assert!(id_a > 0);

        // Pusty kadr powtorzony az track pojazdu A wypadnie (misses > MAX_MISSES).
        for i in 0..(MAX_MISSES + 2) {
            let mut pusto: Vec<Detection> = Vec::new();
            update(cam, &mut pusto, Some((i as u64 + 1) * 1_000_000_000));
        }

        // Pojazd B pojawia sie w tym samym miejscu co A po pustym kadrze. Bez fixu
        // dostalby id=1 (reset next_id) → reuzycie i wyciek stanu. Po fixie id musi
        // byc WIEKSZE niz id pojazdu A w obrebie tej samej sesji kamery.
        let mut fb = vec![det([0.10, 0.10, 0.20, 0.20])];
        update(cam, &mut fb, Some(100_000_000_000));
        let id_b = fb[0].track_id;
        assert!(
            id_b > id_a,
            "track_id musi rosnac monotonicznie po pustym kadrze: id_a={id_a}, id_b={id_b}"
        );
    }

    #[test]
    fn update_pustego_trackera_nie_panikuje() {
        let cam = "trk-test-empty";
        // Pusty tracker (n_trk=0) + detekcja → nowy track. Koncowa petla
        // niedopasowanych tracków nie moze siegac poza oryginalne tracki.
        let mut f1 = vec![det([0.10, 0.10, 0.20, 0.20])];
        update(cam, &mut f1, Some(0));
        assert!(f1[0].track_id > 0);
    }
}
