// ===== File: persist.rs — how wide a one-wave decode grid should be =====
//
// One decision, and it is about the CARD rather than about the model: how much
// of a grid-striding GEMV the device runs at once. It lives apart because the
// number used to be written down, and a written-down number is right on exactly
// one part.

/// Warps per multiprocessor the persistent decode GEMV asks for.
///
/// Narrow matrices (`ssm_out`, `attn_output` — 640 tiles) end in a partial last
/// wave of workgroups; a block that walks tiles by grid stride stands in ONE
/// wave and quantizes the activation once instead of once per tile.
///
/// The count is in WARPS, not blocks, because the two shapes of this kernel do
/// not have the same block: the narrow-staging form runs four warps to a block
/// and the wide-staging form eight. Measured on a 48-SM GB10 with Q4_K decode,
/// sweeping each form's grid on its own, both peak at eight warps per
/// multiprocessor (4,4 warps: 40,7 tok/s; 8: 42,2; 16: 42,0; 24: 41,8) — and
/// the peak is flat enough on the upper side that the exact figure is not
/// delicate. What matters is that a warp of this kernel reads a LONG
/// consecutive run of one matrix, so adding warps past the point where the
/// memory system is busy only cuts the runs shorter.
pub(super) const PERSIST_WARPS_PER_SM: u32 = 8;

/// The one-wave grid for a decode GEMV whose blocks are `warps_per_block`
/// wide and which has `tiles` row tiles, or `None` when this shape or this
/// device should not take that path.
///
/// A device that does not report its multiprocessor count cannot be asked for a
/// grid that matches it, so it keeps the tile-per-block form. Above roughly two
/// thousand tiles the kernel runs long enough to amortize its own memory ramp
/// and a sweep showed a tie there, so the persistent form is for the narrow
/// matrices only.
pub(super) fn persist_wave(sm_count: u32, warps_per_block: u32, tiles: u32) -> Option<u32> {
    let wave = sm_count.checked_mul(PERSIST_WARPS_PER_SM)? / warps_per_block.max(1);
    (wave > 0 && tiles > wave && tiles <= 2048).then_some(wave)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_persistent_grid_follows_the_card() {
        // Two parts, same shape: the grid is the card's, not a number.
        assert_eq!(persist_wave(48, 8, 640), Some(48));
        assert_eq!(persist_wave(64, 8, 640), Some(64));
    }

    #[test]
    fn a_narrower_block_gets_proportionally_more_of_them() {
        // Half the warps in a block, twice the blocks: the same warps per part.
        assert_eq!(persist_wave(48, 4, 640), Some(96));
    }

    #[test]
    fn a_device_without_a_multiprocessor_count_stays_tile_per_block() {
        assert_eq!(persist_wave(0, 4, 640), None);
    }

    #[test]
    fn wide_and_narrow_shapes_both_fall_out() {
        // Fewer tiles than one wave: nothing to walk.
        assert_eq!(persist_wave(48, 4, 64), None);
        // Long enough to amortize its own ramp, so the plain form ties.
        assert_eq!(persist_wave(48, 4, 4096), None);
    }

}
