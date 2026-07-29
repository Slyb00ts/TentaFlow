// ===== File: multi_gpu.rs — podział pracy między RÓŻNE karty =====
//
// Cel: dwie karty o różnej mocy mają dawać SUMĘ swoich możliwości, a nie
// tempo najwolniejszej. Cały ten moduł jest o jednej rzeczy — jak wyznaczyć
// udziały, żeby obie skończyły w tym samym momencie.
//
// DLACZEGO NIE STAŁY STOSUNEK. Zmierzone na tej maszynie:
//
//   |                | RX 6900 XT | RX 7900 XT |
//   | odczyt DRAM    |  336 GB/s  |  735 GB/s  |  <- 7900 jest 2,19x szybsza
//   | int8 `dot4`    |   97 TOPS  |   43 TOPS  |  <- 6900 jest 2,26x szybsza
//
// Stosunek mocy tych kart ZALEŻY OD RODZAJU PRACY i potrafi się odwrócić.
// Dekodowanie jest ograniczone pasmem, prefill liczeniem — więc jeden podział
// dla obu byłby zły dla któregoś z nich. Stąd `WorkKind` i osobna waga.

use forge_types::{ForgeError, Result};

/// Rodzaj pracy decyduje, KTÓRA zmierzona przepustowość jest wagą podziału.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkKind {
    /// Dekodowanie: czas wyznacza odczyt wag z pamięci.
    MemoryBound,
    /// Prefill: czas wyznacza przepustowość mnożenia macierzy.
    ComputeBound,
}

/// Zmierzone możliwości jednej karty. Nic tu nie jest zgadywane z nazwy
/// urządzenia — wszystko pochodzi z kalibracji albo z obserwacji.
#[derive(Clone, Copy, Debug)]
pub struct DeviceCapability {
    pub stream_bytes_per_s: f64,
    pub matmul_ops_per_s: f64,
    /// Ile bajtów wag zmieści się jeszcze na tej karcie.
    pub free_bytes: usize,
}

impl DeviceCapability {
    fn rate(&self, kind: WorkKind) -> f64 {
        match kind {
            WorkKind::MemoryBound => self.stream_bytes_per_s,
            WorkKind::ComputeBound => self.matmul_ops_per_s,
        }
    }
}

/// Ile wierszy macierzy dostaje każde urządzenie. Suma jest DOKŁADNIE równa
/// liczbie wierszy — reszta z zaokrąglenia idzie do najszybszego urządzenia.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SplitPlan {
    pub rows: Vec<usize>,
}

impl SplitPlan {
    /// Przesunięcie pierwszego wiersza urządzenia `index`.
    pub fn offset(&self, index: usize) -> usize {
        self.rows[..index].iter().sum()
    }

    pub fn total(&self) -> usize {
        self.rows.iter().sum()
    }
}

/// Minimalny udział wierszy macierzy, poniżej którego nie warto angażować
/// karty: koszt wymiany aktywacji (zmierzone 6,45 us na 10 KiB) przestaje się
/// zwracać. Przy dzieleniu innych jednostek (np. warstw modelu w pipelinie)
/// próg jest INNY, dlatego `plan_split` przyjmuje go jako argument.
pub const MIN_USEFUL_ROWS: usize = 64;

