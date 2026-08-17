// ===== File: grammar.rs — compiled byte/codepoint-level grammar automaton =====
// A grammar is a set of rules, each a flat element stream (the proven
// llama.cpp GBNF encoding): alternates separated by `Alt`, terminated by
// `End`. A character class is `Char`/`CharNot` optionally followed by
// `CharRangeUpper` (range) and further `CharAlt` entries (union). Everything
// else — repetitions, groups, optionals — is desugared by the builders into
// auxiliary rules referencing each other, so the runtime automaton only ever
// walks single-codepoint terminals and rule references.

/// One element of a rule's flat instruction stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElemType {
    /// End of a rule (after the last alternate).
    End,
    /// Separator between alternates of the same rule.
    Alt,
    /// Reference to another rule (`value` = rule index).
    RuleRef,
    /// Start of a positive char class / a literal codepoint (`value` = cp).
    Char,
    /// Upper bound of the immediately preceding `Char`/`CharAlt` range.
    CharRangeUpper,
    /// Additional codepoint (or range start) in the current char class.
    CharAlt,
    /// Start of a negated char class (`value` = first excluded cp).
    CharNot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Elem {
    pub ty: ElemType,
    pub value: u32,
}

impl Elem {
    pub fn new(ty: ElemType, value: u32) -> Self {
        Self { ty, value }
    }
}

/// A compiled grammar: a list of rules (each a flat `Elem` stream) plus the
/// index of the root rule.
#[derive(Debug, Clone)]
pub struct Grammar {
    pub rules: Vec<Vec<Elem>>,
    pub root: usize,
}

/// A parse position: `(rule index, element index)` into `Grammar::rules`.
pub type Pos = (u32, u32);
/// One nondeterministic parser stack. The last entry is the top (next to
/// process). An empty stack means "this alternate is complete".
pub type Stack = Vec<Pos>;

impl Grammar {
    /// Initial normalized stack set: enter every alternate of the root rule
    /// and advance each until a terminal (or completion) sits on top.
    pub fn init_stacks(&self) -> Vec<Stack> {
        let mut out = Vec::new();
        for alt_start in self.alternate_starts(self.root) {
            let rule = &self.rules[self.root];
            let stack: Stack = if rule[alt_start as usize].ty == ElemType::End
                || rule[alt_start as usize].ty == ElemType::Alt
            {
                Vec::new()
            } else {
                vec![(self.root as u32, alt_start)]
            };
            self.advance_stack_d(stack, &mut out, 0);
        }
        dedup_stacks(out)
    }

    /// Recursion-guarded entry to stack expansion.
    fn advance_stack(&self, stack: Stack, out: &mut Vec<Stack>) {
        self.advance_stack_d(stack, out, 0);
    }

    /// Maximum rule-reference expansion depth before a branch is abandoned;
    /// guards against nullable left-recursive user grammars overflowing the
    /// stack (well-formed grammars never approach it).
    const MAX_DEPTH: u32 = 4096;

    /// Advance `stack` until its top is a terminal char class (a ready state)
    /// or it is empty (a complete state), expanding any rule references on
    /// top. Each resulting normalized stack is pushed onto `out`.
    fn advance_stack_d(&self, stack: Stack, out: &mut Vec<Stack>, depth: u32) {
        if depth > Self::MAX_DEPTH {
            return;
        }
        let Some(&(rid, idx)) = stack.last() else {
            out.push(stack);
            return;
        };
        let elem = self.rules[rid as usize][idx as usize];
        match elem.ty {
            ElemType::RuleRef => {
                let refrule = elem.value as usize;
                let cont = idx + 1; // element after the ref = next term
                for alt_start in self.alternate_starts(refrule) {
                    let mut ns = stack.clone();
                    ns.pop();
                    if !is_terminator(self.rules[rid as usize][cont as usize].ty) {
                        ns.push((rid, cont));
                    }
                    let sub = &self.rules[refrule];
                    let first = sub[alt_start as usize];
                    if first.ty != ElemType::End && first.ty != ElemType::Alt {
                        ns.push((refrule as u32, alt_start));
                    }
                    self.advance_stack_d(ns, out, depth + 1);
                }
            }
            ElemType::Char | ElemType::CharNot => out.push(stack),
            _ => {
                // A rule element vector should never place these on top of a
                // stack; treat as a dead end rather than panic.
            }
        }
    }

