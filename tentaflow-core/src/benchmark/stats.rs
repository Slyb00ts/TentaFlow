// ===== File: benchmark/stats.rs — mean/sigma and percentile math for benchmark aggregates =====

/// Aggregate statistics over one variant's samples: mean ± sample stddev and
/// linear-interpolated percentiles, reported like llama-bench.
#[derive(Debug, Clone, PartialEq)]
pub struct Aggregate {
    pub mean: f64,
    pub sigma: f64,
    pub p50: f64,
    pub p90: f64,
    pub p99: f64,
    pub count: usize,
}

/// Returns `None` for an empty slice (an all-error variant has no timings).
pub fn aggregate(values: &[f64]) -> Option<Aggregate> {
    if values.is_empty() {
        return None;
    }
    let (mean, sigma) = mean_sigma(values);
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.total_cmp(b));
    Some(Aggregate {
        mean,
        sigma,
        p50: percentile(&sorted, 50.0),
        p90: percentile(&sorted, 90.0),
        p99: percentile(&sorted, 99.0),
        count: values.len(),
    })
}

/// Sample standard deviation (n-1 denominator); sigma is 0 for a single sample.
pub fn mean_sigma(values: &[f64]) -> (f64, f64) {
    let n = values.len();
    if n == 0 {
        return (0.0, 0.0);
    }
    let mean = values.iter().sum::<f64>() / n as f64;
    if n < 2 {
        return (mean, 0.0);
    }
    let var = values.iter().map(|v| (v - mean) * (v - mean)).sum::<f64>() / (n - 1) as f64;
    (mean, var.sqrt())
}

/// Percentile over ASCENDING-sorted data with linear interpolation between
/// closest ranks (rank = p/100 · (n-1)), matching the metrics module approach.
pub fn percentile(sorted: &[f64], p: f64) -> f64 {
    match sorted.len() {
        0 => 0.0,
        1 => sorted[0],
        n => {
            let rank = (p / 100.0).clamp(0.0, 1.0) * (n - 1) as f64;
            let lo = rank.floor() as usize;
            let hi = rank.ceil() as usize;
            if lo == hi {
                sorted[lo]
            } else {
                let frac = rank - lo as f64;
                sorted[lo] + (sorted[hi] - sorted[lo]) * frac
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mean_sigma_known_values() {
        // Sample stddev of [2,4,4,4,5,5,7,9]: mean 5, variance 32/7.
        let vals = [2.0, 4.0, 4.0, 4.0, 5.0, 5.0, 7.0, 9.0];
        let (mean, sigma) = mean_sigma(&vals);
        assert!((mean - 5.0).abs() < 1e-12);
        assert!((sigma - (32.0f64 / 7.0).sqrt()).abs() < 1e-12);
    }

    #[test]
    fn mean_sigma_single_sample_has_zero_sigma() {
        let (mean, sigma) = mean_sigma(&[42.0]);
        assert_eq!(mean, 42.0);
        assert_eq!(sigma, 0.0);
    }

    #[test]
    fn percentile_linear_interpolation() {
        let sorted = [10.0, 20.0, 30.0, 40.0];
        // rank(50%) = 1.5 → halfway between 20 and 30.
        assert!((percentile(&sorted, 50.0) - 25.0).abs() < 1e-12);
        // rank(90%) = 2.7 → 30 + 0.7·10.
        assert!((percentile(&sorted, 90.0) - 37.0).abs() < 1e-12);
        assert_eq!(percentile(&sorted, 0.0), 10.0);
        assert_eq!(percentile(&sorted, 100.0), 40.0);
    }

    #[test]
    fn percentile_edge_cases() {
        assert_eq!(percentile(&[], 50.0), 0.0);
        assert_eq!(percentile(&[7.0], 99.0), 7.0);
    }

    #[test]
    fn aggregate_empty_is_none() {
        assert!(aggregate(&[]).is_none());
    }

    #[test]
    fn aggregate_combines_mean_and_percentiles() {
        let vals: Vec<f64> = (1..=100).map(|v| v as f64).collect();
        let agg = aggregate(&vals).unwrap();
        assert!((agg.mean - 50.5).abs() < 1e-12);
        assert_eq!(agg.count, 100);
        // rank(50%) = 49.5 → between 50 and 51.
        assert!((agg.p50 - 50.5).abs() < 1e-12);
        // rank(99%) = 98.01 → between 99 and 100.
        assert!((agg.p99 - 99.01).abs() < 1e-9);
    }
}
