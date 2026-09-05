// ===== File: tentaquant/state.rs — reduced quantities of a run's state, on demand =====
//
// Plan §13.6 keeps the entanglement map cheap by recording only the pairs a
// gate touched, and answers the full map — every pair, the whole register — as
// a REQUEST (`StateQuery`). This is that request.
//
// It has two sources and they are not interchangeable, which is why the answer
// says which one it came from:
//
//   * the STORED FINAL STATE. Every quantity is computed here and now, so any
//     pair can be asked for, and the numbers are the crate's own — the same
//     functions the browser tier calls, so a query and a keyframe of the same
//     state agree bit for bit.
//   * the LAST KEYFRAME, for a run that recorded its evolution but stored no
//     state. A frame carries what was recorded and nothing more: a pair the
//     run never computed cannot be reconstructed from it, so the answer holds
//     the recorded subset rather than silently filling it with zeros.
//
// A run with neither is an error, not an empty answer: "no entanglement" and
// "nothing was stored" must never look the same.

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::path::Path;

use anyhow::{anyhow, Result};
use num_complex::Complex64;
use tentaflow_protocol::tentaquant::{
    KeyframePair, KeyframeProbability, StateKeyframe, RUN_STATE_QUERY_MAX_PAIRS,
};
use tentaflow_quantum::sim::analysis;
use tentaflow_quantum::sim::statevector::bitstring;

use super::runs::{self, StoredState};
use crate::db::DbPool;

/// Where an answer came from, as the wire names it.
pub const SOURCE_STATE: &str = "state";
pub const SOURCE_KEYFRAME: &str = "keyframe";

/// One answer to a state query.
#[derive(Debug, Clone, PartialEq)]
pub struct StateAnswer {
    pub source: &'static str,
    pub step: u32,
    pub num_qubits: u32,
    pub bloch: Vec<[f64; 3]>,
    pub purity: Vec<f64>,
    pub pairs: Vec<KeyframePair>,
    pub probs_top: Vec<KeyframeProbability>,
}

/// Why a query could not be answered. `Missing` is its own outcome and not an
/// empty answer: "nothing was stored" and "no entanglement" must never look
/// the same to a reader.
pub enum StateQueryError {
    Invalid(String),
    Missing,
    Internal(anyhow::Error),
}

/// The whole query as ONE blocking step: pick the source, read it and reduce
/// it.
///
/// It belongs on a blocking thread — reading the state artifact and computing
/// up to [`RUN_STATE_QUERY_MAX_PAIRS`] reduced density matrices are both
/// passes over 2^n amplitudes, which is not work an async reactor thread may
/// do.
pub fn answer(
    pool: &DbPool,
    data_dir: &Path,
    run_id: &str,
    keyframes_sha256: Option<&str>,
    pairs: &[[u32; 2]],
    top_k: u32,
) -> std::result::Result<StateAnswer, StateQueryError> {
    if let Some(stored) =
        runs::stored_state(pool, data_dir, run_id).map_err(StateQueryError::Internal)?
    {
        let selection = requested_pairs(stored.num_qubits, pairs)
            .map_err(|e| StateQueryError::Invalid(e.to_string()))?;
        return from_state(&stored, &selection, top_k).map_err(StateQueryError::Internal);
    }
    let frames = match keyframes_sha256 {
        Some(sha256) => {
            runs::stored_keyframes(data_dir, sha256).map_err(StateQueryError::Internal)?
        }
        None => Vec::new(),
    };
    frames
        .last()
        .map(|frame| from_keyframe(frame, pairs, top_k))
        .ok_or(StateQueryError::Missing)
}

