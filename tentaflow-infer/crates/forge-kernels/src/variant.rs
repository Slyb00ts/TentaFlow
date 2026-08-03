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

/// Forms of the quantized matrix product on Metal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatmulForm {
    /// One SIMD group per output row, one token. Decode.
    Vector,
    /// A tile of tokens in registers. Batches too small for a matrix block.
    RegisterBlocked,
    /// SIMD matrix units over a block of tokens and rows. Prefill.
    MatrixUnits,
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

fn always(_: &Problem) -> bool {
    true
}

/// Order and thresholds from EKS-A4: the matrix form costs 29.8 us per token at
/// a full block and 176.7 at eight tokens, where the register-blocked form
/// costs 79.7; the vector form is three times faster than either at one token,
/// because a tile would compute eight columns and keep one.
pub const MATMUL_FORMS: Registry<MatmulForm> = Registry {
    op: "qmatmul",
    variants: &[
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
