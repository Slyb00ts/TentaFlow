// ===== File: registry.rs — PTX artifact loading (embedded defaults + dir override) =====

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

use forge_hal::{Device, KernelHandle, Module};
use forge_types::{ForgeError, Result};
use serde::Deserialize;

/// Parsed kernels/build/<arch>/manifest.json.
#[derive(Debug, Deserialize)]
pub struct Manifest {
    pub arch: String,
    pub kernels: HashMap<String, ManifestEntry>,
}

#[derive(Debug, Deserialize)]
pub struct ManifestEntry {
    pub file: String,
    pub entry: String,
}

/// One embedded (name, ptx) pair for the default artifact set. The list is
/// kept in lockstep with build_kernels.mojo registrations; a mismatch against
/// the manifest fails loudly at load time rather than at first launch.
struct EmbeddedArtifact {
    name: &'static str,
    ptx: &'static [u8],
}

/// Jeden zestaw na architekturę. NVIDIA wnosi tekstowy PTX, AMD gotowe code
/// objecty (HSACO) — poza katalogiem i rozszerzeniem kontrakt jest ten sam, więc
/// dołożenie kolejnej karty to nowa stała, a nie kopia makra.
macro_rules! embedded_arch {
    ($dir:literal, $ext:literal, $($name:literal),+ $(,)?) => {
        &[$(EmbeddedArtifact {
            name: $name,
            ptx: include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../kernels/mojo/build/",
                $dir,
                "/",
                $name,
                $ext
            )),
        }),+]
    };
}

/// W4A8 (int4-weight x int8-activation) prefill GEMM cubin (kernels/cuda/
/// w4a8_gemm.cu; QServe dense_kernel0, ADR-0001 exception). Non-default: routed
/// only under `FORGE_GEMM=w4a8`; the native Mojo int8 Q4_K GEMM is the default path.
const EMBEDDED_CUDA_CUBIN_W4A8_SM89: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../kernels/mojo/build/sm_89/w4a8_gemm_cuda.cubin"
));

/// (registry key, cubin entry symbol) for each W4A8 CTA config (one per QServe
/// dispatch branch, selected in the launcher by token count / K).
const CUDA_W4A8_ENTRIES: &[(&str, &str)] = &[
    ("w4a8_gemm_m128", "forge_w4a8_gemm_m128"),
    ("w4a8_gemm_m64_ksm", "forge_w4a8_gemm_m64_ksm"),
    ("w4a8_gemm_m64_klg", "forge_w4a8_gemm_m64_klg"),
    ("w4a8_gemm_m32", "forge_w4a8_gemm_m32"),
    ("w4a8_quant_act", "forge_w4a8_quant_act_pertoken"),
];

/// Tensor-core flash-attention prefill cubin (kernels/cuda/fattn_prefill.cu;
/// ADR-0001 exception). f16 mma QK^T + online softmax + P·V over the paged KV
/// cache. Non-default: routed only under `FORGE_ATTN=fa`; the Mojo scalar
/// `attn_prefill` stays the default so the golden path is bit-exact.
const EMBEDDED_CUDA_CUBIN_FATTN_SM89: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../kernels/mojo/build/sm_89/fattn_prefill_cuda.cubin"
));

/// (registry key, cubin entry symbol) for each flash-attention head_dim variant.
const CUDA_FATTN_ENTRIES: &[(&str, &str)] = &[
    ("attn_prefill_fa_f16_hd64", "forge_attn_prefill_fa_f16_hd64"),
    (
        "attn_prefill_fa_f16_hd128",
        "forge_attn_prefill_fa_f16_hd128",
    ),
];

/// One complete set of embedded artifacts for a single GPU architecture.
struct EmbeddedSet {
    arch: &'static str,
    manifest: &'static str,
    artifacts: &'static [EmbeddedArtifact],
    /// Name of the const for error messages.
    name: &'static str,
}

/// True if a Mojo PTX module is Ada-only, i.e. it declares `.target sm_89`
/// (fp8 mma/cvt, NVFP4 fp8-scale cvt). build_kernels lowers every portable
/// kernel to `.target sm_80`, so an sm_89 floor is a reliable marker that the
/// module uses instructions absent on pre-Ada parts and must not be JIT-loaded
/// there. Scans the PTX header bytes (the `.target` directive is near the top).
fn is_sm89_only(ptx: &[u8]) -> bool {
    let head = &ptx[..ptx.len().min(256)];
    head.windows(b".target sm_89".len())
        .any(|w| w == b".target sm_89")
}