/// The pairs a request asks for: the ones it named, or every pair of the
/// register when it named none.
///
/// Each pair is a pass over the state vector, so "all" is bounded — an
/// unbounded `all` on a wide register is a request that never returns, which
/// is a refusal the caller can act on rather than a timeout.
pub fn requested_pairs(num_qubits: u32, asked: &[[u32; 2]]) -> Result<Vec<(usize, usize)>> {
    let n = num_qubits as usize;
    if asked.is_empty() {
        let total = n * n.saturating_sub(1) / 2;
        if total > RUN_STATE_QUERY_MAX_PAIRS {
            return Err(anyhow!(
                "every pair of {num_qubits} qubits is {total} reduced density matrices, over the \
                 {RUN_STATE_QUERY_MAX_PAIRS} this query allows; ask for the pairs you need"
            ));
        }
        let mut pairs = Vec::with_capacity(total);
        for i in 0..n {
            for j in (i + 1)..n {
                pairs.push((i, j));
            }
        }
        return Ok(pairs);
    }
    if asked.len() > RUN_STATE_QUERY_MAX_PAIRS {
        return Err(anyhow!(
            "a state query asks for at most {RUN_STATE_QUERY_MAX_PAIRS} pairs"
        ));
    }
    asked
        .iter()
        .map(|[i, j]| {
            let (i, j) = (*i as usize, *j as usize);
            if i >= n || j >= n {
                return Err(anyhow!(
                    "qubit pair ({i}, {j}) is outside a register of {num_qubits} qubits"
                ));
            }
            if i == j {
                return Err(anyhow!("a qubit pair must name two different qubits"));
            }
            Ok((i, j))
        })
        .collect()
}

/// Everything §13.6 draws, computed from the stored final state.
pub fn from_state(
    state: &StoredState,
    pairs: &[(usize, usize)],
    top_k: u32,
) -> Result<StateAnswer> {
    let n = state.num_qubits as usize;
    let bloch =
        analysis::bloch_vectors(&state.amplitudes, n).map_err(|e| anyhow!("bloch vectors: {e}"))?;
    let purity = analysis::purity_from_bloch(&bloch);
    let mut computed = Vec::with_capacity(pairs.len());
    for (i, j) in pairs {
        let rho = analysis::reduced_density_matrix(&state.amplitudes, n, &[*i, *j])
            .map_err(|e| anyhow!("reduced density matrix of ({i}, {j}): {e}"))?;
        computed.push(KeyframePair {
            qubits: [*i as u32, *j as u32],
            rho: rho.iter().map(|v| [v.re, v.im]).collect(),
            mutual_information: analysis::mutual_information(&state.amplitudes, n, *i, *j)
                .map_err(|e| anyhow!("mutual information of ({i}, {j}): {e}"))?,
            concurrence: analysis::concurrence(&state.amplitudes, n, *i, *j)
                .map_err(|e| anyhow!("concurrence of ({i}, {j}): {e}"))?,
        });
    }
    Ok(StateAnswer {
        source: SOURCE_STATE,
        // A stored final state carries no step index — it IS the end of the
        // program — so the answer says 0 rather than inventing a position in
        // a timeline this source does not have.
        step: 0,
        num_qubits: state.num_qubits,
        bloch,
        purity,
        pairs: computed,
        probs_top: top_probabilities(&state.amplitudes, n, top_k as usize),
    })
}

/// The same answer out of a recorded frame — restricted to what that frame
/// holds. `asked` empty means "every pair", and for a frame that is every pair
/// it recorded.
pub fn from_keyframe(frame: &StateKeyframe, asked: &[[u32; 2]], top_k: u32) -> StateAnswer {
    let pairs = frame
        .pairs
        .iter()
        .filter(|pair| {
            asked.is_empty()
                || asked
                    .iter()
                    .any(|[i, j]| pair.qubits == [*i, *j] || pair.qubits == [*j, *i])
        })
        .cloned()
        .collect();
    let mut probs_top = frame.probs_top.clone();
    if top_k > 0 {
        probs_top.truncate(top_k as usize);
    }
    StateAnswer {
        source: SOURCE_KEYFRAME,
        step: frame.step,
        num_qubits: frame.bloch.len() as u32,
        bloch: frame.bloch.clone(),
        purity: frame.purity.clone(),
        pairs,
        probs_top,
    }
}

