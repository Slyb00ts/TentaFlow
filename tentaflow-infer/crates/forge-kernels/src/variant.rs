// ===== File: variant.rs — which form of an operation serves which problem =====
//
// An operation usually has more than one good kernel, and which one is good
// depends on the problem: a matrix product with one token wants a different
// shape of work than one with five hundred. Today that choice lives in `if`
// chains at the call site, with the measurement that justified each threshold
// in a comment beside it. That is how a cliff gets in — some size falls into a
// branch nobody measured, and nothing says so.
//
// A registry makes three things checkable that the `if` chain does not:
//
//   * TOTALITY — every problem is served by something. The last entry must
//     apply to everything, so a shape nobody anticipated degrades instead of
//     failing.
//   * ORDER IS PREFERENCE — the first entry that applies wins, so the list is
//     read top to bottom as "fastest first", and each entry carries the
//     measurement that put it where it is.
//   * NO CLIFF — a size served by a later entry may be slower, but not by a
//     step. That is a measured gate, not a structural one, and it lives with
//     the model because only there does a number mean anything.
//
// This is PLAN_NAPRAWY §6.4 point 1, applied to the forms that exist today.

/// What a kernel is being asked to compute. Enough to choose a form, not enough
/// to run one — the caller still owns the buffers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Problem {
    /// Rows of activation carried together. One means decode.
    pub tokens: u32,
    /// Output width.
    pub rows: u32,
    /// Reduction width.
    pub cols: u32,
    /// Width of one weight code.
    ///
    /// A SECOND axis, and it earns its place: one model can hold both widths at
    /// once. Q4_K_M puts six bits on attn_v, ffn_down and the output head and
    /// four on everything else, so "which form" stops being a question about
    /// shape alone.
    pub bits: u32,
    /// Wiersze OGONA macierzy, które liczy Neural Engine: `[rows - ane_rows, rows)`.
    ///
    /// Zero oznacza „brak ramienia ANE" i tak konstruuje problem każdy, kto
    /// nie ma dla tej wagi skompilowanego modelu CoreML. Liczba jest STAŁA —
    /// zaszyta w kształcie modelu CoreML przy jego budowie — więc nie jest
    /// polityką jak udział CPU, tylko faktem o wadze. `split_rows` wymaga
    /// wielokrotności bloku `QMG_BN` i wartości mniejszej niż `rows`, a
    /// poniżej progu wsadu zeruje ją: dekodowanie nigdy nie idzie na ANE.
    pub ane_rows: u32,
}

impl Problem {
    /// The common case: four-bit weights, no ANE arm.
    pub fn new(tokens: u32, rows: u32, cols: u32) -> Self {
        Self {
            tokens,
            rows,
            cols,
            bits: 4,
            ane_rows: 0,
        }
    }

    /// Ten sam problem z ogonem `ane_rows` wierszy oddanym Neural Engine.
    pub fn with_ane_rows(self, ane_rows: u32) -> Self {
        Self { ane_rows, ..self }
    }
}

/// One way of computing an operation.
pub struct Variant<K: 'static> {
    /// Name as it appears in a trace. Carries the geometry, per §6.4.
    pub name: &'static str,
    pub form: K,
    /// Whether this form may serve the problem AT ALL. Shape divisibility goes
    /// here, and so does the batch range it was measured to win.
    pub applies: fn(&Problem) -> bool,
    /// The measurement that put this entry at this position. Not decoration:
    /// an entry whose order nobody can justify is an entry nobody will dare
    /// reorder later.
    pub because: &'static str,
}

/// An ordered list of forms, fastest first. The last one must be universal.
pub struct Registry<K: 'static> {
    pub op: &'static str,
    pub variants: &'static [Variant<K>],
}

impl<K: Copy + 'static> Registry<K> {
    /// The first form that applies. Never fails when the registry is total,
    /// which `totality_holds` checks over a sweep.
    pub fn pick(&self, problem: &Problem) -> Option<&Variant<K>> {
        self.variants.iter().find(|v| (v.applies)(problem))
    }

    /// Whether the last entry serves this problem — i.e. whether the fallback
    /// really is one.
    pub fn fallback_covers(&self, problem: &Problem) -> bool {
        self.variants.last().is_some_and(|v| (v.applies)(problem))
    }
}