fn supports_sm89_cubin(arch: &str) -> bool {
    arch == "sm_89"
}

// Generated artifact embedding — detects available GPU architectures and includes
// only those with complete artifact sets. See build.rs for generation logic.
include!(concat!(env!("OUT_DIR"), "/embedded_artifacts.rs"));

/// Pokolenie NVIDII i to, czy nazwa opisuje wariant ZAWĘŻONY do architektury.
///
/// `sm_89` → (89, false), `sm_121a` → (121, true). Litera oznacza zestaw
/// instrukcji dostępny wyłącznie na tej generacji — Mojo nazywa tak artefakty
/// na GB10 — więc taki zestaw NIE jest przenośny w górę, w odróżnieniu od
/// zwykłego PTX. AMD celowo nie ma odpowiednika: patrz `select_embedded_set`.
fn nvidia_capability(arch: &str) -> Option<(u32, bool)> {
    let rest = arch.strip_prefix("sm_")?;
    let digits = rest.trim_end_matches(|c: char| c.is_ascii_alphabetic());
    Some((digits.parse().ok()?, digits.len() != rest.len()))
}

/// Wybiera zestaw artefaktów dla karty.
///
/// Rodziny różnią się TYM, czym są artefakty, więc muszą różnić się regułą:
/// NVIDIA dostaje przenośny PTX, który sterownik kompiluje JIT dla bieżącej
/// karty, więc zestaw zbudowany dla STARSZEJ architektury działa na nowszej —
/// bez tego GB10 (sm_121) nie wczytałby niczego, mając komplet gotowych
/// artefaktów sm_89. AMD dostaje gotowe code objecty (HSACO) związane z
/// konkretnym ISA, więc tu jedynym poprawnym dopasowaniem jest DOKŁADNE; „prawie
/// pasujący" zestaw nie wczytałby się albo, gorzej, policzył co innego.
fn select_embedded_set(arch: &str, vendor: forge_types::Vendor) -> Option<&'static EmbeddedSet> {
    if let Some(exact) = EMBEDDED_SETS.iter().find(|set| set.arch == arch) {
        return Some(exact);
    }
    if vendor != forge_types::Vendor::Nvidia {
        return None;
    }
    let (device_capability, _) = nvidia_capability(arch)?;
    EMBEDDED_SETS
        .iter()
        .filter(|set| match nvidia_capability(set.arch) {
            // Zestaw zawężony do architektury pasuje WYŁĄCZNIE do swojego
            // pokolenia; zwykły PTX dociera na każdą nowszą kartę.
            Some((built, true)) => built == device_capability,
            Some((built, false)) => built <= device_capability,
            None => false,
        })
        .max_by_key(|set| {
            nvidia_capability(set.arch)
                .map(|(built, _)| built)
                .unwrap_or(0)
        })
}

fn resolve_artifact_path(arch_dir: &Path, file: &str) -> Result<PathBuf> {
    let relative = Path::new(file);
    if relative.as_os_str().is_empty()
        || relative.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(ForgeError::Kernel(format!(
            "niedozwolona ścieżka artefaktu: {file}"
        )));
    }
    let canonical_root = arch_dir.canonicalize().map_err(|error| {
        ForgeError::Kernel(format!("canonicalize {}: {error}", arch_dir.display()))
    })?;
    let candidate = canonical_root
        .join(relative)
        .canonicalize()
        .map_err(|error| ForgeError::Kernel(format!("canonicalize {file}: {error}")))?;
    if !candidate.starts_with(&canonical_root) {
        return Err(ForgeError::Kernel(format!(
            "artefakt wychodzi poza katalog architektury: {file}"
        )));
    }
    Ok(candidate)
}

/// Loaded modules + resolved kernel handles for one device.
pub struct KernelArtifacts {
    handles: HashMap<String, KernelHandle>,
    arch: String,
}

impl KernelArtifacts {
    /// Load kernel artifacts for `device`. `FORGE_KERNEL_DIR` (pointing at
    /// kernels/mojo/build) overrides the embedded set for development
    /// iteration without a Rust rebuild.
    pub fn load(device: &dyn Device) -> Result<Self> {
        let arch = device.caps().arch.clone();
        if let Ok(dir) = std::env::var("FORGE_KERNEL_DIR") {
            return Self::load_dir(device, Path::new(&dir), &arch);
        }
        Self::load_embedded(device, &arch)
    }