    /// Element indices at which each alternate of `rule` begins.
    fn alternate_starts(&self, rule: usize) -> Vec<u32> {
        let mut starts = vec![0u32];
        let elems = &self.rules[rule];
        let mut i = 0usize;
        while i < elems.len() {
            match elems[i].ty {
                ElemType::End => break,
                ElemType::Alt => starts.push((i + 1) as u32),
                _ => {}
            }
            i += 1;
        }
        starts
    }

    /// Advance the whole stack set by one accepted codepoint. Returns the new
    /// normalized stack set (empty when `cp` is rejected by every stack).
    pub fn accept(&self, stacks: &[Stack], cp: u32) -> Vec<Stack> {
        let mut out = Vec::new();
        for stack in stacks {
            let Some(&(rid, idx)) = stack.last() else {
                continue;
            };
            if !self.char_class_matches(rid as usize, idx as usize, cp) {
                continue;
            }
            let end = self.char_class_end(rid as usize, idx as usize) as u32;
            let mut ns = stack.clone();
            ns.pop();
            if !is_terminator(self.rules[rid as usize][end as usize].ty) {
                ns.push((rid, end));
            }
            self.advance_stack(ns, &mut out);
        }
        dedup_stacks(out)
    }

    /// Whether any stack is in a complete (accepting) state.
    pub fn is_complete(stacks: &[Stack]) -> bool {
        stacks.iter().any(|s| s.is_empty())
    }

    /// Does some codepoint reachable at any stack top have a UTF-8 encoding
    /// that begins with `partial` (an incomplete multi-byte prefix)? Used to
    /// keep byte-fragment tokens sound: a token that leaves an incomplete
    /// scalar is only allowed when a valid, accepted completion exists.
    pub fn any_prefix_accepts(&self, stacks: &[Stack], partial: &[u8]) -> bool {
        let Some((lo, hi)) = utf8_prefix_cp_range(partial) else {
            return false;
        };
        for stack in stacks {
            let Some(&(rid, idx)) = stack.last() else {
                continue;
            };
            if self.char_class_intersects(rid as usize, idx as usize, lo, hi) {
                return true;
            }
        }
        false
    }

    /// Index of the element just past the char class starting at `idx`.
    fn char_class_end(&self, rule: usize, idx: usize) -> usize {
        let elems = &self.rules[rule];
        let mut i = idx + 1; // skip the initial Char/CharNot
        while i < elems.len() && matches!(elems[i].ty, ElemType::CharRangeUpper | ElemType::CharAlt)
        {
            i += 1;
        }
        i
    }

    /// Iterate the (lo, hi) codepoint ranges of the char class at `idx`,
    /// returning whether it is negated and the collected ranges.
    fn char_class_ranges(&self, rule: usize, idx: usize) -> (bool, Vec<(u32, u32)>) {
        let elems = &self.rules[rule];
        let negated = elems[idx].ty == ElemType::CharNot;
        let mut ranges = Vec::new();
        let mut i = idx;
        // First entry.
        let mut lo = elems[i].value;
        i += 1;
        loop {
            if i < elems.len() && elems[i].ty == ElemType::CharRangeUpper {
                ranges.push((lo, elems[i].value));
                i += 1;
            } else {
                ranges.push((lo, lo));
            }
            if i < elems.len() && elems[i].ty == ElemType::CharAlt {
                lo = elems[i].value;
                i += 1;
            } else {
                break;
            }
        }
        (negated, ranges)
    }

    fn char_class_matches(&self, rule: usize, idx: usize, cp: u32) -> bool {
        let (negated, ranges) = self.char_class_ranges(rule, idx);
        let inside = ranges.iter().any(|&(a, b)| cp >= a && cp <= b);
        inside != negated
    }

