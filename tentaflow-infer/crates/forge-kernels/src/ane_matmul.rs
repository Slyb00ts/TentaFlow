// =============================================================================
// Plik: ane_matmul.rs
// Opis: Trzecie ramię podziału wierszy w prefillu — ogon wierszy projekcji
//       liczony przez Neural Engine z modeli CoreML wyeksportowanych przez
//       tools/ane-export. Ładuje katalog z manifest.json, wiąże części grup
//       z wagami wykonawcy, trzyma jeden wątek roboczy, który woła predict,
//       i oddaje wynik jako ciągły bufor [T', width] f16 do rozrzucenia
//       kernelem scatter.
// Przykład:
//       let (ane, report) = AneMatmul::load(&*device, dir, &bindings, units, 1024)?;
//       let rows = ane.ane_rows(w, tokens);           // 0 = brak ramienia
//       ane.ensure_loaded(w, tokens)?;                 // doładowanie PRZED podziałem
//       unsafe { ane.start(w, tokens, &x_buf)? };     // rola First/Solo
//       let done = ane.join()?;                        // rola Last/Solo
// =============================================================================

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde::Deserialize;

#[cfg(not(feature = "ane"))]
pub use coreml_stub::ComputeUnits;
#[cfg(not(feature = "ane"))]
use coreml_stub::CoreMlModel;
use forge_graph::WeightId;
#[cfg(feature = "ane")]
pub use forge_hal::coreml::ComputeUnits;
#[cfg(feature = "ane")]
use forge_hal::coreml::CoreMlModel;
use forge_hal::{DevBuffer, Device, Pool};
use forge_types::{ForgeError, MemKind, Result};

/// Zaślepka wiązania CoreML dla cechy `ane-check`: ten sam kształt API co
/// `forge_hal::coreml`, każde wywołanie odmawia. Istnieje, żeby reszta tego
/// modułu i `MetalExec` typowały się na maszynie bez Apple SDK.
#[cfg(not(feature = "ane"))]
mod coreml_stub {
    use std::path::Path;

    use forge_types::{ForgeError, Result};

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum ComputeUnits {
        CpuAndNeuralEngine,
        All,
        CpuOnly,
        CpuAndGpu,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct IoShape {
        pub in_rows: u32,
        pub in_cols: u32,
        pub out_rows: u32,
        pub out_cols: u32,
    }

    #[derive(Debug)]
    pub struct CoreMlModel(());

    fn refuse<T>() -> Result<T> {
        Err(ForgeError::Unsupported(
            "CoreML: zbudowano z cechą `ane-check` (zaślepka bez wiązania)".into(),
        ))
    }

    impl CoreMlModel {
        pub fn load(_path: &Path, _function: Option<&str>, _units: ComputeUnits) -> Result<Self> {
            refuse()
        }

        pub fn shape(&self, _in_name: &str, _out_name: &str) -> Result<IoShape> {
            refuse()
        }

        /// # Safety
        /// Nic nie czyta ani nie pisze; sygnatura zgodna z prawdziwym wiązaniem.
        pub unsafe fn predict(
            &self,
            _in_name: &str,
            _x: *const u8,
            _out_name: &str,
            _out: *mut u8,
        ) -> Result<bool> {
            refuse()
        }
    }
}

/// Najmniejszy wsad, przy którym ogon ANE w ogóle wchodzi w grę. Ten sam
/// próg co `variant::split_rows`: poniżej dekodowanie i małe kafle zostają
/// w całości na GPU, więc nie ma sensu nawet szukać funkcji.
const MIN_ANE_TOKENS: u32 = 256;

/// Wątków ładujących modele równolegle. Cztery, bo tyle rdzeni wydajnościowych
/// ma M1, a ładowanie to głównie parsowanie i mapowanie plików.
const LOAD_THREADS: usize = 4;

/// Zmienna środowiskowa zawężająca zestaw kształtów T ładowanych z
/// manifestu, np. `256` albo `256,1024`. Bez niej — wszystkie. Mniejszy T
/// to mniejsze bufory we/wy programu ANE (wired), kosztem kilku predict na
/// jeden kafel prefillu.
const SHAPES_ENV: &str = "FORGE_ANE_SHAPES";

/// Projekcja w nazewnictwie manifestu. Własne wyliczenie, bo ten crate nie
/// zależy od `forge-model` (warstwy idą w drugą stronę); wołający mapuje
/// swój typ na ten.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AneProjKind {
    Q,
    K,
    V,
    O,
    Gate,
    Up,
    Down,
}

impl AneProjKind {
    /// Nazwa z manifestu (`proj`), małymi literami.
    pub fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "q" => Self::Q,
            "k" => Self::K,
            "v" => Self::V,
            "o" => Self::O,
            "gate" => Self::Gate,
            "up" => Self::Up,
            "down" => Self::Down,
            _ => return None,
        })
    }
}

/// Jedno wiązanie: waga wykonawcy, jej rola i kształt. Kształt jest tu po
/// to, żeby część manifestu ODMÓWIŁA, gdy nie pasuje do wagi — zły kształt
/// nie objawiłby się błędem, tylko innym modelem.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AneBindingLite {
    pub layer: u32,
    pub proj: AneProjKind,
    pub id: WeightId,
    pub rows: u32,
    pub cols: u32,
}

/// Miejsce wagi w grupie: kto uruchamia predict, a kto na niego czeka.
///
/// Grupa `gate_up` liczy obie projekcje jednym predict, więc `gate` (First)
/// startuje, a `up` (Last) dołącza i rozrzuca wynik obu. Grupa
/// jednoczęściowa (`down`) robi jedno i drugie (Solo).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AneRole {
    First,
    Middle,
    Last,
    Solo,
}

/// Jedna część grupy: wycinek jednej projekcji w wyjściu modelu.
#[derive(Debug, Clone)]
pub struct AnePart {
    pub proj: AneProjKind,
    pub id: WeightId,
    /// Pełna liczba wierszy projekcji (krok zapisu w slocie wyjściowym).
    pub rows: u32,
    pub cols: u32,
    /// Pierwszy wiersz ogona: `rows - ane_rows`.
    pub ane0: u32,
    pub ane_rows: u32,
    /// Kolumna w wyjściu `y`, od której zaczyna się ta część.
    pub out_col0: u32,
}