/// Predykat wpisu koncowego: obsluguje kazdy problem. Rejestr bez takiego wpisu
/// nie jest totalny, wiec jakis ksztalt zostalby bez formy.
fn always(_: &Problem) -> bool {
    true
}

// ---------------------------------------------------------------------------
// CUDA — te same reguly, inne formy. Rejestr jest wspolny, bo wybor kernela to
// pytanie o KSZTALT PROBLEMU, a nie o platforme; platforma decyduje tylko,
// ktore formy w ogole istnieja.
// ---------------------------------------------------------------------------

/// Formy iloczynu macierzowego dla wag NVFP4 na CUDA.
#[cfg(not(any(feature = "metal", feature = "metal-check")))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Nvfp4MatmulForm {
    /// Wagi przepakowane do FP8 przy ladowaniu; GEMM czyta e4m3.
    /// Szybsze dzis, ale kosztuje DRUGA kopie wag w pamieci.
    Fp8Repacked,
    /// Wagi czytane w NVFP4, rozpakowywane w kernelu. Jedna kopia wag i
    /// polowa bajtow przez HBM.
    DirectUnpack,
}

#[cfg(not(any(feature = "metal", feature = "metal-check")))]
pub static NVFP4_MATMUL: Registry<Nvfp4MatmulForm> = Registry {
    op: "nvfp4_matmul",
    variants: &[
        Variant {
            name: "fp8_repacked",
            form: Nvfp4MatmulForm::Fp8Repacked,
            // Prefill wielotokenowy: 4 899 wobec 2 064 tok/s na Bieliku 7B.
            // Roznica nie bierze sie z pamieci ani zajetosci — kernel wprost ma
            // 80 rejestrow i 56,4% przepustowosci SM wobec 224 i 43,5% — tylko
            // z tego, ze polowa jego pracy to rozpakowywanie FP4 (jednostka
            // tensorowa 25,1%). Gdy to sie poprawi, kolejnosc tu sie odwroci.
            applies: |p| p.tokens > 1,
            because: "prefill 4899 vs 2064 tok/s (Bielik 7B, prompt 2048)",
        },
        Variant {
            name: "direct_unpack",
            form: Nvfp4MatmulForm::DirectUnpack,
            // Wpis koncowy MUSI obslugiwac wszystko. Dla dekodowania jest tez
            // wlasciwym wyborem: 38,2 wobec 38,4 tok/s, czyli tyle samo, przy
            // 7,35 GB mniej pamieci.
            applies: always,
            because: "decode 38,2 vs 38,4 tok/s przy 7,35 GB mniej pamieci",
        },
    ],
};

// The Metal forms live in a module so their tests sit beside them, but the
// registry is a public interface — `dense_exec` picks its kernel through it.
#[cfg(any(feature = "metal", feature = "metal-check"))]
pub use metal_forms::*;

