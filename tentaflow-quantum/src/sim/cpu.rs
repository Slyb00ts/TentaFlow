// ===== File: sim/cpu.rs — CPU state-vector backend (rayon natively, single-thread in wasm) =====

use num_complex::{Complex, Complex64};

use super::{cast, uncast, Backend, GateOp, Precision, Scalar};

/// Below this many amplitudes per half-block the inner loop is left serial:
/// splitting it costs more than the work it hands out.
#[cfg(not(target_arch = "wasm32"))]
const INNER_PARALLEL_THRESHOLD: usize = 1 << 12;

/// Size of the chunk a run of cache-local gates works on. It has to stay in a
/// core's L2 while every gate of the run passes over it; 256 KiB fits the
/// smallest L2 on any machine this runs on.
const CHUNK_BYTES: usize = 256 * 1024;

pub struct CpuBackend<S: Scalar> {
    num_qubits: usize,
    amps: Vec<Complex<S>>,
}

impl<S: Scalar> CpuBackend<S> {
    pub fn new(num_qubits: usize) -> CpuBackend<S> {
        let mut backend = CpuBackend {
            num_qubits,
            amps: vec![Complex::new(S::zero(), S::zero()); 1usize << num_qubits],
        };
        backend.reset_to_zero();
        backend
    }

    /// Apply `ops` in order, taking the state through memory as few times as
    /// the circuit allows. A gate is one pass over the whole register, and past
    /// a few million amplitudes that pass is bound by memory bandwidth, not by
    /// arithmetic. Gates whose qubits all sit below `local_qubits` only ever mix
    /// amplitudes inside one aligned chunk, so a run of them is applied chunk by
    /// chunk while the chunk is still in cache: one pass for the whole run.
    fn apply_batch(&mut self, ops: &[GateOp]) {
        let local = local_qubits::<S>(self.num_qubits);
        if local >= self.num_qubits || ops.len() < 2 {
            for op in ops {
                Kernel::<S>::new(op).run(&mut self.amps);
            }
            return;
        }
        let mut rest: Vec<Kernel<S>> = ops.iter().map(Kernel::new).collect();
        while !rest.is_empty() {
            let (cached, pending) = split_cached(rest, local);
            if cached.is_empty() {
                // The first gate reaches above the chunk, so it is a pass of its
                // own; everything behind it is looked at again.
                let mut pending = pending.into_iter();
                if let Some(first) = pending.next() {
                    first.run(&mut self.amps);
                }
                rest = pending.collect();
                continue;
            }
            for_each_chunk(&mut self.amps, 1usize << local, |chunk| {
                for kernel in &cached {
                    kernel.run_serial(chunk);
                }
            });
            rest = pending;
        }
    }
}

/// How many low qubits a chunk covers: the largest power of two of amplitudes
/// that fits `CHUNK_BYTES`.
fn local_qubits<S: Scalar>(num_qubits: usize) -> usize {
    let amplitude = std::mem::size_of::<Complex<S>>();
    let fit = (CHUNK_BYTES / amplitude).ilog2() as usize;
    fit.min(num_qubits)
}

/// Split a program into the gates that can run in one cached pass and the ones
/// that have to wait. A gate may join the cached run only when it is local and
/// shares no qubit with an earlier gate that is being deferred, because gates on
/// disjoint qubits commute and a gate behind a deferred one on the same qubit
/// does not. Order inside each half is the program order.
fn split_cached<S: Scalar>(ops: Vec<Kernel<S>>, local: usize) -> (Vec<Kernel<S>>, Vec<Kernel<S>>) {
    let mut cached = Vec::new();
    let mut pending = Vec::new();
    let mut deferred: u64 = 0;
    for op in ops {
        let mask = op.qubit_mask();
        if op.highest_qubit() < local && deferred & mask == 0 {
            cached.push(op);
        } else {
            deferred |= mask;
            pending.push(op);
        }
    }
    (cached, pending)
}

/// The 2x2 block a gate applies, classified by its structure. Most gates are
/// far from dense — `rz`, `s`, `t`, `z` are diagonal, `x` and `y` are
/// anti-diagonal — and the dense product spends four complex multiplications
/// per pair on zeros.
enum Block<S: Scalar> {
    Dense([Complex<S>; 4]),
    Diagonal([Complex<S>; 2]),
    /// `x` is the case with both entries 1: a pure swap, no arithmetic.
    AntiDiagonal {
        entries: [Complex<S>; 2],
        swap_only: bool,
    },
}

