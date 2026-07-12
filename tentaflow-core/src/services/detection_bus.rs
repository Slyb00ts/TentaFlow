// =============================================================================
// Plik: services/detection_bus.rs
// Opis: Magistrala pub/sub detekcji per kamera. Forwarduje detekcje
//       (ramki/klasy/stan) do warstwy WS przegladarki na potrzeby overlayu
//       podgladu live. Format wiadomosci jest serializowany do JSON dokladnie
//       w ksztalcie, ktory konsumuje frontend.
// Przyklad:
//   detection_bus::publish_detections("cam1", vec![Detection { .. }]);
//   let mut rx = detection_bus::subscribe("cam1");
// =============================================================================
//
// Most miedzy zrodlem detekcji a przegladarka. Docelowo zrodlem bedzie realna
// inferencja (RF-DETR + OCR + klasyfikacja stanu) wolajaca `publish_detections`
// tym samym formatem; dopoki modeli nie ma, `spawn_detection_stub` generuje
// sztuczne, ruszajace sie ramki, zeby zademonstrowac overlay.
//
// Kanal: jeden `broadcast::Sender<DetectionsMessage>` per camera_id, tworzony
// leniwie przy pierwszym `publish` lub `subscribe`. Backpressure: gdy odbiorca
// nie nadaza, `broadcast` zwraca `Lagged` i odbiorca pomija zalegle ramki —
// detekcje sa "best-effort" (kolejna ramka i tak nadpisuje stan overlayu).

use std::sync::OnceLock;

use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

/// Pojemnosc bufora broadcast per kamera. Przy ~10 fps to ~6 s zaleglosci,
/// po czym wolny odbiorca dostaje `Lagged` i przeskakuje do najnowszej ramki.
const DETECTION_BROADCAST_CAPACITY: usize = 64;

/// Pojedyncza detekcja jednego obiektu na klatce.
///
/// Pola odwzorowuja schemat JSON konsumowany przez frontend:
///   * `klasa`  — nazwa klasy z naszego zbioru (np. "tablica_adr").
///   * `bbox`   — [x, y, w, h] ZNORMALIZOWANE 0..1 wzgledem klatki; front
///     przeskaluje do rozmiaru elementu <video>.
///   * `score`  — pewnosc detekcji 0..1.
///   * `stan`   — lista cech stanu (np. ["uszkodzona"]); moze byc pusta.
///   * `tekst`  — odczyt OCR albo `None` (serializowany jako `null`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Detection {
    pub klasa: String,
    pub bbox: [f32; 4],
    pub score: f32,
    #[serde(default)]
    pub stan: Vec<String>,
    #[serde(default)]
    pub tekst: Option<String>,
    /// Mean OCR confidence (0..1) of the WINNING `tekst` — the vote's weighted
    /// winner confidence, not one raw frame. `None` when there is no OCR text
    /// (or the read was gated out as unreadable). Serialized as `null`; the
    /// `#[serde(default)]` keeps older CBOR/JSON without the field decodable.
    #[serde(default)]
    pub tekst_conf: Option<f32>,
    /// Snapshot ref (`snap_<uuid>`) of the FULL downscaled camera frame captured
    /// at the moment this track's OCR read reached a new best confidence — the
    /// whole visible scene, NOT a crop. `None` when no thumbnail was captured for
    /// this frame's read. The event recorder promotes this ref into the
    /// `recordings.plate_thumb_ref`/`adr_thumb_ref` list thumbnail when the read
    /// is the event's best so far. Serialized as `null`; `#[serde(default)]`
    /// keeps older CBOR/JSON without the field decodable.
    #[serde(default)]
    pub tekst_thumb_ref: Option<String>,
    /// Stabilny identyfikator sledzenia nadany przez tracker IOU. 0 = brak
    /// przypisania (np. detekcje ze zrodel bez trackera).
    #[serde(default)]
    pub track_id: u32,
    /// Stabilny identyfikator POJAZDU, na ktorym siedzi ten znak/tablica, nadany
    /// przez asocjacje (`assign_vehicle`) do stabilnego track_id trackera
    /// "vehicles". 0 = brak przypisania (znak poza jakimkolwiek pojazdem albo
    /// model pojazdow niedostepny) — trzymany do overlayu, ale wykluczony z
    /// grupowania per-pojazd. Dla samych boxow pojazdow rowna sie ich track_id.
    /// `#[serde(default)]` zachowuje kompatybilnosc CBOR/JSON jak `track_id`.
    #[serde(default)]
    pub vehicle_id: u32,
    /// Prędkość srodka boxa w jednostkach znormalizowanych/s (os X). 0 gdy brak
    /// bazy czasu (pts_ns) albo pierwsza obserwacja tracku.
    #[serde(default)]
    pub vx: f32,
    /// Prędkość srodka boxa w jednostkach znormalizowanych/s (os Y).
    #[serde(default)]
    pub vy: f32,
}

