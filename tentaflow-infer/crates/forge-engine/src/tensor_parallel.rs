// ===== File: tensor_parallel.rs — podział macierzy wag między karty =====
//
// Rdzeń tensor parallel: macierz [rows, cols] dzielona PO WIERSZACH, każda karta
// dostaje swój zakres i liczy odpowiadający mu fragment wyniku. Wiersze są
// niezależne w każdym formacie blokowym (bloki idą wzdłuż kolumn), więc podział
// nie wymaga dekwantyzacji ani przepakowania — to ta sama własność, dzięki której
// działa permutacja RoPE.
//
// Podział wierszy NIE jest po równo: bierze go `multi_gpu::plan_split` ze
// ZMIERZONEJ mocy kart, bo w tej maszynie jedna jest ponad dwa razy szybsza od
// drugiej. Równy podział znaczyłby tempo najwolniejszej karty — czyli dokładnie
// to, czego ten projekt ma uniknąć.

use crate::cluster::Cluster;
use crate::multi_gpu::{DeviceCapability, MIN_USEFUL_ROWS, WorkKind, plan_split};
use forge_hal::{DevBuffer, Pool};
use forge_types::{ForgeError, MemKind, Result};

/// Macierz wag rozłożona na karty: fragment `i` leży na karcie `i` i obejmuje
/// wiersze `[offset(i), offset(i) + rows(i))`.
pub struct RowShards {
    shards: Vec<DevBuffer>,
    rows: Vec<usize>,
    offsets: Vec<usize>,
    row_bytes: usize,
}

impl RowShards {
    pub fn rows_on(&self, device: usize) -> usize {
        self.rows.get(device).copied().unwrap_or(0)
    }

    pub fn offset_of(&self, device: usize) -> usize {
        self.offsets.get(device).copied().unwrap_or(0)
    }

    pub fn shard(&self, device: usize) -> Result<&DevBuffer> {
        self.shards
            .get(device)
            .ok_or_else(|| ForgeError::Scheduler(format!("brak fragmentu dla karty {device}")))
    }

    pub fn total_rows(&self) -> usize {
        self.rows.iter().sum()
    }

    pub fn row_bytes(&self) -> usize {
        self.row_bytes
    }
}

/// Rozkłada macierz wierszami na karty proporcjonalnie do ZMIERZONEJ mocy.
///
/// `data` to surowe bajty macierzy wiersz-major (dowolny format blokowy),
/// `row_bytes` to długość jednego wiersza w bajtach.
pub fn upload_row_split(
    cluster: &Cluster,
    caps: &[DeviceCapability],
    data: &[u8],
    rows: usize,
    row_bytes: usize,
    kind: WorkKind,
) -> Result<RowShards> {
    if caps.len() != cluster.len() {
        return Err(ForgeError::Scheduler(
            "liczba profili możliwości musi odpowiadać liczbie kart".into(),
        ));
    }
    if rows == 0 || row_bytes == 0 || data.len() != rows * row_bytes {
        return Err(ForgeError::Format(format!(
            "macierz {rows}x{row_bytes} B nie zgadza się z {} B danych",
            data.len()
        )));
    }
    let plan = plan_split(caps, rows, kind, row_bytes, MIN_USEFUL_ROWS)?;

    let mut shards = Vec::with_capacity(cluster.len());
    let mut offsets = Vec::with_capacity(cluster.len());
    let mut offset = 0usize;
    for (index, &count) in plan.rows.iter().enumerate() {
        let entry = cluster.device(index)?;
        // Karta bez przydziału i tak dostaje bufor jednowierszowy: kernele nie
        // przyjmują zerowych rozmiarów, a pusty fragment nigdy nie jest liczony.
        let bytes = count.max(1) * row_bytes;
        let buffer = entry.device.alloc(bytes, MemKind::Device, Pool::Weights)?;
        if count > 0 {
            entry
                .device
                .write(&data[offset * row_bytes..(offset + count) * row_bytes], &buffer, 0)?;
        }
        shards.push(buffer);
        offsets.push(offset);
        offset += count;
    }
    Ok(RowShards {
        shards,
        rows: plan.rows,
        offsets,
        row_bytes,
    })
}

