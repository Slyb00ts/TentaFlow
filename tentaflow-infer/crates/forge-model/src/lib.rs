// ===== File: lib.rs — forge-model: model graphs built from weights =====
//
// Where a checkpoint becomes a sequence of operations. The crate knows about
// architectures; it does NOT know which hardware is underneath, and — since the
// executor moved out — it has no way to find out. That is the boundary
// PLAN_NAPRAWY §5.1 draws, and it is now a property of the dependencies rather
// than a rule somebody has to remember.

pub mod dense;