impl<S: Scalar> Block<S> {
    fn classify(m: &[Complex64; 4]) -> Block<S> {
        let zero = |z: Complex64| z.re == 0.0 && z.im == 0.0;
        let one = |z: Complex64| z.re == 1.0 && z.im == 0.0;
        if zero(m[1]) && zero(m[2]) {
            Block::Diagonal([cast(m[0]), cast(m[3])])
        } else if zero(m[0]) && zero(m[3]) {
            Block::AntiDiagonal {
                entries: [cast(m[1]), cast(m[2])],
                swap_only: one(m[1]) && one(m[2]),
            }
        } else {
            Block::Dense([cast(m[0]), cast(m[1]), cast(m[2]), cast(m[3])])
        }
    }

    /// The block applied to every pair `(a, b)` that `iter` visits.
    fn drive<I: Visit>(&self, amps: &mut [Complex<S>], half: usize) {
        match self {
            Block::Dense(m) => {
                let m = *m;
                I::pairs(amps, half, move |a, b| mix_pair(&m, a, b));
            }
            Block::Diagonal([d0, d1]) => {
                let (d0, d1) = (*d0, *d1);
                I::pairs(amps, half, move |a, b| {
                    *a = d0 * *a;
                    *b = d1 * *b;
                });
            }
            Block::AntiDiagonal {
                swap_only: true, ..
            } => I::pairs(amps, half, std::mem::swap),
            Block::AntiDiagonal {
                entries: [e0, e1], ..
            } => {
                let (e0, e1) = (*e0, *e1);
                I::pairs(amps, half, move |a, b| {
                    let (x, y) = (*a, *b);
                    *a = e0 * y;
                    *b = e1 * x;
                });
            }
        }
    }
}

/// One gate with its matrix already in the register's precision, and the
/// structure that lets it skip work.
enum Kernel<S: Scalar> {
    One {
        qubit: usize,
        block: Block<S>,
    },
    /// A two-qubit gate that acts on one 2x2 block of the register only: the
    /// control qubit selects half of the amplitudes and the rest is untouched
    /// (`cx`, `cy`, `ch`, `crx`, `cu`, ...).
    Controlled {
        high: usize,
        low: usize,
        control_is_high: bool,
        block: Block<S>,
    },
    /// Diagonal in the computational basis (`cz`, `cp`, `rzz`, ...).
    Diagonal {
        high: usize,
        low: usize,
        entries: [Complex<S>; 4],
    },
    Dense {
        high: usize,
        low: usize,
        matrix: [Complex<S>; 16],
    },
}

impl<S: Scalar> Kernel<S> {
    fn new(op: &GateOp) -> Kernel<S> {
        match op {
            GateOp::One { qubit, matrix } => Kernel::One {
                qubit: *qubit,
                block: Block::classify(matrix),
            },
            GateOp::Two { qubits, matrix } => {
                let (first, second) = *qubits;
                // The kernel always addresses (high bit, low bit); when the
                // first operand is the low bit the matrix basis order 01 <-> 10
                // is swapped.
                let ordered = if first > second {
                    *matrix
                } else {
                    swap_basis_middle(matrix)
                };
                let (high, low) = (first.max(second), first.min(second));
                Self::classify_two(high, low, &ordered)
            }
        }
    }