    /// Whether the char class at `idx` accepts any codepoint in `[lo, hi]`.
    fn char_class_intersects(&self, rule: usize, idx: usize, lo: u32, hi: u32) -> bool {
        let (negated, ranges) = self.char_class_ranges(rule, idx);
        if !negated {
            ranges.iter().any(|&(a, b)| a <= hi && lo <= b)
        } else {
            // Negated: accepted iff SOME cp in [lo,hi] is outside every range.
            // Cheap sufficient check: the interval is larger than the union of
            // the ranges it overlaps, or it extends past them.
            let mut cp = lo;
            // Walk sorted range boundaries within [lo,hi]; bounded work since
            // grammars have few ranges per class.
            let mut sorted = ranges.clone();
            sorted.sort_unstable();
            for &(a, b) in &sorted {
                if cp < a {
                    return true; // gap before this range
                }
                if b >= cp {
                    cp = b.saturating_add(1);
                }
                if cp > hi {
                    return false;
                }
            }
            cp <= hi
        }
    }
}

/// Whether an element type ends the current alternate/sequence frame.
fn is_terminator(ty: ElemType) -> bool {
    matches!(ty, ElemType::End | ElemType::Alt)
}

/// Deduplicate a stack set (order-independent), keeping the automaton small.
fn dedup_stacks(mut stacks: Vec<Stack>) -> Vec<Stack> {
    stacks.sort_unstable();
    stacks.dedup();
    stacks
}

/// The inclusive codepoint range whose UTF-8 encodings start with `partial`,
/// an incomplete (1..=3 byte) UTF-8 prefix. Returns `None` when `partial` is
/// not a valid UTF-8 lead prefix.
fn utf8_prefix_cp_range(partial: &[u8]) -> Option<(u32, u32)> {
    match partial.len() {
        1 => {
            let b0 = partial[0];
            if b0 < 0x80 {
                // ASCII lead is a complete scalar, not a prefix.
                None
            } else if (0xC2..=0xDF).contains(&b0) {
                let base = (b0 as u32 & 0x1F) << 6;
                Some((base, base | 0x3F))
            } else if (0xE0..=0xEF).contains(&b0) {
                // 3-byte lead: clamp to the overlong-encoding minimum (0x800)
                // so a lone lead byte cannot masquerade as a prefix of a
                // lower codepoint (e.g. 0xE0 is never a prefix of ASCII).
                let base = (b0 as u32 & 0x0F) << 12;
                Some((base.max(0x800), (base | 0xFFF).min(0x10FFFF)))
            } else if (0xF0..=0xF4).contains(&b0) {
                let base = (b0 as u32 & 0x07) << 18;
                Some((base.max(0x10000), (base | 0x3FFFF).min(0x10FFFF)))
            } else {
                None
            }
        }
        2 => {
            let (b0, b1) = (partial[0], partial[1]);
            if b1 & 0xC0 != 0x80 {
                return None;
            }
            if (0xE0..=0xEF).contains(&b0) {
                let base = ((b0 as u32 & 0x0F) << 12) | ((b1 as u32 & 0x3F) << 6);
                if base < 0x800 {
                    return None; // overlong: invalid
                }
                Some((base, (base | 0x3F).min(0x10FFFF)))
            } else if (0xF0..=0xF4).contains(&b0) {
                let base = ((b0 as u32 & 0x07) << 18) | ((b1 as u32 & 0x3F) << 12);
                Some((base.max(0x10000), (base | 0xFFF).min(0x10FFFF)))
            } else {
                None
            }
        }
        3 => {
            let (b0, b1, b2) = (partial[0], partial[1], partial[2]);
            if b1 & 0xC0 != 0x80 || b2 & 0xC0 != 0x80 {
                return None;
            }
            if (0xF0..=0xF4).contains(&b0) {
                let base = ((b0 as u32 & 0x07) << 18)
                    | ((b1 as u32 & 0x3F) << 12)
                    | ((b2 as u32 & 0x3F) << 6);
                if base < 0x10000 {
                    return None; // overlong: invalid
                }
                Some((base, (base | 0x3F).min(0x10FFFF)))
            } else {
                None
            }
        }
        _ => None,
    }
}