#[cfg(any(feature = "metal", feature = "metal-check"))]
mod metal_forms {
    use super::*;
    /// Forms of the quantized matrix product on Metal.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum MatmulForm {
        /// One SIMD group per output row, one token. Decode.
        Vector,
        /// A tile of tokens in registers. Batches too small for a matrix block.
        RegisterBlocked,
        /// SIMD matrix units over a block of tokens and rows. Prefill.
        MatrixUnits,
        /// The same kernel over the leading rows, with the tail computed on the
        /// CPU at the same time. Prefill only, and only when the product is
        /// large enough to pay for the command buffer that starting the GPU
        /// early costs.
        MatrixUnitsSharedWithCpu,
        /// Trzy jednostki naraz: GPU na czele, CPU w środku, Neural Engine na
        /// OGONIE wierszy. Ogon ANE jest stały (zaszyty w modelu CoreML), więc
        /// ta forma istnieje tylko dla wag, które taki model mają, i tylko dla
        /// wsadów, przy których podział w ogóle się opłaca.
        MatrixUnitsSharedWithCpuAndAne,
    }

    /// How the rows of one product are divided between the units.
    ///
    /// Układ jest zawsze ten sam: GPU `[0, gpu)`, CPU `[gpu, gpu + cpu)`,
    /// ANE `[rows - ane, rows)`. Kontrakt: `gpu + cpu + ane == rows`,
    /// `gpu % QMG_BN == 0` i `(gpu + cpu) % QMG_BN == 0`, bo kernel
    /// macierzowy pisze całe bloki wierszy, a ogon ANE zaczyna się tam, gdzie
    /// kończy się ostatni blok, który mógłby napisać ktokolwiek inny.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct RowSplit {
        /// Rows `[0, gpu_rows)` — the matrix-unit kernel.
        pub gpu_rows: u32,
        /// Rows `[gpu_rows, gpu_rows + cpu_rows)` — Accelerate on the CPU.
        /// Zero, gdy CPU się nie kwalifikuje, a ANE i tak bierze ogon.
        pub cpu_rows: u32,
        /// Rows `[rows - ane_rows, rows)` — the Neural Engine. Zero without it.
        pub ane_rows: u32,
    }

    /// Fraction of rows left to the GPU, as a function of the batch.
    ///
    /// NOT a constant, and not derived — swept at both ends, and the optimum
    /// moves: 256 tokens peaks at 0,74 (0,72 -> 247,4 tok/s, **0,74 -> 256,5**,
    /// 0,76 -> 240,8) while 512 peaks at 0,70 (0,68 -> 257,0, **0,70 -> 261,9**,
    /// 0,72 -> 258,5).
    ///
    /// It moves because the CPU has to unpack its rows before it can multiply
    /// them, and unpacking costs the same at 256 tokens as at 512 while there
    /// is half as much multiplying to hide it behind. So the smaller the batch,
    /// the worse the CPU's effective rate and the more the GPU should take.
    /// Two measured points and a straight line between them. Above 512 the
    /// line stops: swept at a full 1024-token chunk, 0,67 and 0,70 came out
    /// indistinguishable (268,3 / 266,3 against 267,3 / 264,5 tok/s) and only
    /// 0,64 was clearly worse, so there is nothing there to fit.
    fn gpu_row_share(tokens: u32) -> f32 {
        const AT_256: f32 = 0.74;
        const AT_512: f32 = 0.70;
        let t = tokens.clamp(MIN_SPLIT_TOKENS, 512) as f32;
        AT_256 + (AT_512 - AT_256) * (t - 256.0) / 256.0
    }

    /// Smallest product worth splitting.
    ///
    /// Starting the GPU early means committing a command buffer of its own:
    /// 19,6 us against 0,61 for a dispatch that joins the open one (EKS-A3).
    /// The cut sits between two measured cases and clear of both: k/v at 256
    /// tokens (2,1 GiB of work) stay whole, because the boundary would cost a
    /// fifth of their GPU time, while the same k/v at 512 (4,3 GiB) do split
    /// and are worth +1,2% end to end.
    const MIN_SPLIT_WORK: u64 = 3 * 1024 * 1024 * 1024;

    /// Smallest batch worth splitting.
    ///
    /// The CPU has to unpack its rows before it can multiply them, and that
    /// unpacking costs the same whether it then multiplies by 128 tokens or by
    /// 512 — it is proportional to rows x cols, the product to rows x cols x
    /// tokens. So the CPU's share gets worse as the batch shrinks: 27% overhead
    /// at 256 tokens, about twice that at 128. Measured, that is the difference
    /// between +10,9% at 256 and -17% at 128, which is where this cut sits.
    const MIN_SPLIT_TOKENS: u32 = 256;

    /// Where the boundaries fall, or `None` when the whole product stays on
    /// the GPU. Every shape is allowed to answer `None`; that is the fallback
    /// and it is always correct.
    ///
    /// Ogon ANE (`p.ane_rows`) jest odejmowany NAJPIERW i progi opłacalności
    /// CPU liczą się na tym, co zostaje: to reszta jest dzielona między GPU i
    /// CPU, a nie cała macierz. Gdy CPU się nie kwalifikuje, a ogon ANE jest,
    /// GPU bierze całą resztę i podział wciąż istnieje. Poniżej progu wsadu
    /// ogon jest zerowany bez względu na wejście — dekodowanie jest
    /// ograniczone pasmem i żadna trzecia jednostka mu nie pomoże (EKS-A9,
    /// przy małym T ANE czyta wagi na każde wywołanie) — więc wtedy GPU
    /// liczy wszystko, jak bez ANE.
    ///
    /// Ogon niewyrównany do bloku albo nie mniejszy niż `rows` to odmowa
    /// (`None`), nie przycięcie: model CoreML ma stały kształt, a przycięty
    /// ogon liczyłby inne wiersze, niż ten model oddaje.
    pub fn split_rows(p: &Problem) -> Option<RowSplit> {
        let block = crate::msl::QMG_BN;
        if p.tokens < MIN_SPLIT_TOKENS {
            return None;
        }
        if p.ane_rows > 0 && (!p.ane_rows.is_multiple_of(block) || p.ane_rows >= p.rows) {
            return None;
        }
        let remaining = p.rows - p.ane_rows;
        // Granica GPU/CPU pada na blok, a ogon ANE też jest blokowy, więc
        // reszta musi się składać z całych bloków — inaczej ostatni blok GPU
        // wjechałby w wiersze ANE. Bez ogona kształt sprawdza `qmg_fits`.
        if p.ane_rows > 0 && !remaining.is_multiple_of(block) {
            return None;
        }
        let ane_only = || {
            (p.ane_rows > 0).then_some(RowSplit {
                gpu_rows: remaining,
                cpu_rows: 0,
                ane_rows: p.ane_rows,
            })
        };
        // The CPU unpacker has complete decoders for both affine code widths.
        // Other widths stay on the GPU until their high-bit layout is carried
        // through this contract; using only low nibbles would change the model.
        if !matches!(p.bits, 4 | 6) {
            return ane_only();
        }
        let work = 2 * u64::from(remaining) * u64::from(p.cols) * u64::from(p.tokens);
        if work < MIN_SPLIT_WORK {
            return ane_only();
        }
        // The kernel writes whole blocks of QMG_BN rows, so the boundary has to
        // fall on one or the GPU would overwrite the CPU's rows.
        let gpu = ((remaining as f32 * gpu_row_share(p.tokens)) as u32 / block) * block;
        if gpu == 0 || gpu >= remaining {
            return ane_only();
        }
        Some(RowSplit {
            gpu_rows: gpu,
            cpu_rows: remaining - gpu,
            ane_rows: p.ane_rows,
        })
    }

    /// Forms of attention on Metal.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum AttentionForm {
        /// One threadgroup per (token, head), incremental softmax. Decode.
        PerToken,
        /// Blocked over queries and keys, both products on the matrix units.
        Blocked,
    }

    /// Smallest batch the matrix form takes.
    ///
    /// NOT the block height. The kernel already tolerates a partial block — it
    /// clamps its reads and guards its writes — and a partial block costs
    /// exactly what a full one costs, because the work is the block. So the
    /// question is not "does the batch fill a block" but "is a whole block
    /// cheaper than the blocked form would be for this many tokens", and past
    /// roughly a third of a block it is.
    const MIN_MATRIX_TOKENS: u32 = 32;

    fn qmg_serves(p: &Problem) -> bool {
        p.tokens >= MIN_MATRIX_TOKENS && crate::msl::qmg_fits(p.rows, p.cols)
    }

    fn qmm_serves(p: &Problem) -> bool {
        p.tokens > 1
    }

    /// Order and thresholds from EKS-A4: the matrix form costs 29.8 us per token at
    /// a full block and 176.7 at eight tokens, where the register-blocked form
    /// costs 79.7; the vector form is three times faster than either at one token,
    /// because a tile would compute eight columns and keep one.
    pub const MATMUL_FORMS: Registry<MatmulForm> = Registry {
        op: "qmatmul",
        variants: &[
            Variant {
                name: "qmg_matrix_units_shared_with_cpu_and_ane",
                form: MatmulForm::MatrixUnitsSharedWithCpuAndAne,
                // Tylko gdy podział ma NIEZEROWY ogon ANE: bez niego ten wpis
                // nie ma nic do dodania i przepuszcza problem niżej, więc dla
                // `ane_rows == 0` rejestr odpowiada dokładnie tak jak przedtem.
                applies: |p| qmg_serves(p) && split_rows(p).is_some_and(|s| s.ane_rows > 0),
                because: "EKS-A9: 1,54 + 6,58 + 0,45 TFLOPS współbieżnie",
            },
            Variant {
                name: "qmg_matrix_units_shared_with_cpu",
                form: MatmulForm::MatrixUnitsSharedWithCpu,
                applies: |p| qmg_serves(p) && split_rows(p).is_some(),
                because: "EKS-A7: 3,02 + 1,47 TFLOPS współbieżnie, GPU traci 0,3%",
            },
            Variant {
                name: "qmg_matrix_units",
                form: MatmulForm::MatrixUnits,
                applies: qmg_serves,
                because: "EKS-A4: 29,8 us/token przy pełnym bloku wobec 72,2 blokowo",
            },
            Variant {
                name: "qmm_register_blocked",
                form: MatmulForm::RegisterBlocked,
                applies: qmm_serves,
                because: "EKS-A4: przy 8 tokenach 79,7 us/token wobec 176,7 macierzowo",
            },
            Variant {
                name: "qmv_vector",
                form: MatmulForm::Vector,
                applies: always,
                because: "EKS-A4: przy jednym tokenie 344 us wobec 1004 blokowo",
            },
        ],
    };

    /// Order and thresholds from EKS-A6: the blocked form needs a full block of
    /// queries to be worth its shape, and below it the per-token form is the only
    /// sensible one.
    pub const ATTENTION_FORMS: Registry<AttentionForm> = Registry {
        op: "attention",
        variants: &[
            Variant {
                name: "flash_blocked",
                form: AttentionForm::Blocked,
                applies: |p| p.tokens >= crate::msl::FLASH_BQ,
                because: "EKS-A6: uwaga z 431 na 230 ms przy 1024 tokenach",
            },
            Variant {
                name: "attn_per_token",
                form: AttentionForm::PerToken,
                applies: always,
                because: "EKS-A6: przy jednym tokenie blok liczyłby 31 pustych wierszy",
            },
        ],
    };

    #[cfg(test)]
    mod tests {
        use super::*;

        /// Kształty warstw Bielika-7B plus jeden nietypowy, żeby sprawdzić, że
        /// wybór nie zależy od tego, czy kształt jest „ładny".
        const SHAPES: &[(u32, u32)] = &[
            (4096, 4096),
            (1024, 4096),
            (11264, 4096),
            (4096, 11264),
            (32128, 4096),
            (100, 300),
        ];

        #[test]
        fn every_problem_is_served_by_something() {
            // Bez tego rejestr jest tylko listą: pierwszy kształt, którego nikt nie
            // przewidział, nie ma czym się policzyć i kernel odmawia w środku
            // przebiegu, a nie przy wczytywaniu.
            for &(rows, cols) in SHAPES {
                for tokens in [1u32, 2, 7, 31, 32, 63, 64, 128, 511, 512] {
                    let p = Problem::new(tokens, rows, cols);
                    assert!(
                        MATMUL_FORMS.pick(&p).is_some(),
                        "mnożenie: {p:?} bez wariantu"
                    );
                    assert!(
                        ATTENTION_FORMS.pick(&p).is_some(),
                        "uwaga: {p:?} bez wariantu"
                    );
                    assert!(
                        MATMUL_FORMS.fallback_covers(&p),
                        "mnożenie: ostatni wariant nie jest uniwersalny"
                    );
                    assert!(
                        ATTENTION_FORMS.fallback_covers(&p),
                        "uwaga: ostatni wariant nie jest uniwersalny"
                    );
                }
            }
        }

        #[test]
        fn the_choice_changes_with_the_batch() {
            // Kontrola samego rejestru: gdyby wszystkie problemy trafiały w ten sam
            // wariant, powyższy test przechodziłby i nie znaczyłby nic.
            let shape = (4096u32, 4096u32);
            let at = |tokens| {
                MATMUL_FORMS
                    .pick(&Problem::new(tokens, shape.0, shape.1))
                    .unwrap()
                    .form
            };
            assert_eq!(at(1), MatmulForm::Vector);
            assert_eq!(at(8), MatmulForm::RegisterBlocked);
            assert_eq!(at(128), MatmulForm::MatrixUnits);
        }

        #[test]
        fn only_products_that_can_pay_for_the_boundary_are_shared_with_the_cpu() {
            let at = |tokens, rows, cols| {
                MATMUL_FORMS
                    .pick(&Problem::new(tokens, rows, cols))
                    .unwrap()
                    .form
            };
            // gate/up i q/o przy pełnym kaflu — dość pracy, żeby granica
            // kosztowała kilka procent.
            assert_eq!(at(256, 11264, 4096), MatmulForm::MatrixUnitsSharedWithCpu);
            assert_eq!(at(256, 4096, 4096), MatmulForm::MatrixUnitsSharedWithCpu);
            // k/v są za małe: granica zjadłaby jedną piątą czasu GPU.
            assert_eq!(at(256, 1024, 4096), MatmulForm::MatrixUnits);
            // Przy 128 tokenach rozpakowanie kosztuje tyle samo, a jest czym
            // dzielić o połowę mniej — zmierzone -17%, więc podziału nie ma
            // NAWET dla największego kształtu.
            assert_eq!(at(128, 11264, 4096), MatmulForm::MatrixUnits);
            // Dekodowanie nie ma prawa się dzielić NIEZALEŻNIE od kształtu —
            // jest ograniczone pasmem, a pomiar pokazał tam 20,9 -> 17,9 tok/s.
            assert_eq!(at(1, 11264, 4096), MatmulForm::Vector);
            assert!(split_rows(&Problem::new(1, 11264, 4096)).is_none());
        }

        /// Sześciobitowa waga NIGDY nie idzie w podział.
        ///
        /// Sześciobitowa waga również może dzielić prefill, bo CPU przenosi
        /// kompletne high bits razem z niską płaszczyzną.
        #[test]
        fn a_six_bit_weight_can_be_shared_with_the_cpu() {
            let six = |tokens, rows, cols| Problem {
                tokens,
                rows,
                cols,
                bits: 6,
                ane_rows: 0,
            };
            assert!(split_rows(&six(256, 11264, 4096)).is_some());
            assert!(split_rows(&six(1024, 4096, 11264)).is_some());
            assert_eq!(
                MATMUL_FORMS.pick(&six(1024, 4096, 11264)).unwrap().form,
                MatmulForm::MatrixUnitsSharedWithCpu,
                "sześć bitów ma użyć kompletnego dekodera CPU"
            );
            // Bramka dotyczy szerokości kodu, a nie konkretnego modelu.
            assert!(split_rows(&Problem::new(1024, 4096, 11264)).is_some());
        }

        #[test]
        fn the_split_leaves_whole_blocks_to_the_gpu_and_the_rest_to_the_cpu() {
            let p = Problem::new(256, 11264, 4096);
            let s = split_rows(&p).expect("gate_proj powinien się dzielić");
            // Gdyby granica nie padła na blok, kernel nadpisałby wiersze CPU.
            assert_eq!(s.gpu_rows % crate::msl::QMG_BN, 0);
            assert_eq!(s.gpu_rows + s.cpu_rows, p.rows, "wiersze muszą się domykać");
            assert!(s.cpu_rows > 0, "podział bez pracy dla CPU to nie podział");
            // Udział ma odpowiadać zmierzonemu optimum dla TEGO wsadu, z
            // dokładnością do zaokrąglenia w dół do pełnego bloku.
            let want = f64::from(gpu_row_share(p.tokens));
            let share = f64::from(s.gpu_rows) / f64::from(p.rows);
            let block = f64::from(crate::msl::QMG_BN) / f64::from(p.rows);
            assert!(
                share <= want && share > want - block,
                "udział GPU {share:.4} nie jest zaokrągleniem {want:.4} w dół do bloku"
            );

            // Mniejszy wsad musi zostawiać GPU WIĘCEJ, bo rozpakowanie po
            // stronie CPU kosztuje tyle samo, a jest czym je ukryć o połowę mniej.
            let at = |t| split_rows(&Problem::new(t, 11264, 4096)).unwrap();
            assert!(
                at(256).gpu_rows > at(512).gpu_rows,
                "udział GPU nie maleje z rosnącym wsadem"
            );
        }

        /// Kształty Bielika z ogonem ANE: suma wierszy się domyka, granice
        /// padają na bloki, a udział GPU liczy się z RESZTY, nie z całości.
        #[test]
        fn the_ane_tail_comes_off_first_and_the_rest_is_split_as_before() {
            let block = crate::msl::QMG_BN;
            for &(rows, cols, ane) in &[
                (11264u32, 4096u32, 3072u32),
                (4096, 11264, 1024),
                (4096, 4096, 1024),
                (4096, 4096, 64),
            ] {
                for tokens in [256u32, 512, 1024] {
                    let p = Problem::new(tokens, rows, cols).with_ane_rows(ane);
                    let s = split_rows(&p).expect("ogon ANE to zawsze jakiś podział");
                    assert_eq!(s.ane_rows, ane, "{p:?}");
                    assert_eq!(s.gpu_rows + s.cpu_rows + s.ane_rows, rows, "{p:?}");
                    assert_eq!(s.gpu_rows % block, 0, "{p:?}");
                    assert_eq!((s.gpu_rows + s.cpu_rows) % block, 0, "{p:?}");
                    assert!(s.gpu_rows > 0, "{p:?}: GPU bez pracy");
                    // Reszta dzieli się DOKŁADNIE tak, jak dzieliłaby się
                    // macierz o tylu wierszach bez ANE.
                    let without = split_rows(&Problem::new(tokens, rows - ane, cols));
                    match without {
                        Some(w) => {
                            assert_eq!((w.gpu_rows, w.cpu_rows), (s.gpu_rows, s.cpu_rows), "{p:?}");
                        }
                        None => assert_eq!((s.gpu_rows, s.cpu_rows), (rows - ane, 0), "{p:?}"),
                    }
                }
            }
        }

        #[test]
        fn below_the_batch_threshold_the_ane_tail_is_zeroed_whatever_the_caller_said() {
            // Dekodowanie i małe wsady: GPU bierze wszystko, jak bez ANE.
            for tokens in [1u32, 8, 32, 128, 255] {
                let p = Problem::new(tokens, 11264, 4096).with_ane_rows(3072);
                assert!(split_rows(&p).is_none(), "{p:?}");
                let form = MATMUL_FORMS.pick(&p).unwrap().form;
                assert_ne!(form, MatmulForm::MatrixUnitsSharedWithCpuAndAne, "{p:?}");
                assert_ne!(form, MatmulForm::MatrixUnitsSharedWithCpu, "{p:?}");
                assert_eq!(
                    form,
                    MATMUL_FORMS
                        .pick(&Problem::new(tokens, 11264, 4096))
                        .unwrap()
                        .form,
                    "{p:?}: ogon ANE zmienił wybór poniżej progu"
                );
            }
        }

        #[test]
        fn the_cpu_drops_out_when_what_is_left_after_the_ane_tail_is_too_small() {
            // k/v: 1024 wierszy z ogonem 512 zostawia 512 x 4096 x 256 — o rząd
            // za mało pracy na granicę bufora poleceń. ANE zostaje, CPU nie.
            let p = Problem::new(256, 1024, 4096).with_ane_rows(512);
            let s = split_rows(&p).expect("ogon ANE bez CPU to nadal podział");
            assert_eq!(
                s,
                RowSplit {
                    gpu_rows: 512,
                    cpu_rows: 0,
                    ane_rows: 512
                }
            );
            assert_eq!(
                MATMUL_FORMS.pick(&p).unwrap().form,
                MatmulForm::MatrixUnitsSharedWithCpuAndAne
            );
            // Szerokość kodu, której CPU nie dekoduje, też wyłącza tylko CPU.
            let eight = Problem {
                bits: 8,
                ..Problem::new(1024, 11264, 4096).with_ane_rows(3072)
            };
            let s = split_rows(&eight).expect("ANE nie zależy od dekodera CPU");
            assert_eq!(
                s,
                RowSplit {
                    gpu_rows: 8192,
                    cpu_rows: 0,
                    ane_rows: 3072
                }
            );
            // Bez ogona ten sam brak pracy oznacza brak podziału — jak dotąd.
            assert!(split_rows(&Problem::new(256, 1024, 4096)).is_none());
        }

        #[test]
        fn a_misaligned_or_oversized_ane_tail_is_refused_not_trimmed() {
            let at = |ane| split_rows(&Problem::new(512, 11264, 4096).with_ane_rows(ane));
            assert!(at(3072).is_some(), "kontrola: wyrównany ogon przechodzi");
            assert!(at(3000).is_none(), "ogon nie na bloku");
            assert!(at(32).is_none(), "pół bloku");
            assert!(at(11264).is_none(), "ogon równy całości");
            assert!(at(11264 + 64).is_none(), "ogon większy niż macierz");
            // Odmowa spada na formę bez ANE, a nie na błąd w środku przebiegu.
            let p = Problem::new(512, 11264, 4096).with_ane_rows(3000);
            let form = MATMUL_FORMS.pick(&p).unwrap().form;
            assert_eq!(form, MatmulForm::MatrixUnits);
            // Reszta niewyrównana do bloku również: 4096 - 64 = 4032 jest
            // blokowe, ale 4100 - 64 nie.
            assert!(split_rows(&Problem::new(512, 4100, 4096).with_ane_rows(64)).is_none());
        }

        #[test]
        fn the_ane_form_sits_first_and_is_picked_only_with_a_tail() {
            assert_eq!(
                MATMUL_FORMS.variants[0].form,
                MatmulForm::MatrixUnitsSharedWithCpuAndAne,
                "forma z trzema jednostkami ma być pierwsza — jest najszybsza"
            );
            let with = Problem::new(512, 11264, 4096).with_ane_rows(3072);
            assert_eq!(
                MATMUL_FORMS.pick(&with).unwrap().form,
                MatmulForm::MatrixUnitsSharedWithCpuAndAne
            );
            let without = Problem::new(512, 11264, 4096);
            assert_eq!(
                MATMUL_FORMS.pick(&without).unwrap().form,
                MatmulForm::MatrixUnitsSharedWithCpu,
                "bez ogona wybór ma być ten, co dotąd"
            );
            assert_eq!(
                split_rows(&without).unwrap(),
                RowSplit {
                    gpu_rows: 7872,
                    cpu_rows: 3392,
                    ane_rows: 0
                }
            );
        }

        #[test]
        fn a_shape_the_matrix_form_cannot_take_falls_back_instead_of_failing() {
            // 300 kolumn nie dzieli się na bloki po 32, więc forma macierzowa nie
            // ma prawa jej dotknąć — i właśnie dlatego rejestr ma ostatni wpis.
            let p = Problem::new(256, 100, 300);
            assert_eq!(
                MATMUL_FORMS.pick(&p).unwrap().form,
                MatmulForm::RegisterBlocked
            );
        }

        #[test]
        fn every_entry_says_why_it_is_where_it_is() {
            let named: Vec<(&str, &str)> = MATMUL_FORMS
                .variants
                .iter()
                .map(|v| (v.name, v.because))
                .chain(ATTENTION_FORMS.variants.iter().map(|v| (v.name, v.because)))
                .collect();
            for (name, because) in named {
                assert!(
                    because.contains("EKS-"),
                    "{name}: uzasadnienie bez odwołania do pomiaru"
                );
                assert!(!name.is_empty());
            }
        }
    }
}

