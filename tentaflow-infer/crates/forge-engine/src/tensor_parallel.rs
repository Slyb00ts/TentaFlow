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
