// ===== File: tentaquant/compare.rs — comparing the distributions of several runs =====
//
// Plan §13.6 asks for one histogram carrying several series, a metrics table
// and a diff row. Three properties decide the shape of everything below:
//
//   * the SERIES ARE ALIGNED. Two runs of the same circuit may have seen
//     different bitstrings, and a table whose columns mean something different
//     per row is worse than no table. So one axis — the union, ordered by the
//     summed weight — is computed once and every run answers against it, `0`
//     included.
//   * the METRICS ARE NOT computed on that axis. The union of 2^n bitstrings
//     does not fit an answer, so the axis is a WINDOW of the heaviest columns
//     while total variation distance and Hellinger fidelity run over the whole
//     distributions. Measuring them on the window would flatter every pair of
//     runs that disagree in the tail.
//   * the REFERENCE is the first run of the request, because the caller chose
//     that order and "vs the first" is the only reading that needs no further
//     explanation in the table.

use std::collections::{BTreeMap, BTreeSet};

use tentaflow_protocol::tentaquant::{RunComparison, RUN_COMPARE_BARS};

/// One run's measured distribution, normalized once so every metric below
/// works on probabilities.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Distribution {
    pub counts: BTreeMap<String, u64>,
    pub shots: u64,
}

impl Distribution {
    pub fn probability(&self, bitstring: &str) -> f64 {
        if self.shots == 0 {
            return 0.0;
        }
        self.counts.get(bitstring).copied().unwrap_or(0) as f64 / self.shots as f64
    }

    fn is_empty(&self) -> bool {
        self.shots == 0 || self.counts.is_empty()
    }
}

/// Total variation distance `½ Σ|p − q|` over the union of both supports.
///
/// `None` when either run measured nothing: a distance to an absent
/// distribution is not zero, and reporting zero would read as "identical".
pub fn total_variation_distance(a: &Distribution, b: &Distribution) -> Option<f64> {
    if a.is_empty() || b.is_empty() {
        return None;
    }
    let sum: f64 = support(a, b)
        .iter()
        .map(|bits| (a.probability(bits) - b.probability(bits)).abs())
        .sum();
    Some(0.5 * sum)
}

/// Hellinger fidelity `(Σ √(p·q))²` — the Bhattacharyya coefficient squared,
/// which is the number Qiskit's `hellinger_fidelity` reports, so a value from
/// this laboratory and one from a paper mean the same thing.
pub fn hellinger_fidelity(a: &Distribution, b: &Distribution) -> Option<f64> {
    if a.is_empty() || b.is_empty() {
        return None;
    }
    let coefficient: f64 = support(a, b)
        .iter()
        .map(|bits| (a.probability(bits) * b.probability(bits)).sqrt())
        .sum();
    Some(coefficient * coefficient)
}

fn support(a: &Distribution, b: &Distribution) -> BTreeSet<String> {
    a.counts.keys().chain(b.counts.keys()).cloned().collect()
}

/// The aligned axis: the heaviest [`RUN_COMPARE_BARS`] bitstrings of the union,
/// ranked by the probability summed over the runs so a column that matters to
/// ANY of them survives. Ties break on the bitstring, so the same set of runs
/// always produces the same axis.
pub fn axis(runs: &[Distribution]) -> Vec<String> {
    let mut weight: BTreeMap<&str, f64> = BTreeMap::new();
    for run in runs {
        for bits in run.counts.keys() {
            *weight.entry(bits.as_str()).or_insert(0.0) += run.probability(bits);
        }
    }
    let mut ranked: Vec<(&str, f64)> = weight.into_iter().collect();
    ranked.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(b.0))
    });
    ranked.truncate(RUN_COMPARE_BARS);
    ranked
        .into_iter()
        .map(|(bits, _)| bits.to_string())
        .collect()
}

/// The diff row of §13.6: per column the spread `max − min` of the compared
/// probabilities. One row whatever the number of series — for two runs it is
/// exactly `|p₂ − p₁|`, and for more it points at the column they disagree on.
pub fn diff_row(runs: &[Distribution], axis: &[String]) -> Vec<f64> {
    axis.iter()
        .map(|bits| {
            let mut min = f64::INFINITY;
            let mut max = f64::NEG_INFINITY;
            for run in runs {
                let p = run.probability(bits);
                min = min.min(p);
                max = max.max(p);
            }
            if min.is_finite() && max.is_finite() {
                max - min
            } else {
                0.0
            }
        })
        .collect()
}