    fn load_embedded(device: &dyn Device, arch: &str) -> Result<Self> {
        // Każda architektura ma własny wkompilowany zestaw. Brak zestawu jest
        // błędem z instrukcją, a nie nieczytelnym błędem ładowania modułu ze
        // sterownika.
        let selected = select_embedded_set(arch, device.caps().vendor).ok_or_else(|| {
            ForgeError::Kernel(format!(
                "brak wkompilowanych artefaktów dla {arch}; zbuduj katalog \
                 kerneli dla tej architektury (scripts/build_kernel_catalog.py), \
                 wpisz listę przez scripts/sync_embedded_arch.py i dopisz wiersz \
                 do EMBEDDED_SETS — albo wskaż gotowy katalog w FORGE_KERNEL_DIR"
            ))
        })?;
        let (manifest_src, set, set_name) = (selected.manifest, selected.artifacts, selected.name);
        // Przenośny PTX Mojo ma `.target sm_80`, a sterownik JIT kompiluje go
        // dla bieżącej karty. Moduły z `.target sm_89` wymagają Ada lub nowszej
        // architektury, natomiast cubiny SASS są ładowane tylko na dokładnym sm_89.
        let ada = device.caps().fp8_native;
        let manifest: Manifest = serde_json::from_str(manifest_src)
            .map_err(|e| ForgeError::Kernel(format!("embedded manifest parse: {e}")))?;
        if manifest.arch != selected.arch {
            return Err(ForgeError::Kernel(format!(
                "wkompilowany manifest opisuje {}, a zestaw {set_name} deklaruje {}",
                manifest.arch, selected.arch
            )));
        }
        let mut handles = HashMap::new();
        for art in set {
            let entry = manifest.kernels.get(art.name).ok_or_else(|| {
                ForgeError::Kernel(format!(
                    "kernel {} missing from embedded manifest",
                    art.name
                ))
            })?;
            if !ada && is_sm89_only(art.ptx) {
                continue;
            }
            let module = device.load_module(art.ptx)?;
            handles.insert(art.name.to_string(), module.kernel(&entry.entry)?);
        }
        // The reverse direction: manifest entries with no embedded bytes mean
        // build_kernels.mojo and this crate went out of sync.
        for name in manifest.kernels.keys() {
            if !set.iter().any(|a| a.name == name) {
                return Err(ForgeError::Kernel(format!(
                    "manifest kernel {name} not embedded — update forge-kernels {set_name}"
                )));
            }
        }
        // Cubiny zawierają SASS dla sm_89 bez przenośnego PTX, więc wymagają
        // dokładnie tej samej architektury. Kerneli Mojo PTX ten warunek nie dotyczy.
        if supports_sm89_cubin(arch) {
            Self::load_cuda_cubin(
                device,
                EMBEDDED_CUDA_CUBIN_W4A8_SM89,
                CUDA_W4A8_ENTRIES,
                &mut handles,
            )?;
            Self::load_cuda_cubin(
                device,
                EMBEDDED_CUDA_CUBIN_FATTN_SM89,
                CUDA_FATTN_ENTRIES,
                &mut handles,
            )?;
        }
        Ok(Self {
            handles,
            arch: arch.to_string(),
        })
    }

    /// Resolve a raw-CUDA cubin's entry points into `handles`. The cubin
    /// loads exactly like Mojo PTX (`load_module` → cuModuleLoadData).
    fn load_cuda_cubin(
        device: &dyn Device,
        cubin: &[u8],
        entries: &[(&str, &str)],
        handles: &mut HashMap<String, KernelHandle>,
    ) -> Result<()> {
        let module = device.load_module(cubin)?;
        for (key, entry) in entries {
            handles.insert((*key).to_string(), module.kernel(entry)?);
        }
        Ok(())
    }

