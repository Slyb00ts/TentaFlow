// ===== File: persist.rs — how wide a one-wave decode grid should be =====
//
// One decision, and it is about the CARD rather than about the model: how many
// blocks of a grid-striding GEMV the device holds resident at once. It lives
// apart because the number used to be written down, and a written-down number
// is right on exactly one part.

/// Blocks per multiprocessor the persistent decode GEMV asks for.
///
/// Narrow matrices (`ssm_out`, `attn_output` — 640 tiles) end in a partial last
/// wave of workgroups; a block that walks tiles by grid stride stands in ONE
/// wave and quantizes the activation once instead of once per tile. The point
/// is therefore that the grid equals what the card holds resident — which is a
/// property of the DEVICE and not a number to write down.
///
/// It used to be a flat 384, measured on an R9700. That part has 64 compute
/// units, so the measurement says SIX blocks of this shape per unit; on a part
/// with a different count the same 384 meant something else entirely. Six is
/// kept because it is what was actually measured, and a sweep of 96..384 on a
/// 48-SM GB10 came out flat (37,1-37,9 tok/s), so the factor is not delicate —
/// what matters is that it scales with the card.
pub(super) const PERSIST_BLOCKS_PER_SM: u32 = 6;

/// The one-wave grid for a decode GEMV of `tiles` row tiles, or `None` when
/// this shape or this device should not take that path.
///
/// A device that does not report its multiprocessor count cannot be asked for a
/// grid that matches it, so it keeps the tile-per-block form. Above roughly two
/// thousand tiles the kernel runs long enough to amortize its own memory ramp
/// and a sweep showed a tie there, so the persistent form is for the narrow
/// matrices only.
pub(super) fn persist_wave(sm_count: u32, tiles: u32) -> Option<u32> {
    let wave = sm_count.checked_mul(PERSIST_BLOCKS_PER_SM)?;
    (wave > 0 && tiles > wave && tiles <= 2048).then_some(wave)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_persistent_grid_follows_the_card() {
        // Two parts, same shape: the grid is the card's, not a number.
        assert_eq!(persist_wave(48, 640), Some(288));
        assert_eq!(persist_wave(64, 640), Some(384));
    }

    #[test]
    fn a_device_without_a_multiprocessor_count_stays_tile_per_block() {
        assert_eq!(persist_wave(0, 640), None);
    }

    #[test]
    fn wide_and_narrow_shapes_both_fall_out() {
        // Fewer tiles than one wave: nothing to walk.
        assert_eq!(persist_wave(48, 128), None);
        // Long enough to amortize its own ramp, so the plain form ties.
        assert_eq!(persist_wave(48, 4096), None);
    }
}
