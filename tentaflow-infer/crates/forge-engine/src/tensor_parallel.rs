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
use crate::multi_gpu::{plan_split, DeviceCapability, WorkKind, MIN_USEFUL_ROWS};
use forge_hal::{DevBuffer, Pool};
use forge_types::{ForgeError, MemKind, QuantKind, Result};

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
            entry.device.write(
                &data[offset * row_bytes..(offset + count) * row_bytes],
                &buffer,
                0,
            )?;
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
    format: BlockFormat,
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

    /// Pełna szerokość macierzy przed podziałem.
    pub fn total_cols(&self) -> usize {
        self.cols.iter().sum()
    }
}

/// Blok formatu kwantyzacji: ile wartości niesie i ile zajmuje bajtów.
///
/// Granica podziału kolumn MUSI paść na całe bloki — fragment zaczynający się w
/// środku bloku nie ma jak zostać odczytany przez żaden kernel. Stąd podział
/// liczony jest w blokach, a nie w kolumnach.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct BlockFormat {
    pub values: usize,
    pub bytes: usize,
    pub quant: QuantKind,
    /// Mnożnik całego tensora. GGUF NVFP4 trzyma go obok bloków i kernel GEMV
    /// przyjmuje go argumentem; pozostałe formaty mają go wtopiony w bloki i
    /// przekazują 1.0. Jest własnością KONKRETNEJ macierzy, nie formatu, więc
    /// `gate` i `up` mogą mieć ten sam `quant` i różne skale — dlatego porównanie
    /// formatów idzie po `quant`, a nie po całej strukturze.
    pub output_scale: f32,
}

impl BlockFormat {
    pub fn of(quant: QuantKind, output_scale: f32) -> Result<Self> {
        let (values, bytes) = match quant {
            QuantKind::Q8_0 => (32, 34),
            QuantKind::Q4K => (256, 144),
            QuantKind::Q6K => (256, 210),
            QuantKind::NVFP4Gguf => (64, 36),
            other => {
                return Err(ForgeError::Unsupported(format!(
                    "podział FFN na karty nie ma ścieżki dla formatu {other:?}"
                )));
            }
        };
        if !output_scale.is_finite() || output_scale <= 0.0 {
            return Err(ForgeError::Format(format!(
                "skala tensora musi być skończona i dodatnia, otrzymano {output_scale}"
            )));
        }
        Ok(Self {
            values,
            bytes,
            quant,
            output_scale,
        })
    }

    fn row_bytes(&self, cols: usize) -> usize {
        (cols / self.values) * self.bytes
    }
}

/// Rozkłada macierz `[rows, cols]` w Q8_0 po kolumnach, proporcjonalnie do
/// ZMIERZONEJ mocy kart i z zaokrągleniem do pełnych bloków.
pub fn upload_column_split(
    cluster: &Cluster,
    caps: &[DeviceCapability],
    data: &[u8],
    rows: usize,
    cols: usize,
    kind: WorkKind,
    format: BlockFormat,
) -> Result<ColShards> {
    upload_column_split_with(cluster, caps, data, rows, cols, kind, format, None)
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
    format: BlockFormat,
    forced: Option<&[usize]>,
) -> Result<ColShards> {
    if caps.len() != cluster.len() {
        return Err(ForgeError::Scheduler(
            "liczba profili możliwości musi odpowiadać liczbie kart".into(),
        ));
    }
    if !cols.is_multiple_of(format.values) {
        return Err(ForgeError::Format(format!(
            "podział kolumnowy {:?} wymaga cols % {} == 0, jest {cols}",
            format.quant, format.values
        )));
    }
    let row_bytes = format.row_bytes(cols);
    if rows == 0 || data.len() != rows * row_bytes {
        return Err(ForgeError::Format(format!(
            "macierz {rows}x{cols} to {} B, otrzymano {}",
            rows * row_bytes,
            data.len()
        )));
    }
    // Dzielimy BLOKI, nie pojedyncze kolumny — stąd podział liczony w blokach
    // i przemnożony z powrotem przez 32.
    let blocks = cols / format.values;
    let plan = match forced {
        Some(columns) => {
            if columns.len() != cluster.len() || columns.iter().sum::<usize>() != cols {
                return Err(ForgeError::Scheduler(format!(
                    "narzucony podział {columns:?} nie sumuje się do {cols} kolumn na {} kart",
                    cluster.len()
                )));
            }
            if let Some(bad) = columns.iter().find(|c| !c.is_multiple_of(format.values)) {
                return Err(ForgeError::Scheduler(format!(
                    "narzucony podział musi iść po pełnych blokach {}, jest {bad}",
                    format.values
                )));
            }
            crate::multi_gpu::SplitPlan {
                rows: columns.iter().map(|c| c / format.values).collect(),
            }
        }
        None => plan_split(caps, blocks, kind, format.bytes * rows, 1)?,
    };

    let mut shards = Vec::with_capacity(cluster.len());
    let mut offsets = Vec::with_capacity(cluster.len());
    let mut cols_per_device = Vec::with_capacity(cluster.len());
    let mut block_offset = 0usize;
    for (index, &count) in plan.rows.iter().enumerate() {
        let entry = cluster.device(index)?;
        let shard_bytes = count.max(1) * format.bytes * rows;
        let buffer = entry
            .device
            .alloc(shard_bytes, MemKind::Device, Pool::Weights)?;
        if count > 0 {
            // Fragment jest ciągły w obrębie wiersza, ale nie w całej macierzy,
            // więc składamy go wierszami do bufora pośredniego.
            let mut staged = Vec::with_capacity(count * format.bytes * rows);
            for row in 0..rows {
                let from = row * row_bytes + block_offset * format.bytes;
                staged.extend_from_slice(&data[from..from + count * format.bytes]);
            }
            entry.device.write(&staged, &buffer, 0)?;
        }
        shards.push(buffer);
        offsets.push(block_offset * format.values);
        cols_per_device.push(count * format.values);
        block_offset += count;
    }
    Ok(ColShards {
        shards,
        cols: cols_per_device,
        offsets,
        rows,
        format,
    })
}

