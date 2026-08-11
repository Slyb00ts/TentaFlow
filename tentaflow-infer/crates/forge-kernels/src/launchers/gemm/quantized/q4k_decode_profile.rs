// =============================================================================
// Plik: q4k_decode_profile.rs
// Opis: Dobiera artefakt i plan uruchomienia GEMV Q4_K do formatu, operacji i GPU.
// Przykład: let profile = q4k_decode_profile(&caps);
// =============================================================================

use forge_types::{DeviceCaps, QuantKind, Vendor};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Operation {
    DecodeGemv,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ModelFamily {
    Any,
    Dense,
    Qwen35,
    Bielik,
}

#[derive(Clone, Copy)]
pub enum Q4kDecodeModelFamily {
    Any,
    Dense,
    Qwen35,
    Bielik,
}

pub(super) fn uses_portable32_bielik(
    caps: &DeviceCaps,
    model: Q4kDecodeModelFamily,
    rows: usize,
    cols: usize,
) -> bool {
    caps.vendor == Vendor::Amd
        && caps.arch == "gfx1201"
        && matches!(model, Q4kDecodeModelFamily::Bielik)
        && cols == 5120
        && matches!(rows, 14_336 | 28_672)
}

impl Q4kDecodeModelFamily {
    fn profile_family(self) -> ModelFamily {
        match self {
            Self::Any => ModelFamily::Any,
            Self::Dense => ModelFamily::Dense,
            Self::Qwen35 => ModelFamily::Qwen35,
            Self::Bielik => ModelFamily::Bielik,
        }
    }
}

#[derive(Clone, Copy)]
enum DeviceScope {
    Any,
    VendorArch(Vendor, &'static str),
}

impl DeviceScope {
    fn matches(self, caps: &DeviceCaps) -> bool {
        match self {
            Self::Any => true,
            Self::VendorArch(vendor, arch) => caps.vendor == vendor && caps.arch == arch,
        }
    }

    fn specificity(self) -> u8 {
        match self {
            Self::Any => 0,
            Self::VendorArch(_, _) => 1,
        }
    }
}

fn model_specificity(model: ModelFamily) -> u8 {
    match model {
        ModelFamily::Any => 0,
        ModelFamily::Dense => 1,
        ModelFamily::Qwen35 => 1,
        ModelFamily::Bielik => 1,
    }
}

#[derive(Clone, Copy)]
pub(super) struct Profile {
    format: QuantKind,
    operation: Operation,
    model: ModelFamily,
    device: DeviceScope,
    plain_artifact: &'static str,
    persistent_artifact: Option<&'static str>,
    narrow_persistent_artifact: Option<&'static str>,
    rows_per_block: u32,
    persistent_blocks_per_cu: Option<u32>,
    group4_artifact: Option<&'static str>,
    group4_cols: Option<usize>,
    persistent_exact_shape: Option<(usize, usize)>,
}

const PROFILES: &[Profile] = &[
    Profile {
        format: QuantKind::Q4K,
        operation: Operation::DecodeGemv,
        model: ModelFamily::Dense,
        device: DeviceScope::VendorArch(Vendor::Amd, "gfx1201"),
        plain_artifact: "gemv_q4_k_dp4a_amd_u4_f16",
        persistent_artifact: Some("gemv_q4_k_dp4a_amd_u4_persist_f16"),
        narrow_persistent_artifact: Some("gemv_q4_k_dp4a_amd_u4_persist_x4k_f16"),
        rows_per_block: 8,
        persistent_blocks_per_cu: Some(6),
        group4_artifact: None,
        group4_cols: None,
        persistent_exact_shape: None,
    },
    Profile {
        format: QuantKind::Q4K,
        operation: Operation::DecodeGemv,
        model: ModelFamily::Qwen35,
        device: DeviceScope::VendorArch(Vendor::Amd, "gfx1201"),
        plain_artifact: "gemv_q4_k_dp4a_amd_u4_f16",
        persistent_artifact: Some("gemv_q4_k_dp4a_amd_u4_persist_f16"),
        narrow_persistent_artifact: Some("gemv_q4_k_dp4a_amd_u4_persist_x4k_f16"),
        rows_per_block: 8,
        persistent_blocks_per_cu: Some(6),
        group4_artifact: Some("gemv_q4_k_dp4a_group4_f16"),
        group4_cols: Some(5120),
        persistent_exact_shape: None,
    },
    Profile {
        format: QuantKind::Q4K,
        operation: Operation::DecodeGemv,
        model: ModelFamily::Bielik,
        device: DeviceScope::VendorArch(Vendor::Amd, "gfx1201"),
        plain_artifact: "gemv_q4_k_dp4a_amd_u4_f16",
        persistent_artifact: Some("gemv_q4_k_dp4a_amd_u4_persist_f16"),
        narrow_persistent_artifact: Some("gemv_q4_k_dp4a_amd_u4_persist_x4k_f16"),
        rows_per_block: 8,
        persistent_blocks_per_cu: Some(6),
        group4_artifact: None,
        group4_cols: None,
        persistent_exact_shape: Some((28672, 5120)),
    },
    Profile {
        format: QuantKind::Q4K,
        operation: Operation::DecodeGemv,
        model: ModelFamily::Any,
        device: DeviceScope::Any,
        plain_artifact: "gemv_q4_k_dp4a_f16",
        persistent_artifact: Some("gemv_q4_k_dp4a_persist_f16"),
        narrow_persistent_artifact: Some("gemv_q4_k_dp4a_persist_x4k_f16"),
        rows_per_block: 8,
        persistent_blocks_per_cu: None,
        group4_artifact: Some("gemv_q4_k_dp4a_group4_f16"),
        group4_cols: None,
        persistent_exact_shape: None,
    },
];

pub(super) fn q4k_decode_profile(caps: &DeviceCaps, model: Q4kDecodeModelFamily) -> Profile {
    PROFILES
        .iter()
        .filter(|profile| profile.format == QuantKind::Q4K)
        .filter(|profile| profile.operation == Operation::DecodeGemv)
        .filter(|profile| {
            profile.model == model.profile_family() || profile.model == ModelFamily::Any
        })
        .filter(|profile| profile.device.matches(caps))
        .max_by_key(|profile| {
            (
                (profile.model == model.profile_family())
                    .then_some(model_specificity(profile.model))
                    .unwrap_or(0),
                profile.device.specificity(),
            )
        })
        .copied()
        .expect("Q4_K decode must have a portable profile")
}

pub(super) fn uses_profiled_persistent_grid(profile: Profile) -> bool {
    profile.persistent_blocks_per_cu.is_some()
}

pub(super) fn plain_artifact(profile: Profile) -> &'static str {
    profile.plain_artifact
}

pub(super) fn persistent_artifact(profile: Profile, cols: usize) -> Option<&'static str> {
    if cols <= 4096 {
        profile
            .narrow_persistent_artifact
            .or(profile.persistent_artifact)
    } else {
        profile.persistent_artifact
    }
}