/// Wiadomosc wysylana do przegladarki (server→browser). Serializuje sie do
/// JSON dokladnie w ksztalcie:
/// ```json
/// {
///   "type": "detections",
///   "camera_id": "cam1",
///   "ts_ms": 0,
///   "items": [ { "klasa": "...", "bbox": [..], "score": .., "stan": [..], "tekst": null } ]
/// }
/// ```
#[derive(Debug, Clone, Serialize)]
pub struct DetectionsMessage {
    /// Staly dyskryminator typu wiadomosci — zawsze "detections".
    #[serde(rename = "type")]
    pub msg_type: &'static str,
    pub camera_id: String,
    /// Znacznik czasu w milisekundach (unix epoch ms).
    pub ts_ms: u64,
    /// PTS klatki w osi mediów (nanosekundy) — wspolna oś czasu z init-segmentem
    /// MSE (`mux_base_pts_ns`), pozwala klientowi kotwiczyc overlay dokladnie na
    /// klatce wideo, niezaleznie od zegara wall-clock.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pts_ns: Option<u64>,
    /// Czas CALOSCI obrobki klatki w ms (detekcja + OCR + klasyfikacja stanu).
    /// Klient pokazuje go jako badge. 0 gdy nieznany (np. surowa ramka FAZY 1).
    pub proc_ms: u32,
    pub items: Vec<Detection>,
}

impl DetectionsMessage {
    fn new(
        camera_id: String,
        ts_ms: u64,
        pts_ns: Option<u64>,
        proc_ms: u32,
        items: Vec<Detection>,
    ) -> Self {
        Self {
            msg_type: "detections",
            camera_id,
            ts_ms,
            pts_ns,
            proc_ms,
            items,
        }
    }
}

/// Rejestr nadawcow broadcast per camera_id. Proces-wide singleton.
struct DetectionBus {
    senders: DashMap<String, broadcast::Sender<DetectionsMessage>>,
}

impl DetectionBus {
    fn new() -> Self {
        Self {
            senders: DashMap::new(),
        }
    }

    /// Zwraca istniejacego nadawce dla kamery albo tworzy nowego. Nadawca
    /// zyje do konca procesu (lekki — pusty ring buffer gdy brak odbiorcow).
    fn sender(&self, camera_id: &str) -> broadcast::Sender<DetectionsMessage> {
        if let Some(tx) = self.senders.get(camera_id) {
            return tx.clone();
        }
        self.senders
            .entry(camera_id.to_string())
            .or_insert_with(|| broadcast::channel(DETECTION_BROADCAST_CAPACITY).0)
            .clone()
    }
}

fn detection_bus() -> &'static DetectionBus {
    static BUS: OnceLock<DetectionBus> = OnceLock::new();
    BUS.get_or_init(DetectionBus::new)
}

/// Subskrybuje strumien detekcji dla danej kamery. Handler WS woła to po
/// upgrade i forwarduje kazda wiadomosc jako JSON do przegladarki.
pub fn subscribe(camera_id: &str) -> broadcast::Receiver<DetectionsMessage> {
    detection_bus().sender(camera_id).subscribe()
}

/// Czysty punkt wpiecia dla zrodla detekcji. Realna inferencja
/// (RF-DETR + OCR + stan) bedzie wolac dokladnie te funkcje tym samym
/// `Detection` -> JSON kontraktem co stub.
///
/// `ts_ms` to czas PRZECHWYCENIA klatki (unix epoch ms, wall-clock), NIE czas
/// publikacji. Overlay w przegladarce kotwiczy `video.currentTime` (odtwarzanie
/// w czasie rzeczywistym) do tego znacznika, wiec musi on odpowiadac klatce na
/// ktorej wykonano detekcje — inaczej pudelka lądują na spóźnionej klatce
/// (opóźnienie dekod+inferencja+publish).
///
/// Gdy nikt nie subskrybuje danej kamery, wiadomosc jest po cichu pomijana
/// (brak odbiorcow broadcast) — to zamierzone, nie blokuje producenta.
pub fn publish_detections(
    camera_id: &str,
    ts_ms: u64,
    pts_ns: Option<u64>,
    proc_ms: u32,
    items: Vec<Detection>,
) {
    // Per-vehicle event recording rides the same bus: the hook lazily spawns a
    // recorder task for this camera (cheap set-probe once one exists). Fired
    // for EMPTY frames too, so the recorder's pre-roll buffer is warm before
    // the first vehicle of the day.
    #[cfg(feature = "camera")]
    crate::services::event_recorder::on_detections_published(camera_id);
    let msg = DetectionsMessage::new(camera_id.to_string(), ts_ms, pts_ns, proc_ms, items);
    // `send` zwraca Err tylko gdy nie ma zadnych odbiorcow — ignorujemy.
    let _ = detection_bus().sender(camera_id).send(msg);
}