    fn load_dir(device: &dyn Device, dir: &Path, arch: &str) -> Result<Self> {
        let arch_dir = dir.join(arch);
        let manifest_path = arch_dir.join("manifest.json");
        let manifest_src = std::fs::read_to_string(&manifest_path)
            .map_err(|e| ForgeError::Kernel(format!("read {}: {e}", manifest_path.display())))?;
        let manifest: Manifest = serde_json::from_str(&manifest_src)
            .map_err(|e| ForgeError::Kernel(format!("manifest parse: {e}")))?;
        if manifest.arch != arch {
            return Err(ForgeError::Kernel(format!(
                "manifest architecture {} does not match device {arch}",
                manifest.arch
            )));
        }
        let ada = device.caps().fp8_native;
        let mut handles = HashMap::new();
        for (name, entry) in &manifest.kernels {
            // Schemat manifestu nie ma obecnie digestu; jego dodanie wymaga
            // osobnej wersjonowanej zmiany kontraktu artefaktów.
            let artifact_path = resolve_artifact_path(&arch_dir, &entry.file)?;
            let ptx = std::fs::read(artifact_path)
                .map_err(|e| ForgeError::Kernel(format!("read {}: {e}", entry.file)))?;
            if !ada && is_sm89_only(&ptx) {
                continue;
            }
            let module: Module = device.load_module(&ptx)?;
            handles.insert(name.clone(), module.kernel(&entry.entry)?);
        }
        // Cubiny w katalogu nadpisującym zawierają wyłącznie SASS dla sm_89.
        // Natywne FP8 nie gwarantuje zgodności binarnej z tą architekturą.
        if supports_sm89_cubin(arch) {
            let w4a8 = std::fs::read(arch_dir.join("w4a8_gemm_cuda.cubin"))
                .map_err(|e| ForgeError::Kernel(format!("read w4a8_gemm_cuda.cubin: {e}")))?;
            Self::load_cuda_cubin(device, &w4a8, CUDA_W4A8_ENTRIES, &mut handles)?;
            let fattn = std::fs::read(arch_dir.join("fattn_prefill_cuda.cubin"))
                .map_err(|e| ForgeError::Kernel(format!("read fattn_prefill_cuda.cubin: {e}")))?;
            Self::load_cuda_cubin(device, &fattn, CUDA_FATTN_ENTRIES, &mut handles)?;
        }
        Ok(Self {
            handles,
            arch: arch.to_string(),
        })
    }

    pub fn arch(&self) -> &str {
        &self.arch
    }

    pub fn get(&self, name: &str) -> Result<&KernelHandle> {
        self.handles
            .get(name)
            .ok_or_else(|| ForgeError::Kernel(format!("kernel not loaded: {name}")))
    }

    pub fn has(&self, name: &str) -> bool {
        self.handles.contains_key(name)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        is_sm89_only, resolve_artifact_path, select_embedded_set, supports_sm89_cubin,
        EMBEDDED_SETS, EMBEDDED_SM_89,
    };
    use forge_types::Vendor;

    /// PTX jest przenośny w górę, więc karta NVIDII nowsza od zbudowanego
    /// zestawu ma go dostać. Bez tego GB10 (sm_121) nie wczytywał NICZEGO, mimo
    /// kompletu gotowych artefaktów sm_89 — sam warunek równości architektur
    /// odrzucał je na wejściu.
    #[test]
    fn nvidia_dostaje_najwyzszy_zestaw_nie_nowszy_od_karty() {
        // Karty bez własnego zestawu biorą najwyższy PRZENOŚNY nie nowszy od
        // siebie; GB10 ma własny, zawężony do swojej generacji.
        for (arch, expected) in [
            ("sm_89", "sm_89"),
            ("sm_90", "sm_89"),
            ("sm_120", "sm_89"),
            ("sm_121", "sm_121a"),
        ] {
            let set = select_embedded_set(arch, Vendor::Nvidia)
                .unwrap_or_else(|| panic!("brak zestawu dla {arch}"));
            assert_eq!(set.arch, expected, "{arch}");
        }
        // Karta STARSZA od jedynego zbudowanego zestawu nie dostaje niczego —
        // sm_89 PTX z instrukcjami Ady nie uruchomi się na sm_80.
        assert!(select_embedded_set("sm_80", Vendor::Nvidia).is_none());
    }

