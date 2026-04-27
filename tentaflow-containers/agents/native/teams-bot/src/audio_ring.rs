// =============================================================================
// Plik: audio_ring.rs
// Opis: Cache-friendly ring buffer i16 o stalej pojemnosci (const generic).
//       Uzywany jako prepad ringbuffer w glownym audio loopie — zastepuje
//       VecDeque (pop_front/push_back per sample) tablica inline + write_idx.
// =============================================================================

/// Ring buffer o stalej pojemnosci `N`, dopisuje samples zastepujac
/// najstarsze gdy pelny. `drain_into` zwraca zawartosc w kolejnosci
/// logicznej (najstarszy → najnowszy) przez maks. dwa `extend_from_slice`,
/// co pozwala backendowi zrobic vectorized memcpy zamiast sample-by-sample.
pub struct PrepadRing<const N: usize> {
    buf: [i16; N],
    write_idx: usize,
    len: usize,
}

impl<const N: usize> PrepadRing<N> {
    pub fn new() -> Self {
        Self {
            buf: [0; N],
            write_idx: 0,
            len: 0,
        }
    }

    #[inline]
    pub fn push(&mut self, sample: i16) {
        self.buf[self.write_idx] = sample;
        self.write_idx += 1;
        if self.write_idx == N {
            self.write_idx = 0;
        }
        if self.len < N {
            self.len += 1;
        }
    }

    /// Dopisuje slice; gdy slice jest dluzszy niz `N`, tylko ostatnie `N`
    /// elementow przezyje (zgodnie z semantyka ring buffera).
    #[inline]
    pub fn extend_from_slice(&mut self, samples: &[i16]) {
        // Gdy slice >= cap — wystarczy skopiowac ostatnie N do bufora,
        // ustawic full state i write_idx=0 (najstarszy sample lezy na 0).
        if samples.len() >= N {
            let tail = &samples[samples.len() - N..];
            self.buf.copy_from_slice(tail);
            self.write_idx = 0;
            self.len = N;
            return;
        }
        // Slice mieści się w cap — kopiuj w jednym lub dwóch kawalkach.
        let first_chunk = (N - self.write_idx).min(samples.len());
        self.buf[self.write_idx..self.write_idx + first_chunk]
            .copy_from_slice(&samples[..first_chunk]);
        let remaining = samples.len() - first_chunk;
        if remaining > 0 {
            self.buf[..remaining].copy_from_slice(&samples[first_chunk..]);
            self.write_idx = remaining;
        } else {
            self.write_idx += first_chunk;
            if self.write_idx == N {
                self.write_idx = 0;
            }
        }
        self.len = (self.len + samples.len()).min(N);
    }

    /// Wypisuje zawartosc do `dest` w kolejnosci od najstarszego do najnowszego
    /// i resetuje stan. Maksymalnie dwa memcpy (split przy wrap-around).
    pub fn drain_into(&mut self, dest: &mut Vec<i16>) {
        if self.len < N {
            // Bufor nie jest pelny — zawartosc lezy na pozycjach 0..len
            // (write_idx == len w tym stanie).
            dest.extend_from_slice(&self.buf[..self.len]);
        } else {
            // Pelny — najstarszy sample jest na write_idx, koncuje sie na
            // write_idx-1 modulo N. Dwa memcpy: [write_idx..N] + [0..write_idx].
            dest.extend_from_slice(&self.buf[self.write_idx..]);
            dest.extend_from_slice(&self.buf[..self.write_idx]);
        }
        self.write_idx = 0;
        self.len = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fills_below_capacity() {
        let mut r: PrepadRing<8> = PrepadRing::new();
        r.extend_from_slice(&[1, 2, 3]);
        let mut out = Vec::new();
        r.drain_into(&mut out);
        assert_eq!(out, vec![1, 2, 3]);
    }

    #[test]
    fn wraps_when_full_preserves_order() {
        let mut r: PrepadRing<4> = PrepadRing::new();
        for s in 1..=6i16 {
            r.push(s);
        }
        let mut out = Vec::new();
        r.drain_into(&mut out);
        assert_eq!(out, vec![3, 4, 5, 6]);
    }

    #[test]
    fn extend_with_slice_longer_than_capacity() {
        let mut r: PrepadRing<4> = PrepadRing::new();
        r.extend_from_slice(&[1, 2, 3, 4, 5, 6, 7]);
        let mut out = Vec::new();
        r.drain_into(&mut out);
        assert_eq!(out, vec![4, 5, 6, 7]);
    }

    #[test]
    fn extend_partial_then_wrap() {
        let mut r: PrepadRing<4> = PrepadRing::new();
        r.extend_from_slice(&[1, 2, 3]);
        r.extend_from_slice(&[4, 5, 6]);
        let mut out = Vec::new();
        r.drain_into(&mut out);
        assert_eq!(out, vec![3, 4, 5, 6]);
    }

    #[test]
    fn drain_resets_state() {
        let mut r: PrepadRing<4> = PrepadRing::new();
        r.extend_from_slice(&[1, 2, 3, 4, 5]);
        let mut out = Vec::new();
        r.drain_into(&mut out);
        assert_eq!(out, vec![2, 3, 4, 5]);
        let mut out2 = Vec::new();
        r.drain_into(&mut out2);
        assert!(out2.is_empty());
    }

    #[test]
    fn matches_vecdeque_semantics() {
        use std::collections::VecDeque;
        const CAP: usize = 16;
        let mut r: PrepadRing<CAP> = PrepadRing::new();
        let mut d: VecDeque<i16> = VecDeque::with_capacity(CAP);
        for i in 0..100i16 {
            r.push(i);
            if d.len() >= CAP {
                d.pop_front();
            }
            d.push_back(i);
        }
        let mut out = Vec::new();
        r.drain_into(&mut out);
        let expected: Vec<i16> = d.iter().copied().collect();
        assert_eq!(out, expected);
    }
}