/// Jedna funkcja modelu (jeden kształt T): skąd ją wziąć i czy jest w pamięci.
///
/// Uchwyt jest `Arc`, bo wątek roboczy trzyma go przez czas predict, a
/// wypieranie z budżetu może zdjąć go z listy w tym samym czasie — model
/// znika dopiero, gdy puści go ostatni.
struct FnSlot {
    tokens: u32,
    path: PathBuf,
    function: Option<String>,
    model: Option<Arc<CoreMlModel>>,
    /// Znacznik ostatniego użycia do wypierania LRU.
    last_used: u64,
}

/// Jedna grupa manifestu: jeden model, kilka funkcji (po jednej na T), kilka
/// części dzielących wejście.
pub struct AneGroup {
    pub layer: u32,
    pub name: String,
    pub in_name: String,
    pub out_name: String,
    pub in_cols: u32,
    pub out_width: u32,
    pub parts: Vec<AnePart>,
    /// Kształty posortowane po T rosnąco (bez modeli — te są w `Residency`).
    shapes: Vec<u32>,
}

/// Jak grupa liczy `tokens` wierszy dostępnymi kształtami.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AnePlan {
    /// Indeks funkcji w `shapes`.
    fn_idx: usize,
    /// T tej funkcji.
    shape_tokens: u32,
    /// Ile predict kolejno na kolejnych kawałkach wejścia i wyjścia.
    predicts: u32,
}

impl AneGroup {
    /// Najmniejsza funkcja o T' >= tokens jednym predict; gdy takiej nie
    /// ma — największa T_s < tokens i ceil(tokens / T_s) predict po kolei,
    /// każdy o T_s wierszy dalej. Ostatni kawałek czyta wiersze ponad
    /// `tokens` tak samo, jak pojedynczy predict z T' > tokens.
    fn plan_for(&self, tokens: u32) -> Option<AnePlan> {
        if let Some((i, t)) = self.shapes.iter().enumerate().find(|(_, t)| **t >= tokens) {
            return Some(AnePlan {
                fn_idx: i,
                shape_tokens: *t,
                predicts: 1,
            });
        }
        let i = self.shapes.len().checked_sub(1)?;
        let t = self.shapes[i];
        Some(AnePlan {
            fn_idx: i,
            shape_tokens: t,
            predicts: tokens.div_ceil(t),
        })
    }
}

/// Które funkcje są w pamięci. Osobno od grup, bo to jedyna zmienna część.
struct Residency {
    /// [grupa][funkcja]
    slots: Vec<Vec<FnSlot>>,
    resident: usize,
    clock: u64,
}

/// Najwięcej funkcji z ANE, które jeden proces może trzymać naraz.
///
/// Zmierzone, nie wyczytane: 129. `MLModel` z `cpuAndNeuralEngine` odmawia
/// z komunikatem o `functionName`, który nie ma nic wspólnego z przyczyną;
/// zwolnienie uchwytu oddaje miejsce, a modele `cpuOnly` nie liczą się wcale.
/// Budżet niżej niż 128 zostawia miejsce na funkcję w locie (wyparta z listy,
/// ale trzymana przez wątek roboczy) i na cudzy model w tym samym procesie.
const MAX_RESIDENT: usize = 120;

/// Co ładowanie przyniosło — do raportu w teście i logu.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AneLoadReport {
    /// Modeli (`.mlmodelc`) w użyciu.
    pub models: usize,
    /// Funkcji (model x T) załadowanych przy starcie — komplet NAJMNIEJSZEGO
    /// dozwolonego T (najmniej wired); pozostałe ładują się przy pierwszym
    /// użyciu, w ramach budżetu `MAX_RESIDENT`.
    pub functions: usize,
    /// Wszystkich funkcji dozwolonych (manifest zawężony przez
    /// `FORGE_ANE_SHAPES`).
    pub functions_total: usize,
    /// Najmniejszy i największy dozwolony T.
    pub shape_min: u32,
    pub shape_max: u32,
    /// Grup manifestu pominiętych, bo wołający nie dał dla nich wiązań
    /// (np. filtr warstw przy bisekcji).
    pub skipped: usize,
    pub load_ms: f64,
}

/// Liczniki od ostatniego `reset_stats` — do profilowania ramienia bez
/// subskrybenta `tracing`: ile predict, ile czasu w nich, ile czekania w
/// `join`, ile doładowań funkcji.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct AneStats {
    /// Zleceń (par start/join).
    pub predicts: u64,
    /// Wywołań `prediction` CoreML — więcej niż `predicts`, gdy kafel jest
    /// liczony kilkoma predict mniejszego T.
    pub sub_predicts: u64,
    pub predict_ms: f64,
    pub wait_ms: f64,
    /// Predict, w których CoreML skopiował wynik zamiast pisać w nasz bufor.
    pub copies: u64,
    pub loads: u64,
    pub load_ms: f64,
    pub evictions: u64,
}

/// Wynik jednego predict, oddany przez `join`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AneDone {
    pub group_idx: usize,
    /// Tokeny, o które prosił wołający.
    pub tokens: u32,
    /// T funkcji, która to policzyła (>= tokens przy jednym predict).
    pub shape_tokens: u32,
    /// Ile predict złożyło się na ten wynik (ceil(tokens / T) gdy T < tokens).
    pub predicts: u32,
    /// Czas wszystkich predict razem, zmierzony w wątku roboczym.
    pub predict_ms: f64,
    /// Ile wołający czekał w `join` (zero, gdy ANE skończył wcześniej).
    pub wait_ms: f64,
    /// Czy CoreML pisał wprost w nasz bufor (false = shim skopiował).
    pub in_place: bool,
}

// ---- Manifest (tylko pola, których używamy; reszta jest ignorowana) ----

#[derive(Deserialize)]
struct ManifestJson {
    version: u32,
    groups: Vec<GroupJson>,
}