#[cfg(all(test, not(any(feature = "metal", feature = "metal-check"))))]
mod cuda_registry_tests {
    use super::*;

    fn problem(tokens: u32, rows: u32, cols: u32) -> Problem {
        Problem::new(tokens, rows, cols)
    }

    /// Totalnosc: kazdy ksztalt dostaje jakas forme. To wlasnie ta wlasnosc
    /// odrozia rejestr od lancucha `if` — tam ksztalt, ktorego nikt nie
    /// przewidzial, po prostu wypada.
    #[test]
    fn every_shape_gets_a_form() {
        for tokens in [1u32, 2, 7, 64, 1024, 4096] {
            for (rows, cols) in [(4096u32, 4096u32), (11264, 4096), (1024, 4096)] {
                let p = problem(tokens, rows, cols);
                assert!(
                    NVFP4_MATMUL.pick(&p).is_some(),
                    "brak formy dla {tokens} tokenow, {rows}x{cols}"
                );
                assert!(
                    NVFP4_MATMUL.fallback_covers(&p),
                    "wpis koncowy nie obsluguje {tokens} tokenow"
                );
            }
        }
    }

    /// Dekodowanie (jeden token) ma isc sciezka bez drugiej kopii wag: tam
    /// przepakowanie do FP8 nic nie daje (38,2 vs 38,4 tok/s), a kosztuje
    /// 7,35 GB.
    #[test]
    fn decode_prefers_the_path_without_a_second_copy() {
        let f = NVFP4_MATMUL.pick(&problem(1, 4096, 4096)).expect("forma");
        assert_eq!(f.form, Nvfp4MatmulForm::DirectUnpack);
    }

    /// Prefill dzis wybiera przepakowanie — dopoki kernel wprost nie przestanie
    /// tracic polowy pracy na rozpakowywanie.
    #[test]
    fn prefill_prefers_the_faster_kernel_today() {
        let f = NVFP4_MATMUL
            .pick(&problem(2048, 11264, 4096))
            .expect("forma");
        assert_eq!(f.form, Nvfp4MatmulForm::Fp8Repacked);
    }
}
