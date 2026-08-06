// ===== File: gemm/mxf4.rs — natywne blokowo-skalowane MMA na czterech bitach =====
//
// Osobno od `nvfp4.rs`, bo to inna rodzina: tam wartości NVFP4 są rozpakowywane
// programowo i mnożone jak f16 albo int8, tu tensor core czyta e2m1 wprost, a
// skale jadą w samej instrukcji. Wspólna jest tylko nazwa formatu.
use super::*;

/// Bramka samej instrukcji blokowo-skalowanej; `src/mma_fp4.mojo`.
const MMA_MXF4_PROBE: &str = "mma_mxf4_probe";

/// To samo dla skal per 16 w E4M3, czyli układu bloku `NVFP4Gguf`.
const MMA_NVF4_PROBE: &str = "mma_nvf4_probe";

/// Ile instrukcji wykonuje jeden pas w `mma_rate_*`; `_RATE_STEPS * _RATE_MMAS`.
pub const MMA_RATE_OPS: u64 = 2048 * 8;

/// Rodzaj instrukcji mierzony przez `mma_rate`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MmaKind {
    /// `kind::mxf4`, k=64, skale per 32 w UE8M0.
    Mxf4,
    /// `kind::mxf4nvf4`, k=64, skale per 16 w UE4M3.
    Nvf4,
    /// Zwykłe `m16n8k32.e4m3` — to, czym mnoży dzisiejsza ścieżka FP8.
    E4m3,
    /// `m16n8k16.f16` — kafel przenośny, na którym stoi reszta katalogu.
    F16,
}

impl MmaKind {
    fn artifact(self) -> &'static str {
        match self {
            MmaKind::Mxf4 => "mma_rate_mxf4",
            MmaKind::Nvf4 => "mma_rate_nvf4",
            MmaKind::E4m3 => "mma_rate_e4m3",
            MmaKind::F16 => "mma_rate_f16",
        }
    }

    /// Mnożenia-dodawania na jedną instrukcję: k=64 wobec k=32.
    pub fn macs(self) -> u64 {
        match self {
            MmaKind::Mxf4 | MmaKind::Nvf4 => 16 * 8 * 64,
            MmaKind::E4m3 => 16 * 8 * 32,
            MmaKind::F16 => 16 * 8 * 16,
        }
    }
}

impl Kernels {
    /// Czy karta i artefakty niosą blokowo-skalowane MMA na czterech bitach.
    ///
    /// `sm_121a` przyjmuje `kind::mxf4.block_scale`, a `sm_121` bez sufiksu tej
    /// samej linii odmawia — to instrukcje właściwe architekturze, więc obecność
    /// artefaktu jest tu jedynym wiarygodnym testem.
    pub fn supports_mxf4_block_scale(&self) -> bool {
        let caps = self.device.caps();
        forge_types::nvidia_warp32(caps.vendor, caps.warp_size)
            && self.artifacts.has(MMA_MXF4_PROBE)
            && self.artifacts.has(MMA_NVF4_PROBE)
    }

