// ===== File: sim/wgpu/shards.rs — how a state is cut across storage buffers =====

use crate::error::{Error, Result};

/// Storage buffers one state may occupy (plan 6.3). Four of them can be bound
/// to a single dispatch, so eight shards still leave every gate expressible as
/// one kernel over the buffers it actually touches.
pub const MAX_SHARDS: usize = 8;

/// Widest register the kernels can address. WGSL indexes a storage array with a
/// `u32`, and `sample_search` reports a GLOBAL basis index in one, so 2^32
/// amplitudes is the hard ceiling of this backend whatever the adapter says.
/// The CPU backend has no such limit, which is why it is checked here.
pub const MAX_ADDRESSABLE_QUBITS: usize = 32;

/// The split of a state vector into equally sized storage buffers.
///
/// The cut is at the TOP bits of the basis index: shard `s` holds the
/// amplitudes whose index starts with `s`. A qubit below `local_bits` therefore
/// pairs amplitudes inside one shard, and a qubit at or above it pairs two
/// shards element for element — which is exactly the two kernel families in
/// `kernels.wgsl`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShardLayout {
    num_qubits: usize,
    shard_bits: usize,
}

impl ShardLayout {
    /// Fewest shards that keep every buffer within `max_shard_amplitudes`.
    pub fn plan(num_qubits: usize, max_shard_amplitudes: u64) -> Result<ShardLayout> {
        if max_shard_amplitudes == 0 {
            return Err(Error::DeviceUnavailable {
                device: "wgpu".to_string(),
                reason: "the adapter reports a zero-sized storage binding".to_string(),
            });
        }
        if num_qubits > MAX_ADDRESSABLE_QUBITS {
            return Err(Error::TooManyQubits {
                qubits: num_qubits,
                limit: MAX_ADDRESSABLE_QUBITS,
            });
        }
        let total = 1u64 << num_qubits;
        let mut shard_bits = 0usize;
        while (total >> shard_bits) > max_shard_amplitudes {
            shard_bits += 1;
        }
        if shard_bits > num_qubits || (1usize << shard_bits) > MAX_SHARDS {
            return Err(Error::TooManyQubits {
                qubits: num_qubits,
                limit: largest_fitting(max_shard_amplitudes),
            });
        }
        Ok(ShardLayout {
            num_qubits,
            shard_bits,
        })
    }

    pub fn num_qubits(&self) -> usize {
        self.num_qubits
    }

    pub fn shards(&self) -> usize {
        1usize << self.shard_bits
    }

    /// Amplitudes per shard.
    pub fn shard_len(&self) -> usize {
        1usize << self.local_bits()
    }

    /// Qubits whose pairs stay inside one shard: `0..local_bits`.
    pub fn local_bits(&self) -> usize {
        self.num_qubits - self.shard_bits
    }

    pub fn is_local(&self, qubit: usize) -> bool {
        qubit < self.local_bits()
    }

    /// Bit of the SHARD index a non-local qubit selects.
    pub fn shard_bit(&self, qubit: usize) -> usize {
        debug_assert!(!self.is_local(qubit));
        1usize << (qubit - self.local_bits())
    }

    /// First basis index held by `shard`.
    pub fn origin(&self, shard: usize) -> usize {
        shard << self.local_bits()
    }
}

/// Widest register `max_shard_amplitudes` can still hold across [`MAX_SHARDS`].
/// A shard is a power of two, so only the bits below the reported ceiling count.
fn largest_fitting(max_shard_amplitudes: u64) -> usize {
    let per_shard_bits = (u64::BITS - 1 - max_shard_amplitudes.leading_zeros()) as usize;
    per_shard_bits + MAX_SHARDS.trailing_zeros() as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_state_that_fits_one_binding_is_not_split() {
        let layout = ShardLayout::plan(10, 1 << 20).unwrap();
        assert_eq!(layout.shards(), 1);
        assert_eq!(layout.shard_len(), 1 << 10);
        assert!(layout.is_local(9));
    }

    #[test]
    fn the_top_qubits_become_shard_bits() {
        let layout = ShardLayout::plan(10, 1 << 7).unwrap();
        assert_eq!(layout.shards(), 8);
        assert_eq!(layout.local_bits(), 7);
        assert!(layout.is_local(6));
        assert!(!layout.is_local(7));
        assert_eq!(layout.shard_bit(7), 1);
        assert_eq!(layout.shard_bit(9), 4);
        assert_eq!(layout.origin(3), 3 << 7);
    }

    #[test]
    fn a_register_wider_than_a_u32_index_is_refused() {
        let error = ShardLayout::plan(33, 1 << 28).unwrap_err();
        assert!(
            matches!(
                error,
                Error::TooManyQubits {
                    qubits: 33,
                    limit: 32
                }
            ),
            "{error}"
        );
    }

    #[test]
    fn a_state_needing_more_than_eight_shards_is_refused() {
        let error = ShardLayout::plan(12, 1 << 7).unwrap_err();
        assert!(
            matches!(
                error,
                Error::TooManyQubits {
                    qubits: 12,
                    limit: 10
                }
            ),
            "{error}"
        );
    }
}