/// What one run contributes to the comparison table: its aligned series and
/// its two metrics against `reference`.
pub fn compare_one(
    run_id: String,
    label: String,
    target: String,
    backend: String,
    started_at: String,
    duration_ms: u64,
    distribution: &Distribution,
    reference: &Distribution,
    is_reference: bool,
    axis: &[String],
) -> RunComparison {
    RunComparison {
        run_id,
        label,
        target,
        backend,
        started_at,
        duration_ms,
        shots: distribution.shots,
        counts: axis
            .iter()
            .map(|bits| distribution.counts.get(bits).copied().unwrap_or(0))
            .collect(),
        probabilities: axis
            .iter()
            .map(|bits| distribution.probability(bits))
            .collect(),
        // The reference is not compared with itself: a row of zero and one
        // would read as a measurement rather than as "this is the baseline".
        total_variation_distance: if is_reference {
            None
        } else {
            total_variation_distance(distribution, reference)
        },
        hellinger_fidelity: if is_reference {
            None
        } else {
            hellinger_fidelity(distribution, reference)
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn distribution(pairs: &[(&str, u64)]) -> Distribution {
        let counts: BTreeMap<String, u64> = pairs
            .iter()
            .map(|(bits, count)| ((*bits).to_string(), *count))
            .collect();
        let shots = counts.values().sum();
        Distribution { counts, shots }
    }

    /// Hand-computed: p = (0.5, 0.5), q = (0.25, 0.75).
    /// TVD = ½(0.25 + 0.25) = 0.25.
    /// Σ√(pq) = √0.125 + √0.375 = 0.9659258…, squared = 0.9330127…
    #[test]
    fn the_metrics_match_the_numbers_computed_by_hand() {
        let a = distribution(&[("00", 500), ("11", 500)]);
        let b = distribution(&[("00", 250), ("11", 750)]);
        assert!((total_variation_distance(&a, &b).expect("tvd") - 0.25).abs() < 1e-12);
        assert!(
            (hellinger_fidelity(&a, &b).expect("fidelity") - 0.933_012_701_892_219_3).abs() < 1e-12
        );
        // A distribution against itself: no distance, perfect fidelity.
        assert!(total_variation_distance(&a, &a).expect("tvd") < 1e-15);
        assert!((hellinger_fidelity(&a, &a).expect("fidelity") - 1.0).abs() < 1e-12);
    }

    /// Disjoint supports are the case the union has to cover: measured on one
    /// run's keys alone the distance would come out as ½ instead of 1.
    #[test]
    fn disjoint_distributions_are_maximally_far_apart() {
        let a = distribution(&[("00", 100)]);
        let b = distribution(&[("11", 100)]);
        assert!((total_variation_distance(&a, &b).expect("tvd") - 1.0).abs() < 1e-12);
        assert!(hellinger_fidelity(&a, &b).expect("fidelity") < 1e-12);
    }

    /// A run that measured nothing has no distance to anything — reporting 0
    /// would read as "identical to the reference".
    #[test]
    fn a_run_without_a_distribution_reports_no_metric() {
        let a = distribution(&[("0", 8)]);
        let empty = Distribution::default();
        assert!(total_variation_distance(&a, &empty).is_none());
        assert!(hellinger_fidelity(&empty, &a).is_none());
        assert_eq!(empty.probability("0"), 0.0);
    }

    /// The axis is the union ranked by summed weight, so a column that only
    /// one run saw is kept when it is heavy there.
    #[test]
    fn the_axis_is_the_union_ranked_by_weight() {
        let a = distribution(&[("00", 900), ("01", 100)]);
        let b = distribution(&[("11", 1000)]);
        let axis = axis(&[a.clone(), b.clone()]);
        assert_eq!(axis, vec!["11", "00", "01"]);
        // The diff row is the spread per column: "11" is 1.0 in one run and 0
        // in the other, "01" differs by 0.1.
        let diff = diff_row(&[a, b], &axis);
        assert!((diff[0] - 1.0).abs() < 1e-12);
        assert!((diff[1] - 0.9).abs() < 1e-12);
        assert!((diff[2] - 0.1).abs() < 1e-12);
    }

    /// The window is bounded, and the metrics are NOT measured on it: two runs
    /// that agree on every heavy column but differ in the tail still report a
    /// distance.
    #[test]
    fn the_window_is_bounded_and_the_metrics_are_not_measured_on_it() {
        let wide: Vec<(String, u64)> = (0..40).map(|i| (format!("{i:06b}"), 100)).collect();
        let mut other = wide.clone();
        other.truncate(39);
        let a = Distribution {
            shots: wide.iter().map(|(_, c)| c).sum(),
            counts: wide.into_iter().collect(),
        };
        let b = Distribution {
            shots: other.iter().map(|(_, c)| c).sum(),
            counts: other.into_iter().collect(),
        };
        assert_eq!(axis(&[a.clone(), b.clone()]).len(), RUN_COMPARE_BARS);
        assert!(total_variation_distance(&a, &b).expect("tvd") > 0.0);
    }
}