/// Dzieli `rows` wierszy proporcjonalnie do zmierzonej mocy, respektując wolny
/// VRAM każdej karty.
///
/// `min_useful` to najmniejszy udział, jaki ma sens przydzielić — poniżej niego
/// praca wraca do najszybszej karty, bo wymiana kosztowałaby więcej niż zysk.
///
/// `bytes_per_row` służy wyłącznie do sprawdzenia, czy przydział się zmieści —
/// urządzenie, któremu zabrakłoby pamięci, dostaje tyle, ile utrzyma, a resztę
/// przejmują pozostałe.
pub fn plan_split(
    caps: &[DeviceCapability],
    rows: usize,
    kind: WorkKind,
    bytes_per_row: usize,
    min_useful: usize,
) -> Result<SplitPlan> {
    if caps.is_empty() {
        return Err(ForgeError::Scheduler("podział bez urządzeń".into()));
    }
    if caps.len() == 1 || rows < 2 * min_useful {
        // Jedna karta albo praca zbyt mała, żeby dzielenie się zwróciło.
        let mut plan = vec![0; caps.len()];
        plan[fastest(caps, kind)] = rows;
        return Ok(SplitPlan { rows: plan });
    }

    let capacity: Vec<usize> = caps
        .iter()
        .map(|c| {
            if bytes_per_row == 0 {
                rows
            } else {
                c.free_bytes / bytes_per_row
            }
        })
        .collect();
    if capacity.iter().sum::<usize>() < rows {
        return Err(ForgeError::OutOfMemory {
            requested: rows * bytes_per_row,
            available: capacity.iter().sum::<usize>() * bytes_per_row,
        });
    }

    let weights: Vec<f64> = caps.iter().map(|c| c.rate(kind).max(0.0)).collect();
    let total_weight: f64 = weights.iter().sum();
    if total_weight <= 0.0 {
        return Err(ForgeError::Scheduler(
            "żadne urządzenie nie zgłasza dodatniej przepustowości".into(),
        ));
    }

    // Przydział proporcjonalny, przycięty pojemnością. Nadwyżkę z przyciętych
    // urządzeń rozdajemy ponownie, aż wszystko będzie rozdane — jedno przejście
    // nie wystarczy, bo przycięcie zmienia proporcje dla reszty.
    let mut assigned = vec![0usize; caps.len()];
    let mut pool = rows;
    loop {
        let active: Vec<usize> = (0..caps.len())
            .filter(|&i| assigned[i] < capacity[i] && weights[i] > 0.0)
            .collect();
        if pool == 0 || active.is_empty() {
            break;
        }
        let active_weight: f64 = active.iter().map(|&i| weights[i]).sum();
        // WSZYSTKIE udziały liczone z TEGO SAMEGO stanu puli. Zmniejszanie jej
        // w trakcie pętli dawałoby kolejnym urządzeniom coraz mniejszą podstawę
        // i podział rozjeżdżał się o kilkadziesiąt procent.
        let snapshot = pool;
        let mut handed = 0usize;
        for &index in &active {
            let want = ((snapshot as f64) * weights[index] / active_weight).floor() as usize;
            let give = want.min(capacity[index] - assigned[index]);
            assigned[index] += give;
            handed += give;
        }
        if handed == 0 {
            break;
        }
        pool -= handed;
    }
    let mut remaining = pool;

    // Reszta z zaokrągleń: do najszybszego urządzenia, które ma jeszcze miejsce.
    while remaining > 0 {
        let Some(index) = order_by_rate(caps, kind)
            .into_iter()
            .find(|&i| assigned[i] < capacity[i])
        else {
            return Err(ForgeError::Scheduler(
                "nie ma gdzie umieścić reszty wierszy".into(),
            ));
        };
        let give = remaining.min(capacity[index] - assigned[index]);
        assigned[index] += give;
        remaining -= give;
    }

    // Udział mniejszy niż próg opłacalności oddajemy najszybszej karcie, która
    // go przyjmie — inaczej płacilibyśmy za wymianę aktywacji bez zysku.
    for index in 0..assigned.len() {
        if assigned[index] == 0 || assigned[index] >= min_useful {
            continue;
        }
        let orphan = assigned[index];
        if let Some(target) = order_by_rate(caps, kind)
            .into_iter()
            .find(|&i| i != index && capacity[i] - assigned[i] >= orphan)
        {
            assigned[target] += orphan;
            assigned[index] = 0;
        }
    }

    Ok(SplitPlan { rows: assigned })
}

fn fastest(caps: &[DeviceCapability], kind: WorkKind) -> usize {
    order_by_rate(caps, kind)[0]
}

/// Indeksy urządzeń od najszybszego. Przy remisie decyduje kolejność, żeby
/// podział był powtarzalny między uruchomieniami.
fn order_by_rate(caps: &[DeviceCapability], kind: WorkKind) -> Vec<usize> {
    let mut order: Vec<usize> = (0..caps.len()).collect();
    order.sort_by(|&a, &b| {
        caps[b]
            .rate(kind)
            .partial_cmp(&caps[a].rate(kind))
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.cmp(&b))
    });
    order
}

/// Ile najwyżej może się zmienić waga urządzenia w jednym kroku. Bez limitu
/// pojedynczy zakłócony pomiar (np. inny proces na karcie) przerzuciłby całą
/// pracę na drugą kartę i następny krok odbiłby z powrotem.
const MAX_STEP: f64 = 0.25;