    fn classify_two(high: usize, low: usize, m: &[Complex64; 16]) -> Kernel<S> {
        let zero = |z: Complex64| z.re == 0.0 && z.im == 0.0;
        let one = |z: Complex64| z.re == 1.0 && z.im == 0.0;
        let at = |row: usize, col: usize| m[row * 4 + col];
        let diagonal = (0..4).all(|r| (0..4).all(|c| r == c || zero(at(r, c))));
        if diagonal {
            return Kernel::Diagonal {
                high,
                low,
                entries: [
                    cast(at(0, 0)),
                    cast(at(1, 1)),
                    cast(at(2, 2)),
                    cast(at(3, 3)),
                ],
            };
        }
        // Basis index is `high * 2 + low`. A control on the high qubit leaves
        // indices 0 and 1 alone and mixes 2 with 3; a control on the low qubit
        // leaves 0 and 2 alone and mixes 1 with 3.
        let identity_on = |keep: [usize; 2], mix: [usize; 2]| {
            keep.iter().all(|&k| {
                (0..4).all(|c| {
                    if c == k {
                        one(at(k, c))
                    } else {
                        zero(at(k, c))
                    }
                })
            }) && keep
                .iter()
                .all(|&k| mix.iter().all(|&c| zero(at(c, k)) && zero(at(k, c))))
        };
        for (control_is_high, keep, mix) in [(true, [0, 1], [2, 3]), (false, [0, 2], [1, 3])] {
            if identity_on(keep, mix) {
                let block = [
                    at(mix[0], mix[0]),
                    at(mix[0], mix[1]),
                    at(mix[1], mix[0]),
                    at(mix[1], mix[1]),
                ];
                return Kernel::Controlled {
                    high,
                    low,
                    control_is_high,
                    block: Block::classify(&block),
                };
            }
        }
        let mut matrix = [Complex::new(S::zero(), S::zero()); 16];
        for (dst, src) in matrix.iter_mut().zip(m.iter()) {
            *dst = cast(*src);
        }
        Kernel::Dense { high, low, matrix }
    }

    fn highest_qubit(&self) -> usize {
        match self {
            Kernel::One { qubit, .. } => *qubit,
            Kernel::Controlled { high, .. }
            | Kernel::Diagonal { high, .. }
            | Kernel::Dense { high, .. } => *high,
        }
    }

    fn qubit_mask(&self) -> u64 {
        match self {
            Kernel::One { qubit, .. } => 1u64 << qubit,
            Kernel::Controlled { high, low, .. }
            | Kernel::Diagonal { high, low, .. }
            | Kernel::Dense { high, low, .. } => 1u64 << high | 1u64 << low,
        }
    }

    /// One pass over the whole register, spread over the available threads.
    fn run(&self, amps: &mut [Complex<S>]) {
        self.drive::<Parallel>(amps);
    }

    /// The same gate over one chunk, on the calling thread. The chunk has to
    /// span every qubit the gate touches.
    fn run_serial(&self, chunk: &mut [Complex<S>]) {
        self.drive::<Serial>(chunk);
    }

    fn drive<I: Visit>(&self, amps: &mut [Complex<S>]) {
        match self {
            Kernel::One { qubit, block } => block.drive::<I>(amps, 1usize << qubit),
            Kernel::Controlled {
                high,
                low,
                control_is_high,
                block,
            } => {
                // Pick the two amplitudes the control leaves live out of each
                // quad; the other two are never read.
                macro_rules! controlled {
                    ($pick:expr, $mix:expr) => {
                        I::quads(amps, *high, *low, move |a00, a01, a10, a11| {
                            let (a, b) = $pick(a00, a01, a10, a11);
                            $mix(a, b)
                        })
                    };
                }
                match (control_is_high, block) {
                    (true, Block::Dense(m)) => {
                        let m = *m;
                        controlled!(|_, _, a, b| (a, b), |a, b| mix_pair(&m, a, b))
                    }
                    (false, Block::Dense(m)) => {
                        let m = *m;
                        controlled!(|_, a, _, b| (a, b), |a, b| mix_pair(&m, a, b))
                    }
                    (true, Block::Diagonal([d0, d1])) => {
                        let (d0, d1) = (*d0, *d1);
                        controlled!(
                            |_, _, a, b| (a, b),
                            |a: &mut Complex<S>, b: &mut Complex<S>| {
                                *a = d0 * *a;
                                *b = d1 * *b;
                            }
                        )
                    }
                    (false, Block::Diagonal([d0, d1])) => {
                        let (d0, d1) = (*d0, *d1);
                        controlled!(
                            |_, a, _, b| (a, b),
                            |a: &mut Complex<S>, b: &mut Complex<S>| {
                                *a = d0 * *a;
                                *b = d1 * *b;
                            }
                        )
                    }
                    (
                        true,
                        Block::AntiDiagonal {
                            entries: [e0, e1],
                            swap_only,
                        },
                    ) => {
                        let (e0, e1, swap_only) = (*e0, *e1, *swap_only);
                        controlled!(
                            |_, _, a, b| (a, b),
                            |a: &mut Complex<S>, b: &mut Complex<S>| {
                                anti_diagonal(e0, e1, swap_only, a, b)
                            }
                        )
                    }
                    (
                        false,
                        Block::AntiDiagonal {
                            entries: [e0, e1],
                            swap_only,
                        },
                    ) => {
                        let (e0, e1, swap_only) = (*e0, *e1, *swap_only);
                        controlled!(
                            |_, a, _, b| (a, b),
                            |a: &mut Complex<S>, b: &mut Complex<S>| {
                                anti_diagonal(e0, e1, swap_only, a, b)
                            }
                        )
                    }
                }
            }
            Kernel::Diagonal { high, low, entries } => {
                let [d0, d1, d2, d3] = *entries;
                I::quads(amps, *high, *low, move |a00, a01, a10, a11| {
                    *a00 = d0 * *a00;
                    *a01 = d1 * *a01;
                    *a10 = d2 * *a10;
                    *a11 = d3 * *a11;
                });
            }
            Kernel::Dense { high, low, matrix } => {
                let m = *matrix;
                I::quads(amps, *high, *low, move |a00, a01, a10, a11| {
                    mix_quad(&m, a00, a01, a10, a11)
                });
            }
        }
    }
}