/// The heaviest `k` outcome probabilities, biggest first.
///
/// A bounded min-heap over INDICES, rendered afterwards: a state at the
/// storage ceiling is millions of amplitudes and `k` reaches
/// [`tentaflow_protocol::tentaquant::RUN_STATE_QUERY_TOP_K_MAX`], so neither
/// collecting them all nor shifting a sorted window per candidate is
/// acceptable — this is `O(n log k)` with `k` entries live. Equal probabilities keep the lower index, which is the lower
/// bitstring, so two queries of one state answer in the same order.
///
/// The heap is keyed on the probability's BIT PATTERN, which orders positive
/// finite floats exactly as the floats do, so the comparison needs no wrapper
/// type and cannot be tripped by a NaN — the loop filters those out.
fn top_probabilities(
    amplitudes: &[Complex64],
    num_qubits: usize,
    k: usize,
) -> Vec<KeyframeProbability> {
    if k == 0 {
        return Vec::new();
    }
    // `(Reverse(bits), index)`: the heap's MAXIMUM is the smallest
    // probability and, among equals, the largest index — exactly the entry to
    // evict.
    let mut heap: BinaryHeap<(Reverse<u64>, usize)> = BinaryHeap::with_capacity(k + 1);
    for (index, amplitude) in amplitudes.iter().enumerate() {
        let probability = amplitude.norm_sqr();
        if !probability.is_finite() || probability <= 0.0 {
            continue;
        }
        let candidate = (Reverse(probability.to_bits()), index);
        if heap.len() == k {
            match heap.peek() {
                Some(worst) if candidate >= *worst => continue,
                _ => {
                    heap.pop();
                }
            }
        }
        heap.push(candidate);
    }
    let mut top = heap.into_vec();
    // Ascending on this key is descending probability, then ascending index.
    top.sort_unstable();
    top.into_iter()
        .map(|(Reverse(bits), index)| KeyframeProbability {
            bitstring: bitstring(index, num_qubits),
            probability: f64::from_bits(bits),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bell() -> StoredState {
        let a = 0.5f64.sqrt();
        StoredState {
            num_qubits: 2,
            amplitudes: vec![
                Complex64::new(a, 0.0),
                Complex64::new(0.0, 0.0),
                Complex64::new(0.0, 0.0),
                Complex64::new(a, 0.0),
            ],
        }
    }

    /// The textbook numbers of a Bell pair: each qubit maximally mixed
    /// (purity ½, Bloch vector of length 0), concurrence 1 and mutual
    /// information exactly 2 bits.
    #[test]
    fn a_bell_state_answers_with_its_textbook_numbers() {
        let state = bell();
        let pairs = requested_pairs(2, &[]).expect("pairs");
        assert_eq!(pairs, vec![(0, 1)]);
        let answer = from_state(&state, &pairs, 4).expect("answer");
        assert_eq!(answer.source, SOURCE_STATE);
        assert_eq!(answer.num_qubits, 2);
        for vector in &answer.bloch {
            assert!(vector.iter().all(|c| c.abs() < 1e-12), "{vector:?}");
        }
        assert!(answer.purity.iter().all(|p| (p - 0.5).abs() < 1e-12));
        assert_eq!(answer.pairs.len(), 1);
        assert!((answer.pairs[0].concurrence - 1.0).abs() < 1e-9);
        assert!((answer.pairs[0].mutual_information - 2.0).abs() < 1e-9);
        assert_eq!(answer.pairs[0].rho.len(), 16);
        // Only |00> and |11> carry weight, half each.
        assert_eq!(answer.probs_top.len(), 2);
        assert_eq!(answer.probs_top[0].bitstring, "00");
        assert!((answer.probs_top[0].probability - 0.5).abs() < 1e-12);
        assert_eq!(answer.probs_top[1].bitstring, "11");
    }

    /// A product state is the control: the same code must report NO
    /// entanglement, or the Bell numbers above prove nothing.
    #[test]
    fn a_product_state_reports_no_entanglement() {
        let a = 0.5f64.sqrt();
        let state = StoredState {
            num_qubits: 2,
            amplitudes: vec![
                Complex64::new(a, 0.0),
                Complex64::new(a, 0.0),
                Complex64::new(0.0, 0.0),
                Complex64::new(0.0, 0.0),
            ],
        };
        let answer = from_state(&state, &[(0, 1)], 0).expect("answer");
        assert!(answer.pairs[0].concurrence < 1e-9);
        assert!(answer.pairs[0].mutual_information.abs() < 1e-9);
        assert!(answer.probs_top.is_empty(), "top_k 0 asks for none");
    }

    /// The bounded selection must answer exactly what a full sort would: the
    /// k largest, biggest first, and the lower bitstring among equals — with
    /// an ASCENDING state, the case that makes a naive window shift on every
    /// amplitude.
    #[test]
    fn the_heaviest_probabilities_are_the_ones_a_full_sort_would_pick() {
        // Amplitudes whose probabilities rise with the index: every candidate
        // beats the current worst.
        let amplitudes: Vec<Complex64> = (0..16)
            .map(|i| Complex64::new(((i + 1) as f64).sqrt(), 0.0))
            .collect();
        let top = top_probabilities(&amplitudes, 4, 3);
        assert_eq!(
            top.iter().map(|p| p.bitstring.as_str()).collect::<Vec<_>>(),
            vec!["1111", "1110", "1101"]
        );
        assert!((top[0].probability - 16.0).abs() < 1e-12);
        assert!(top.windows(2).all(|w| w[0].probability >= w[1].probability));

        // Equal probabilities: the lower index wins, and asking for more than
        // the state holds answers with what there is.
        let flat = vec![Complex64::new(0.5, 0.0); 4];
        let ties = top_probabilities(&flat, 2, 3);
        assert_eq!(
            ties.iter()
                .map(|p| p.bitstring.as_str())
                .collect::<Vec<_>>(),
            vec!["00", "01", "10"]
        );
        // Zero amplitudes are not outcomes.
        let sparse = vec![
            Complex64::new(0.0, 0.0),
            Complex64::new(1.0, 0.0),
            Complex64::new(0.0, 0.0),
            Complex64::new(0.0, 0.0),
        ];
        let only = top_probabilities(&sparse, 2, 8);
        assert_eq!(only.len(), 1);
        assert_eq!(only[0].bitstring, "01");
    }

    #[test]
    fn a_pair_selection_is_validated_against_the_register() {
        assert_eq!(
            requested_pairs(3, &[]).expect("all pairs"),
            vec![(0, 1), (0, 2), (1, 2)]
        );
        assert_eq!(requested_pairs(3, &[[2, 0]]).expect("named"), vec![(2, 0)]);
        assert!(requested_pairs(2, &[[0, 2]]).is_err(), "out of range");
        assert!(requested_pairs(2, &[[1, 1]]).is_err(), "same qubit twice");
        // 33 qubits is 528 pairs, past the ceiling.
        assert!(requested_pairs(33, &[]).is_err());
        assert!(requested_pairs(32, &[]).is_ok());
    }

    /// A frame answers with what it recorded: asking for a pair it never
    /// computed returns nothing for that pair instead of an invented zero.
    #[test]
    fn a_keyframe_answers_only_with_the_pairs_it_recorded() {
        let frame = StateKeyframe {
            step: 5,
            gate: None,
            bloch: vec![[0.0, 0.0, 1.0], [0.0, 0.0, 1.0], [0.0, 0.0, 1.0]],
            purity: vec![1.0, 1.0, 1.0],
            pairs: vec![KeyframePair {
                qubits: [0, 1],
                rho: vec![[0.5, 0.0]],
                mutual_information: 2.0,
                concurrence: 1.0,
            }],
            top: Vec::new(),
            probs_top: vec![
                KeyframeProbability {
                    bitstring: "000".to_string(),
                    probability: 0.5,
                },
                KeyframeProbability {
                    bitstring: "111".to_string(),
                    probability: 0.5,
                },
            ],
        };
        let all = from_keyframe(&frame, &[], 0);
        assert_eq!(all.source, SOURCE_KEYFRAME);
        assert_eq!(all.step, 5);
        assert_eq!(all.num_qubits, 3);
        assert_eq!(all.pairs.len(), 1);
        assert_eq!(all.probs_top.len(), 2);

        let missing = from_keyframe(&frame, &[[0, 2]], 1);
        assert!(missing.pairs.is_empty());
        assert_eq!(missing.probs_top.len(), 1);

        // Order does not matter: the map is symmetric.
        let reversed = from_keyframe(&frame, &[[1, 0]], 0);
        assert_eq!(reversed.pairs.len(), 1);
    }
}