/// Korekta możliwości na podstawie tego, co RZECZYWIŚCIE się wydarzyło.
///
/// Kalibracja startowa nigdy nie trafia idealnie: nie uwzględnia formatu wag,
/// kształtu warstwy ani tego, że jedna karta liczy inną rodziną kerneli. Dlatego
/// po każdym kroku porównujemy zmierzone czasy i przesuwamy wagi tak, żeby obie
/// karty kończyły razem.
///
/// `assigned` to wykonana praca (wiersze), `elapsed` to zmierzony czas każdej
/// karty. Urządzenia z zerowym przydziałem są pomijane — nie niosą informacji.
pub fn update_capability(
    caps: &mut [DeviceCapability],
    assigned: &[usize],
    elapsed_seconds: &[f64],
    kind: WorkKind,
    smoothing: f64,
) -> Result<()> {
    if caps.len() != assigned.len() || caps.len() != elapsed_seconds.len() {
        return Err(ForgeError::Scheduler(
            "korekta możliwości: niezgodne długości".into(),
        ));
    }
    if !(0.0..=1.0).contains(&smoothing) {
        return Err(ForgeError::Scheduler(
            "współczynnik wygładzania musi być w 0..=1".into(),
        ));
    }
    // Zmierzona wydajność w wierszach na sekundę. Jednostka zapamiętanej wagi
    // (bajty albo operacje) jest bez znaczenia — dla podziału liczy się WYŁĄCZNIE
    // stosunek, więc obserwacje normalizujemy do sumy dotychczasowych wag
    // uczestników. Dzięki temu skala nie dryfuje przez kolejne korekty.
    let participants: Vec<usize> = (0..caps.len())
        .filter(|&i| assigned[i] > 0 && elapsed_seconds[i] > 0.0)
        .collect();
    if participants.len() < 2 {
        return Ok(());
    }
    let observed: Vec<f64> = participants
        .iter()
        .map(|&i| assigned[i] as f64 / elapsed_seconds[i])
        .collect();
    let observed_sum: f64 = observed.iter().sum();
    let current_sum: f64 = participants.iter().map(|&i| caps[i].rate(kind)).sum();
    if observed_sum <= 0.0 || current_sum <= 0.0 {
        return Ok(());
    }
    for (slot, &index) in participants.iter().enumerate() {
        let current = caps[index].rate(kind);
        let target = observed[slot] / observed_sum * current_sum;
        let step = ((target - current) / current).clamp(-MAX_STEP, MAX_STEP);
        let updated = current * (1.0 + smoothing * step);
        match kind {
            WorkKind::MemoryBound => caps[index].stream_bytes_per_s = updated,
            WorkKind::ComputeBound => caps[index].matmul_ops_per_s = updated,
        }
    }
    Ok(())
}

/// Mierzy możliwości KAŻDEJ karty tym samym testem, zamiast wpisywać liczby.
///
/// Pasmo mierzymy kopią D2D w obrębie karty: to jedyny test niewymagający
/// kerneli, więc działa identycznie na architekturze bez WMMA i z WMMA — a
/// właśnie porównywalność między różnymi kartami jest tu celem. Kopia czyta i
/// zapisuje, stąd czynnik 2.
///
/// `free_bytes` bierzemy z urządzenia, bo to on ogranicza udział w podziale.
/// `quant` MUSI być formatem wag modelu, który faktycznie pojedzie. Stały probe
/// był błędem i widać to w liczbach: dla NVFP4 stosunek tych dwóch kart wychodzi
/// 1 : 8,8 (7900 XT liczy na WMMA, 6900 XT bez jednostki macierzowej kończy się
/// na wsadzie T=16), a dla Q4_K tych SAMYCH kart 0,95 : 1 — bo Q4_K nie ma
/// kernela WMMA i obie liczą go na `dot4`, gdzie szybsza jest 6900 XT. Jeden
/// pomiar nie opisuje obu przypadków, więc format musi wejść z zewnątrz.
/// Mierzy JEDNĄ kartę. Wydzielone z `calibrate`, bo klaster trzyma zestawy
/// kerneli w swoich strukturach i nie może z nich zrobić płaskiego wycinka.
pub fn measure_device(
    device: &dyn forge_hal::Device,
    kernels: &forge_kernels::Kernels,
    quant: forge_types::QuantKind,
) -> Result<DeviceCapability> {
    const PROBE_BYTES: usize = 128 << 20;
    const WARMUP: usize = 2;
    const ITERS: usize = 8;
        let stream = device.create_stream()?;
        let source = device.alloc(PROBE_BYTES, forge_types::MemKind::Device, forge_hal::Pool::Weights)?;
        let target = device.alloc(PROBE_BYTES, forge_types::MemKind::Device, forge_hal::Pool::Weights)?;
        for _ in 0..WARMUP {
            device.copy(&source, 0, &target, 0, PROBE_BYTES, &stream)?;
        }
        stream.synchronize()?;
        let started = std::time::Instant::now();
        for _ in 0..ITERS {
            device.copy(&source, 0, &target, 0, PROBE_BYTES, &stream)?;
        }
        stream.synchronize()?;
        let seconds = started.elapsed().as_secs_f64();
        let bytes_per_s = if seconds > 0.0 {
            2.0 * (PROBE_BYTES * ITERS) as f64 / seconds
        } else {
            0.0
        };
        let free_bytes = device.pool_available(forge_hal::Pool::Weights).unwrap_or(0);
    Ok(DeviceCapability {
        stream_bytes_per_s: bytes_per_s,
        matmul_ops_per_s: measure_matmul(device, kernels, quant)?,
        free_bytes,
    })
}