#[inline(always)]
fn anti_diagonal<S: Scalar>(
    e0: Complex<S>,
    e1: Complex<S>,
    swap_only: bool,
    a: &mut Complex<S>,
    b: &mut Complex<S>,
) {
    if swap_only {
        std::mem::swap(a, b);
    } else {
        let (x, y) = (*a, *b);
        *a = e0 * y;
        *b = e1 * x;
    }
}

/// How a kernel walks the register: across all threads for a pass over the whole
/// state, or on the calling thread for the chunk a cached run is working on.
trait Visit {
    fn pairs<S, F>(amps: &mut [Complex<S>], half: usize, f: F)
    where
        S: Scalar,
        F: Fn(&mut Complex<S>, &mut Complex<S>) + Send + Sync;

    fn quads<S, F>(amps: &mut [Complex<S>], high: usize, low: usize, f: F)
    where
        S: Scalar,
        F: Fn(&mut Complex<S>, &mut Complex<S>, &mut Complex<S>, &mut Complex<S>) + Send + Sync;
}

struct Parallel;
struct Serial;

impl Visit for Parallel {
    fn pairs<S, F>(amps: &mut [Complex<S>], half: usize, f: F)
    where
        S: Scalar,
        F: Fn(&mut Complex<S>, &mut Complex<S>) + Send + Sync,
    {
        for_each_pair(amps, half, f);
    }

    fn quads<S, F>(amps: &mut [Complex<S>], high: usize, low: usize, f: F)
    where
        S: Scalar,
        F: Fn(&mut Complex<S>, &mut Complex<S>, &mut Complex<S>, &mut Complex<S>) + Send + Sync,
    {
        for_each_quad(amps, high, low, f);
    }
}

impl Visit for Serial {
    fn pairs<S, F>(amps: &mut [Complex<S>], half: usize, f: F)
    where
        S: Scalar,
        F: Fn(&mut Complex<S>, &mut Complex<S>) + Send + Sync,
    {
        for block in amps.chunks_mut(half << 1) {
            let (lo, hi) = block.split_at_mut(half);
            lo.iter_mut().zip(hi.iter_mut()).for_each(|(a, b)| f(a, b));
        }
    }

    fn quads<S, F>(amps: &mut [Complex<S>], high: usize, low: usize, f: F)
    where
        S: Scalar,
        F: Fn(&mut Complex<S>, &mut Complex<S>, &mut Complex<S>, &mut Complex<S>) + Send + Sync,
    {
        let (high_half, low_half) = (1usize << high, 1usize << low);
        for block in amps.chunks_mut(high_half << 1) {
            let (zero_block, one_block) = block.split_at_mut(high_half);
            for (d0, d1) in zero_block
                .chunks_mut(low_half << 1)
                .zip(one_block.chunks_mut(low_half << 1))
            {
                let (a00, a01) = d0.split_at_mut(low_half);
                let (a10, a11) = d1.split_at_mut(low_half);
                for i in 0..low_half {
                    f(&mut a00[i], &mut a01[i], &mut a10[i], &mut a11[i]);
                }
            }
        }
    }
}