    /// Jedna instrukcja `m16n8k64` e2m1·e2m1: `nvf4` wybiera skale per 16 w
    /// E4M3 (`NVFP4Gguf`) zamiast per 32 w UE8M0 (`MXFP4`).
    ///
    /// Rejestry A i B przychodzą już ułożone tak, jak chce instrukcja, bo to,
    /// co ta bramka sprawdza, to INSTRUKCJA i odwzorowanie rejestrów, a nie
    /// kafelkowanie. Każdy kafel FP4 zbudowany później liczy tym samym `_mma_mxf4`.
    /// Skale są per pas, bo selektor wskazuje WĄTEK w czwórce jako dostawcę
    /// słowa skali — wartość rozgłoszona nie powiedziałaby, który pas trafił.
    pub fn mma_mxf4_probe(
        &self,
        d: &DevBuffer,
        a: &DevBuffer,
        b: &DevBuffer,
        scale_a: &DevBuffer,
        scale_b: &DevBuffer,
        nvf4: bool,
        stream: &Stream,
    ) -> Result<()> {
        if d.len() < 128 * 4
            || a.len() < 32 * 16
            || b.len() < 32 * 8
            || scale_a.len() < 32 * 4
            || scale_b.len() < 32 * 4
        {
            return Err(ForgeError::Kernel(
                "mma_mxf4_probe: bufor jest mniejszy od fragmentu jednej osnowy".into(),
            ));
        }
        let k = self
            .artifacts
            .get(if nvf4 { MMA_NVF4_PROBE } else { MMA_MXF4_PROBE })?;
        let cfg = LaunchConfig {
            grid: (1, 1, 1),
            block: (32, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(d)
            .buf(a)
            .buf(b)
            .buf(scale_a)
            .buf(scale_b);
        self.device.launch(k, &cfg, &args, stream)
    }
}

impl Kernels {
    /// Tempo wydawania jednej instrukcji mma, z operandami już w rejestrach.
    ///
    /// Nie mierzy kafla i nie ma mierzyć: kafel wnosi pamięć, której obie
    /// rodziny nie dzielą, a pytanie brzmi, czy jednostka macierzowa w ogóle
    /// oddaje cztery bity szybciej niż osiem. Odpowiedź na nie decyduje, czy
    /// budowanie kafla FP4 ma sens.
    pub fn mma_rate(
        &self,
        kind: MmaKind,
        d: &DevBuffer,
        a: &DevBuffer,
        blocks: u32,
        threads: u32,
        stream: &Stream,
    ) -> Result<()> {
        if d.len() < (blocks * threads) as usize * 4 || a.len() < 32 * 16 {
            return Err(ForgeError::Kernel(
                "mma_rate: bufor jest mniejszy od siatki".into(),
            ));
        }
        let k = self.artifacts.get(kind.artifact())?;
        let cfg = LaunchConfig {
            grid: (blocks, 1, 1),
            block: (threads, 1, 1),
            shared_mem_bytes: 0,
        };
        self.device
            .launch(k, &cfg, &LaunchArgs::new().buf(d).buf(a), stream)
    }
}

/// Wierszy wag i tokenów na blok wariantów `gemm_nvfp4_mma_f16_*`.
const MMA_TILE: [(&str, usize, usize, u32); 3] = [
    ("gemm_nvfp4_mma_f16_bm128_bn256", 128, 256, 256),
    ("gemm_nvfp4_mma_f16_bm128_bn128", 128, 128, 256),
    ("gemm_nvfp4_mma_f16_bm64_bn64", 64, 64, 128),
];

impl Kernels {
    /// Kwantyzuje aktywacje f16 do bloków NVFP4 plus mnożnik na token.
    ///
    /// Osobne przejście, a nie część GEMM-u, bo ten sam skwantyzowany bufor
    /// karmi wszystkie projekcje warstwy: kwantyzowanie go w każdym GEMM-ie
    /// płaciłoby za to samo cztery razy.
    pub fn quantize_act_nvfp4(
        &self,
        xq: &DevBuffer,
        xs: &DevBuffer,
        x: &DevBuffer,
        cols: usize,
        tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        if cols == 0 || !cols.is_multiple_of(64) {
            return Err(ForgeError::Kernel(format!(
                "quantize_act_nvfp4 wymaga cols % 64 == 0, otrzymano {cols}"
            )));
        }
        if xq.len() < tokens * cols / 64 * 36 || xs.len() < tokens * 4 {
            return Err(ForgeError::Kernel(
                "quantize_act_nvfp4: bufor wyjsciowy jest za maly".into(),
            ));
        }
        let k = self.artifacts.get("quantize_act_nvfp4")?;
        let cfg = LaunchConfig {
            grid: (u32::try_from(tokens).unwrap_or(u32::MAX), 1, 1),
            block: (256, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new().buf(xq).buf(xs).buf(x).scalar(cols as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// GEMM, w ktorym CZTERY BITY SA PO OBU STRONACH.
    ///
    /// Waga wchodzi wprost z blokow GGUF NVFP4, aktywacja z
    /// `quantize_act_nvfp4`. Wynik `[token, row]` — ten sam uklad co w rodzinie
    /// f16, wiec wywolujacy nie widzi, ktorym kernelem policzono.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_nvfp4_mma_f16(
        &self,
        y: &DevBuffer,
        weights: &DevBuffer,
        xq: &DevBuffer,
        xs: &DevBuffer,
        rows: usize,
        cols: usize,
        tokens: usize,
        output_scale: f32,
        stream: &Stream,
    ) -> Result<()> {
        if rows == 0 || tokens == 0 || cols < 64 || !cols.is_multiple_of(64) {
            return Err(ForgeError::Kernel(format!(
                "gemm_nvfp4_mma_f16 wymaga cols % 64 == 0, otrzymano rows={rows}, cols={cols}, tokens={tokens}"
            )));
        }
        let block_bytes = cols / 64 * 36;
        if weights.len() < rows * block_bytes
            || xq.len() < tokens * block_bytes
            || xs.len() < tokens * 4
            || y.len() < rows * tokens * 2
        {
            return Err(ForgeError::Kernel(
                "gemm_nvfp4_mma_f16: bufor jest mniejszy od ksztaltu".into(),
            ));
        }
        // Wiekszy kafel wygrywa, dopoki jest czym go wypelnic; przy krotkim
        // wsadzie polowa jego tokenow liczylaby zera.
        let (name, brows, btok, threads) = MMA_TILE
            .iter()
            .copied()
            .find(|(name, _, btok, _)| tokens >= *btok && self.artifacts.has(name))
            .unwrap_or(MMA_TILE[1]);
        let k = self.artifacts.get(name)?;
        let cfg = LaunchConfig {
            grid: (
                u32::try_from(rows.div_ceil(brows)).unwrap_or(u32::MAX),
                u32::try_from(tokens.div_ceil(btok)).unwrap_or(u32::MAX),
                1,
            ),
            block: (threads, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(weights)
            .buf(xq)
            .buf(xs)
            .scalar(cols as i64)
            .scalar(rows as i64)
            .scalar(tokens as i64)
            .scalar(output_scale);
        self.device.launch(k, &cfg, &args, stream)
    }
}

/// Tokeny na kafel wariantow zgrupowanych, od najszerszego.
const GROUPED_TILE: [(&str, usize, u32); 2] = [
    ("gemm_mxf4_grouped_f16_bm128_bn32", 32, 128),
    ("gemm_mxf4_grouped_f16_bm128_bn16", 16, 128),
];

/// Wierszy wyjscia na blok obu wariantow zgrupowanych.
const GROUPED_ROWS: usize = 128;

impl Kernels {
    /// Czy stos ekspertow MXFP4 da sie mnozyc na jednostce macierzowej.
    pub fn supports_mxf4_grouped(&self) -> bool {
        self.supports_mxf4_block_scale()
            && self.artifacts.has("quantize_act_mxf4")
            && GROUPED_TILE
                .iter()
                .any(|(name, ..)| self.artifacts.has(name))
    }

    /// Kwantyzuje aktywacje do postaci MXFP4, w ukladzie `pack_mxfp4_mma`.
    pub fn quantize_act_mxf4(
        &self,
        xq: &DevBuffer,
        xs: &DevBuffer,
        x: &DevBuffer,
        cols: usize,
        tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        if cols == 0 || !cols.is_multiple_of(64) {
            return Err(ForgeError::Kernel(format!(
                "quantize_act_mxf4 wymaga cols % 64 == 0, otrzymano {cols}"
            )));
        }
        if xq.len() < tokens * cols / 64 * 36 || xs.len() < tokens * 4 {
            return Err(ForgeError::Kernel(
                "quantize_act_mxf4: bufor wyjsciowy jest za maly".into(),
            ));
        }
        let k = self.artifacts.get("quantize_act_mxf4")?;
        let cfg = LaunchConfig {
            grid: (u32::try_from(tokens).unwrap_or(u32::MAX), 1, 1),
            block: (256, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new().buf(xq).buf(xs).buf(x).scalar(cols as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Wszyscy eksperci MXFP4 kroku w JEDNYM uruchomieniu.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_mxf4_grouped(
        &self,
        y: &DevBuffer,
        table: &DevBuffer,
        xq: &DevBuffer,
        xs: &DevBuffer,
        tiles: crate::launchers::moe::GroupedTiles<'_>,
        rows: usize,
        cols: usize,
        selections: usize,
        stream: &Stream,
    ) -> Result<()> {
        if rows == 0 || cols < 64 || !cols.is_multiple_of(64) {
            return Err(ForgeError::Kernel(format!(
                "gemm_mxf4_grouped wymaga cols % 64 == 0, otrzymano rows={rows}, cols={cols}"
            )));
        }
        // Kafel szerszy w tokenach liczylby zera: przy 256 ekspertach na
        // jednego przypada kilkanascie wierszy, a `GROUPED_TILE_ROWS` jest
        // gorna granica, nie srednia.
        let (name, _, threads) = GROUPED_TILE
            .iter()
            .copied()
            .find(|(name, tokens, _)| {
                crate::launchers::moe::GROUPED_TILE_ROWS >= *tokens && self.artifacts.has(name)
            })
            .unwrap_or(GROUPED_TILE[1]);
        let k = self.artifacts.get(name)?;
        let cfg = LaunchConfig {
            grid: (
                u32::try_from(rows.div_ceil(GROUPED_ROWS)).unwrap_or(u32::MAX),
                u32::try_from(tiles.count).unwrap_or(u32::MAX),
                1,
            ),
            block: (threads, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(table)
            .buf(xq)
            .buf(xs)
            .buf(tiles.expert)
            .buf(tiles.first)
            .buf(tiles.end)
            .scalar(cols as i64)
            .scalar(rows as i64)
            .scalar(selections as i64)
            .scalar(1.0f32);
        self.device.launch(k, &cfg, &args, stream)
    }
}