/// Liczy `y = W·x` z macierzą rozłożoną po kolumnach: każda karta mnoży swój
/// wycinek kolumn przez odpowiadający wycinek wejścia, a wyniki są SUMOWANE na
/// karcie `gather_on`.
///
/// `x_parts[i]` musi zawierać wycinek wejścia `[offset(i), offset(i)+cols(i))`
/// — nie całe wejście, bo to jest właśnie ten wymiar, który jest podzielony.
#[allow(clippy::too_many_arguments)]
pub fn gemv_column_split(
    cluster: &Cluster,
    shards: &ColShards,
    x_parts: &[&DevBuffer],
    y_parts: &[DevBuffer],
    y_full: &DevBuffer,
    out_f16: Option<&DevBuffer>,
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
        //
        // Próg dp4a liczony z PEŁNEJ szerokości, nie z kawałka karty. Macierz
        // szersza od progu idzie bez dp4a na jednej karcie, ale jej połowy już
        // się w progu mieszczą — wybór po kawałku znaczyłby, że podział zmienia
        // matematykę modelu, a ma zmieniać wyłącznie to, która karta go liczy.
        let dp4a = shards.total_cols() <= forge_kernels::Kernels::DP4A_MAX_COLS;
        match (shards.format.quant, dp4a) {
            (QuantKind::Q8_0, true) => entry.kernels.gemv_q8_0_dp4a_out_f32(
                &y_parts[index],
                shards.shard(index)?,
                x_parts[index],
                rows,
                cols,
                stream,
            )?,
            (QuantKind::Q8_0, false) => entry.kernels.gemv_q8_0_out_f32(
                &y_parts[index],
                shards.shard(index)?,
                x_parts[index],
                rows,
                cols,
                stream,
            )?,
            (QuantKind::NVFP4Gguf, _) => entry.kernels.gemv_nvfp4_gguf_q8_1_out_f32(
                &y_parts[index],
                shards.shard(index)?,
                x_parts[index],
                rows,
                cols,
                shards.format.output_scale,
                stream,
            )?,
            (QuantKind::Q4K, true) => entry.kernels.gemv_q4_k_dp4a_out_f32(
                &y_parts[index],
                0,
                shards.shard(index)?,
                x_parts[index],
                0,
                rows,
                cols,
                stream,
            )?,
            (QuantKind::Q4K, false) => entry.kernels.gemv_q4_k_out_f32(
                &y_parts[index],
                0,
                shards.shard(index)?,
                x_parts[index],
                0,
                rows,
                cols,
                stream,
            )?,
            (QuantKind::Q6K, _) => entry.kernels.gemv_q6_k_out_f32(
                &y_parts[index],
                0,
                shards.shard(index)?,
                x_parts[index],
                0,
                rows,
                cols,
                stream,
            )?,
            (other, _) => {
                return Err(ForgeError::Unsupported(format!(
                    "podział FFN nie ma ścieżki GEMV dla {other:?}"
                )));
            }
        }
    }
    // Redukcja: karta zbierająca dodaje do siebie sumy cząstkowe pozostałych.
    // Każda z nich to pełny wektor `rows`, więc tu nie ma przesunięć — jest
    // dodawanie.
    let target = cluster.device(gather_on)?;
    let contributors: Vec<usize> = (0..cluster.len())
        .filter(|&index| shards.cols_on(index) > 0)
        .collect();
    let Some((&last, rest)) = contributors.split_last() else {
        return Err(ForgeError::Scheduler(
            "podział kolumnowy nie przydzielił kolumn żadnej karcie".into(),
        ));
    };
    // Sumy cząstkowe wszystkich kart poza OSTATNIĄ trafiają do `y_full`. Ostatnia
    // domyka redukcję — i gdy wołający chce wyniku w f16, to domknięcie jest tym
    // samym uruchomieniem co zawężenie. Osobne dodawanie i osobne zawężenie
    // kosztowały dwa uruchomienia na warstwę, czyli 130 na token przy zmierzonym
    // narzucie rzędu 4,5 us — więcej niż cała wymiana aktywacji między kartami.
    let mut accumulated = 0usize;
    for &index in rest {
        let bring = |destination: &DevBuffer| -> Result<()> {
            if index == gather_on {
                target
                    .device
                    .copy(&y_parts[index], 0, destination, 0, rows * 4, gather_stream)
            } else {
                cluster.exchange(
                    index,
                    &y_parts[index],
                    0,
                    gather_on,
                    destination,
                    0,
                    rows * 4,
                )?;
                cluster.order(
                    index,
                    &cluster.device(index)?.stream,
                    gather_on,
                    gather_stream,
                )
            }
        };
        if accumulated == 0 {
            bring(y_full)?;
        } else {
            bring(staging)?;
            target
                .kernels
                .add_f32(y_full, y_full, staging, rows, gather_stream)?;
        }
        accumulated += 1;
    }

    let tail = if last == gather_on {
        &y_parts[last]
    } else {
        cluster.exchange(last, &y_parts[last], 0, gather_on, staging, 0, rows * 4)?;
        cluster.order(
            last,
            &cluster.device(last)?.stream,
            gather_on,
            gather_stream,
        )?;
        staging
    };
    match (out_f16, accumulated) {
        (Some(out), 0) => target.kernels.cast_f32_f16(out, tail, rows, gather_stream),
        (Some(out), _) => target
            .kernels
            .add_f32_out_f16(out, y_full, tail, rows, gather_stream),
        (None, 0) => target
            .device
            .copy(tail, 0, y_full, 0, rows * 4, gather_stream),
        (None, _) => target
            .kernels
            .add_f32(y_full, y_full, tail, rows, gather_stream),
    }
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
    /// Formaty `gate` i `up`. Mogą być INNE niż `down`: `Q4_K_M` trzyma projekcję
    /// `down` w Q6_K. Dzielenie po wierszach nie stawia warunku na granicę, więc
    /// wystarczy, że pokrywa się ona z granicą bloków `down`. Kwantyzacja `gate`
    /// i `up` jest ta sama, ale skala tensora bywa różna, więc oba dostają swój
    /// opis.
    gate_format: BlockFormat,
    up_format: BlockFormat,
}

