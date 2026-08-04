// ===== File: lib.rs — forge-state: where a sequence's context lives =====
//
// Pages and the tree over them. Neither knows how a layer is computed, and that
// is the whole reason they live here instead of inside one engine: a KV page is
// bytes at an address and a shared prefix is a refcount, so both are the same
// on every backend and for every architecture.
//
// This crate was the last piece owned by ONE execution path. While it was, the
// second path had to grow its own paging — and two implementations of "where
// does this sequence's context sit" is exactly the state where a fix lands in
// one of them and the other keeps the bug.

pub mod kv;
pub mod prefix;