    /// Mojo nazywa artefakty na GB10 `sm_121a` — to wariant z instrukcjami
    /// zawężonymi do tej generacji, więc NIE wolno go podać karcie nowszej.
    #[test]
    fn zestaw_zawezony_do_architektury_nie_wedruje_w_gore() {
        assert_eq!(super::nvidia_capability("sm_121a"), Some((121, true)));
        assert_eq!(super::nvidia_capability("sm_89"), Some((89, false)));
        let sets = [
            super::EmbeddedSet {
                arch: "sm_89",
                manifest: "",
                artifacts: &[],
                name: "A",
            },
            super::EmbeddedSet {
                arch: "sm_121a",
                manifest: "",
                artifacts: &[],
                name: "B",
            },
        ];
        let pick = |arch: &str| {
            sets.iter()
                .filter(|set| match super::nvidia_capability(set.arch) {
                    Some((built, true)) => built == super::nvidia_capability(arch).unwrap().0,
                    Some((built, false)) => built <= super::nvidia_capability(arch).unwrap().0,
                    None => false,
                })
                .max_by_key(|set| super::nvidia_capability(set.arch).unwrap().0)
                .map(|set| set.arch)
        };
        // GB10 zgłasza się jako sm_121 i ma dostać swój zestaw, nie sm_89.
        assert_eq!(pick("sm_121"), Some("sm_121a"));
        // Hipotetyczna nowsza karta bierze przenośny sm_89, a NIE sm_121a.
        assert_eq!(pick("sm_130"), Some("sm_89"));
    }

    /// HSACO jest związany z konkretnym ISA, więc dla AMD jedynym poprawnym
    /// dopasowaniem jest dokładne — „prawie pasujący" zestaw albo się nie
    /// wczyta, albo policzy co innego.
    #[test]
    fn amd_wymaga_dokladnej_architektury() {
        assert_eq!(
            select_embedded_set("gfx1030", Vendor::Amd).map(|set| set.arch),
            Some("gfx1030")
        );
        assert_eq!(
            select_embedded_set("gfx1100", Vendor::Amd).map(|set| set.arch),
            Some("gfx1100")
        );
        assert_eq!(
            select_embedded_set("gfx1201", Vendor::Amd).map(|set| set.arch),
            Some("gfx1201")
        );
        // Karta bez zbudowanego katalogu ma dostać czytelny błąd, a NIE cudzy
        // code object — gfx1201 zdał ten egzamin, zanim doczekał się własnego
        // zestawu, i tak samo ma być dla każdej następnej.
        assert!(select_embedded_set("gfx1151", Vendor::Amd).is_none());
        assert!(select_embedded_set("gfx90a", Vendor::Amd).is_none());
    }

    #[test]
    fn kazdy_zestaw_ma_manifest_swojej_architektury() {
        assert!(!EMBEDDED_SETS.is_empty());
        for set in EMBEDDED_SETS {
            let manifest: super::Manifest = serde_json::from_str(set.manifest)
                .unwrap_or_else(|e| panic!("manifest {}: {e}", set.arch));
            assert_eq!(manifest.arch, set.arch, "{}", set.name);
            let embedded: std::collections::HashSet<&str> =
                set.artifacts.iter().map(|a| a.name).collect();
            let declared: std::collections::HashSet<&str> =
                manifest.kernels.keys().map(String::as_str).collect();
            assert_eq!(
                embedded, declared,
                "{} rozjechal sie z manifestem",
                set.name
            );
        }
    }