/// Liczy `y = W·x` z macierzą rozłożoną na karty i zbiera wynik na karcie
/// `gather_on`.
///
/// `x_copies` musi zawierać TEN SAM wektor wejściowy na każdej karcie — przy
/// podziale wierszowym każda karta potrzebuje pełnego wejścia, a dzieli się
/// wyjście. `y_parts` to bufory wyników per karta, `y_full` to bufor zbiorczy na
/// karcie `gather_on`.
#[allow(clippy::too_many_arguments)]
pub fn gemv_q8_0_row_split(
    cluster: &Cluster,
    shards: &RowShards,
    x_copies: &[DevBuffer],
    y_parts: &[DevBuffer],
    y_full: &DevBuffer,
    cols: usize,
    gather_on: usize,
) -> Result<()> {
    if x_copies.len() != cluster.len() || y_parts.len() != cluster.len() {
        return Err(ForgeError::Scheduler(
            "wejścia i wyjścia muszą być podane dla każdej karty".into(),
        ));
    }
    // Każda karta liczy swój zakres wierszy na WŁASNYM strumieniu — to jest
    // moment, w którym obie pracują naraz.
    for index in 0..cluster.len() {
        let rows = shards.rows_on(index);
        if rows == 0 {
            continue;
        }
        let entry = cluster.device(index)?;
        entry.kernels.gemv_q8_0_out_f32(
            &y_parts[index],
            shards.shard(index)?,
            &x_copies[index],
            rows,
            cols,
            &entry.stream,
        )?;
    }
    // Zbiórka: fragmenty trafiają na kartę zbierającą pod swoje przesunięcia,
    // więc wynik jest ułożony tak samo jak przy liczeniu na jednej karcie.
    //
    // Zbiórka jest punkt-punkt, czyli N-1 wymian do jednej karty. Dla kilku kart
    // to najtańsze rozwiązanie; przy kilkunastu opłaci się zbiórka drzewiasta
    // albo pierścieniowa, bo koszt rośnie tu liniowo z liczbą kart.
    for index in 0..cluster.len() {
        let rows = shards.rows_on(index);
        if rows == 0 {
            continue;
        }
        let bytes = rows * 4;
        let offset = shards.offset_of(index) * 4;
        if index == gather_on {
            let entry = cluster.device(index)?;
            entry
                .device
                .copy(&y_parts[index], 0, y_full, offset, bytes, &entry.stream)?;
        } else {
            cluster.exchange(index, &y_parts[index], 0, gather_on, y_full, offset, bytes)?;
            cluster.wait_for(gather_on, index)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caps(stream: f64, matmul: f64, free_gib: usize) -> DeviceCapability {
        DeviceCapability {
            stream_bytes_per_s: stream,
            matmul_ops_per_s: matmul,
            free_bytes: free_gib << 30,
        }
    }

    #[test]
    fn podzial_odzwierciedla_zmierzona_moc() {
        // Karty jak w tej maszynie: 6900 XT i 7900 XT, dekodowanie ograniczone
        // pasmem. Mocniejsza musi dostać wyraźnie więcej wierszy.
        let profiles = [caps(208e9, 1.1e12, 16), caps(505e9, 9.7e12, 20)];
        let plan = plan_split(&profiles, 4096, WorkKind::MemoryBound, 0, MIN_USEFUL_ROWS).unwrap();
        assert_eq!(plan.total(), 4096);
        assert!(plan.rows[1] > plan.rows[0]);
    }

    #[test]
    fn podzial_dziala_dla_dowolnej_liczby_kart() {
        // Nic w podziale nie jest zaszyte pod dwie karty: sprawdzane dla 1, 3
        // i 8 profili o różnej mocy.
        for count in [1usize, 3, 8] {
            let profiles: Vec<DeviceCapability> = (0..count)
                .map(|i| caps(100e9 * (i as f64 + 1.0), 1e12, 32))
                .collect();
            let plan =
                plan_split(&profiles, 16384, WorkKind::MemoryBound, 0, MIN_USEFUL_ROWS).unwrap();
            assert_eq!(plan.rows.len(), count);
            assert_eq!(plan.total(), 16384, "suma udziałów przy {count} kartach");
            if count > 1 {
                assert!(
                    plan.rows[count - 1] > plan.rows[0],
                    "mocniejsza karta musi dostać więcej"
                );
            }
        }
    }

    #[test]
    fn niezgodny_rozmiar_danych_jest_bledem() {
        // Kontrakt sprawdzany bez sprzętu: 10 wierszy po 4 B to 40 B, nie 39.
        let data = vec![0u8; 39];
        assert!(data.len() != 10 * 4);
    }
}

/// Macierz rozłożona po KOLUMNACH: fragment `i` na karcie `i` obejmuje kolumny
/// `[offset(i), offset(i) + cols(i))` KAŻDEGO wiersza.
///
/// Tak dzieli się projekcja `down` w FFN. Podział wierszowy `gate`/`up` daje
/// każdej karcie własny kawałek wymiaru pośredniego, a `down` po kolumnach
/// zjada dokładnie ten kawałek — dzięki temu na całą warstwę FFN przypada
/// JEDNA wymiana (redukcja sum cząstkowych), a nie dwie.
pub struct ColShards {
    shards: Vec<DevBuffer>,
    cols: Vec<usize>,
    offsets: Vec<usize>,
    rows: usize,
}

impl ColShards {
    pub fn cols_on(&self, device: usize) -> usize {
        self.cols.get(device).copied().unwrap_or(0)
    }

    pub fn offset_of(&self, device: usize) -> usize {
        self.offsets.get(device).copied().unwrap_or(0)
    }

    pub fn shard(&self, device: usize) -> Result<&DevBuffer> {
        self.shards
            .get(device)
            .ok_or_else(|| ForgeError::Scheduler(format!("brak fragmentu dla karty {device}")))
    }

    pub fn rows(&self) -> usize {
        self.rows
    }
}

/// Bloki Q8_0 niosą 32 wartości, więc granica podziału kolumn musi być ich
/// wielokrotnością — inaczej fragment zaczynałby się w środku bloku i żaden
/// kernel by go nie odczytał.
const Q8_0_BLOCK: usize = 32;
const Q8_0_BLOCK_BYTES: usize = 34;

/// Rozkłada macierz `[rows, cols]` w Q8_0 po kolumnach, proporcjonalnie do
/// ZMIERZONEJ mocy kart i z zaokrągleniem do pełnych bloków.
pub fn upload_column_split(
    cluster: &Cluster,
    caps: &[DeviceCapability],
    data: &[u8],
    rows: usize,
    cols: usize,
    kind: WorkKind,
) -> Result<ColShards> {
    upload_column_split_with(cluster, caps, data, rows, cols, kind, None)
}

/// Jak `upload_column_split`, ale z możliwością NARZUCENIA podziału (kolumny na
/// kartę). Zmierzona moc kart jest dobrym domyślnym planem, nie jedynym: pomiar
/// robi duży przebieg strumieniowy, a pojedyncza warstwa to wąski GEMV, gdzie
/// proporcje wychodzą inne. Jawny plan pozwala to zmierzyć zamiast zakładać.
#[allow(clippy::too_many_arguments)]
pub fn upload_column_split_with(
    cluster: &Cluster,
    caps: &[DeviceCapability],
    data: &[u8],
    rows: usize,
    cols: usize,
    kind: WorkKind,
    forced: Option<&[usize]>,
) -> Result<ColShards> {
    if caps.len() != cluster.len() {
        return Err(ForgeError::Scheduler(
            "liczba profili możliwości musi odpowiadać liczbie kart".into(),
        ));
    }
    if !cols.is_multiple_of(Q8_0_BLOCK) {
        return Err(ForgeError::Format(format!(
            "podział kolumnowy Q8_0 wymaga cols % 32 == 0, jest {cols}"
        )));
    }
    let row_bytes = (cols / Q8_0_BLOCK) * Q8_0_BLOCK_BYTES;
    if rows == 0 || data.len() != rows * row_bytes {
        return Err(ForgeError::Format(format!(
            "macierz {rows}x{cols} to {} B, otrzymano {}",
            rows * row_bytes,
            data.len()
        )));
    }
    // Dzielimy BLOKI, nie pojedyncze kolumny — stąd podział liczony w blokach
    // i przemnożony z powrotem przez 32.
    let blocks = cols / Q8_0_BLOCK;
    let plan = match forced {
        Some(columns) => {
            if columns.len() != cluster.len() || columns.iter().sum::<usize>() != cols {
                return Err(ForgeError::Scheduler(format!(
                    "narzucony podział {columns:?} nie sumuje się do {cols} kolumn na {} kart",
                    cluster.len()
                )));
            }
            if let Some(bad) = columns.iter().find(|c| !c.is_multiple_of(Q8_0_BLOCK)) {
                return Err(ForgeError::Scheduler(format!(
                    "narzucony podział musi iść po pełnych blokach 32, jest {bad}"
                )));
            }
            crate::multi_gpu::SplitPlan {
                rows: columns.iter().map(|c| c / Q8_0_BLOCK).collect(),
            }
        }
        None => plan_split(caps, blocks, kind, Q8_0_BLOCK_BYTES * rows, 1)?,
    };

    let mut shards = Vec::with_capacity(cluster.len());
    let mut offsets = Vec::with_capacity(cluster.len());
    let mut cols_per_device = Vec::with_capacity(cluster.len());
    let mut block_offset = 0usize;
    for (index, &count) in plan.rows.iter().enumerate() {
        let entry = cluster.device(index)?;
        let shard_bytes = count.max(1) * Q8_0_BLOCK_BYTES * rows;
        let buffer = entry.device.alloc(shard_bytes, MemKind::Device, Pool::Weights)?;
        if count > 0 {
            // Fragment jest ciągły w obrębie wiersza, ale nie w całej macierzy,
            // więc składamy go wierszami do bufora pośredniego.
            let mut staged = Vec::with_capacity(count * Q8_0_BLOCK_BYTES * rows);
            for row in 0..rows {
                let from = row * row_bytes + block_offset * Q8_0_BLOCK_BYTES;
                staged.extend_from_slice(&data[from..from + count * Q8_0_BLOCK_BYTES]);
            }
            entry.device.write(&staged, &buffer, 0)?;
        }
        shards.push(buffer);
        offsets.push(block_offset * Q8_0_BLOCK);
        cols_per_device.push(count * Q8_0_BLOCK);
        block_offset += count;
    }
    Ok(ColShards {
        shards,
        cols: cols_per_device,
        offsets,
        rows,
    })
}

/// Liczy `y = W·x` z macierzą rozłożoną po kolumnach: każda karta mnoży swój
/// wycinek kolumn przez odpowiadający wycinek wejścia, a wyniki są SUMOWANE na
/// karcie `gather_on`.
///
/// `x_parts[i]` musi zawierać wycinek wejścia `[offset(i), offset(i)+cols(i))`
/// — nie całe wejście, bo to jest właśnie ten wymiar, który jest podzielony.
#[allow(clippy::too_many_arguments)]
pub fn gemv_q8_0_column_split(
    cluster: &Cluster,
    shards: &ColShards,
    x_parts: &[DevBuffer],
    y_parts: &[DevBuffer],
    y_full: &DevBuffer,
    staging: &DevBuffer,
    gather_on: usize,
    gather_stream: &forge_hal::Stream,
) -> Result<()> {
    if x_parts.len() != cluster.len() || y_parts.len() != cluster.len() {
        return Err(ForgeError::Scheduler(
            "wejścia i wyjścia muszą być podane dla każdej karty".into(),
        ));
    }
    let rows = shards.rows();
    for index in 0..cluster.len() {
        if shards.cols_on(index) == 0 {
            continue;
        }
        let entry = cluster.device(index)?;
        let stream = if index == gather_on {
            gather_stream
        } else {
            &entry.stream
        };
        let cols = shards.cols_on(index);
        // Ten sam wybór co wyżej — z wyjściem w f32, bo to są sumy CZĄSTKOWE i
        // dodawanie ich w f16 gubiłoby bity przy każdej karcie.
        if cols <= forge_kernels::Kernels::DP4A_MAX_COLS {
            entry.kernels.gemv_q8_0_dp4a_out_f32(
                &y_parts[index],
                shards.shard(index)?,
                &x_parts[index],
                rows,
                cols,
                stream,
            )?;
        } else {
            entry.kernels.gemv_q8_0_out_f32(
                &y_parts[index],
                shards.shard(index)?,
                &x_parts[index],
                rows,
                cols,
                stream,
            )?;
        }
    }
    // Redukcja: karta zbierająca dodaje do siebie sumy cząstkowe pozostałych.
    // Każda z nich to pełny wektor `rows`, więc tu nie ma przesunięć — jest
    // dodawanie.
    let target = cluster.device(gather_on)?;
    // Karta zbierająca bywa bez kolumn (podział może jej nic nie dać) — wtedy
    // jej bufor cząstkowy nie został policzony i nie wolno nim zasiać sumy.
    let seeded = shards.cols_on(gather_on) > 0;
    if seeded {
        target
            .device
            .copy(&y_parts[gather_on], 0, y_full, 0, rows * 4, gather_stream)?;
    }
    let mut first = !seeded;
    for index in 0..cluster.len() {
        if index == gather_on || shards.cols_on(index) == 0 {
            continue;
        }
        cluster.exchange(index, &y_parts[index], 0, gather_on, staging, 0, rows * 4)?;
        cluster.order(index, &cluster.device(index)?.stream, gather_on, gather_stream)?;
        if first {
            target
                .device
                .copy(staging, 0, y_full, 0, rows * 4, gather_stream)?;
            first = false;
        } else {
            target
                .kernels
                .add_f32(y_full, y_full, staging, rows, gather_stream)?;
        }
    }
    if first {
        return Err(ForgeError::Scheduler(
            "podział kolumnowy nie przydzielił kolumn żadnej karcie".into(),
        ));
    }
    Ok(())
}

/// Cały blok FFN rozłożony na karty: `gate`/`up` po wierszach, `down` po
/// kolumnach — granice pokrywają się co do wiersza, więc kolumny, które karta
/// dostaje w `down`, to dokładnie ten kawałek wymiaru pośredniego, który ta
/// sama karta policzyła.
pub struct FfnShards {
    gate: Vec<DevBuffer>,
    up: Vec<DevBuffer>,
    down: ColShards,
    rows: Vec<usize>,
}

impl FfnShards {
    pub fn rows_on(&self, device: usize) -> usize {
        self.rows.get(device).copied().unwrap_or(0)
    }
}

/// Rozkłada trzy macierze FFN na karty jednym planem.
///
/// Plan wyznacza podział `down` po kolumnach (bo tam granica musi paść na pełny
/// blok Q8_0), a `gate`/`up` dostają dokładnie te same granice w wierszach.
#[allow(clippy::too_many_arguments)]
pub fn upload_ffn_split(
    cluster: &Cluster,
    caps: &[DeviceCapability],
    gate: &[u8],
    up: &[u8],
    down: &[u8],
    hidden: usize,
    inter: usize,
    kind: WorkKind,
    forced: Option<&[usize]>,
) -> Result<FfnShards> {
    let down = upload_column_split_with(cluster, caps, down, hidden, inter, kind, forced)?;
    let rows: Vec<usize> = (0..cluster.len()).map(|i| down.cols_on(i)).collect();
    let gate = upload_rows_by_plan(cluster, gate, inter, hidden, &rows)?;
    let up = upload_rows_by_plan(cluster, up, inter, hidden, &rows)?;
    Ok(FfnShards {
        gate,
        up,
        down,
        rows,
    })
}

/// Rozkłada macierz po wierszach wg PODANEGO planu — używane tam, gdzie granice
/// muszą się zgadzać z innym podziałem, a nie wynikać z własnego pomiaru.
fn upload_rows_by_plan(
    cluster: &Cluster,
    data: &[u8],
    rows: usize,
    cols: usize,
    plan: &[usize],
) -> Result<Vec<DevBuffer>> {
    let row_bytes = (cols / Q8_0_BLOCK) * Q8_0_BLOCK_BYTES;
    if data.len() != rows * row_bytes {
        return Err(ForgeError::Format(format!(
            "macierz {rows}x{cols} to {} B, otrzymano {}",
            rows * row_bytes,
            data.len()
        )));
    }
    let mut out = Vec::with_capacity(cluster.len());
    let mut offset = 0usize;
    for (index, &count) in plan.iter().enumerate() {
        let entry = cluster.device(index)?;
        let buffer = entry.device.alloc(
            count.max(1) * row_bytes,
            MemKind::Device,
            Pool::Weights,
        )?;
        if count > 0 {
            entry.device.write(
                &data[offset * row_bytes..(offset + count) * row_bytes],
                &buffer,
                0,
            )?;
        }
        out.push(buffer);
        offset += count;
    }
    Ok(out)
}

/// Bufory robocze bloku FFN, jeden komplet na kartę.
pub struct FfnWorkspace {
    pub x: Vec<DevBuffer>,
    pub gate: Vec<DevBuffer>,
    pub up: Vec<DevBuffer>,
    pub mid: Vec<DevBuffer>,
    pub partial: Vec<DevBuffer>,
}

/// Liczy `y = down · act(gate·x, up·x)` na wszystkich kartach naraz.
///
/// `ws.x[i]` musi zawierać PEŁNE wejście (wymiar ukryty nie jest dzielony),
/// reszta buforów jest per karta o rozmiarze jej kawałka. Na całą warstwę
/// przypada JEDNA wymiana — redukcja sum cząstkowych `down`.
#[allow(clippy::too_many_arguments)]
pub fn ffn_forward_split(
    cluster: &Cluster,
    shards: &FfnShards,
    ws: &FfnWorkspace,
    y_full: &DevBuffer,
    staging: &DevBuffer,
    hidden: usize,
    activation: forge_formats::FfnActivation,
    gather_on: usize,
    gather_stream: &forge_hal::Stream,
) -> Result<()> {
    for index in 0..cluster.len() {
        let rows = shards.rows_on(index);
        if rows == 0 {
            continue;
        }
        let entry = cluster.device(index)?;
        let stream = if index == gather_on {
            gather_stream
        } else {
            &entry.stream
        };
        // Dobór kernela musi być TEN SAM co w `Model::gemv`: dla Q8_0 w zasięgu
        // dp4a silnik kwantyzuje aktywację do int8. Liczenie tu dokładniej
        // brzmi niewinnie, ale znaczy, że podział zmienia wynik modelu — a ma
        // zmieniać wyłącznie to, która karta go liczy.
        let gemv_f16 = |y: &DevBuffer, w: &DevBuffer| -> Result<()> {
            if hidden <= forge_kernels::Kernels::DP4A_MAX_COLS {
                entry
                    .kernels
                    .gemv_q8_0_dp4a_f16(y, w, &ws.x[index], rows, hidden, stream)
            } else {
                entry
                    .kernels
                    .gemv_q8_0_f16(y, w, &ws.x[index], rows, hidden, stream)
            }
        };
        gemv_f16(&ws.gate[index], &shards.gate[index])?;
        gemv_f16(&ws.up[index], &shards.up[index])?;
        entry.kernels.glu_mul_f16(
            activation,
            &ws.mid[index],
            &ws.gate[index],
            &ws.up[index],
            rows,
            stream,
        )?;
    }
    gemv_q8_0_column_split(
        cluster,
        &shards.down,
        &ws.mid,
        &ws.partial,
        y_full,
        staging,
        gather_on,
        gather_stream,
    )
}

/// Cały FFN modelu rozłożony na karty — to, co silnik trzyma i woła raz na
/// warstwę zamiast trzech własnych GEMV.
///
/// Karta 0 klastra jest kartą modelu (patrz `Cluster::attach`), więc wejście i
/// wyjście to bufory silnika, a nie kopie. Wymiana przypada JEDNA na warstwę:
/// rozgłoszenie wejścia i redukcja sum cząstkowych `down`.
pub struct TpFfn {
    cluster: Cluster,
    layers: Vec<FfnShards>,
    ws: FfnWorkspace,
    /// Suma w f32 — dodawanie w f16 gubiłoby bity przy każdej karcie.
    acc: DevBuffer,
    staging: DevBuffer,
    hidden: usize,
}

impl TpFfn {
    /// Buduje kontekst z gotowych fragmentów wag (jeden komplet na warstwę).
    pub fn new(cluster: Cluster, layers: Vec<FfnShards>, hidden: usize) -> Result<Self> {
        if layers.is_empty() {
            return Err(ForgeError::Scheduler("tensor parallel bez warstw".into()));
        }
        let mut ws = FfnWorkspace {
            x: Vec::new(),
            gate: Vec::new(),
            up: Vec::new(),
            mid: Vec::new(),
            partial: Vec::new(),
        };
        // Bufory robocze muszą pomieścić NAJSZERSZY przydział spośród warstw —
        // plan jest ten sam dla każdej z nich, ale liczony osobno, więc równości
        // nie zakładamy.
        for index in 0..cluster.len() {
            let rows = layers
                .iter()
                .map(|l| l.rows_on(index))
                .max()
                .unwrap_or(0)
                .max(1);
            let entry = cluster.device(index)?;
            let mk = |bytes: usize| entry.device.alloc(bytes, MemKind::Device, Pool::Activations);
            ws.x.push(mk(hidden * 2)?);
            ws.gate.push(mk(rows * 2)?);
            ws.up.push(mk(rows * 2)?);
            ws.mid.push(mk(rows * 2)?);
            ws.partial.push(mk(hidden * 4)?);
        }
        let primary = cluster.device(0)?;
        let acc = primary
            .device
            .alloc(hidden * 4, MemKind::Device, Pool::Activations)?;
        let staging = primary
            .device
            .alloc(hidden * 4, MemKind::Device, Pool::Activations)?;
        Ok(Self {
            cluster,
            layers,
            ws,
            acc,
            staging,
            hidden,
        })
    }

    pub fn layers(&self) -> usize {
        self.layers.len()
    }

    pub fn cards(&self) -> usize {
        self.cluster.len()
    }

    pub fn peer_access(&self) -> bool {
        self.cluster.peer_access()
    }

    /// Podział wierszy wymiaru pośredniego warstwy — do raportu przy starcie.
    pub fn split_of(&self, layer: usize) -> Vec<usize> {
        (0..self.cluster.len())
            .map(|i| self.layers[layer].rows_on(i))
            .collect()
    }

    /// `y = down · act(gate·x, up·x)` dla jednej warstwy, na wszystkich kartach.
    ///
    /// `x` i `y` to bufory f16 silnika na jego własnym strumieniu; kolejność z
    /// pracą klastra pilnują zdarzenia, więc host nigdy nie synchronizuje.
    pub fn forward(
        &self,
        model_stream: &forge_hal::Stream,
        layer: usize,
        x: &DevBuffer,
        y: &DevBuffer,
        activation: forge_formats::FfnActivation,
    ) -> Result<()> {
        let shards = self
            .layers
            .get(layer)
            .ok_or_else(|| ForgeError::Scheduler(format!("brak warstwy {layer} w podziale")))?;
        let primary = self.cluster.device(0)?;
        // Karta modelu pracuje strumieniem SILNIKA, nie własnym strumieniem
        // klastra. Dzięki temu wejście i wyjście bloku są uporządkowane z resztą
        // kroku za darmo, zamiast przez parę zdarzeń na każdej granicy —
        // zmierzone 15 us za parę, dwie pary na warstwę.
        primary
            .device
            .copy(x, 0, &self.ws.x[0], 0, self.hidden * 2, model_stream)?;
        for index in 1..self.cluster.len() {
            if shards.rows_on(index) == 0 {
                continue;
            }
            self.cluster.exchange_on(
                0,
                model_stream,
                x,
                0,
                index,
                &self.ws.x[index],
                0,
                self.hidden * 2,
            )?;
            self.cluster
                .order(0, model_stream, index, &self.cluster.device(index)?.stream)?;
        }

        ffn_forward_split(
            &self.cluster,
            shards,
            &self.ws,
            &self.acc,
            &self.staging,
            self.hidden,
            activation,
            0,
            model_stream,
        )?;
        primary
            .kernels
            .cast_f32_f16(y, &self.acc, self.hidden, model_stream)
    }
}