pub fn calibrate(
    devices: &[std::sync::Arc<dyn forge_hal::Device>],
    kernels: &[forge_kernels::Kernels],
    quant: forge_types::QuantKind,
) -> Result<Vec<DeviceCapability>> {
    if devices.len() != kernels.len() {
        return Err(ForgeError::Scheduler(
            "kalibracja: liczba urządzeń i zestawów kerneli musi być równa".into(),
        ));
    }
    const PROBE_BYTES: usize = 128 << 20;
    const WARMUP: usize = 2;
    const ITERS: usize = 8;
    let mut caps = Vec::with_capacity(devices.len());
    for (index, device) in devices.iter().enumerate() {
        caps.push(measure_device(device.as_ref(), &kernels[index], quant)?);
    }
    Ok(caps)
}

fn measure_matmul(
    device: &dyn forge_hal::Device,
    kernels: &forge_kernels::Kernels,
    quant: forge_types::QuantKind,
) -> Result<f64> {
    use forge_hal::Pool;
    use forge_types::{MemKind, QuantKind};
    const ROWS: usize = 4096;
    const COLS: usize = 4096;
    // Karty różnią się MAKSYMALNYM wsadem, jaki obsłuży ich ścieżka: bez
    // jednostki macierzowej NVFP4 kończy się na T=16. Schodzimy więc do
    // pierwszego wsadu, który dana karta uciągnie, i to jest jej realna
    // przepustowość prefillu — nie artefakt pomiaru, tylko właściwość karty
    // razem z kernelami, które na niej działają.
    const TOKEN_CANDIDATES: [usize; 3] = [128, 32, 16];
    const WARMUP: usize = 2;
    const ITERS: usize = 5;

    // Bajty na wiersz wynikają z układu bloku danego formatu.
    let weight_bytes = match quant {
        QuantKind::NVFP4Gguf => ROWS * (COLS / 64) * 36,
        QuantKind::Q8_0 => ROWS * (COLS / 32) * 34,
        QuantKind::Q4K => ROWS * (COLS / 256) * 144,
        other => {
            return Err(ForgeError::Unsupported(format!(
                "kalibracja liczenia nie ma ścieżki dla formatu {other:?}"
            )));
        }
    };
    let max_tokens = TOKEN_CANDIDATES[0];
    let weights = device.alloc(weight_bytes, MemKind::Device, Pool::Weights)?;
    let activations = device.alloc(max_tokens * COLS * 2, MemKind::Device, Pool::Weights)?;
    let output = device.alloc(max_tokens * ROWS * 2, MemKind::Device, Pool::Weights)?;
    let stream = device.create_stream()?;
    let mut last_error: Option<ForgeError> = None;
    for tokens in TOKEN_CANDIDATES {
        let run = || match quant {
            QuantKind::NVFP4Gguf => kernels.gemm_nvfp4_gguf_f16(
                &output,
                &weights,
                &activations,
                ROWS,
                COLS,
                tokens,
                1.0,
                &stream,
            ),
            // `gemm_q8_0_f16` to rodzina wymagająca artefaktów `_bm*`, których
            // AMD nie ma; wejście `_i8mma_at` jest wspólne dla wszystkich
            // architektur i samo schodzi na wariant dostępny na danej karcie.
            QuantKind::Q8_0 => kernels.gemm_q8_0_i8mma_at(
                &output,
                &weights,
                0,
                &activations,
                ROWS,
                COLS,
                tokens,
                &stream,
            ),
            // `gemm_q4_k_f16` to rodzina wymagajaca artefaktow `_bm*`, ktorych
            // AMD nie ma. Wejscie `_i8mma_at` jest wspolne dla wszystkich
            // architektur i samo schodzi na kafle `dot4` tam, gdzie nie ma
            // jednostki macierzowej — czyli mierzy to, czym karta REALNIE liczy.
            QuantKind::Q4K => kernels.gemm_q4_k_i8mma_at(
                &output,
                &weights,
                0,
                &activations,
                ROWS,
                COLS,
                tokens,
                &stream,
            ),
            _ => unreachable!("format odrzucony wyżej"),
        };
        if let Err(error) = run() {
            last_error = Some(error);
            continue;
        }
        for _ in 1..WARMUP {
            run()?;
        }
        stream.synchronize()?;
        let started = std::time::Instant::now();
        for _ in 0..ITERS {
            run()?;
        }
        stream.synchronize()?;
        let seconds = started.elapsed().as_secs_f64();
        if !(seconds > 0.0) {
            continue;
        }
        let ops = 2.0 * ROWS as f64 * COLS as f64 * tokens as f64 * ITERS as f64;
        return Ok(ops / seconds);
    }
    Err(ForgeError::Unsupported(match last_error {
        Some(error) => format!("kalibracja liczenia ({quant:?}) nie przeszła: {error}"),
        None => format!("kalibracja liczenia ({quant:?}): brak wsadu do zmierzenia"),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Zmierzone na tej maszynie — te liczby są punktem odniesienia testów.
    fn measured_pair() -> Vec<DeviceCapability> {
        vec![
            DeviceCapability {
                stream_bytes_per_s: 336e9,
                matmul_ops_per_s: 97e12,
                free_bytes: 16 << 30,
            },
            DeviceCapability {
                stream_bytes_per_s: 735e9,
                matmul_ops_per_s: 43e12,
                free_bytes: 20 << 30,
            },
        ]
    }

    #[test]
    fn podzial_idzie_za_pasmem_w_dekodowaniu() {
        let plan = plan_split(&measured_pair(), 17408, WorkKind::MemoryBound, 0, MIN_USEFUL_ROWS).unwrap();
        assert_eq!(plan.total(), 17408);
        // 336 / (336+735) = 31,4%
        let share = plan.rows[0] as f64 / 17408.0;
        assert!(
            (share - 0.314).abs() < 0.01,
            "udział 6900 XT w dekodowaniu: {share}"
        );
    }

    /// To jest sedno: w prefillu stosunek tych kart jest ODWROTNY, bo RDNA3
    /// zdegradowała `dot4`. Stały podział byłby tu zły o rząd wielkości.
    #[test]
    fn podzial_odwraca_sie_dla_pracy_ograniczonej_liczeniem() {
        let caps = measured_pair();
        let decode = plan_split(&caps, 17408, WorkKind::MemoryBound, 0, MIN_USEFUL_ROWS).unwrap();
        let prefill = plan_split(&caps, 17408, WorkKind::ComputeBound, 0, MIN_USEFUL_ROWS).unwrap();
        assert!(
            decode.rows[0] < decode.rows[1],
            "w dekodowaniu więcej ma dostać 7900 XT"
        );
        assert!(
            prefill.rows[0] > prefill.rows[1],
            "w prefillu więcej ma dostać 6900 XT"
        );
        // 97 / (97+43) = 69,3%
        let share = prefill.rows[0] as f64 / 17408.0;
        assert!((share - 0.693).abs() < 0.01, "udział 6900 XT w prefillu: {share}");
    }

    #[test]
    fn suma_udzialow_jest_dokladna_takze_przy_brzydkich_liczbach() {
        for rows in [129, 1000, 4097, 17408, 248320] {
            let plan = plan_split(&measured_pair(), rows, WorkKind::MemoryBound, 0, MIN_USEFUL_ROWS).unwrap();
            assert_eq!(plan.total(), rows, "rows={rows}");
            assert_eq!(plan.offset(0), 0);
            assert_eq!(plan.offset(1), plan.rows[0]);
        }
    }

    #[test]
    fn ciasny_vram_przesuwa_prace_na_druga_karte() {
        let mut caps = measured_pair();
        // Szybsza karta ma miejsce tylko na 1000 wierszy.
        caps[1].free_bytes = 1000 * 4096;
        let plan = plan_split(&caps, 8000, WorkKind::MemoryBound, 4096, MIN_USEFUL_ROWS).unwrap();
        assert_eq!(plan.total(), 8000);
        assert!(plan.rows[1] <= 1000, "przydział ponad pojemność: {:?}", plan.rows);
        assert_eq!(plan.rows[0], 8000 - plan.rows[1]);
    }

    #[test]
    fn brak_lacznej_pojemnosci_jest_bledem_a_nie_cichym_przycieciem() {
        let mut caps = measured_pair();
        caps[0].free_bytes = 1024;
        caps[1].free_bytes = 1024;
        let error = plan_split(&caps, 8000, WorkKind::MemoryBound, 4096, MIN_USEFUL_ROWS).unwrap_err();
        assert!(matches!(error, ForgeError::OutOfMemory { .. }));
    }

    #[test]
    fn mala_praca_nie_jest_dzielona() {
        // Poniżej progu opłacalności wszystko idzie na najszybszą kartę —
        // wymiana aktywacji kosztuje 6,45 us i przy paru wierszach się nie zwraca.
        let plan = plan_split(&measured_pair(), 64, WorkKind::MemoryBound, 0, MIN_USEFUL_ROWS).unwrap();
        assert_eq!(plan.rows, vec![0, 64]);
    }

    #[test]
    fn jedno_urzadzenie_dostaje_calosc() {
        let caps = vec![measured_pair()[0]];
        let plan = plan_split(&caps, 5000, WorkKind::MemoryBound, 0, MIN_USEFUL_ROWS).unwrap();
        assert_eq!(plan.rows, vec![5000]);
    }

    #[test]
    fn korekta_zblizakonce_do_siebie() {
        let mut caps = measured_pair();
        // Start ze ZŁEGO założenia: obie karty uznane za równe.
        caps[0].stream_bytes_per_s = 500e9;
        caps[1].stream_bytes_per_s = 500e9;
        // Prawda: karta 1 jest 2x szybsza.
        let truth = [336e9, 735e9];
        let rows = 10000;
        let mut spread_before = f64::MAX;
        for _ in 0..40 {
            let plan = plan_split(&caps, rows, WorkKind::MemoryBound, 0, MIN_USEFUL_ROWS).unwrap();
            let elapsed: Vec<f64> = (0..2)
                .map(|i| plan.rows[i] as f64 / truth[i])
                .collect();
            let spread = (elapsed[0] - elapsed[1]).abs() / elapsed[0].max(elapsed[1]);
            spread_before = spread;
            update_capability(&mut caps, &plan.rows, &elapsed, WorkKind::MemoryBound, 0.5)
                .unwrap();
        }
        // Po zbieżności obie karty kończą praktycznie razem.
        assert!(
            spread_before < 0.05,
            "różnica czasów po korekcie: {spread_before}"
        );
    }

    #[test]
    fn korekta_nie_oscyluje_przy_zaklóconym_pomiarze() {
        let mut caps = measured_pair();
        let before = caps[0].stream_bytes_per_s;
        // Jeden absurdalny pomiar (np. inny proces zajął kartę).
        update_capability(
            &mut caps,
            &[5000, 5000],
            &[100.0, 0.001],
            WorkKind::MemoryBound,
            0.5,
        )
        .unwrap();
        let change = (caps[0].stream_bytes_per_s - before).abs() / before;
        assert!(
            change <= MAX_STEP * 0.5 + 1e-9,
            "pojedynczy pomiar przesunął wagę o {change}"
        );
    }

    #[test]
    fn niezgodne_dlugosci_i_zly_wspolczynnik_sa_bledem() {
        let mut caps = measured_pair();
        assert!(update_capability(&mut caps, &[1], &[1.0], WorkKind::MemoryBound, 0.5).is_err());
        assert!(
            update_capability(&mut caps, &[1, 1], &[1.0, 1.0], WorkKind::MemoryBound, 2.0)
                .is_err()
        );
    }
}
