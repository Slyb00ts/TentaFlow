// ===== File: graph.rs — one decode step, recorded once and replayed =====
//
// A decode step of this model is about 860 launches, and the profiler measured
// 29 295 gaps between them over 32 tokens: median 2,24 us, p99 3,01 us, and a
// flat distribution — no synchronization, just the cost of telling the driver
// about a kernel, paid once per kernel. That was 15,7% of decode wall time.
//
// The step is the same 860 launches every token. Same buffers, same grids, same
// arguments; only the CONTENTS of five small control buffers move, and those
// arrive through copies from pinned memory, which record as graph nodes like
// anything else. So it is described to the driver once and afterwards launched
// as one thing.
//
// What has to hold for that to be legal, and where each is enforced:
//
//   * The operation sequence must repeat. Keyed on the lane count of a
//     one-token step; a prompt chunk is never recorded, because its token count
//     and half its grids differ per chunk.
//   * Nothing may allocate or synchronize inside the recording. The FIRST step
//     of a shape therefore runs plainly — it is the one that builds expert
//     tables, mixture scratch and packed weight forms — and the second is the
//     one recorded.
//   * Grids may not depend on how long the context has grown. They do not: the
//     decode attention takes its lengths and its page table from device
//     buffers, so its grid is a function of the model's shape alone.

use forge_graph::{Executor, Op, Step};
use forge_types::Result;

use super::CudaExec;

impl CudaExec {
    /// One step, whole.
    pub(super) fn run_whole_step(&self, ops: &[Op]) -> Result<()> {
        let Some((tokens, step)) = embed_of(ops) else {
            // A step that does not begin by embedding is not one of ours to
            // record — it still has to run.
            return ops.iter().try_for_each(|op| self.run(op));
        };
        self.admit(step)?;
        self.stage_values(tokens, step)?;

        // A lane at position zero is a sequence STARTING, and a starting
        // sequence gives back its pages and zeroes whatever a recurrent mixer
        // folded for the slot's previous occupant. That is a different sequence
        // of operations from the one that repeats — recording it would clear
        // the state every token, and replaying the ordinary one in its place
        // would never clear it at all. So it is neither recorded nor replayed.
        let repeats = step.tokens() == 1 && step.lanes().iter().all(|l| l.pos != 0);
        if !repeats {
            self.copy_control()?;
            ops.iter().try_for_each(|op| self.run(op))?;
            return self.fence_control();
        }

        let key = step.lanes().len() as u32;
        if let Some(recorded) = self.graphs.borrow().get(&key) {
            self.device.launch_graph(recorded, &self.stream)?;
            return self.fence_control();
        }
        if self.warmed.borrow_mut().insert(key) {
            self.copy_control()?;
            ops.iter().try_for_each(|op| self.run(op))?;
            return self.fence_control();
        }

        self.device.begin_capture(&self.stream)?;
        // Whatever happens between here and `end_capture`, the capture must be
        // ended: a stream left recording refuses every later launch.
        let issued = self
            .copy_control()
            .and_then(|()| ops.iter().try_for_each(|op| self.run(op)));
        let recorded = self.device.end_capture(&self.stream);
        issued?;
        let recorded = recorded?;
        self.device.launch_graph(&recorded, &self.stream)?;
        self.graphs.borrow_mut().insert(key, recorded);
        self.fence_control()
    }

    /// How many decode steps are currently recorded.
    ///
    /// The invariant behind `forget_graphs` is STRUCTURAL — a recording must
    /// not name a buffer the executor has released — and the damage from
    /// breaking it is latent: the released region stays mapped, so a stale
    /// recording reads and writes its own now-unowned scratch and answers
    /// correctly until something else is handed that memory. There is no token
    /// to compare, so this is what a test can hold the invariant by.
    pub fn recorded_steps(&self) -> usize {
        self.graphs.borrow().len()
    }

    /// Drops every recorded step.
    ///
    /// A recording NAMES the buffers it launches over, so it outlives them only
    /// as a set of dangling addresses. The scratch a mixture or a recurrent
    /// mixer keeps is allocated for the widest step seen so far and REPLACED
    /// when a wider one arrives, which is exactly that case — so replacing it
    /// is where the recordings have to go.
    pub(super) fn forget_graphs(&self) {
        self.graphs.borrow_mut().clear();
        self.warmed.borrow_mut().clear();
    }
}

/// The tokens and the step a list of operations begins with.
///
/// The embedding is where a step's own data enters, so it is also the only
/// operation that carries both.
fn embed_of(ops: &[Op]) -> Option<(&[u32], &Step)> {
    match ops.first()? {
        Op::Embed { tokens, step, .. } => Some((tokens, step)),
        _ => None,
    }
}