    const PORTABLE_RAW_NVFP4: &[&str] = &[
        "gemv_nvfp4_gguf_f16",
        "gemv_nvfp4_gguf_out_f32",
        "pack_q8_0_nvfp4_gguf",
        "gemv_nvfp4_gguf_q8_1_f16",
        "mtp_prepare_f16",
        "mtp_stage_step",
        "mtp_norm_join_shifted_f16",
        "mtp_project_joined_q8_f16",
        "gather_q8_0_row_f16",
        "gather_nvfp4_gguf_row_f16",
        "gemm_nvfp4_gguf_f16_b2",
        "gemm_nvfp4_gguf_out_f32_b2",
        "gemm_nvfp4_gguf_out_f32_b4",
        "gemm_nvfp4_gguf_out_f32_b8",
        "gemm_nvfp4_gguf_out_f32_b16",
        "gemm_nvfp4_gguf_f16_b3",
        "gemm_nvfp4_gguf_f16_b4",
        "gemm_nvfp4_gguf_f16_b3_nvidia",
        "gemm_nvfp4_gguf_out_f32_b1_nvidia",
        "gemm_nvfp4_gguf_f16_b4_nvidia",
        "gemm_nvfp4_gguf_f16_b8_nvidia",
        "gemm_nvfp4_gguf_f16_b8",
        "gemm_nvfp4_gguf_f16_b16",
        "gemm_nvfp4_gguf_f16_b16_nvidia",
        "gemm_nvfp4_gguf_mma_f16_bm32",
        "gemm_nvfp4_gguf_mma_f16_bm128",
        "gemm_nvfp4_gguf_mma_f16_bm128_bn32",
        "gemm_nvfp4_gguf_mma_f16_bm128_prefetch",
        "nvfp4_repack_tile128",
        "gemv_nvfp4_tile128_coop_q8_1_f16",
        "gemm_nvfp4_tile128_mma_f16_bm128_bn64",
        "gemm_nvfp4_tile128_mma_f16_bm128_bn128",
        "repack_nvfp4_ct_s0_n64k128_into",
        "gemv_nvfp4_ct_s0_n64k128_f16",
        "gemv_batch_nvfp4_ct_s0_n64k128_f16_b4",
        "gemv_batch_nvfp4_ct_s0_n64k128_f16_b8",
        "gemv_batch_nvfp4_ct_s0_n64k128_f16_b16",
        "gemm_nvfp4_ct_s0_f16_bm64",
        "gemm_nvfp4_ct_s0_f16_bm128",
        "gemv_norm_nvfp4_ct_s0_f16",
        "gemv_norm_silu_nvfp4_ct_s0_f16",
        "gemv_residual_nvfp4_ct_s0_f16",
        "pack_nvfp4_ct_s0_fp8",
        "gemm_nvfp4_ct_bm16_qkv_m4",
        "gemm_nvfp4_ct_bm16_qkv_m8",
        "gemm_nvfp4_ct_bm16_qkv_m16",
        "gemm_nvfp4_ct_bm16_o_m4",
        "gemm_nvfp4_ct_bm16_o_m8",
        "gemm_nvfp4_ct_bm16_o_m16",
        "gemm_nvfp4_ct_bm16_gateup_m4",
        "gemm_nvfp4_ct_bm16_gateup_m8",
        "gemm_nvfp4_ct_bm16_gateup_m16",
        "gemm_nvfp4_ct_bm16_down_m4",
        "gemm_nvfp4_ct_bm16_down_m8",
        "gemm_nvfp4_ct_bm16_down_m16",
        "gemm_nvfp4_ct_bm32_qkv_m24",
        "gemm_nvfp4_ct_bm32_qkv_m32",
        "gemm_nvfp4_ct_bm32_o_m24",
        "gemm_nvfp4_ct_bm32_o_m32",
        "gemm_nvfp4_ct_bm32_gateup_m24",
        "gemm_nvfp4_ct_bm32_gateup_m32",
        "gemm_nvfp4_ct_bm32_down_m24",
        "gemm_nvfp4_ct_bm32_down_m32",
        "reduce_nvfp4_ct_bm16",
    ];

    const PORTABLE_Q8_SMALL: &[&str] = &[
        "gemm_q8_0_i8mma_b2",
        "gemm_q8_0_i8mma_b3",
        "gemm_q8_0_i8mma_b4",
        "gemm_q8_0_i8mma_b8",
        "gemm_q8_0_f16_exact_out_f32_b8",
        "gemm_q8_0_i8mma_out_f32_b3",
        "gemm_q8_0_i8mma_out_f32_b4",
        "gemm_q8_0_f16_exact_out_f32_b2",
        "gemm_q8_0_f16_exact_out_f32_b3",
        "gemm_q8_0_f16_exact_out_f32_b4",
    ];

    const PORTABLE_DELTANET_PREPARE: &[&str] = &[
        "deltanet_prepare_t2_f16",
        "deltanet_prepare_t3_f16",
        "deltanet_prepare_t4_f16",
        "deltanet_prepare_dynamic_f16",
    ];

    const PORTABLE_DELTANET_SCAN: &[&str] = &[
        "deltanet_gated_scan_t3_d128_f16",
        "deltanet_gated_scan_t4_d128_f16",
        "deltanet_gated_scan_dynamic_f16",
        "deltanet_gated_scan_dynamic_d128_f16",
        "deltanet_gated_scan_inplace_dynamic_d128_f16",
        "deltanet_gated_scan_inplace_shared_d128_f16",
        "deltanet_gated_scan_segmented_shared_d128_f16",
        "deltanet_commit_recompute_segmented_shared_d128_f32",
    ];