// -----------------------------------------------------------------------------
// Stub detekcji — tymczasowe rusztowanie do demonstracji overlayu BEZ modeli.
// Docelowo zrodlem detekcji bedzie realna inferencja wolajaca
// `publish_detections`; ten task tylko wstrzykuje sztuczne, ruszajace sie ramki.
// -----------------------------------------------------------------------------

/// Klasy z naszego zbioru uzywane przez stub, zeby overlay wygladal
/// realistycznie (te same nazwy, ktorych uzyje realna inferencja).
const STUB_CLASSES: &[&str] = &[
    "tablica_adr",
    "tablica_rejestracyjna",
    "nalepka_3",
    "nalepka_9",
    "znak_srodowiskowy",
    "termometr",
];

/// Uruchamia zadanie tla generujace sztuczne detekcje dla kamery co ~100 ms
/// (10 fps): powoli przesuwajaca sie tablica ADR + nalepka ze zmiennym stanem.
/// Zwraca uchwyt zadania — porzucenie go nie zatrzymuje petli (zadanie zyje do
/// konca procesu albo do `abort()`); handler endpointu testowego trzyma uchwyt,
/// zeby ponowny start nie mnozyl petli.
///
/// To rusztowanie demo — usun gdy wepniesz realna inferencje.
pub fn spawn_detection_stub(camera_id: String) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(std::time::Duration::from_millis(100));
        let mut tick: u64 = 0;
        loop {
            ticker.tick().await;
            tick += 1;

            // Tablica ADR sunaca poziomo tam i z powrotem (trojkat fali na x).
            let phase = (tick % 200) as f32 / 200.0;
            let x = 0.15 + 0.5 * (0.5 - (phase - 0.5).abs()) * 2.0;
            let adr = Detection {
                klasa: "tablica_adr".to_string(),
                bbox: [x, 0.22, 0.12, 0.06],
                score: 0.96,
                stan: Vec::new(),
                tekst: Some("30/1202".to_string()),
                tekst_conf: None,
                tekst_thumb_ref: None,
                track_id: 0,
                vehicle_id: 0,
                vx: 0.,
                vy: 0.,
            };

            // Nalepka ze zmiennym stanem: co ~3 s przelacza "uszkodzona".
            let damaged = (tick / 30).is_multiple_of(2);
            let nalepka = Detection {
                klasa: "nalepka_3".to_string(),
                bbox: [0.30, 0.15 + 0.03 * (phase - 0.5), 0.05, 0.07],
                score: 0.94,
                stan: if damaged {
                    vec!["uszkodzona".to_string()]
                } else {
                    Vec::new()
                },
                tekst: None,
                tekst_conf: None,
                tekst_thumb_ref: None,
                track_id: 0,
                vehicle_id: 0,
                vx: 0.,
                vy: 0.,
            };

            // Co ~5 s dorzuca trzecia ramke z rotujaca klasa, zeby overlay
            // pokazywal wiecej niz dwa staticzne pudelka.
            let mut items = vec![adr, nalepka];
            if (tick / 50).is_multiple_of(2) {
                let klasa = STUB_CLASSES[(tick / 50) as usize % STUB_CLASSES.len()];
                items.push(Detection {
                    klasa: klasa.to_string(),
                    bbox: [0.60, 0.55, 0.10, 0.10],
                    score: 0.88,
                    stan: Vec::new(),
                    tekst: None,
                    tekst_conf: None,
                    tekst_thumb_ref: None,
                    track_id: 0,
                    vehicle_id: 0,
                    vx: 0.,
                    vy: 0.,
                });
            }

            // Stub nie ma realnej klatki, wiec brak naturalnego czasu
            // przechwycenia — uzywamy biezacego czasu wall-clock lokalnie.
            let ts_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            publish_detections(&camera_id, ts_ms, None, 0, items);
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serializacja `DetectionsMessage` -> JSON musi byc zgodna ze schematem
    /// uzgodnionym z frontendem: pole `type`, `camera_id`, `ts_ms`, `items`,
    /// a w kazdym item: `klasa`, `bbox` [x,y,w,h], `score`, `stan` (lista),
    /// `tekst` (string albo null).
    #[test]
    fn detection_message_serializes_to_agreed_schema() {
        let msg = DetectionsMessage::new(
            "cam1".to_string(),
            0,
            None,
            0,
            vec![
                Detection {
                    klasa: "tablica_adr".to_string(),
                    bbox: [0.41, 0.22, 0.12, 0.06],
                    score: 0.96,
                    stan: Vec::new(),
                    tekst: Some("30/1202".to_string()),
                    tekst_conf: None,
                    tekst_thumb_ref: None,
                    track_id: 0,
                    vehicle_id: 0,
                    vx: 0.,
                    vy: 0.,
                },
                Detection {
                    klasa: "nalepka_3".to_string(),
                    bbox: [0.30, 0.15, 0.05, 0.07],
                    score: 0.94,
                    stan: vec!["uszkodzona".to_string()],
                    tekst: None,
                    tekst_conf: None,
                    tekst_thumb_ref: None,
                    track_id: 0,
                    vehicle_id: 0,
                    vx: 0.,
                    vy: 0.,
                },
            ],
        );

        let v: serde_json::Value = serde_json::to_value(&msg).expect("serializacja JSON");

        assert_eq!(v["type"], "detections");
        assert_eq!(v["camera_id"], "cam1");
        assert_eq!(v["ts_ms"], 0);

        let items = v["items"].as_array().expect("items to tablica");
        assert_eq!(items.len(), 2);

        // bbox/score sa f32 — porownujemy z tolerancja, bo f32->f64 w JSON nie
        // odwzorowuje literalow f64 bit w bit.
        let approx = |actual: &serde_json::Value, expected: f64| {
            let got = actual.as_f64().expect("liczba");
            assert!(
                (got - expected).abs() < 1e-4,
                "oczekiwano ~{expected}, dostano {got}"
            );
        };

        let a = &items[0];
        assert_eq!(a["klasa"], "tablica_adr");
        assert!(a["bbox"].is_array());
        approx(&a["bbox"][0], 0.41);
        approx(&a["bbox"][1], 0.22);
        approx(&a["bbox"][2], 0.12);
        approx(&a["bbox"][3], 0.06);
        approx(&a["score"], 0.96);
        assert!(a["stan"].as_array().expect("stan lista").is_empty());
        assert_eq!(a["tekst"], "30/1202");

        let b = &items[1];
        assert_eq!(b["klasa"], "nalepka_3");
        assert_eq!(b["stan"][0], "uszkodzona");
        assert!(b["tekst"].is_null());
    }

    #[tokio::test]
    async fn publish_reaches_subscriber() {
        let cam = "detbus-test-cam-pub";
        let mut rx = subscribe(cam);
        publish_detections(
            cam,
            0,
            None,
            0,
            vec![Detection {
                klasa: "termometr".to_string(),
                bbox: [0.1, 0.1, 0.2, 0.2],
                score: 0.5,
                stan: Vec::new(),
                tekst: None,
                tekst_conf: None,
                tekst_thumb_ref: None,
                track_id: 0,
                vehicle_id: 0,
                vx: 0.,
                vy: 0.,
            }],
        );
        let msg = rx.recv().await.expect("wiadomosc detekcji");
        assert_eq!(msg.camera_id, cam);
        assert_eq!(msg.items.len(), 1);
        assert_eq!(msg.items[0].klasa, "termometr");
    }

    #[tokio::test]
    async fn publish_without_subscriber_is_noop() {
        // Brak subskrybenta — publish nie panikuje i nie blokuje producenta.
        publish_detections(
            "detbus-test-cam-ghost",
            0,
            None,
            0,
            vec![Detection {
                klasa: "nalepka_9".to_string(),
                bbox: [0.0, 0.0, 0.1, 0.1],
                score: 0.9,
                stan: Vec::new(),
                tekst: None,
                tekst_conf: None,
                tekst_thumb_ref: None,
                track_id: 0,
                vehicle_id: 0,
                vx: 0.,
                vy: 0.,
            }],
        );
    }

    #[tokio::test(start_paused = true)]
    async fn stub_emits_moving_detections() {
        let cam = "detbus-test-cam-stub";
        let mut rx = subscribe(cam);
        let handle = spawn_detection_stub(cam.to_string());

        // Pierwszy tick interwalu wypada natychmiast (paused time).
        tokio::time::advance(std::time::Duration::from_millis(120)).await;
        let first = rx.recv().await.expect("pierwsza ramka stub");
        assert!(first.items.len() >= 2, "stub emituje 1-3 ramki");
        assert_eq!(first.items[0].klasa, "tablica_adr");
        let x_first = first.items[0].bbox[0];

        // Po kolejnych ~50 tickach bbox tablicy ADR powinien sie przesunac.
        tokio::time::advance(std::time::Duration::from_millis(5000)).await;
        let mut later = rx.recv().await.expect("kolejna ramka stub");
        // Przewin do najnowszej dostepnej ramki (broadcast moze lagowac).
        while let Ok(m) = rx.try_recv() {
            later = m;
        }
        let x_later = later.items[0].bbox[0];
        assert!(
            (x_first - x_later).abs() > f32::EPSILON,
            "tablica ADR musi sie ruszac: {x_first} vs {x_later}"
        );

        handle.abort();
    }
}