#[derive(Deserialize)]
struct GroupJson {
    layer: u32,
    group: String,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    models: Option<HashMap<String, String>>,
    #[serde(default)]
    functions: Option<HashMap<String, String>>,
    input: InputJson,
    output: OutputJson,
    parts: Vec<PartJson>,
}

#[derive(Deserialize)]
struct InputJson {
    name: String,
    cols: u32,
}

#[derive(Deserialize)]
struct OutputJson {
    name: String,
    width: u32,
}

#[derive(Deserialize)]
struct PartJson {
    proj: String,
    rows: u32,
    cols: u32,
    ane0: u32,
    ane_rows: u32,
    out_col0: u32,
}

/// Jedno zadanie dla wątku: który model, jaka funkcja, gdzie x, gdzie y.
/// Bufory jako KLONY uchwytów (`DevBuffer` to `Arc`), a nie gołe adresy:
/// zadanie trzyma pamięć przy życiu do końca predict, więc kolejność
/// zwalniania pól wykonawcy (scratch przed ramieniem) nie ma znaczenia.
/// Kontrakt tego, że nikt nie pisze `x` ani nie czyta `out` w trakcie,
/// jest po stronie `start` (patrz SAFETY tam).
struct Job {
    group_idx: usize,
    model: Arc<CoreMlModel>,
    x: DevBuffer,
    /// Przesunięcie bajtowe pierwszego wiersza x w buforze.
    x_offset: usize,
    out: DevBuffer,
    /// Ile predict kolejno; między nimi x przesuwa się o `in_stride`,
    /// a out o `out_stride` bajtów.
    predicts: u32,
    in_stride: usize,
    out_stride: usize,
}

struct JobResult {
    predict_ms: f64,
    in_place: Result<bool>,
}

struct Pending {
    group_idx: usize,
    tokens: u32,
    shape_tokens: u32,
    predicts: u32,
    started: Instant,
}