    const PORTABLE_SAMPLING_PENALTIES: &[&str] = &[
        "penalized_argmax_f32",
        "penalize_histogram_f32",
        "penalize_batched_f32",
    ];

    #[test]
    fn raw_nvfp4_jest_dostepne_na_sm80_sm86_i_sm89() {
        for name in PORTABLE_RAW_NVFP4 {
            let artifact = EMBEDDED_SM_89
                .iter()
                .find(|artifact| artifact.name == *name)
                .unwrap();
            for (arch, fp8_native) in [("sm_80", false), ("sm_86", false), ("sm_89", true)] {
                let available = fp8_native || !is_sm89_only(artifact.ptx);
                assert!(available, "{name} powinien być dostępny na {arch}");
            }
        }
        let ada_only = EMBEDDED_SM_89
            .iter()
            .find(|artifact| artifact.name == "gemv_nvfp4_f16")
            .unwrap();
        assert!(is_sm89_only(ada_only.ptx));
    }

    #[test]
    fn male_q8_jest_dostepne_na_sm80_sm86_i_sm89() {
        for name in PORTABLE_Q8_SMALL {
            let artifact = EMBEDDED_SM_89
                .iter()
                .find(|artifact| artifact.name == *name)
                .unwrap();
            assert!(!is_sm89_only(artifact.ptx));
        }
    }

    #[test]
    fn fused_deltanet_prepare_jest_dostepne_od_sm80() {
        for name in PORTABLE_DELTANET_PREPARE {
            let artifact = EMBEDDED_SM_89
                .iter()
                .find(|artifact| artifact.name == *name)
                .unwrap();
            assert!(!is_sm89_only(artifact.ptx));
        }
    }

    #[test]
    fn kafelkowany_deltanet_scan_jest_dostepny_od_sm80() {
        for name in PORTABLE_DELTANET_SCAN {
            let artifact = EMBEDDED_SM_89
                .iter()
                .find(|artifact| artifact.name == *name)
                .unwrap();
            assert!(!is_sm89_only(artifact.ptx));
        }
    }

    #[test]
    fn fused_sampling_penalties_jest_dostepne_od_sm80() {
        for name in PORTABLE_SAMPLING_PENALTIES {
            let artifact = EMBEDDED_SM_89
                .iter()
                .find(|artifact| artifact.name == *name)
                .unwrap();
            assert!(!is_sm89_only(artifact.ptx));
        }
    }

    #[test]
    fn cubiny_sm89_sa_ladowane_tylko_na_dokladnie_sm89() {
        assert!(!supports_sm89_cubin("sm_80"));
        assert!(!supports_sm89_cubin("sm_86"));
        assert!(supports_sm89_cubin("sm_89"));
        assert!(!supports_sm89_cubin("sm_90"));
        assert!(!supports_sm89_cubin("sm_100"));
        assert!(!supports_sm89_cubin("gfx942"));
        assert!(!supports_sm89_cubin("apple-m3"));
    }

    #[test]
    fn odrzuca_absolute_i_parent_traversal() {
        let missing_root = std::path::Path::new("/katalog/ktory/nie/istnieje");
        assert!(resolve_artifact_path(missing_root, "../escape.ptx").is_err());
        assert!(resolve_artifact_path(missing_root, "/tmp/escape.ptx").is_err());
        assert!(resolve_artifact_path(missing_root, "").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn odrzuca_symlink_wychodzacy_poza_katalog() {
        use std::os::unix::fs::symlink;
        use std::time::{SystemTime, UNIX_EPOCH};

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let base = std::env::temp_dir().join(format!(
            "forge-kernel-registry-{}-{nonce}",
            std::process::id()
        ));
        let arch_dir = base.join("sm_89");
        let outside = base.join("outside.ptx");
        std::fs::create_dir_all(&arch_dir).unwrap();
        std::fs::write(&outside, b"ptx").unwrap();
        symlink(&outside, arch_dir.join("escape.ptx")).unwrap();

        assert!(resolve_artifact_path(&arch_dir, "escape.ptx").is_err());
        std::fs::write(arch_dir.join("ok.ptx"), b"ptx").unwrap();
        assert!(resolve_artifact_path(&arch_dir, "ok.ptx").is_ok());
        std::fs::remove_dir_all(base).unwrap();
    }
}
