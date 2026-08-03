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
        self.variants
            .last()
            .is_some_and(|v| (v.applies)(problem))
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
#[cfg(not(all(feature = "metal", target_os = "macos")))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Nvfp4MatmulForm {
    /// Wagi przepakowane do FP8 przy ladowaniu; GEMM czyta e4m3.
    /// Szybsze dzis, ale kosztuje DRUGA kopie wag w pamieci.
    Fp8Repacked,
    /// Wagi czytane w NVFP4, rozpakowywane w kernelu. Jedna kopia wag i
    /// polowa bajtow przez HBM.
    DirectUnpack,
}

#[cfg(not(all(feature = "metal", target_os = "macos")))]
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
// registry is a public interface — `mlx_dense` picks its kernel through it.
#[cfg(all(feature = "metal", target_os = "macos"))]
pub use metal_forms::*;

#[cfg(all(feature = "metal", target_os = "macos"))]
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
    }

    /// How the rows of one product are divided between the two units.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct RowSplit {
        /// Rows `[0, gpu_rows)` — the matrix-unit kernel.
        pub gpu_rows: u32,
        /// Rows `[gpu_rows, rows)` — Accelerate on the CPU.
        pub cpu_rows: u32,
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

    /// Where the boundary falls, or `None` when the whole product stays on the
    /// GPU. Every shape is allowed to answer `None`; that is the fallback and
    /// it is always correct.
    pub fn split_rows(p: &Problem) -> Option<RowSplit> {
        let work = 2 * u64::from(p.rows) * u64::from(p.cols) * u64::from(p.tokens);
        if p.tokens < MIN_SPLIT_TOKENS || work < MIN_SPLIT_WORK {
            return None;
        }
        // The kernel writes whole blocks of QMG_BN rows, so the boundary has to
        // fall on one or the GPU would overwrite the CPU's rows.
        let gpu = ((p.rows as f32 * gpu_row_share(p.tokens)) as u32 / crate::msl::QMG_BN)
            * crate::msl::QMG_BN;
        if gpu == 0 || gpu >= p.rows {
            return None;
        }
        Some(RowSplit {
            gpu_rows: gpu,
            cpu_rows: p.rows - gpu,
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

    fn qmg_serves(p: &Problem) -> bool {
        p.tokens >= crate::msl::QMG_BM && crate::msl::qmg_fits(p.rows, p.cols)
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
                    let p = Problem { tokens, rows, cols };
                    assert!(
                        MATMUL_FORMS.pick(&p).is_some(),
                        "mnożenie: {p:?} bez wariantu"
                    );
                    assert!(
                        ATTENTION_FORMS.pick(&p).is_some(),
                        "uwaga: {p:?} bez wariantu"
                    );
                    assert!(MATMUL_FORMS.fallback_covers(&p), "mnożenie: ostatni wariant nie jest uniwersalny");
                    assert!(ATTENTION_FORMS.fallback_covers(&p), "uwaga: ostatni wariant nie jest uniwersalny");
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
                    .pick(&Problem {
                        tokens,
                        rows: shape.0,
                        cols: shape.1,
                    })
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
                    .pick(&Problem { tokens, rows, cols })
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
            assert!(split_rows(&Problem { tokens: 1, rows: 11264, cols: 4096 }).is_none());
        }

        #[test]
        fn the_split_leaves_whole_blocks_to_the_gpu_and_the_rest_to_the_cpu() {
            let p = Problem {
                tokens: 256,
                rows: 11264,
                cols: 4096,
            };
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
            let at = |t| split_rows(&Problem { tokens: t, rows: 11264, cols: 4096 }).unwrap();
            assert!(
                at(256).gpu_rows > at(512).gpu_rows,
                "udział GPU nie maleje z rosnącym wsadem"
            );
        }

        #[test]
        fn a_shape_the_matrix_form_cannot_take_falls_back_instead_of_failing() {
            // 300 kolumn nie dzieli się na bloki po 32, więc forma macierzowa nie
            // ma prawa jej dotknąć — i właśnie dlatego rejestr ma ostatni wpis.
            let p = Problem {
                tokens: 256,
                rows: 100,
                cols: 300,
            };
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
                .chain(
                    ATTENTION_FORMS
                        .variants
                        .iter()
                        .map(|v| (v.name, v.because)),
                )
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

#[cfg(all(test, not(all(feature = "metal", target_os = "macos"))))]
mod cuda_registry_tests {
    use super::*;

    fn problem(tokens: u32, rows: u32, cols: u32) -> Problem {
        Problem { tokens, rows, cols }
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
        let f = NVFP4_MATMUL.pick(&problem(2048, 11264, 4096)).expect("forma");
        assert_eq!(f.form, Nvfp4MatmulForm::Fp8Repacked);
    }
}
