// =============================================================================
// File: collectors/intern.rs — Deduplicating interners for names and stack frames.
// =============================================================================

use std::collections::HashMap;
use tentaflow_protocol::profiling::Frame;

/// Interner for arbitrary strings (kernel names, NVTX labels, API call names).
/// Returns a stable `u32` id for each unique string.
pub struct NameInterner {
    table: HashMap<String, u32>,
    list: Vec<String>,
}

impl NameInterner {
    pub fn new() -> Self {
        Self {
            table: HashMap::new(),
            list: Vec::new(),
        }
    }

    /// Intern a string; returns its index in the final names vector.
    pub fn intern(&mut self, s: &str) -> u32 {
        if let Some(&id) = self.table.get(s) {
            return id;
        }
        let id = u32::try_from(self.list.len()).expect("more than u32::MAX interned names");
        let owned = s.to_owned();
        self.list.push(owned.clone());
        self.table.insert(owned, id);
        id
    }

    /// Consume the interner and return the names vector indexed by the ids
    /// previously returned from `intern`.
    pub fn into_vec(self) -> Vec<String> {
        self.list
    }

    pub fn len(&self) -> usize {
        self.list.len()
    }

    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }
}

impl Default for NameInterner {
    fn default() -> Self {
        Self::new()
    }
}

/// Hash-friendly mirror of `Frame` (the rkyv `Frame` cannot derive `Hash`
/// because of its `Option<u32>` and `Option<String>` fields' equality
/// semantics being preserved here explicitly).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FrameKey {
    pub symbol: String,
    pub module: String,
    pub file: Option<String>,
    pub line: Option<u32>,
}

impl From<Frame> for FrameKey {
    fn from(f: Frame) -> Self {
        Self {
            symbol: f.symbol,
            module: f.module,
            file: f.file,
            line: f.line,
        }
    }
}

impl From<FrameKey> for Frame {
    fn from(k: FrameKey) -> Self {
        Self {
            symbol: k.symbol,
            module: k.module,
            file: k.file,
            line: k.line,
        }
    }
}

/// Interner for stack frames (deduplicated by all four fields) and full
/// stacks (vectors of frame ids).
pub struct FrameInterner {
    table: HashMap<FrameKey, u32>,
    list: Vec<Frame>,
    stack_table: HashMap<Vec<u32>, u32>,
    stacks: Vec<Vec<u32>>,
}

impl FrameInterner {
    pub fn new() -> Self {
        Self {
            table: HashMap::new(),
            list: Vec::new(),
            stack_table: HashMap::new(),
            stacks: Vec::new(),
        }
    }

    /// Intern a single frame; returns the FrameId.
    pub fn intern_frame(&mut self, key: FrameKey) -> u32 {
        if let Some(&id) = self.table.get(&key) {
            return id;
        }
        let id = u32::try_from(self.list.len()).expect("more than u32::MAX interned frames");
        let frame: Frame = key.clone().into();
        self.list.push(frame);
        self.table.insert(key, id);
        id
    }

    /// Intern a full stack (leaf-first ordering); returns the StackId.
    pub fn intern_stack(&mut self, frames_leaf_first: Vec<u32>) -> u32 {
        if let Some(&id) = self.stack_table.get(&frames_leaf_first) {
            return id;
        }
        let id = u32::try_from(self.stacks.len()).expect("more than u32::MAX interned stacks");
        self.stacks.push(frames_leaf_first.clone());
        self.stack_table.insert(frames_leaf_first, id);
        id
    }

    /// Consume the interner; returns `(frames, stacks)` for embedding in a
    /// `ProfileReportV2`.
    pub fn into_parts(self) -> (Vec<Frame>, Vec<Vec<u32>>) {
        (self.list, self.stacks)
    }

    pub fn frame_count(&self) -> usize {
        self.list.len()
    }

    pub fn stack_count(&self) -> usize {
        self.stacks.len()
    }
}

impl Default for FrameInterner {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_interner_dedup() {
        let mut n = NameInterner::new();
        let a = n.intern("foo");
        let b = n.intern("foo");
        let c = n.intern("bar");
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_eq!(n.len(), 2);
    }

    #[test]
    fn name_interner_into_vec_order() {
        let mut n = NameInterner::new();
        let id_alpha = n.intern("alpha");
        let id_beta = n.intern("beta");
        let id_gamma = n.intern("gamma");
        let v = n.into_vec();
        assert_eq!(v[id_alpha as usize], "alpha");
        assert_eq!(v[id_beta as usize], "beta");
        assert_eq!(v[id_gamma as usize], "gamma");
    }

    #[test]
    fn name_interner_empty() {
        let n = NameInterner::new();
        assert!(n.is_empty());
        assert_eq!(n.len(), 0);
    }

    fn key(symbol: &str, module: &str, file: Option<&str>, line: Option<u32>) -> FrameKey {
        FrameKey {
            symbol: symbol.into(),
            module: module.into(),
            file: file.map(str::to_owned),
            line,
        }
    }

    #[test]
    fn frame_interner_dedup_by_all_fields() {
        let mut fi = FrameInterner::new();
        let id1 = fi.intern_frame(key("f", "m", Some("a.rs"), Some(10)));
        let id2 = fi.intern_frame(key("f", "m", Some("a.rs"), Some(10)));
        let id3 = fi.intern_frame(key("f", "m", Some("a.rs"), Some(11))); // different line
        let id4 = fi.intern_frame(key("f", "m", Some("b.rs"), Some(10))); // different file
        let id5 = fi.intern_frame(key("g", "m", Some("a.rs"), Some(10))); // different symbol
        assert_eq!(id1, id2);
        assert_ne!(id1, id3);
        assert_ne!(id1, id4);
        assert_ne!(id1, id5);
        assert_eq!(fi.frame_count(), 4);
    }

    #[test]
    fn stack_interner_dedup() {
        let mut fi = FrameInterner::new();
        let f0 = fi.intern_frame(key("a", "m", None, None));
        let f1 = fi.intern_frame(key("b", "m", None, None));
        let s1 = fi.intern_stack(vec![f0, f1]);
        let s2 = fi.intern_stack(vec![f0, f1]);
        let s3 = fi.intern_stack(vec![f1, f0]); // reversed = different
        assert_eq!(s1, s2);
        assert_ne!(s1, s3);
        assert_eq!(fi.stack_count(), 2);
    }

    #[test]
    fn frame_interner_into_parts_round_trip() {
        let mut fi = FrameInterner::new();
        let f0 = fi.intern_frame(key("a", "m", Some("x.rs"), Some(1)));
        let _s0 = fi.intern_stack(vec![f0]);
        let (frames, stacks) = fi.into_parts();
        assert_eq!(frames.len(), 1);
        assert_eq!(stacks.len(), 1);
        assert_eq!(frames[0].symbol, "a");
        assert_eq!(stacks[0], vec![f0]);
    }
}
