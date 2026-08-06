// ===== File: gemm/mxf4.rs — natywne blokowo-skalowane MMA na czterech bitach =====
//
// Osobno od `nvfp4.rs`, bo to inna rodzina: tam wartości NVFP4 są rozpakowywane
// programowo i mnożone jak f16 albo int8, tu tensor core czyta e2m1 wprost, a
// skale jadą w samej instrukcji. Wspólna jest tylko nazwa formatu.
use super::*;

/// Bramka samej instrukcji blokowo-skalowanej; `src/mma_fp4.mojo`.
const MMA_MXF4_PROBE: &str = "mma_mxf4_probe";

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
    }

    /// Jedna instrukcja `m16n8k64` e2m1·e2m1 z per-32 skalami UE8M0.
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
        let k = self.artifacts.get(MMA_MXF4_PROBE)?;
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