impl FfnShards {
    pub fn rows_on(&self, device: usize) -> usize {
        self.rows.get(device).copied().unwrap_or(0)
    }
}

/// Ile kolumn wymiaru pośredniego dostaje każda karta, licząc pojemność dla
/// WSZYSTKICH `layers` warstw naraz.
///
/// Plan musi powstać raz dla całego modelu, a nie osobno przy każdej warstwie.
/// Wolny VRAM w profilach kart jest odczytem sprzed ładowania, więc plan liczony
/// per warstwa uznawał całą pulę za dostępną 65 razy z rzędu — karta modelu, na
/// której leży już cały model, dostawała udział mieszczący się raz i kończyła
/// brakiem pamięci w połowie ładowania zamiast wziąć mniej.
pub fn plan_ffn_split(
    caps: &[DeviceCapability],
    hidden: usize,
    inter: usize,
    kind: WorkKind,
    gate_format: BlockFormat,
    down_format: BlockFormat,
    layers: usize,
) -> Result<Vec<usize>> {
    if layers == 0 {
        return Err(ForgeError::Scheduler("podział FFN bez warstw".into()));
    }
    if !inter.is_multiple_of(down_format.values) {
        return Err(ForgeError::Format(format!(
            "podział kolumnowy {:?} wymaga inter % {} == 0, jest {inter}",
            down_format.quant, down_format.values
        )));
    }
    // Blok podziału to `down_format.values` kolumn wymiaru pośredniego. Kosztuje
    // tyle bajtów `down`, ile ma wierszy ukrytych, plus tyle wierszy `gate` i
    // `up`, ile obejmuje — te dwie macierze idą tym samym planem, więc muszą
    // wejść do wyceny, inaczej jest ona trzykrotnie za niska.
    let per_block = (down_format.bytes * hidden
        + 2 * down_format.values * gate_format.row_bytes(hidden))
        * layers;
    Ok(
        plan_split(caps, inter / down_format.values, kind, per_block, 1)?
            .rows
            .iter()
            .map(|blocks| blocks * down_format.values)
            .collect(),
    )
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
    gate_format: BlockFormat,
    up_format: BlockFormat,
    down_format: BlockFormat,
    forced: Option<&[usize]>,
) -> Result<FfnShards> {
    if gate_format.quant != up_format.quant {
        return Err(ForgeError::Unsupported(format!(
            "gate i up dzieli jeden plan, więc muszą mieć tę samą kwantyzację, jest {:?} i {:?}",
            gate_format.quant, up_format.quant
        )));
    }
    // Plan liczony JEDEN RAZ dla całej trójki i dopiero potem narzucony podziałowi
    // kolumnowemu. Sam `upload_column_split_with` wycenia wyłącznie `down`, a
    // karta rezerwuje jeszcze swój kawałek `gate` i `up` — czyli około trzy razy
    // więcej. Przy takiej wycenie karta modelu, której pula wag jest już zajęta
    // przez cały model, dostawała udział, jakiego nie miała gdzie umieścić, i
    // podział kończył się brakiem pamięci zamiast mniejszym udziałem.
    let plan = match forced {
        Some(columns) => columns.to_vec(),
        None => plan_ffn_split(caps, hidden, inter, kind, gate_format, down_format, 1)?,
    };
    let down = upload_column_split_with(
        cluster,
        caps,
        down,
        hidden,
        inter,
        kind,
        down_format,
        Some(&plan),
    )?;
    let rows: Vec<usize> = (0..cluster.len()).map(|i| down.cols_on(i)).collect();
    let gate = upload_rows_by_plan(cluster, gate, inter, hidden, &rows, gate_format)?;
    let up = upload_rows_by_plan(cluster, up, inter, hidden, &rows, up_format)?;
    Ok(FfnShards {
        gate,
        up,
        down,
        rows,
        gate_format,
        up_format,
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
    format: BlockFormat,
) -> Result<Vec<DevBuffer>> {
    let row_bytes = format.row_bytes(cols);
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
        let buffer =
            entry
                .device
                .alloc(count.max(1) * row_bytes, MemKind::Device, Pool::Weights)?;
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
    /// Wejście per karta. Karta zbierająca ma `None` — czyta wprost z bufora
    /// wołającego, więc kopiowanie go do własnego bufora roboczego byłoby
    /// uruchomieniem na warstwę bez żadnego skutku.
    pub x: Vec<Option<DevBuffer>>,
    pub gate: Vec<DevBuffer>,
    pub up: Vec<DevBuffer>,
    pub mid: Vec<DevBuffer>,
    pub partial: Vec<DevBuffer>,
}

/// Liczy `y = down · act(gate·x, up·x)` na wszystkich kartach naraz.
///
/// Wejście karty zbierającej to `x_primary`, pozostałych — `ws.x[i]`, i każde z
/// nich musi zawierać PEŁNE wejście (wymiar ukryty nie jest dzielony). Reszta
/// buforów jest per karta o rozmiarze jej kawałka. `out_f16` domyka redukcję
/// zawężeniem do f16; `None` zostawia sumę w `y_full`.
#[allow(clippy::too_many_arguments)]
pub fn ffn_forward_split(
    cluster: &Cluster,
    shards: &FfnShards,
    ws: &FfnWorkspace,
    x_primary: &DevBuffer,
    y_full: &DevBuffer,
    out_f16: Option<&DevBuffer>,
    staging: &DevBuffer,
    hidden: usize,
    activation: forge_formats::FfnActivation,
    gather_on: usize,
    gather_stream: &forge_hal::Stream,
) -> Result<()> {
    let input = |index: usize| -> Result<&DevBuffer> {
        if index == gather_on {
            return Ok(x_primary);
        }
        ws.x[index]
            .as_ref()
            .ok_or_else(|| ForgeError::Scheduler(format!("karta {index} nie ma bufora wejścia")))
    };
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
        let dp4a = hidden <= forge_kernels::Kernels::DP4A_MAX_COLS;
        let gemv_f16 = |y: &DevBuffer, w: &DevBuffer, format: BlockFormat| -> Result<()> {
            match (format.quant, dp4a) {
                (QuantKind::Q8_0, true) => {
                    entry
                        .kernels
                        .gemv_q8_0_dp4a_f16(y, w, input(index)?, rows, hidden, stream)
                }
                (QuantKind::Q8_0, false) => {
                    entry
                        .kernels
                        .gemv_q8_0_f16(y, w, input(index)?, rows, hidden, stream)
                }
                (QuantKind::Q4K, true) => {
                    entry
                        .kernels
                        .gemv_q4_k_dp4a_f16(y, w, input(index)?, rows, hidden, stream)
                }
                (QuantKind::Q4K, false) => {
                    entry
                        .kernels
                        .gemv_q4_k_f16(y, w, input(index)?, rows, hidden, stream)
                }
                (QuantKind::Q6K, _) => {
                    entry
                        .kernels
                        .gemv_q6_k_f16(y, w, input(index)?, rows, hidden, stream)
                }
                (QuantKind::NVFP4Gguf, _) => entry.kernels.gemv_nvfp4_gguf_f16(
                    y,
                    w,
                    input(index)?,
                    rows,
                    hidden,
                    format.output_scale,
                    stream,
                ),
                (other, _) => Err(ForgeError::Unsupported(format!(
                    "podział FFN nie ma ścieżki GEMV dla {other:?}"
                ))),
            }
        };
        // GGUF NVFP4 ma grupowy wariant, który kwantyzuje aktywację do Q8_1 RAZ
        // dla obu projekcji i liczy je przez dp4a — to jest ten sam kernel,
        // którym `gate`/`up` idą bez podziału. Wołanie tu wariantu f16 byłoby
        // liczeniem połowy pracy wolniejszą matematyką, czyli oddaniem części
        // zysku z drugiej karty zanim się pojawi.
        if shards.gate_format.quant == QuantKind::NVFP4Gguf {
            entry.kernels.gemv_nvfp4_gguf_q8_1_group_f16(
                &[
                    forge_kernels::Nvfp4GgufQ8Projection {
                        output: &ws.gate[index],
                        weights: &shards.gate[index],
                        rows,
                        output_scale: shards.gate_format.output_scale,
                    },
                    forge_kernels::Nvfp4GgufQ8Projection {
                        output: &ws.up[index],
                        weights: &shards.up[index],
                        rows,
                        output_scale: shards.up_format.output_scale,
                    },
                ],
                input(index)?,
                hidden,
                stream,
            )?;
        } else {
            gemv_f16(&ws.gate[index], &shards.gate[index], shards.gate_format)?;
            gemv_f16(&ws.up[index], &shards.up[index], shards.up_format)?;
        }
        entry.kernels.glu_mul_f16(
            activation,
            &ws.mid[index],
            &ws.gate[index],
            &ws.up[index],
            rows,
            stream,
        )?;
    }
    let mid: Vec<&DevBuffer> = ws.mid.iter().collect();
    gemv_column_split(
        cluster,
        &shards.down,
        &mid,
        &ws.partial,
        y_full,
        out_f16,
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
            let mk = |bytes: usize| {
                entry
                    .device
                    .alloc(bytes, MemKind::Device, Pool::Activations)
            };
            // Karta 0 jest kartą modelu i liczy wprost z jego bufora.
            ws.x.push(if index == 0 {
                None
            } else {
                Some(mk(hidden * 2)?)
            });
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
        // Karta modelu pracuje strumieniem SILNIKA, nie własnym strumieniem
        // klastra. Dzięki temu wejście i wyjście bloku są uporządkowane z resztą
        // kroku za darmo, zamiast przez parę zdarzeń na każdej granicy —
        // zmierzone 15 us za parę, dwie pary na warstwę. Sama karta modelu liczy
        // wprost z `x`, więc rozgłoszenie dotyczy tylko kart wspierających.
        for index in 1..self.cluster.len() {
            if shards.rows_on(index) == 0 {
                continue;
            }
            let destination = self.ws.x[index].as_ref().ok_or_else(|| {
                ForgeError::Scheduler(format!("karta {index} nie ma bufora wejścia"))
            })?;
            self.cluster.exchange_on(
                0,
                model_stream,
                x,
                0,
                index,
                destination,
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
            x,
            &self.acc,
            Some(y),
            &self.staging,
            self.hidden,
            activation,
            0,
            model_stream,
        )
    }
}
