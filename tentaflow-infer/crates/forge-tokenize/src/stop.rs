// ===== File: stop.rs — stop-sequence matcher with partial-suffix holdback (SPEC 8.1.2) =====
//
// Stop strings can span multiple decoded fragments (multi-token stops, stops
// crossing token boundaries). The matcher buffers the longest suffix of the
// stream that is still a proper prefix of some stop string and only emits
// text once it can no longer be part of a match — so clients never see a
// partial stop sequence, and matched stops are excluded from the output.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StopStep {
    /// Text safe to emit to the client now.
    pub emit: String,
    /// The stop string that matched, if any. Once set the stream is finished:
    /// the stop itself and anything after it are discarded.
    pub matched: Option<String>,
}

pub struct StopMatcher {
    stops: Vec<String>,
    held: String,
    max_hold: usize,
    done: bool,
}

impl StopMatcher {
    pub fn new(stops: impl IntoIterator<Item = String>) -> Self {
        let stops: Vec<String> = stops.into_iter().filter(|s| !s.is_empty()).collect();
        // A partial match can never be longer than the longest stop minus one byte.
        let max_hold = stops.iter().map(|s| s.len()).max().unwrap_or(0).saturating_sub(1);
        Self {
            stops,
            held: String::new(),
            max_hold,
            done: false,
        }
    }

    /// Feed a decoded text fragment. Returns the text that is now safe to
    /// emit plus the matched stop string, if any.
    pub fn push(&mut self, fragment: &str) -> StopStep {
        if self.done {
            return StopStep {
                emit: String::new(),
                matched: None,
            };
        }
        if self.stops.is_empty() {
            return StopStep {
                emit: fragment.to_string(),
                matched: None,
            };
        }
        self.held.push_str(fragment);

        // Earliest full match wins; on a position tie prefer the longest stop
        // so overlapping stops resolve deterministically.
        let mut best: Option<(usize, usize)> = None; // (byte_pos, stop_index)
        for (i, stop) in self.stops.iter().enumerate() {
            if let Some(pos) = self.held.find(stop.as_str()) {
                let better = match best {
                    None => true,
                    Some((best_pos, best_i)) => {
                        pos < best_pos
                            || (pos == best_pos && stop.len() > self.stops[best_i].len())
                    }
                };
                if better {
                    best = Some((pos, i));
                }
            }
        }
        if let Some((pos, i)) = best {
            let emit = self.held[..pos].to_string();
            let matched = self.stops[i].clone();
            self.held.clear();
            self.done = true;
            return StopStep {
                emit,
                matched: Some(matched),
            };
        }

        // No full match: hold back the longest suffix that is still a prefix
        // of some stop and release everything before it.
        let hold_from = self.holdback_start();
        let emit = self.held[..hold_from].to_string();
        self.held.drain(..hold_from);
        StopStep {
            emit,
            matched: None,
        }
    }

    /// Text currently held back pending stop resolution.
    pub fn pending(&self) -> &str {
        &self.held
    }

    /// End of stream: release whatever was held back (it never completed a stop).
    pub fn finish(&mut self) -> String {
        std::mem::take(&mut self.held)
    }