pub(super) fn rows_per_block(profile: Profile) -> u32 {
    profile.rows_per_block
}

pub(super) fn persistent_blocks_per_cu(profile: Profile) -> Option<u32> {
    profile.persistent_blocks_per_cu
}

pub(super) fn persistent_grid(
    profile: Profile,
    caps: &DeviceCaps,
    rows: usize,
    cols: usize,
    tiles: u32,
) -> Option<u32> {
    let blocks_per_cu = persistent_blocks_per_cu(profile)?;
    let wave = caps.sm_count.checked_mul(blocks_per_cu)?;
    if wave == 0 || tiles <= wave {
        return None;
    }
    if profile.persistent_exact_shape == Some((rows, cols)) {
        return Some(wave);
    }
    (tiles <= 2048).then_some(wave)
}

pub(super) fn group4_artifact(
    caps: &DeviceCaps,
    model: Q4kDecodeModelFamily,
    cols: usize,
) -> Option<&'static str> {
    let profile = q4k_decode_profile(caps, model);
    if model.profile_family() == ModelFamily::Qwen35 {
        return match profile.group4_cols {
            Some(expected) if expected == cols => profile.group4_artifact,
            _ => None,
        };
    }
    PROFILES
        .iter()
        .find(|candidate| {
            candidate.model == ModelFamily::Any
                && matches!(candidate.device, DeviceScope::Any)
                && candidate.group4_cols.is_none()
        })
        .and_then(|candidate| candidate.group4_artifact)
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_types::Vendor;

    fn caps(arch: &str, vendor: Vendor) -> DeviceCaps {
        DeviceCaps {
            name: arch.into(),
            vendor,
            arch: arch.into(),
            total_memory: 0,
            max_shared_mem_per_block: 0,
            max_threads_per_block: 0,
            warp_size: 32,
            sm_count: 64,
            fp8_native: false,
            fp4_block_scale_ue8m0: false,
            fp4_block_scale_e4m3: false,
            wgmma: false,
            tcgen05: false,
            tma: false,
            bf16_native: false,
            supports_p2p: false,
            supports_graph_capture: false,
        }
    }

    #[test]
    fn measured_gfx1201_uses_its_own_decode_profile() {
        let profile =
            q4k_decode_profile(&caps("gfx1201", Vendor::Amd), Q4kDecodeModelFamily::Dense);
        assert_eq!(plain_artifact(profile), "gemv_q4_k_dp4a_amd_u4_f16");
        assert_eq!(persistent_blocks_per_cu(profile), Some(6));
    }

    #[test]
    fn unmeasured_amd_architecture_keeps_portable_profile() {
        let profile =
            q4k_decode_profile(&caps("gfx1100", Vendor::Amd), Q4kDecodeModelFamily::Dense);
        assert_eq!(plain_artifact(profile), "gemv_q4_k_dp4a_f16");
        assert_eq!(persistent_blocks_per_cu(profile), None);
    }

    #[test]
    fn model_specific_profile_wins_over_any_profile() {
        let profile =
            q4k_decode_profile(&caps("gfx1201", Vendor::Amd), Q4kDecodeModelFamily::Dense);
        assert_eq!(plain_artifact(profile), "gemv_q4_k_dp4a_amd_u4_f16");
    }

    #[test]
    fn any_model_uses_any_profile() {
        let profile = q4k_decode_profile(&caps("gfx1201", Vendor::Amd), Q4kDecodeModelFamily::Any);
        assert_eq!(plain_artifact(profile), "gemv_q4_k_dp4a_f16");
    }

    #[test]
    fn qwen35_gfx1201_uses_group4_only_at_measured_shape() {
        let caps = caps("gfx1201", Vendor::Amd);
        assert_eq!(
            group4_artifact(&caps, Q4kDecodeModelFamily::Qwen35, 5120),
            Some("gemv_q4_k_dp4a_group4_f16")
        );
        assert_eq!(
            group4_artifact(&caps, Q4kDecodeModelFamily::Qwen35, 4096),
            None
        );
    }

    #[test]
    fn dense_model_keeps_its_generic_group4_profile() {
        assert_eq!(
            group4_artifact(
                &caps("gfx1201", Vendor::Amd),
                Q4kDecodeModelFamily::Dense,
                5120
            ),
            Some("gemv_q4_k_dp4a_group4_f16")
        );
    }

    #[test]
    fn bielik_gfx1201_wide_projection_keeps_persistent_grid() {
        let caps = caps("gfx1201", Vendor::Amd);
        let profile = q4k_decode_profile(&caps, Q4kDecodeModelFamily::Bielik);
        assert_eq!(persistent_grid(profile, &caps, 28672, 5120, 3584), Some(384));
    }

    #[test]
    fn bielik_override_rejects_other_device_or_shape() {
        let gfx1201 = caps("gfx1201", Vendor::Amd);
        let gfx1100 = caps("gfx1100", Vendor::Amd);
        let profile = q4k_decode_profile(&gfx1201, Q4kDecodeModelFamily::Bielik);
        let other = q4k_decode_profile(&gfx1100, Q4kDecodeModelFamily::Bielik);
        assert_eq!(persistent_grid(profile, &gfx1201, 28672, 4096, 3584), None);
        assert_eq!(persistent_grid(other, &gfx1100, 28672, 5120, 3584), None);
    }

    #[test]
    fn portable32_bielik_uses_only_measured_device_and_shapes() {
        assert!(uses_portable32_bielik(
            &caps("gfx1201", Vendor::Amd),
            Q4kDecodeModelFamily::Bielik,
            14_336,
            5120,
        ));
        assert!(!uses_portable32_bielik(
            &caps("gfx1200", Vendor::Amd),
            Q4kDecodeModelFamily::Bielik,
            14_336,
            5120,
        ));
        assert!(!uses_portable32_bielik(
            &caps("gfx1201", Vendor::Amd),
            Q4kDecodeModelFamily::Qwen35,
            14_336,
            5120,
        ));
        assert!(!uses_portable32_bielik(
            &caps("gfx1201", Vendor::Amd),
            Q4kDecodeModelFamily::Bielik,
            14_336,
            4096,
        ));
    }

}
