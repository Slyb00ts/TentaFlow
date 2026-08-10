// ===== File: recurrent.rs — where a sequence's recurrent state lives =====
//
// A linear-attention layer carries state BETWEEN tokens: a convolution window
// of the last few inputs, and a matrix that has absorbed everything the
// sequence has said. Attention keeps the same information as pages of keys and
// values; this keeps it folded up, at a fixed size that does not grow with the
// context.
//
// It lives here, next to the pages, for the reason the pages do. Speculation
// proposes tokens that may be rejected, so whatever a step advanced must be
// restorable — and a state hidden inside an executor is a state the step that
// rolls back cannot reach. Two implementations of "where does this sequence's
// state sit" is the arrangement where a fix lands in one of them.
//
// Layers are allocated ON DEMAND rather than up front. A hybrid stack mixes
// recurrent layers with attention ones, and which is which is stated by the
// operations that arrive; reserving a slab for every layer index would waste a
// quarter of it here and all of it for a model that has none.

use std::collections::HashMap;

use forge_hal::{DevBuffer, Device, Pool};
use forge_types::{ForgeError, MemKind, Result};

/// The geometry of one recurrent layer, for every sequence held at once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecurrentConfig {
    /// Sequences held at once.
    pub slots: usize,
    /// Channels the causal convolution runs over — the whole mixed q|k|v
    /// stream, one tap set per channel.
    pub conv_channels: usize,
    /// Width of the convolution. The window holds one sample FEWER, because
    /// the newest sample is the input rather than history.
    pub conv_taps: usize,
    /// Value heads, each with a state matrix of its own.
    pub v_heads: usize,
    /// Both dimensions of that matrix, and the width of a head.
    pub d_state: usize,
}

impl RecurrentConfig {
    /// Bytes of convolution window per sequence — f16, like the activations
    /// that flow through it.
    pub fn conv_bytes(&self) -> usize {
        self.conv_channels * self.conv_taps.saturating_sub(1) * 2
    }

    /// Bytes of state matrix per sequence.
    ///
    /// f32 and not f16, deliberately. This matrix is written every token and
    /// read every token, so its rounding does not wash out — it compounds over
    /// the whole sequence, which is exactly the error that shows up as a model
    /// slowly losing the thread rather than as a wrong number.
    pub fn state_bytes(&self) -> usize {
        self.v_heads * self.d_state * self.d_state * 4
    }

    /// Byte offset of one slot's window inside a layer's buffer.
    pub fn conv_offset(&self, slot: usize) -> usize {
        slot * self.conv_bytes()
    }

    /// Byte offset of one slot's state matrix.
    pub fn state_offset(&self, slot: usize) -> usize {
        slot * self.state_bytes()
    }

    fn total(&self) -> Result<usize> {
        let per = self.conv_bytes() + self.state_bytes();
        if self.slots == 0 || per == 0 {
            return Err(ForgeError::Unsupported(format!(
                "stan rekurencyjny: {} slotów po {per} B",
                self.slots
            )));
        }
        Ok(per * self.slots)
    }
}

/// One layer's window and state, for all slots.
pub struct LayerState {
    pub conv: DevBuffer,
    pub state: DevBuffer,
}

/// Every recurrent layer this executor has been asked for.
pub struct RecurrentState {
    cfg: RecurrentConfig,
    layers: HashMap<usize, LayerState>,
}

impl RecurrentState {
    pub fn new(cfg: RecurrentConfig) -> Result<Self> {
        cfg.total()?;
        Ok(Self {
            cfg,
            layers: HashMap::new(),
        })
    }

    pub fn config(&self) -> RecurrentConfig {
        self.cfg
    }

    /// This layer's buffers, allocated and ZEROED the first time it is asked
    /// for.
    ///
    /// The zeroing is not hygiene. An allocator hands back whatever was in
    /// those bytes, and a state matrix that starts at noise is a sequence that
    /// begins mid-thought — fluent, and about nothing that was said.
    pub fn ensure(&mut self, device: &dyn Device, layer: usize) -> Result<&LayerState> {
        if !self.layers.contains_key(&layer) {
            let conv = device.alloc(
                self.cfg.conv_bytes() * self.cfg.slots,
                MemKind::Device,
                Pool::Activations,
            )?;
            let state = device.alloc(
                self.cfg.state_bytes() * self.cfg.slots,
                MemKind::Device,
                Pool::Activations,
            )?;
            let zeros = vec![0u8; self.cfg.conv_bytes().max(self.cfg.state_bytes())];
            for slot in 0..self.cfg.slots {
                device.write(
                    &zeros[..self.cfg.conv_bytes()],
                    &conv,
                    slot * self.cfg.conv_bytes(),
                )?;
                device.write(
                    &zeros[..self.cfg.state_bytes()],
                    &state,
                    slot * self.cfg.state_bytes(),
                )?;
            }
            self.layers.insert(layer, LayerState { conv, state });
        }
        Ok(&self.layers[&layer])
    }

    /// Forgets what one sequence said in one layer.
    ///
    /// Called when a lane starts at position zero, which is the same signal the
    /// paged cache uses to overwrite from the front — so a reused slot cannot
    /// keep half of the previous conversation folded into its state.
    pub fn clear(&self, device: &dyn Device, layer: usize, slot: usize) -> Result<()> {
        let Some(held) = self.layers.get(&layer) else {
            return Ok(());
        };
        if slot >= self.cfg.slots {
            return Err(ForgeError::Unsupported(format!(
                "slot {slot}, a stan rekurencyjny trzyma {}",
                self.cfg.slots
            )));
        }
        let zeros = vec![0u8; self.cfg.conv_bytes().max(self.cfg.state_bytes())];
        device.write(
            &zeros[..self.cfg.conv_bytes()],
            &held.conv,
            slot * self.cfg.conv_bytes(),
        )?;
        device.write(
            &zeros[..self.cfg.state_bytes()],
            &held.state,
            slot * self.cfg.state_bytes(),
        )
    }
}