#[inline(always)]
fn mix_pair<S: Scalar>(m: &[Complex<S>; 4], a: &mut Complex<S>, b: &mut Complex<S>) {
    let (x, y) = (*a, *b);
    *a = m[0] * x + m[1] * y;
    *b = m[2] * x + m[3] * y;
}

#[inline(always)]
fn mix_quad<S: Scalar>(
    m: &[Complex<S>; 16],
    a00: &mut Complex<S>,
    a01: &mut Complex<S>,
    a10: &mut Complex<S>,
    a11: &mut Complex<S>,
) {
    let (x0, x1, x2, x3) = (*a00, *a01, *a10, *a11);
    *a00 = m[0] * x0 + m[1] * x1 + m[2] * x2 + m[3] * x3;
    *a01 = m[4] * x0 + m[5] * x1 + m[6] * x2 + m[7] * x3;
    *a10 = m[8] * x0 + m[9] * x1 + m[10] * x2 + m[11] * x3;
    *a11 = m[12] * x0 + m[13] * x1 + m[14] * x2 + m[15] * x3;
}

#[cfg(not(target_arch = "wasm32"))]
fn for_each_chunk<S, F>(amps: &mut [Complex<S>], size: usize, f: F)
where
    S: Scalar,
    F: Fn(&mut [Complex<S>]) + Send + Sync,
{
    use rayon::prelude::*;
    amps.par_chunks_mut(size).for_each(f);
}

#[cfg(target_arch = "wasm32")]
fn for_each_chunk<S, F>(amps: &mut [Complex<S>], size: usize, f: F)
where
    S: Scalar,
    F: Fn(&mut [Complex<S>]) + Send + Sync,
{
    amps.chunks_mut(size).for_each(f);
}

impl<S: Scalar> Backend for CpuBackend<S> {
    fn name(&self) -> &'static str {
        "cpu"
    }

    fn adapter_name(&self) -> Option<&str> {
        None
    }

    fn precision(&self) -> Precision {
        S::PRECISION
    }

    fn num_qubits(&self) -> usize {
        self.num_qubits
    }

    fn reset_to_zero(&mut self) {
        self.amps.fill(Complex::new(S::zero(), S::zero()));
        self.amps[0] = Complex::new(S::one(), S::zero());
    }

    fn set_amplitudes(&mut self, amps: &[Complex64]) {
        debug_assert_eq!(amps.len(), self.amps.len());
        for (dst, src) in self.amps.iter_mut().zip(amps) {
            *dst = cast(*src);
        }
    }

    fn apply(&mut self, ops: &[GateOp]) {
        self.apply_batch(ops);
    }

    fn apply_global_phase(&mut self, angle: f64) {
        let factor = cast::<S>(Complex64::from_polar(1.0, angle));
        for a in self.amps.iter_mut() {
            *a = *a * factor;
        }
    }

    fn probability_of_one(&self, qubit: usize) -> f64 {
        let mask = 1usize << qubit;
        self.amps
            .iter()
            .enumerate()
            .filter(|(i, _)| i & mask != 0)
            .map(|(_, a)| a.norm_sqr().as_f64())
            .sum()
    }

    fn collapse(&mut self, qubit: usize, outcome: bool) {
        let mask = 1usize << qubit;
        let mut norm = 0.0f64;
        for (i, a) in self.amps.iter_mut().enumerate() {
            if ((i & mask) != 0) == outcome {
                norm += a.norm_sqr().as_f64();
            } else {
                *a = Complex::new(S::zero(), S::zero());
            }
        }
        debug_assert!(
            norm > 0.0,
            "collapse onto an outcome with zero probability has no normalisation"
        );
        let scale = S::from_real(1.0 / norm.sqrt());
        for a in self.amps.iter_mut() {
            *a = Complex::new(a.re * scale, a.im * scale);
        }
    }

    fn probabilities(&self) -> Vec<f64> {
        self.amps.iter().map(|a| a.norm_sqr().as_f64()).collect()
    }

    fn sample(&self, sorted_draws: &[f64]) -> Vec<usize> {
        let mut out = Vec::with_capacity(sorted_draws.len());
        let mut cumulative = 0.0f64;
        let mut last = 0usize;
        for (index, amp) in self.amps.iter().enumerate() {
            if out.len() == sorted_draws.len() {
                break;
            }
            let p = amp.norm_sqr().as_f64();
            if p == 0.0 {
                continue;
            }
            cumulative += p;
            last = index;
            while out.len() < sorted_draws.len() && sorted_draws[out.len()] < cumulative {
                out.push(index);
            }
        }
        // Rounding can leave the last draws past the accumulated total; they
        // belong to the last outcome with non-zero probability, which is where
        // the missing mass came from.
        while out.len() < sorted_draws.len() {
            out.push(last);
        }
        out
    }

    fn read_amplitudes(&self, out: &mut [Complex64]) {
        debug_assert_eq!(out.len(), self.amps.len());
        for (dst, src) in out.iter_mut().zip(self.amps.iter()) {
            *dst = uncast(*src);
        }
    }
}