/// Ramię ANE: modele, wiązania, bufor wyjściowy i wątek roboczy.
pub struct AneMatmul {
    groups: Arc<Vec<AneGroup>>,
    residency: Mutex<Residency>,
    units: ComputeUnits,
    /// WeightId -> (grupa, część).
    by_weight: HashMap<u32, (usize, usize)>,
    /// Ciągłe wyjście predict: `[T_max, width_max]` f16, wyrównane do strony
    /// (bufor Metala `Shared`), więc CoreML może w nie pisać bez kopii.
    out: DevBuffer,
    /// Wierszy w slocie aktywacji wołającego (`PREFILL_CHUNK`); górna
    /// granica `tokens` i gwarancja, że ostatni kawałek nie wyjdzie poza slot.
    max_tokens: u32,
    tx: Option<Sender<Job>>,
    rx: Receiver<JobResult>,
    pending: Mutex<Option<Pending>>,
    stats: Mutex<AneStats>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl AneMatmul {
    /// Wczytuje `manifest.json` z `dir`, ładuje każdą funkcję każdej grupy,
    /// dla której wołający dał wiązania, i sprawdza kształty.
    ///
    /// Grupa, której którejkolwiek części brakuje w `bindings`, jest
    /// POMIJANA (liczona w raporcie) — tak działa filtr warstw przy
    /// bisekcji. Grupa związana, ale o innym kształcie niż waga, to ODMOWA.
    ///
    /// `max_tokens` to liczba wierszy slotu aktywacji wołającego: każdy
    /// dozwolony T musi go dzielić, żeby kawałki predict nie wyszły poza
    /// slot. `FORGE_ANE_SHAPES` zawęża kształty z manifestu.
    pub fn load(
        device: &dyn Device,
        dir: &Path,
        bindings: &[AneBindingLite],
        units: ComputeUnits,
        max_tokens: u32,
    ) -> Result<(Self, AneLoadReport)> {
        let t0 = Instant::now();
        let allowed = allowed_shapes()?;
        let manifest_path = dir.join("manifest.json");
        let text = std::fs::read_to_string(&manifest_path)
            .map_err(|e| ForgeError::Other(format!("ANE: {}: {e}", manifest_path.display())))?;
        let manifest: ManifestJson = serde_json::from_str(&text)
            .map_err(|e| ForgeError::Other(format!("ANE: manifest: {e}")))?;
        if manifest.version != 1 {
            return Err(ForgeError::Unsupported(format!(
                "ANE: manifest w wersji {}, a znana jest 1",
                manifest.version
            )));
        }

        let by_binding: HashMap<(u32, AneProjKind), &AneBindingLite> =
            bindings.iter().map(|b| ((b.layer, b.proj), b)).collect();

        // Najpierw opis grup bez modeli (walidacja tania i sekwencyjna),
        // potem lista (grupa, T, ścieżka, funkcja) do załadowania równolegle.
        struct Planned {
            layer: u32,
            name: String,
            in_name: String,
            out_name: String,
            in_cols: u32,
            out_width: u32,
            parts: Vec<AnePart>,
            /// (T, ścieżka modelu, nazwa funkcji lub None = domyślna)
            shapes: Vec<(u32, PathBuf, Option<String>)>,
        }
        let mut planned: Vec<Planned> = Vec::new();
        let mut skipped = 0usize;
        for g in &manifest.groups {
            let mut parts = Vec::with_capacity(g.parts.len());
            let mut complete = true;
            for p in &g.parts {
                let proj = AneProjKind::parse(&p.proj).ok_or_else(|| {
                    ForgeError::Format(format!("ANE: nieznana projekcja '{}'", p.proj))
                })?;
                let Some(b) = by_binding.get(&(g.layer, proj)) else {
                    complete = false;
                    break;
                };
                if (b.rows, b.cols) != (p.rows, p.cols) {
                    return Err(ForgeError::Format(format!(
                        "ANE: L{} {}: część {:?} ma [{}x{}], a waga [{}x{}]",
                        g.layer, g.group, proj, p.rows, p.cols, b.rows, b.cols
                    )));
                }
                let ane_end = p.ane0.checked_add(p.ane_rows).ok_or_else(|| {
                    ForgeError::Format(format!(
                        "ANE: L{} {}: część {:?}: ane0 + ane_rows przekracza u32",
                        g.layer, g.group, proj
                    ))
                })?;
                if ane_end != p.rows || p.ane_rows == 0 {
                    return Err(ForgeError::Format(format!(
                        "ANE: L{} {}: część {:?} ma ogon [{}, {ane_end}) w {} wierszach — to nie ogon",
                        g.layer, g.group, proj, p.ane0, p.rows
                    )));
                }
                // Ogon i reszta muszą składać się z całych bloków kernela
                // GPU (`QMG_BN`), a ogon nie może być całą macierzą —
                // inaczej `split_rows` odmówi po cichu i ramię nigdy nie
                // policzy ani wiersza. Lepiej odmówić TU, z nazwą części.
                let block = crate::msl::QMG_BN;
                if p.ane_rows >= p.rows
                    || !p.ane_rows.is_multiple_of(block)
                    || !(p.rows - p.ane_rows).is_multiple_of(block)
                {
                    return Err(ForgeError::Format(format!(
                        "ANE: L{} {}: część {:?}: ogon {} z {} wierszy nie dzieli się na bloki \
                         {block} (ogon i reszta muszą być wielokrotnością bloku, ogon < rows)",
                        g.layer, g.group, proj, p.ane_rows, p.rows
                    )));
                }
                if p.cols != g.input.cols {
                    return Err(ForgeError::Format(format!(
                        "ANE: L{} {}: część {:?} ma {} kolumn, wejście grupy {}",
                        g.layer, g.group, proj, p.cols, g.input.cols
                    )));
                }
                let col_end = p.out_col0.checked_add(p.ane_rows);
                if col_end.is_none_or(|e| e > g.output.width) {
                    return Err(ForgeError::Format(format!(
                        "ANE: L{} {}: część {:?} wystaje poza wyjście ({} + {} > {})",
                        g.layer, g.group, proj, p.out_col0, p.ane_rows, g.output.width
                    )));
                }
                parts.push(AnePart {
                    proj,
                    id: b.id,
                    rows: p.rows,
                    cols: p.cols,
                    ane0: p.ane0,
                    ane_rows: p.ane_rows,
                    out_col0: p.out_col0,
                });
            }
            if !complete {
                skipped += 1;
                continue;
            }
            let total: u32 = parts.iter().map(|p| p.ane_rows).sum();
            if total != g.output.width {
                return Err(ForgeError::Format(format!(
                    "ANE: L{} {}: części sumują się do {total}, wyjście ma {}",
                    g.layer, g.group, g.output.width
                )));
            }
            let mut shapes = Vec::new();
            match (&g.model, &g.functions, &g.models) {
                (Some(model), Some(functions), _) => {
                    let path = model_path(dir, model, g.layer, &g.group)?;
                    for (t, f) in functions {
                        shapes.push((parse_t(t)?, path.clone(), Some(f.clone())));
                    }
                }
                (_, _, Some(models)) => {
                    for (t, m) in models {
                        shapes.push((parse_t(t)?, model_path(dir, m, g.layer, &g.group)?, None));
                    }
                }
                _ => {
                    return Err(ForgeError::Format(format!(
                        "ANE: L{} {}: brak 'model'+'functions' albo 'models'",
                        g.layer, g.group
                    )))
                }
            }
            let all: Vec<u32> = shapes.iter().map(|s| s.0).collect();
            if let Some(keep) = &allowed {
                shapes.retain(|s| keep.contains(&s.0));
                if shapes.is_empty() {
                    return Err(ForgeError::Unsupported(format!(
                        "ANE: L{} {}: {SHAPES_ENV}={keep:?} nie zostawia żadnego kształtu z {all:?}",
                        g.layer, g.group
                    )));
                }
            }
            for (t, _, _) in &shapes {
                if *t == 0 || !max_tokens.is_multiple_of(*t) {
                    return Err(ForgeError::Unsupported(format!(
                        "ANE: L{} {}: kształt T{t} nie dzieli slotu {max_tokens} wierszy — \
                         ostatni kawałek wyszedłby poza slot",
                        g.layer, g.group
                    )));
                }
            }
            shapes.sort_by_key(|s| s.0);
            planned.push(Planned {
                layer: g.layer,
                name: g.group.clone(),
                in_name: g.input.name.clone(),
                out_name: g.output.name.clone(),
                in_cols: g.input.cols,
                out_width: g.output.width,
                parts,
                shapes,
            });
        }

        // Przy starcie ładujemy równolegle KOMPLET NAJMNIEJSZEGO T — to
        // najmniej pamięci wired (bufory we/wy programu ANE rosną z T), a
        // większy kafel i tak da się policzyć kilkoma predict. Resztę
        // leniwie: wszystkich funkcji jest więcej, niż proces może trzymać
        // (`MAX_RESIDENT`).
        let mut slots: Vec<Vec<FnSlot>> = planned
            .iter()
            .map(|p| {
                p.shapes
                    .iter()
                    .map(|(t, path, function)| FnSlot {
                        tokens: *t,
                        path: path.clone(),
                        function: function.clone(),
                        model: None,
                        last_used: 0,
                    })
                    .collect()
            })
            .collect();
        let functions_total: usize = slots.iter().map(Vec::len).sum();
        let jobs: Vec<(usize, usize)> = planned
            .iter()
            .enumerate()
            .filter(|(_, p)| !p.shapes.is_empty())
            .map(|(gi, _)| (gi, 0usize))
            .take(MAX_RESIDENT)
            .collect();
        let results: Vec<Mutex<Option<Result<CoreMlModel>>>> =
            jobs.iter().map(|_| Mutex::new(None)).collect();
        let next = std::sync::atomic::AtomicUsize::new(0);
        std::thread::scope(|scope| {
            for _ in 0..LOAD_THREADS.min(jobs.len().max(1)) {
                scope.spawn(|| loop {
                    let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let Some(&(gi, si)) = jobs.get(i) else { break };
                    let p = &planned[gi];
                    let (t, path, function) = &p.shapes[si];
                    let loaded = load_checked(
                        path,
                        function.as_deref(),
                        units,
                        *t,
                        &p.in_name,
                        p.in_cols,
                        &p.out_name,
                        p.out_width,
                        p.layer,
                        &p.name,
                    );
                    *results[i].lock().expect("wynik ładowania") = Some(loaded);
                });
            }
        });
        let mut resident = 0usize;
        for (i, &(gi, si)) in jobs.iter().enumerate() {
            let r = results[i]
                .lock()
                .expect("wynik ładowania")
                .take()
                .ok_or_else(|| ForgeError::Other("ANE: zadanie ładowania bez wyniku".into()))?;
            slots[gi][si].model = Some(Arc::new(r?));
            resident += 1;
        }
        let functions = resident;

        let mut groups = Vec::with_capacity(planned.len());
        let mut by_weight = HashMap::new();
        // Bufor wyjściowy musi pomieścić największy kształt, ale też
        // `max_tokens` wierszy liczonych kawałkami najmniejszego T (każdy
        // dozwolony T dzieli `max_tokens`, więc to dokładnie `max_tokens`).
        let mut max_t = 0u32;
        let mut max_width = 0u32;
        let mut shape_min = u32::MAX;
        let mut shape_max = 0u32;
        for (gi, p) in planned.into_iter().enumerate() {
            for (t, _, _) in &p.shapes {
                shape_min = shape_min.min(*t);
                shape_max = shape_max.max(*t);
                max_t = max_t.max(*t).max(max_tokens);
            }
            max_width = max_width.max(p.out_width);
            for (pi, part) in p.parts.iter().enumerate() {
                if by_weight.insert(part.id.0, (gi, pi)).is_some() {
                    return Err(ForgeError::Format(format!(
                        "ANE: waga {} występuje w dwóch grupach",
                        part.id.0
                    )));
                }
            }
            groups.push(AneGroup {
                layer: p.layer,
                name: p.name,
                in_name: p.in_name,
                out_name: p.out_name,
                in_cols: p.in_cols,
                out_width: p.out_width,
                parts: p.parts,
                shapes: p.shapes.iter().map(|s| s.0).collect(),
            });
        }
        let models = groups.len();
        if models == 0 {
            shape_min = 0;
        }

        let out_bytes = (max_t as usize * max_width as usize * 2).max(4096);
        let out = device.alloc(out_bytes, MemKind::Device, Pool::Activations)?;
        if out.host_ptr().is_none() {
            return Err(ForgeError::Other(
                "ANE: bufor wyjściowy bez adresu hosta — to nie jest pamięć wspólna".into(),
            ));
        }

        let groups = Arc::new(groups);
        let (tx, job_rx) = mpsc::channel::<Job>();
        let (res_tx, rx) = mpsc::channel::<JobResult>();
        let worker_groups = Arc::clone(&groups);
        let worker = std::thread::Builder::new()
            .name("forge-ane".into())
            .spawn(move || {
                for job in job_rx {
                    let g = &worker_groups[job.group_idx];
                    let model = job.model;
                    let t = Instant::now();
                    // Kawałki po kolei: k-ty predict czyta x + k*in_stride i
                    // pisze out + k*out_stride. Wynik jest w miejscu tylko,
                    // gdy każdy kawałek był w miejscu.
                    let (mut in_place, x0, out0) = match (job.x.host_ptr(), job.out.host_ptr()) {
                        (Some(x), Some(out)) => (Ok(true), x as usize + job.x_offset, out as usize),
                        _ => (
                            Err(ForgeError::Other(
                                "ANE: bufor x lub wyjściowy bez adresu hosta".into(),
                            )),
                            0,
                            0,
                        ),
                    };
                    for k in 0..job.predicts as usize {
                        if in_place.is_err() {
                            break;
                        }
                        // SAFETY: kontrakt `start` — x ma predicts*T*cols f16
                        // ważnych i niezmienianych (slot ma `max_tokens`
                        // wierszy, a T dzieli `max_tokens`), out to nasz bufor
                        // o `max_tokens` wierszach, nikt go nie czyta do
                        // `join`. Oba bufory żyją co najmniej tak długo jak
                        // to zadanie, bo trzyma ono ich uchwyty.
                        let r = unsafe {
                            model.predict(
                                &g.in_name,
                                (x0 + k * job.in_stride) as *const u8,
                                &g.out_name,
                                (out0 + k * job.out_stride) as *mut u8,
                            )
                        };
                        in_place = match (in_place, r) {
                            (Err(e), _) => Err(e),
                            (_, Err(e)) => Err(e),
                            (Ok(a), Ok(b)) => Ok(a && b),
                        };
                        if in_place.is_err() {
                            break;
                        }
                    }
                    let predict_ms = t.elapsed().as_secs_f64() * 1e3;
                    // Uchwyt puszczamy PRZED odesłaniem wyniku, żeby miejsce w
                    // budżecie było wolne, zanim wołający zleci następny start.
                    drop(model);
                    if res_tx
                        .send(JobResult {
                            predict_ms,
                            in_place,
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            })
            .map_err(|e| ForgeError::Other(format!("ANE: wątek roboczy: {e}")))?;

        let report = AneLoadReport {
            models,
            functions,
            functions_total,
            skipped,
            shape_min,
            shape_max,
            load_ms: t0.elapsed().as_secs_f64() * 1e3,
        };
        tracing::debug!(
            "ANE: {} modeli, {}/{} funkcji przy starcie (T{}..T{}), {} grup pominiętych, {:.0} ms",
            report.models,
            report.functions,
            report.functions_total,
            report.shape_min,
            report.shape_max,
            report.skipped,
            report.load_ms
        );
        Ok((
            Self {
                groups,
                residency: Mutex::new(Residency {
                    slots,
                    resident,
                    clock: 0,
                }),
                units,
                by_weight,
                out,
                max_tokens,
                tx: Some(tx),
                rx,
                pending: Mutex::new(None),
                stats: Mutex::new(AneStats::default()),
                worker: Some(worker),
            },
            report,
        ))
    }

    /// Indeks grupy, do której należy waga.
    pub fn group_of(&self, w: WeightId) -> Option<usize> {
        self.by_weight.get(&w.0).map(|&(g, _)| g)
    }

    /// (grupa, część) wagi.
    pub fn part_of(&self, w: WeightId) -> Option<(usize, usize)> {
        self.by_weight.get(&w.0).copied()
    }

    pub fn group(&self, idx: usize) -> &AneGroup {
        &self.groups[idx]
    }

    /// Ciągły bufor wyjścia ostatniego predict: `[T', out_width]` f16.
    pub fn out_buf(&self) -> &DevBuffer {
        &self.out
    }

    /// Ogon wierszy dla tej wagi przy tym wsadzie. Zero, gdy waga nie ma
    /// grupy, wsad jest poniżej progu, ponad slot albo grupa nie ma żadnej
    /// funkcji (mniejszy T liczy kawałkami, więc też się kwalifikuje).
    pub fn ane_rows(&self, w: WeightId, tokens: u32) -> u32 {
        if tokens < MIN_ANE_TOKENS || tokens > self.max_tokens {
            return 0;
        }
        let Some(&(gi, pi)) = self.by_weight.get(&w.0) else {
            return 0;
        };
        let g = &self.groups[gi];
        if g.plan_for(tokens).is_none() {
            return 0;
        }
        g.parts[pi].ane_rows
    }

    /// Rola wagi w grupie, po kolejności części.
    pub fn role(&self, w: WeightId) -> Option<AneRole> {
        let &(gi, pi) = self.by_weight.get(&w.0)?;
        let n = self.groups[gi].parts.len();
        Some(match (pi, n) {
            (_, 1) => AneRole::Solo,
            (0, _) => AneRole::First,
            (i, n) if i + 1 == n => AneRole::Last,
            _ => AneRole::Middle,
        })
    }

    /// Liczniki od ostatniego zerowania.
    pub fn stats(&self) -> AneStats {
        *self.stats.lock().expect("stats")
    }

    pub fn reset_stats(&self) {
        *self.stats.lock().expect("stats") = AneStats::default();
    }

    /// Indeks grupy, która liczy się w tej chwili.
    pub fn pending(&self) -> Option<usize> {
        self.pending
            .lock()
            .expect("pending")
            .as_ref()
            .map(|p| p.group_idx)
    }

    /// Doładowuje funkcję, którą `start(w, tokens)` zaraz wybierze, żeby
    /// wołający mógł ODMÓWIĆ ogona (policzyć wszystko na GPU+CPU) zanim
    /// ustali podział, gdy ładowanie zawiedzie. Nic nie robi dla wagi bez
    /// grupy albo wsadu bez planu — `ane_rows` jest wtedy i tak zerem.
    pub fn ensure_loaded(&self, w: WeightId, tokens: u32) -> Result<()> {
        let Some(&(gi, _)) = self.by_weight.get(&w.0) else {
            return Ok(());
        };
        let Some(plan) = self.groups[gi].plan_for(tokens) else {
            return Ok(());
        };
        self.resident(gi, plan.fn_idx).map(drop)
    }

    /// Zleca predict grupy wagi `w` na `x` od bajtu `x_offset`. Wraca
    /// natychmiast. Zadanie trzyma klony uchwytów `x` i bufora wyjściowego,
    /// więc pamięć nie zniknie w trakcie predict nawet, gdy wołający puści
    /// swoje.
    ///
    /// # Safety
    /// * `x` od `x_offset` ma co najmniej `max_tokens * cols` elementów f16 (slot
    ///   aktywacji ma zawsze `PREFILL_CHUNK` wierszy; wiersze ponad `tokens`
    ///   są śmieciami, ale pamięcią ważną — czyta je ostatni predict, czy to
    ///   jeden z T' > tokens, czy ostatni kawałek T_s < tokens); nikt nie
    ///   pisze `x` do `join`;
    /// * bufor wyjściowy (`out_buf`) nie jest czytany ani pisany przez nikogo
    ///   innego do `join` — w szczególności kernel scatter z poprzedniego
    ///   predict musi już być WYKONANY (wołający synchronizuje strumień
    ///   przed startem);
    /// * `x` jest zmaterializowane: wszystko, co je produkuje, zakończone.
    pub unsafe fn start(
        &self,
        w: WeightId,
        tokens: u32,
        x: &DevBuffer,
        x_offset: usize,
    ) -> Result<()> {
        let &(gi, _) = self
            .by_weight
            .get(&w.0)
            .ok_or_else(|| ForgeError::Other(format!("ANE: waga {} nie ma grupy", w.0)))?;
        let g = &self.groups[gi];
        if tokens > self.max_tokens {
            return Err(ForgeError::Other(format!(
                "ANE: L{} {}: {tokens} tokenów ponad slot {} wierszy",
                g.layer, g.name, self.max_tokens
            )));
        }
        let plan = g.plan_for(tokens).ok_or_else(|| {
            ForgeError::Other(format!(
                "ANE: L{} {}: brak funkcji dla {tokens} tokenów",
                g.layer, g.name
            ))
        })?;
        let mut pending = self.pending.lock().expect("pending");
        if let Some(p) = pending.as_ref() {
            return Err(ForgeError::Other(format!(
                "ANE: start L{} {} podczas gdy grupa {} jeszcze liczy",
                g.layer, g.name, p.group_idx
            )));
        }
        let model = self.resident(gi, plan.fn_idx)?;
        if x.host_ptr().is_none() {
            return Err(ForgeError::Other("ANE: x bez adresu hosta".into()));
        }
        let rows = plan.shape_tokens as usize;
        let need = x_offset
            .checked_add(self.max_tokens as usize * g.in_cols as usize * 2)
            .filter(|n| *n <= x.len())
            .is_some();
        if !need {
            return Err(ForgeError::Other(format!(
                "ANE: L{} {}: x ma {} bajtów, a slot od {x_offset} potrzebuje {} wierszy po {} kolumn",
                g.layer,
                g.name,
                x.len(),
                self.max_tokens,
                g.in_cols
            )));
        }
        self.tx
            .as_ref()
            .ok_or_else(|| ForgeError::Other("ANE: wątek roboczy zamknięty".into()))?
            .send(Job {
                group_idx: gi,
                model,
                x: x.clone(),
                x_offset,
                out: self.out.clone(),
                predicts: plan.predicts,
                in_stride: rows * g.in_cols as usize * 2,
                out_stride: rows * g.out_width as usize * 2,
            })
            .map_err(|_| ForgeError::Other("ANE: wątek roboczy nie żyje".into()))?;
        *pending = Some(Pending {
            group_idx: gi,
            tokens,
            shape_tokens: plan.shape_tokens,
            predicts: plan.predicts,
            started: Instant::now(),
        });
        Ok(())
    }

    /// Uchwyt funkcji, ładując ją, gdy trzeba, i wypierając najdawniej
    /// użytą, gdy budżet jest pełny.
    fn resident(&self, gi: usize, fi: usize) -> Result<Arc<CoreMlModel>> {
        let mut r = self.residency.lock().expect("residency");
        r.clock += 1;
        let now = r.clock;
        if let Some(m) = r.slots[gi][fi].model.clone() {
            r.slots[gi][fi].last_used = now;
            return Ok(m);
        }
        while r.resident >= MAX_RESIDENT {
            let victim = r
                .slots
                .iter()
                .enumerate()
                .flat_map(|(g, v)| v.iter().enumerate().map(move |(f, s)| (g, f, s)))
                .filter(|(_, _, s)| s.model.is_some())
                .min_by_key(|(_, _, s)| s.last_used)
                .map(|(g, f, _)| (g, f))
                .ok_or_else(|| {
                    ForgeError::Other("ANE: budżet pełny, a nie ma czego wyprzeć".into())
                })?;
            let evicted = r.slots[victim.0][victim.1].model.take();
            r.resident -= 1;
            tracing::debug!(
                "ANE: wypieram L{} {} T{}",
                self.groups[victim.0].layer,
                self.groups[victim.0].name,
                r.slots[victim.0][victim.1].tokens
            );
            drop(evicted);
            self.stats.lock().expect("stats").evictions += 1;
        }
        let g = &self.groups[gi];
        let slot = &r.slots[gi][fi];
        let t0 = Instant::now();
        let model = Arc::new(load_checked(
            &slot.path,
            slot.function.as_deref(),
            self.units,
            slot.tokens,
            &g.in_name,
            g.in_cols,
            &g.out_name,
            g.out_width,
            g.layer,
            &g.name,
        )?);
        tracing::debug!(
            "ANE: doładowano L{} {} T{} w {:.1} ms",
            g.layer,
            g.name,
            slot.tokens,
            t0.elapsed().as_secs_f64() * 1e3
        );
        {
            let st = &mut *self.stats.lock().expect("stats");
            st.loads += 1;
            st.load_ms += t0.elapsed().as_secs_f64() * 1e3;
        }
        let slot = &mut r.slots[gi][fi];
        slot.model = Some(Arc::clone(&model));
        slot.last_used = now;
        r.resident += 1;
        Ok(model)
    }

    /// Czeka na trwający predict. Błąd, gdy nic nie trwa.
    pub fn join(&self) -> Result<AneDone> {
        let pending = self
            .pending
            .lock()
            .expect("pending")
            .take()
            .ok_or_else(|| ForgeError::Other("ANE: join bez start".into()))?;
        let wait = Instant::now();
        let res = self
            .rx
            .recv()
            .map_err(|_| ForgeError::Other("ANE: wątek roboczy zakończył się bez wyniku".into()))?;
        let wait_ms = wait.elapsed().as_secs_f64() * 1e3;
        let in_place = res.in_place?;
        {
            let st = &mut *self.stats.lock().expect("stats");
            st.predicts += 1;
            st.sub_predicts += u64::from(pending.predicts);
            st.predict_ms += res.predict_ms;
            st.wait_ms += wait_ms;
            st.copies += u64::from(!in_place);
        }
        let g = &self.groups[pending.group_idx];
        tracing::debug!(
            "ANE: L{} {} T{}x{} ({} tok): predict {:.2} ms, czekanie {:.2} ms, od startu {:.2} ms{}",
            g.layer,
            g.name,
            pending.shape_tokens,
            pending.predicts,
            pending.tokens,
            res.predict_ms,
            wait_ms,
            pending.started.elapsed().as_secs_f64() * 1e3,
            if in_place { "" } else { ", KOPIA wyjścia" }
        );
        Ok(AneDone {
            group_idx: pending.group_idx,
            tokens: pending.tokens,
            shape_tokens: pending.shape_tokens,
            predicts: pending.predicts,
            predict_ms: res.predict_ms,
            wait_ms,
            in_place,
        })
    }
}

impl Drop for AneMatmul {
    fn drop(&mut self) {
        // Zamknięcie kanału kończy pętlę wątku; dołączamy, żeby model nie był
        // zwalniany w trakcie predict.
        if self.pending.lock().map(|p| p.is_some()).unwrap_or(false) {
            let _ = self.rx.recv();
        }
        self.tx.take();
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
    }
}

/// Ładuje funkcję i sprawdza, że jej kształt to `[T, in_cols] -> [T, width]`.
#[allow(clippy::too_many_arguments)]
fn load_checked(
    path: &Path,
    function: Option<&str>,
    units: ComputeUnits,
    t: u32,
    in_name: &str,
    in_cols: u32,
    out_name: &str,
    out_width: u32,
    layer: u32,
    group: &str,
) -> Result<CoreMlModel> {
    let m = CoreMlModel::load(path, function, units)?;
    let shape = m.shape(in_name, out_name)?;
    let want = (t, in_cols, t, out_width);
    let got = (shape.in_rows, shape.in_cols, shape.out_rows, shape.out_cols);
    if got != want {
        return Err(ForgeError::Format(format!(
            "ANE: L{layer} {group} T{t}: kształt {got:?}, oczekiwano {want:?}"
        )));
    }
    Ok(m)
}

/// Ścieżka modelu z manifestu: DOKŁADNIE jedna zwykła składowa z sufiksem
/// `.mlmodelc`. Manifest to plik danych, a nie polecenie — `../x.mlmodelc`
/// albo ścieżka bezwzględna nie ma prawa wyprowadzić poza katalog.
fn model_path(dir: &Path, name: &str, layer: u32, group: &str) -> Result<PathBuf> {
    use std::path::Component;
    let mut comps = Path::new(name).components();
    let ok = matches!(
        (comps.next(), comps.next()),
        (Some(Component::Normal(c)), None)
            if c.to_str().is_some_and(|c| c.ends_with(".mlmodelc") && c.len() > ".mlmodelc".len())
    );
    if !ok {
        return Err(ForgeError::Format(format!(
            "ANE: L{layer} {group}: nazwa modelu '{name}' to nie pojedynczy plik *.mlmodelc"
        )));
    }
    Ok(dir.join(name))
}

fn parse_t(s: &str) -> Result<u32> {
    s.parse()
        .map_err(|_| ForgeError::Format(format!("ANE: kształt '{s}' nie jest liczbą")))
}

/// Zestaw kształtów z `FORGE_ANE_SHAPES`; `None` = wszystkie z manifestu.
fn allowed_shapes() -> Result<Option<Vec<u32>>> {
    match std::env::var(SHAPES_ENV) {
        Ok(spec) => parse_shape_set(&spec).map(Some),
        Err(_) => Ok(None),
    }
}

/// `256`, `256,512`; puste i białe znaki są odrzucane, żeby literówka
/// nie oznaczała po cichu „wszystkie”.
fn parse_shape_set(spec: &str) -> Result<Vec<u32>> {
    let mut out: Vec<u32> = spec
        .split(',')
        .map(|p| parse_t(p.trim()))
        .collect::<Result<_>>()?;
    out.sort_unstable();
    out.dedup();
    if out.is_empty() || out.contains(&0) {
        return Err(ForgeError::Format(format!(
            "ANE: {SHAPES_ENV}='{spec}' nie zawiera żadnego kształtu"
        )));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn projection_names_follow_the_manifest() {
        for (name, kind) in [
            ("q", AneProjKind::Q),
            ("k", AneProjKind::K),
            ("v", AneProjKind::V),
            ("o", AneProjKind::O),
            ("gate", AneProjKind::Gate),
            ("up", AneProjKind::Up),
            ("down", AneProjKind::Down),
        ] {
            assert_eq!(AneProjKind::parse(name), Some(kind));
        }
        assert_eq!(AneProjKind::parse("lm_head"), None);
    }

    #[test]
    fn the_manifest_shape_is_what_the_exporter_writes() {
        let text = r#"{"version":1,"groups":[{"layer":0,"group":"gate_up",
            "model":"L00_gate_up.mlmodelc","functions":{"256":"T256","1024":"T1024"},
            "input":{"name":"x","cols":4096},"output":{"name":"y","width":13440},
            "parts":[{"proj":"gate","rows":11264,"cols":4096,"ane0":4544,"ane_rows":6720,"out_col0":0},
                     {"proj":"up","rows":11264,"cols":4096,"ane0":4544,"ane_rows":6720,"out_col0":6720}]}]}"#;
        let m: ManifestJson = serde_json::from_str(text).expect("manifest");
        assert_eq!(m.groups.len(), 1);
        assert_eq!(m.groups[0].parts[1].out_col0, 6720);
        assert_eq!(m.groups[0].functions.as_ref().unwrap()["1024"], "T1024");
    }

    fn group_with(shapes: &[u32]) -> AneGroup {
        AneGroup {
            layer: 0,
            name: "gate_up".into(),
            in_name: "x".into(),
            out_name: "y".into(),
            in_cols: 4096,
            out_width: 13440,
            parts: Vec::new(),
            shapes: shapes.to_vec(),
        }
    }

    #[test]
    fn a_shape_at_least_as_large_takes_one_predict() {
        let g = group_with(&[256, 512, 1024]);
        let p = g.plan_for(300).expect("plan");
        assert_eq!((p.fn_idx, p.shape_tokens, p.predicts), (1, 512, 1));
        let p = g.plan_for(1024).expect("plan");
        assert_eq!((p.fn_idx, p.shape_tokens, p.predicts), (2, 1024, 1));
    }

    #[test]
    fn a_smaller_shape_is_tiled_with_the_largest_available() {
        let g = group_with(&[256]);
        let p = g.plan_for(1024).expect("plan");
        assert_eq!((p.shape_tokens, p.predicts), (256, 4));
        let p = g.plan_for(1000).expect("plan");
        assert_eq!((p.shape_tokens, p.predicts), (256, 4));
        let g = group_with(&[256, 512]);
        let p = g.plan_for(700).expect("plan");
        assert_eq!((p.shape_tokens, p.predicts), (512, 2));
        assert!(group_with(&[]).plan_for(256).is_none());
    }

    #[test]
    fn model_names_are_single_mlmodelc_components() {
        let dir = Path::new("/models");
        assert_eq!(
            model_path(dir, "L00_gate_up.mlmodelc", 0, "g").unwrap(),
            dir.join("L00_gate_up.mlmodelc")
        );
        for bad in [
            "../L00.mlmodelc",
            "/abs/L00.mlmodelc",
            "sub/L00.mlmodelc",
            "L00.mlpackage",
            ".mlmodelc",
            "",
        ] {
            assert!(model_path(dir, bad, 0, "g").is_err(), "{bad}");
        }
    }

    #[test]
    fn the_shape_set_is_parsed_strictly() {
        assert_eq!(parse_shape_set("256").unwrap(), vec![256]);
        assert_eq!(parse_shape_set("1024, 256,256").unwrap(), vec![256, 1024]);
        assert!(parse_shape_set("").is_err());
        assert!(parse_shape_set("256,").is_err());
        assert!(parse_shape_set("abc").is_err());
        assert!(parse_shape_set("0").is_err());
    }
}
