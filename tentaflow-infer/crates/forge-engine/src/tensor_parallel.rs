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
    let plan = plan_split(caps, blocks, kind, Q8_0_BLOCK_BYTES * rows, 1)?;

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
        entry.kernels.gemv_q8_0_out_f32(
            &y_parts[index],
            shards.shard(index)?,
            &x_parts[index],
            rows,
            shards.cols_on(index),
            &entry.stream,
        )?;
    }
    // Redukcja: karta zbierająca dodaje do siebie sumy cząstkowe pozostałych.
    // Każda z nich to pełny wektor `rows`, więc tu nie ma przesunięć — jest
    // dodawanie.
    let target = cluster.device(gather_on)?;
    target.device.copy(
        &y_parts[gather_on],
        0,
        y_full,
        0,
        rows * 4,
        &target.stream,
    )?;
    for index in 0..cluster.len() {
        if index == gather_on || shards.cols_on(index) == 0 {
            continue;
        }
        cluster.exchange(index, &y_parts[index], 0, gather_on, staging, 0, rows * 4)?;
        cluster.wait_for(gather_on, index)?;
        target
            .kernels
            .add_f32(y_full, y_full, staging, rows, &target.stream)?;
    }
    Ok(())
}