    fn holdback_start(&self) -> usize {
        // Scan candidate suffix starts from the longest possible partial match
        // forward; the first suffix that prefixes any stop is the holdback.
        let min_start = self.held.len().saturating_sub(self.max_hold);
        for (start, _) in self
            .held
            .char_indices()
            .skip_while(|(i, _)| *i < min_start)
        {
            let suffix = &self.held[start..];
            if self.stops.iter().any(|s| s.starts_with(suffix)) {
                return start;
            }
        }
        self.held.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matcher(stops: &[&str]) -> StopMatcher {
        StopMatcher::new(stops.iter().map(|s| s.to_string()))
    }

    #[test]
    fn no_stops_passthrough() {
        let mut m = matcher(&[]);
        let step = m.push("hello");
        assert_eq!(step.emit, "hello");
        assert_eq!(step.matched, None);
        assert_eq!(m.finish(), "");
    }

    #[test]
    fn stop_split_across_fragments() {
        let mut m = matcher(&["</s>"]);
        let a = m.push("Hello wor");
        assert_eq!(a.emit, "Hello wor");
        let b = m.push("ld</");
        assert_eq!(b.emit, "ld");
        assert_eq!(m.pending(), "</");
        let c = m.push("s> trailing junk");
        assert_eq!(c.emit, "");
        assert_eq!(c.matched.as_deref(), Some("</s>"));
        // After a match the stream is done.
        assert_eq!(m.push("more").emit, "");
    }

    #[test]
    fn one_byte_per_fragment_stop() {
        let mut m = matcher(&["STOP"]);
        let mut out = String::new();
        for ch in ["S", "T", "O"] {
            out.push_str(&m.push(ch).emit);
        }
        assert_eq!(out, "");
        let last = m.push("P");
        assert_eq!(last.emit, "");
        assert_eq!(last.matched.as_deref(), Some("STOP"));
    }

    #[test]
    fn false_partial_released() {
        let mut m = matcher(&["</s>"]);
        let mut out = String::new();
        out.push_str(&m.push("a<").emit);
        assert_eq!(m.pending(), "<");
        out.push_str(&m.push("/b").emit);
        assert_eq!(out, "a</b");
        assert_eq!(m.finish(), "");
    }

    #[test]
    fn unresolved_partial_flushed_at_finish() {
        let mut m = matcher(&["\n\nUser:"]);
        let step = m.push("answer\n\nUse");
        assert_eq!(step.emit, "answer");
        assert_eq!(m.pending(), "\n\nUse");
        assert_eq!(m.finish(), "\n\nUse");
    }

    #[test]
    fn earliest_match_wins_with_overlapping_stops() {
        let mut m = matcher(&["b", "ab"]);
        let step = m.push("xxab");
        assert_eq!(step.emit, "xx");
        // Position 2 has both "ab" (len 2) and "b" at position 3; earliest
        // position wins, tie broken by length.
        assert_eq!(step.matched.as_deref(), Some("ab"));
    }

    #[test]
    fn multiple_stops_partial_tracking() {
        let mut m = matcher(&["<|im_end|>", "###"]);
        let a = m.push("text ##");
        assert_eq!(a.emit, "text ");
        assert_eq!(m.pending(), "##");
        let b = m.push("x <|im_");
        assert_eq!(b.emit, "##x ");
        assert_eq!(m.pending(), "<|im_");
        let c = m.push("end|>");
        assert_eq!(c.matched.as_deref(), Some("<|im_end|>"));
        assert_eq!(c.emit, "");
    }

    #[test]
    fn multibyte_utf8_holdback_boundaries() {
        // Holdback scanning must respect char boundaries (CJK stop string).
        let mut m = matcher(&["終わり"]);
        let a = m.push("結果終");
        assert_eq!(a.emit, "結果");
        assert_eq!(m.pending(), "終");
        let b = m.push("わり");
        assert_eq!(b.matched.as_deref(), Some("終わり"));
        assert_eq!(b.emit, "");
    }

    #[test]
    fn property_random_slicing_never_leaks_stop() {
        // Property-style: for many random fragmentations of a text containing
        // a stop, the concatenated emissions must equal the text before the
        // stop and the stop must always match.
        let text = "The quick brown fox<|end|> IGNORED TAIL";
        let stop = "<|end|>";
        let mut seed: u64 = 0x9E3779B97F4A7C15;
        for _ in 0..200 {
            let mut m = matcher(&[stop]);
            let mut out = String::new();
            let mut matched = None;
            let mut rest = text;
            while !rest.is_empty() && matched.is_none() {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                let want = (seed >> 33) as usize % 5 + 1;
                let cut = rest
                    .char_indices()
                    .map(|(i, c)| i + c.len_utf8())
                    .take(want)
                    .last()
                    .unwrap_or(rest.len());
                let (frag, tail) = rest.split_at(cut);
                let step = m.push(frag);
                out.push_str(&step.emit);
                matched = step.matched;
                rest = tail;
            }
            assert_eq!(matched.as_deref(), Some(stop));
            assert_eq!(out, "The quick brown fox");
        }
    }
}