/// Swap the 01 and 10 basis elements of a 4x4 matrix, in rows and columns.
pub(super) fn swap_basis_middle(matrix: &[Complex64; 16]) -> [Complex64; 16] {
    const ORDER: [usize; 4] = [0, 2, 1, 3];
    let mut out = [Complex64::new(0.0, 0.0); 16];
    for (row, &r) in ORDER.iter().enumerate() {
        for (col, &c) in ORDER.iter().enumerate() {
            out[row * 4 + col] = matrix[r * 4 + c];
        }
    }
    out
}

#[cfg(not(target_arch = "wasm32"))]
fn for_each_pair<S, F>(amps: &mut [Complex<S>], half: usize, f: F)
where
    S: Scalar,
    F: Fn(&mut Complex<S>, &mut Complex<S>) + Send + Sync,
{
    use rayon::prelude::*;
    amps.par_chunks_mut(half << 1).for_each(|chunk| {
        let (lo, hi) = chunk.split_at_mut(half);
        if half >= INNER_PARALLEL_THRESHOLD {
            lo.par_iter_mut()
                .zip(hi.par_iter_mut())
                .for_each(|(a, b)| f(a, b));
        } else {
            lo.iter_mut().zip(hi.iter_mut()).for_each(|(a, b)| f(a, b));
        }
    });
}

#[cfg(target_arch = "wasm32")]
fn for_each_pair<S, F>(amps: &mut [Complex<S>], half: usize, f: F)
where
    S: Scalar,
    F: Fn(&mut Complex<S>, &mut Complex<S>) + Send + Sync,
{
    for chunk in amps.chunks_mut(half << 1) {
        let (lo, hi) = chunk.split_at_mut(half);
        lo.iter_mut().zip(hi.iter_mut()).for_each(|(a, b)| f(a, b));
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn for_each_quad<S, F>(amps: &mut [Complex<S>], high: usize, low: usize, f: F)
where
    S: Scalar,
    F: Fn(&mut Complex<S>, &mut Complex<S>, &mut Complex<S>, &mut Complex<S>) + Send + Sync,
{
    use rayon::prelude::*;
    let high_half = 1usize << high;
    let low_half = 1usize << low;
    amps.par_chunks_mut(high_half << 1).for_each(|chunk| {
        let (zero_block, one_block) = chunk.split_at_mut(high_half);
        zero_block
            .chunks_mut(low_half << 1)
            .zip(one_block.chunks_mut(low_half << 1))
            .for_each(|(d0, d1)| {
                let (a00, a01) = d0.split_at_mut(low_half);
                let (a10, a11) = d1.split_at_mut(low_half);
                for i in 0..low_half {
                    f(&mut a00[i], &mut a01[i], &mut a10[i], &mut a11[i]);
                }
            });
    });
}

#[cfg(target_arch = "wasm32")]
fn for_each_quad<S, F>(amps: &mut [Complex<S>], high: usize, low: usize, f: F)
where
    S: Scalar,
    F: Fn(&mut Complex<S>, &mut Complex<S>, &mut Complex<S>, &mut Complex<S>) + Send + Sync,
{
    let high_half = 1usize << high;
    let low_half = 1usize << low;
    for chunk in amps.chunks_mut(high_half << 1) {
        let (zero_block, one_block) = chunk.split_at_mut(high_half);
        for (d0, d1) in zero_block
            .chunks_mut(low_half << 1)
            .zip(one_block.chunks_mut(low_half << 1))
        {
            let (a00, a01) = d0.split_at_mut(low_half);
            let (a10, a11) = d1.split_at_mut(low_half);
            for i in 0..low_half {
                f(&mut a00[i], &mut a01[i], &mut a10[i], &mut a11[i]);
            }
        }
    }
}
