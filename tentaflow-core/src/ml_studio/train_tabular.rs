// ===== File: ml_studio/train_tabular.rs — Rust-native multi-model tabular trainer =====
//
// Real, on-device supervised learning for ML Studio's tabular leaderboard. No
// Python, no C, no GPU — only pure-Rust crates, so it builds and runs identically
// on Android as on the desktop. Given a parsed table (headers + string rows) and a
// target column, it encodes features (numeric standardization + one-hot
// categoricals + mean/most-frequent imputation), splits a deterministic
// train/holdout set and trains several REAL models on the same split:
//   - classification: hand-written multinomial logistic regression (softmax) and a
//     most-frequent-class baseline, plus a smartcore decision tree and random
//     forest; scored by accuracy + macro-F1,
//   - regression: hand-written linear regression (full-batch gradient descent) and
//     a mean baseline, plus a smartcore decision-tree and random-forest regressor;
//     scored by RMSE + R^2.
// The smartcore models use an explicit seed so the leaderboard is reproducible.
// Everything is bounded (rows/features) and every failure on the data path is a
// `Result` or a skipped model — never a panic.

use anyhow::{bail, Result};
use smartcore::ensemble::random_forest_classifier::{
    RandomForestClassifier, RandomForestClassifierParameters,
};
use smartcore::ensemble::random_forest_regressor::{
    RandomForestRegressor, RandomForestRegressorParameters,
};
use smartcore::linalg::basic::matrix::DenseMatrix;
use smartcore::tree::decision_tree_classifier::{
    DecisionTreeClassifier, DecisionTreeClassifierParameters,
};
use smartcore::tree::decision_tree_regressor::{
    DecisionTreeRegressor, DecisionTreeRegressorParameters,
};

/// Hard cap on training rows. Beyond this the table is truncated (the trainer is
/// a CPU baseline, not a large-scale learner).
const MAX_TRAIN_ROWS: usize = 50_000;

/// Hard cap on the encoded feature width (after one-hot expansion). A wider
/// design matrix is rejected rather than silently truncated, so the caller gets
/// a clear "too many features" error instead of a meaningless model.
const MAX_FEATURES: usize = 2_000;

/// Distinct values above which a categorical feature column is treated as
/// high-cardinality (likely an identifier) and dropped from the design matrix.
const ONE_HOT_MAX_CARDINALITY: usize = 64;

/// Hard cap on the number of distinct target classes for classification. A wider
/// target would build a giant softmax weight matrix (one row per class) for what
/// is almost certainly an identifier or free-text column, not a label.
const MAX_TARGET_CLASSES: usize = 100;

/// Fraction of a column's distinct values to row count above which the column is
/// treated as an ID-like feature and excluded (e.g. a primary key).
const ID_CARDINALITY_RATIO: f64 = 0.95;

/// Minimum labeled rows before the near-unique target guard activates. Below this
/// a high distinct/row ratio is expected (tiny tables, test fixtures) and is not
/// evidence of an identifier column, so the guard stays off.
const MIN_ROWS_FOR_ID_TARGET_GUARD: usize = 8;

/// Train/holdout split — fraction of rows used for training.
const TRAIN_FRACTION: f64 = 0.70;

/// Deterministic split seed so repeated runs on the same data are reproducible.
const SPLIT_SEED: u64 = 0x9E37_79B9_7F4A_7C15;

/// Gradient-descent iterations for logistic / linear regression.
const GD_ITERS: usize = 400;

/// Learning rate for gradient descent on standardized features.
const LEARNING_RATE: f64 = 0.2;

/// L2 regularization strength (weight decay) — keeps weights bounded on
/// separable or collinear data without distorting the fit.
const L2_LAMBDA: f64 = 1e-4;

/// Stable framework label for the smartcore (pure-Rust) tree/forest models.
const SMARTCORE_FRAMEWORK: &str = "smartcore (Rust)";

/// Seed for smartcore's random forest / tree split sampling. A fixed value keeps
/// the leaderboard reproducible across runs (deterministic tests).
const SMARTCORE_SEED: u64 = 0x5EED;

/// Number of trees in the random forest. Kept small because the trainer is a
/// CPU-only on-device learner on bounded data — 50 trees is enough signal for a
/// leaderboard entry without making training slow.
const RF_N_TREES: u16 = 50;

/// Maximum tree depth for both the decision tree and the forest's trees. Bounding
/// depth keeps training fast and prevents pathological deep trees on noisy data.
const TREE_MAX_DEPTH: u16 = 12;

/// The kind of supervised task the caller requested.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Task {
    Classification,
    Regression,
}

impl Task {
    /// Parses the wire slug (`classification` / `regression`).
    pub fn from_slug(slug: &str) -> Option<Task> {
        match slug {
            "classification" => Some(Task::Classification),
            "regression" => Some(Task::Regression),
            _ => None,
        }
    }

    /// Stable machine slug.
    pub fn slug(self) -> &'static str {
        match self {
            Task::Classification => "classification",
            Task::Regression => "regression",
        }
    }
}

/// One row of the leaderboard: a trained model and its holdout metrics. For
/// classification `accuracy`/`f1_macro` are set and `rmse`/`r2` are `None`; for
/// regression it is the reverse. `train_secs` is wall-clock training time.
#[derive(Debug, Clone)]
pub struct LeaderboardEntry {
    pub model_name: String,
    pub framework: String,
    pub accuracy: Option<f64>,
    pub f1_macro: Option<f64>,
    pub rmse: Option<f64>,
    pub r2: Option<f64>,
    pub train_secs: f64,
}

/// Result of a full training pass: the resolved task, the ranked leaderboard,
/// the name of the best model, and the per-iteration loss of the best gradient
/// model (for an optional convergence chart / metrics_history).
#[derive(Debug, Clone)]
pub struct TrainOutcome {
    pub task: Task,
    pub target_column: String,
    pub feature_names: Vec<String>,
    pub train_rows: usize,
    pub holdout_rows: usize,
    pub class_labels: Vec<String>,
    pub leaderboard: Vec<LeaderboardEntry>,
    pub best_model_name: String,
    pub best_loss_curve: Vec<f64>,
}

/// Trains the tabular baseline. `headers`/`rows` come from
/// `profile::parse_table`; `target_col` names the label column; `task` selects
/// classification vs regression. Returns a ranked leaderboard with REAL holdout
/// metrics. All error paths return `Err` (no panics).
pub fn train_tabular(
    headers: &[String],
    rows: &[Vec<String>],
    target_col: &str,
    task: Task,
) -> Result<TrainOutcome> {
    let target_idx = headers
        .iter()
        .position(|h| h == target_col)
        .ok_or_else(|| anyhow::anyhow!("target column '{}' not found", target_col))?;

    // Drop rows whose target is missing — there is nothing to learn/score there.
    let kept: Vec<&Vec<String>> = rows
        .iter()
        .filter(|r| r.get(target_idx).map(|v| !v.trim().is_empty()).unwrap_or(false))
        .take(MAX_TRAIN_ROWS)
        .collect();
    if kept.len() < 10 {
        bail!(
            "not enough labeled rows to train (have {}, need at least 10)",
            kept.len()
        );
    }

    let feature_cols = select_feature_columns(headers, &kept, target_idx);
    if feature_cols.is_empty() {
        bail!("no usable feature columns (all were empty, constant, or ID-like)");
    }

    let plan = plan_encoding(headers, &kept, &feature_cols)?;
    if plan.feature_names.len() > MAX_FEATURES {
        bail!(
            "too many features after one-hot encoding ({}, limit {})",
            plan.feature_names.len(),
            MAX_FEATURES
        );
    }

    let encoded = materialize_features(&kept, plan);

    let targets: Vec<&str> = kept.iter().map(|r| r[target_idx].trim()).collect();
    let split = split_indices(kept.len());

    match task {
        Task::Classification => {
            train_classification(&encoded, &targets, &split, target_col)
        }
        Task::Regression => train_regression(&encoded, &targets, &split, target_col),
    }
}

/// Deterministic train/holdout index split. A SplitMix64 PRNG keyed by
/// `SPLIT_SEED` assigns each row to train or holdout, so the partition is
/// reproducible and independent of row order patterns.
struct Split {
    train: Vec<usize>,
    holdout: Vec<usize>,
}

fn split_indices(n: usize) -> Split {
    let mut state = SPLIT_SEED;
    let mut train = Vec::new();
    let mut holdout = Vec::new();
    for i in 0..n {
        // SplitMix64 step → uniform f64 in [0,1).
        state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        let u = (z >> 11) as f64 / (1u64 << 53) as f64;
        if u < TRAIN_FRACTION {
            train.push(i);
        } else {
            holdout.push(i);
        }
    }
    // Guard the degenerate case where the random split starved one side.
    if train.is_empty() || holdout.is_empty() {
        train.clear();
        holdout.clear();
        let cut = ((n as f64) * TRAIN_FRACTION) as usize;
        let cut = cut.clamp(1, n.saturating_sub(1).max(1));
        for i in 0..n {
            if i < cut {
                train.push(i);
            } else {
                holdout.push(i);
            }
        }
    }
    Split { train, holdout }
}

/// Selects which raw columns become features: every column except the target,
/// dropping columns that are entirely empty, constant, or ID-like (very high
/// cardinality relative to row count — primary keys, UUIDs, free text).
fn select_feature_columns(
    headers: &[String],
    rows: &[&Vec<String>],
    target_idx: usize,
) -> Vec<usize> {
    let n = rows.len() as f64;
    (0..headers.len())
        .filter(|&c| c != target_idx)
        .filter(|&c| {
            let mut seen = std::collections::HashSet::new();
            let mut non_empty = 0usize;
            for r in rows {
                if let Some(v) = r.get(c) {
                    let v = v.trim();
                    if !v.is_empty() {
                        non_empty += 1;
                        seen.insert(v.to_string());
                    }
                }
            }
            if non_empty == 0 || seen.len() < 2 {
                return false; // empty or constant — no signal
            }
            let near_unique = (seen.len() as f64) / n >= ID_CARDINALITY_RATIO;
            if !near_unique {
                return true;
            }
            // Near-unique columns look like identifiers. Categoricals (UUIDs, free
            // text) are dropped outright. Numeric columns are only ID-like when
            // their values are integers (a primary key / row index 1..N); a
            // near-unique *continuous* numeric column is a legitimate real-valued
            // feature and is kept.
            let numeric = seen.iter().all(|v| parse_num(v).is_some());
            if !numeric {
                return false;
            }
            let all_integers = seen
                .iter()
                .filter_map(|v| parse_num(v))
                .all(|x| x.fract() == 0.0);
            !all_integers
        })
        .collect()
}

/// One encoded feature column block: either a standardized numeric column or a
/// one-hot family for a categorical column. `OneHot` carries the most-frequent
/// category up front so blank-imputation needs no second pass over the rows.
enum ColumnEncoder {
    Numeric { mean: f64, std: f64 },
    OneHot { categories: Vec<String>, most_frequent: String },
}

/// The encoding plan: which raw column maps to which encoder, plus the resulting
/// feature names. Built without allocating the design matrix, so the feature
/// width can be checked against `MAX_FEATURES` before any matrix is materialized.
struct EncodingPlan {
    encoders: Vec<(usize, ColumnEncoder)>,
    feature_names: Vec<String>,
}

/// The fully encoded design matrix.
struct Encoded {
    /// `matrix[i]` is the encoded feature vector for row `i` (no bias term; the
    /// models add their own intercept).
    matrix: Vec<Vec<f64>>,
    feature_names: Vec<String>,
}

fn plan_encoding(
    headers: &[String],
    rows: &[&Vec<String>],
    feature_cols: &[usize],
) -> Result<EncodingPlan> {
    let mut encoders: Vec<(usize, ColumnEncoder)> = Vec::new();
    let mut feature_names: Vec<String> = Vec::new();

    for &c in feature_cols {
        let values: Vec<&str> = rows.iter().map(|r| r[c].trim()).collect();
        let non_empty: Vec<&&str> = values.iter().filter(|v| !v.is_empty()).collect();
        let numeric = !non_empty.is_empty()
            && non_empty.iter().all(|v| parse_num(v).is_some());
        if numeric {
            let nums: Vec<f64> = non_empty.iter().filter_map(|v| parse_num(v)).collect();
            let mean = nums.iter().sum::<f64>() / nums.len() as f64;
            let var = nums.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / nums.len() as f64;
            let std = var.sqrt().max(1e-9);
            encoders.push((c, ColumnEncoder::Numeric { mean, std }));
            feature_names.push(headers[c].clone());
        } else {
            let mut cats: Vec<String> = Vec::new();
            for v in &non_empty {
                let s = (**v).to_string();
                if !cats.contains(&s) {
                    cats.push(s);
                }
            }
            cats.sort();
            if cats.len() > ONE_HOT_MAX_CARDINALITY {
                // Skip — would explode the matrix; treated as unusable here.
                continue;
            }
            let most_frequent = most_frequent_value(rows, c);
            for cat in &cats {
                feature_names.push(format!("{}={}", headers[c], cat));
            }
            encoders.push((c, ColumnEncoder::OneHot { categories: cats, most_frequent }));
        }
    }

    if feature_names.is_empty() {
        bail!("no encodable feature columns");
    }

    Ok(EncodingPlan {
        encoders,
        feature_names,
    })
}

fn materialize_features(rows: &[&Vec<String>], plan: EncodingPlan) -> Encoded {
    let EncodingPlan {
        encoders,
        feature_names,
    } = plan;

    let mut matrix: Vec<Vec<f64>> = Vec::with_capacity(rows.len());
    for r in rows {
        let mut feat: Vec<f64> = Vec::with_capacity(feature_names.len());
        for (c, enc) in &encoders {
            match enc {
                ColumnEncoder::Numeric { mean, std } => {
                    let v = parse_num(r[*c].trim()).unwrap_or(*mean);
                    feat.push((v - mean) / std);
                }
                ColumnEncoder::OneHot { categories, most_frequent } => {
                    let raw = r[*c].trim();
                    let chosen = if raw.is_empty() { most_frequent.as_str() } else { raw };
                    for cat in categories {
                        feat.push(if cat == chosen { 1.0 } else { 0.0 });
                    }
                }
            }
        }
        matrix.push(feat);
    }

    Encoded {
        matrix,
        feature_names,
    }
}

fn most_frequent_value(rows: &[&Vec<String>], col: usize) -> String {
    let mut counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for r in rows {
        let v = r[col].trim();
        if !v.is_empty() {
            *counts.entry(v).or_insert(0) += 1;
        }
    }
    counts
        .into_iter()
        .max_by_key(|(_, n)| *n)
        .map(|(v, _)| v.to_string())
        .unwrap_or_default()
}

/// Parses a numeric cell, tolerating thousands separators and a trailing `%`.
fn parse_num(v: &str) -> Option<f64> {
    let t = v.trim().replace(',', "");
    let t = t.strip_suffix('%').unwrap_or(&t);
    match t.parse::<f64>() {
        Ok(f) if f.is_finite() => Some(f),
        _ => None,
    }
}

// ----------------------------------------------------------------------------
// Classification: multinomial logistic regression (softmax) + baseline.
// ----------------------------------------------------------------------------

fn train_classification(
    enc: &Encoded,
    targets: &[&str],
    split: &Split,
    target_col: &str,
) -> Result<TrainOutcome> {
    // Encode labels into class indices.
    let mut labels: Vec<String> = Vec::new();
    for t in targets {
        if !labels.iter().any(|l| l == t) {
            labels.push((*t).to_string());
        }
    }
    labels.sort();
    if labels.len() < 2 {
        bail!("target has fewer than 2 classes — nothing to classify");
    }
    // Reject identifier-like targets: a column with (near-)unique values per row
    // (e.g. a primary key) is not a class label — training "classifies" each row
    // into its own class and scores ~0 accuracy. Mirror the ID exclusion used for
    // features (`ID_CARDINALITY_RATIO`), but only on non-tiny tables so small test
    // fixtures (few rows, naturally high distinct ratio) are not blocked.
    let n_rows = targets.len();
    if n_rows > MIN_ROWS_FOR_ID_TARGET_GUARD
        && (labels.len() as f64) / (n_rows as f64) >= ID_CARDINALITY_RATIO
    {
        bail!(
            "target column looks like an identifier ({} unique values across {} rows), not a class label — pick a categorical column",
            labels.len(),
            n_rows
        );
    }
    if labels.len() > MAX_TARGET_CLASSES {
        bail!(
            "too many target classes ({}, max {}) — this column looks like an identifier or free text, not a label",
            labels.len(),
            MAX_TARGET_CLASSES
        );
    }
    let label_index = |s: &str| labels.iter().position(|l| l == s).unwrap();
    let y: Vec<usize> = targets.iter().map(|t| label_index(t)).collect();

    let n_classes = labels.len();
    let n_feat = enc.feature_names.len();

    // --- Most-frequent-class baseline ---
    let baseline_start = std::time::Instant::now();
    let mut class_counts = vec![0usize; n_classes];
    for &i in &split.train {
        class_counts[y[i]] += 1;
    }
    let majority = class_counts
        .iter()
        .enumerate()
        .max_by_key(|(_, c)| **c)
        .map(|(k, _)| k)
        .unwrap_or(0);
    let baseline_pred: Vec<usize> = split.holdout.iter().map(|_| majority).collect();
    let baseline_truth: Vec<usize> = split.holdout.iter().map(|&i| y[i]).collect();
    let (b_acc, b_f1) = classification_metrics(&baseline_truth, &baseline_pred, n_classes);
    let baseline_secs = baseline_start.elapsed().as_secs_f64();

    // --- Multinomial logistic regression via gradient descent ---
    let lr_start = std::time::Instant::now();
    // Weight matrix: n_classes rows, (n_feat + 1) cols (last col = bias).
    let mut w = vec![vec![0.0f64; n_feat + 1]; n_classes];
    let train_n = split.train.len() as f64;
    let mut loss_curve: Vec<f64> = Vec::with_capacity(GD_ITERS);

    for _iter in 0..GD_ITERS {
        // Accumulate gradient over the training set.
        let mut grad = vec![vec![0.0f64; n_feat + 1]; n_classes];
        let mut epoch_loss = 0.0f64;
        for &i in &split.train {
            let x = &enc.matrix[i];
            let logits = forward_logits(&w, x, n_feat);
            let probs = softmax(&logits);
            epoch_loss += -(probs[y[i]].max(1e-12)).ln();
            for k in 0..n_classes {
                let err = probs[k] - if k == y[i] { 1.0 } else { 0.0 };
                for j in 0..n_feat {
                    grad[k][j] += err * x[j];
                }
                grad[k][n_feat] += err; // bias
            }
        }
        for k in 0..n_classes {
            for j in 0..=n_feat {
                let mut g = grad[k][j] / train_n;
                if j < n_feat {
                    g += L2_LAMBDA * w[k][j]; // weight decay (not on bias)
                }
                w[k][j] -= LEARNING_RATE * g;
            }
        }
        loss_curve.push(epoch_loss / train_n);
    }

    let lr_pred: Vec<usize> = split
        .holdout
        .iter()
        .map(|&i| {
            let logits = forward_logits(&w, &enc.matrix[i], n_feat);
            argmax(&logits)
        })
        .collect();
    let (lr_acc, lr_f1) = classification_metrics(&baseline_truth, &lr_pred, n_classes);
    let lr_secs = lr_start.elapsed().as_secs_f64();

    let mut leaderboard = vec![
        LeaderboardEntry {
            model_name: "Regresja logistyczna".to_string(),
            framework: "rust-logreg".to_string(),
            accuracy: Some(lr_acc),
            f1_macro: Some(lr_f1),
            rmse: None,
            r2: None,
            train_secs: lr_secs,
        },
        LeaderboardEntry {
            model_name: "Klasa większościowa".to_string(),
            framework: "rust-baseline".to_string(),
            accuracy: Some(b_acc),
            f1_macro: Some(b_f1),
            rmse: None,
            r2: None,
            train_secs: baseline_secs,
        },
    ];

    // smartcore — drzewo decyzyjne i las losowy na TYM SAMYM splicie. Te modele
    // moga zawiesc na zdegenerowanych danych (np. pojedyncza cecha bez wariancji);
    // wtedy logujemy ostrzezenie i pomijamy wpis, nie przerywajac calego treningu.
    if let Some(x_train) = dense_from_indices(enc, &split.train) {
        let y_train: Vec<i64> = split.train.iter().map(|&i| y[i] as i64).collect();
        let truth = &baseline_truth;

        match fit_tree_classifier(&x_train, &y_train, n_classes, enc, split, truth) {
            Ok(entry) => leaderboard.push(entry),
            Err(e) => tracing::warn!(error = %e, "smartcore decision tree (classification) skipped"),
        }
        match fit_forest_classifier(&x_train, &y_train, n_classes, enc, split, truth) {
            Ok(entry) => leaderboard.push(entry),
            Err(e) => tracing::warn!(error = %e, "smartcore random forest (classification) skipped"),
        }
    } else {
        tracing::warn!("smartcore classifiers skipped: empty training matrix");
    }

    leaderboard.sort_by(|a, b| {
        b.accuracy
            .partial_cmp(&a.accuracy)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(
                b.f1_macro
                    .partial_cmp(&a.f1_macro)
                    .unwrap_or(std::cmp::Ordering::Equal),
            )
    });
    let best_model_name = leaderboard[0].model_name.clone();
    let best_loss_curve = if best_model_name == "Regresja logistyczna" {
        loss_curve
    } else {
        Vec::new()
    };

    Ok(TrainOutcome {
        task: Task::Classification,
        target_column: target_col.to_string(),
        feature_names: enc.feature_names.clone(),
        train_rows: split.train.len(),
        holdout_rows: split.holdout.len(),
        class_labels: labels,
        leaderboard,
        best_model_name,
        best_loss_curve,
    })
}

fn forward_logits(w: &[Vec<f64>], x: &[f64], n_feat: usize) -> Vec<f64> {
    w.iter()
        .map(|wk| {
            let mut s = wk[n_feat]; // bias
            for j in 0..n_feat {
                s += wk[j] * x[j];
            }
            s
        })
        .collect()
}

fn softmax(logits: &[f64]) -> Vec<f64> {
    let max = logits.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    let exps: Vec<f64> = logits.iter().map(|l| (l - max).exp()).collect();
    let sum: f64 = exps.iter().sum();
    let sum = if sum > 0.0 { sum } else { 1.0 };
    exps.iter().map(|e| e / sum).collect()
}

fn argmax(v: &[f64]) -> usize {
    v.iter()
        .enumerate()
        .max_by(|a, b| a.1.partial_cmp(b.1).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(i, _)| i)
        .unwrap_or(0)
}

/// Accuracy and macro-averaged F1 computed from a real confusion matrix.
fn classification_metrics(truth: &[usize], pred: &[usize], n_classes: usize) -> (f64, f64) {
    if truth.is_empty() {
        return (0.0, 0.0);
    }
    let mut correct = 0usize;
    let mut tp = vec![0usize; n_classes];
    let mut fp = vec![0usize; n_classes];
    let mut fn_ = vec![0usize; n_classes];
    for (&t, &p) in truth.iter().zip(pred.iter()) {
        if t == p {
            correct += 1;
            tp[t] += 1;
        } else {
            fp[p] += 1;
            fn_[t] += 1;
        }
    }
    let accuracy = correct as f64 / truth.len() as f64;
    // Macro-F1 over classes that actually appear in the truth set.
    let mut f1_sum = 0.0;
    let mut present = 0usize;
    for k in 0..n_classes {
        if tp[k] + fn_[k] == 0 {
            continue; // class absent from holdout truth
        }
        present += 1;
        let precision = if tp[k] + fp[k] == 0 {
            0.0
        } else {
            tp[k] as f64 / (tp[k] + fp[k]) as f64
        };
        let recall = tp[k] as f64 / (tp[k] + fn_[k]) as f64;
        let f1 = if precision + recall == 0.0 {
            0.0
        } else {
            2.0 * precision * recall / (precision + recall)
        };
        f1_sum += f1;
    }
    let f1_macro = if present == 0 { 0.0 } else { f1_sum / present as f64 };
    (accuracy, f1_macro)
}

// ----------------------------------------------------------------------------
// Regression: ordinary linear regression (gradient descent) + mean baseline.
// ----------------------------------------------------------------------------

fn train_regression(
    enc: &Encoded,
    targets: &[&str],
    split: &Split,
    target_col: &str,
) -> Result<TrainOutcome> {
    let y: Vec<f64> = targets
        .iter()
        .map(|t| parse_num(t))
        .collect::<Option<Vec<f64>>>()
        .ok_or_else(|| {
            anyhow::anyhow!("regression target '{}' has non-numeric values", target_col)
        })?;

    let n_feat = enc.feature_names.len();

    // --- Mean baseline ---
    let baseline_start = std::time::Instant::now();
    let train_mean = {
        let s: f64 = split.train.iter().map(|&i| y[i]).sum();
        s / split.train.len() as f64
    };
    let holdout_truth: Vec<f64> = split.holdout.iter().map(|&i| y[i]).collect();
    let baseline_pred: Vec<f64> = holdout_truth.iter().map(|_| train_mean).collect();
    let (b_rmse, b_r2) = regression_metrics(&holdout_truth, &baseline_pred);
    let baseline_secs = baseline_start.elapsed().as_secs_f64();

    // --- Linear regression via full-batch gradient descent ---
    let lin_start = std::time::Instant::now();
    let mut w = vec![0.0f64; n_feat + 1]; // last = bias
    let train_n = split.train.len() as f64;
    let mut loss_curve: Vec<f64> = Vec::with_capacity(GD_ITERS);

    for _iter in 0..GD_ITERS {
        let mut grad = vec![0.0f64; n_feat + 1];
        let mut mse = 0.0f64;
        for &i in &split.train {
            let x = &enc.matrix[i];
            let mut pred = w[n_feat];
            for j in 0..n_feat {
                pred += w[j] * x[j];
            }
            let err = pred - y[i];
            mse += err * err;
            for j in 0..n_feat {
                grad[j] += err * x[j];
            }
            grad[n_feat] += err;
        }
        for j in 0..=n_feat {
            let mut g = grad[j] / train_n;
            if j < n_feat {
                g += L2_LAMBDA * w[j];
            }
            w[j] -= LEARNING_RATE * g;
        }
        loss_curve.push(mse / train_n);
    }

    let lin_pred: Vec<f64> = split
        .holdout
        .iter()
        .map(|&i| {
            let x = &enc.matrix[i];
            let mut pred = w[n_feat];
            for j in 0..n_feat {
                pred += w[j] * x[j];
            }
            pred
        })
        .collect();
    let (lin_rmse, lin_r2) = regression_metrics(&holdout_truth, &lin_pred);
    let lin_secs = lin_start.elapsed().as_secs_f64();

    let mut leaderboard = vec![
        LeaderboardEntry {
            model_name: "Regresja liniowa".to_string(),
            framework: "rust-linreg".to_string(),
            accuracy: None,
            f1_macro: None,
            rmse: Some(lin_rmse),
            r2: Some(lin_r2),
            train_secs: lin_secs,
        },
        LeaderboardEntry {
            model_name: "Średnia (baseline)".to_string(),
            framework: "rust-baseline".to_string(),
            accuracy: None,
            f1_macro: None,
            rmse: Some(b_rmse),
            r2: Some(b_r2),
            train_secs: baseline_secs,
        },
    ];

    // smartcore — drzewo i las regresyjny na tym samym splicie; bledy fit pomijamy.
    if let Some(x_train) = dense_from_indices(enc, &split.train) {
        let y_train: Vec<f64> = split.train.iter().map(|&i| y[i]).collect();
        let truth = &holdout_truth;

        match fit_tree_regressor(&x_train, &y_train, enc, split, truth) {
            Ok(entry) => leaderboard.push(entry),
            Err(e) => tracing::warn!(error = %e, "smartcore decision tree (regression) skipped"),
        }
        match fit_forest_regressor(&x_train, &y_train, enc, split, truth) {
            Ok(entry) => leaderboard.push(entry),
            Err(e) => tracing::warn!(error = %e, "smartcore random forest (regression) skipped"),
        }
    } else {
        tracing::warn!("smartcore regressors skipped: empty training matrix");
    }

    // Lower RMSE is better; tie-break on higher R^2.
    leaderboard.sort_by(|a, b| {
        a.rmse
            .partial_cmp(&b.rmse)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(
                b.r2.partial_cmp(&a.r2)
                    .unwrap_or(std::cmp::Ordering::Equal),
            )
    });
    let best_model_name = leaderboard[0].model_name.clone();
    let best_loss_curve = if best_model_name == "Regresja liniowa" {
        loss_curve
    } else {
        Vec::new()
    };

    Ok(TrainOutcome {
        task: Task::Regression,
        target_column: target_col.to_string(),
        feature_names: enc.feature_names.clone(),
        train_rows: split.train.len(),
        holdout_rows: split.holdout.len(),
        class_labels: Vec::new(),
        leaderboard,
        best_model_name,
        best_loss_curve,
    })
}

/// Root-mean-squared error and coefficient of determination (R^2).
fn regression_metrics(truth: &[f64], pred: &[f64]) -> (f64, f64) {
    if truth.is_empty() {
        return (0.0, 0.0);
    }
    let n = truth.len() as f64;
    let mean = truth.iter().sum::<f64>() / n;
    let mut ss_res = 0.0;
    let mut ss_tot = 0.0;
    for (&t, &p) in truth.iter().zip(pred.iter()) {
        ss_res += (t - p).powi(2);
        ss_tot += (t - mean).powi(2);
    }
    let rmse = (ss_res / n).sqrt();
    let r2 = if ss_tot > 0.0 { 1.0 - ss_res / ss_tot } else { 0.0 };
    (rmse, r2)
}

// ----------------------------------------------------------------------------
// smartcore models (pure-Rust decision tree + random forest). Shared helpers.
// ----------------------------------------------------------------------------

/// Builds a smartcore `DenseMatrix<f64>` from the rows of `enc.matrix` selected by
/// `idx` (a train or holdout index list). Returns `None` when the selection or the
/// feature width is empty — `DenseMatrix::from_2d_vec` rejects empty input, so the
/// caller skips the smartcore models instead of erroring the whole run.
fn dense_from_indices(enc: &Encoded, idx: &[usize]) -> Option<DenseMatrix<f64>> {
    if idx.is_empty() || enc.feature_names.is_empty() {
        return None;
    }
    let rows: Vec<Vec<f64>> = idx.iter().map(|&i| enc.matrix[i].clone()).collect();
    DenseMatrix::from_2d_vec(&rows).ok()
}

/// Scores a fitted smartcore classifier on the holdout set: builds the holdout
/// matrix, predicts, casts the `i64` class indices back to `usize` and reuses
/// `classification_metrics`. `truth` is the per-holdout true class index.
fn score_classifier<F>(
    enc: &Encoded,
    split: &Split,
    n_classes: usize,
    truth: &[usize],
    predict: F,
) -> Result<(f64, f64)>
where
    F: Fn(&DenseMatrix<f64>) -> Result<Vec<i64>>,
{
    let x_holdout = dense_from_indices(enc, &split.holdout)
        .ok_or_else(|| anyhow::anyhow!("empty holdout matrix"))?;
    let raw = predict(&x_holdout)?;
    let pred: Vec<usize> = raw
        .into_iter()
        .map(|c| if c < 0 { 0 } else { c as usize })
        .collect();
    Ok(classification_metrics(truth, &pred, n_classes))
}

/// Scores a fitted smartcore regressor on the holdout set via `regression_metrics`.
fn score_regressor<F>(
    enc: &Encoded,
    split: &Split,
    truth: &[f64],
    predict: F,
) -> Result<(f64, f64)>
where
    F: Fn(&DenseMatrix<f64>) -> Result<Vec<f64>>,
{
    let x_holdout = dense_from_indices(enc, &split.holdout)
        .ok_or_else(|| anyhow::anyhow!("empty holdout matrix"))?;
    let pred = predict(&x_holdout)?;
    Ok(regression_metrics(truth, &pred))
}

fn fit_tree_classifier(
    x_train: &DenseMatrix<f64>,
    y_train: &[i64],
    n_classes: usize,
    enc: &Encoded,
    split: &Split,
    truth: &[usize],
) -> Result<LeaderboardEntry> {
    let start = std::time::Instant::now();
    // Pojedyncze drzewo jest deterministyczne (brak losowego samplingu cech),
    // ale ustawiamy seed jawnie dla powtarzalnosci tie-breakow w splitach.
    let mut params = DecisionTreeClassifierParameters::default().with_max_depth(TREE_MAX_DEPTH);
    params.seed = Some(SMARTCORE_SEED);
    let model = DecisionTreeClassifier::fit(x_train, &y_train.to_vec(), params)
        .map_err(|e| anyhow::anyhow!("smartcore tree fit failed: {e}"))?;
    let (acc, f1) = score_classifier(enc, split, n_classes, truth, |x| {
        model
            .predict(x)
            .map_err(|e| anyhow::anyhow!("smartcore tree predict failed: {e}"))
    })?;
    Ok(LeaderboardEntry {
        model_name: "Drzewo decyzyjne".to_string(),
        framework: SMARTCORE_FRAMEWORK.to_string(),
        accuracy: Some(acc),
        f1_macro: Some(f1),
        rmse: None,
        r2: None,
        train_secs: start.elapsed().as_secs_f64(),
    })
}

fn fit_forest_classifier(
    x_train: &DenseMatrix<f64>,
    y_train: &[i64],
    n_classes: usize,
    enc: &Encoded,
    split: &Split,
    truth: &[usize],
) -> Result<LeaderboardEntry> {
    let start = std::time::Instant::now();
    let params = RandomForestClassifierParameters::default()
        .with_n_trees(RF_N_TREES)
        .with_max_depth(TREE_MAX_DEPTH)
        .with_seed(SMARTCORE_SEED);
    let model = RandomForestClassifier::fit(x_train, &y_train.to_vec(), params)
        .map_err(|e| anyhow::anyhow!("smartcore forest fit failed: {e}"))?;
    let (acc, f1) = score_classifier(enc, split, n_classes, truth, |x| {
        model
            .predict(x)
            .map_err(|e| anyhow::anyhow!("smartcore forest predict failed: {e}"))
    })?;
    Ok(LeaderboardEntry {
        model_name: "Las losowy".to_string(),
        framework: SMARTCORE_FRAMEWORK.to_string(),
        accuracy: Some(acc),
        f1_macro: Some(f1),
        rmse: None,
        r2: None,
        train_secs: start.elapsed().as_secs_f64(),
    })
}

fn fit_tree_regressor(
    x_train: &DenseMatrix<f64>,
    y_train: &[f64],
    enc: &Encoded,
    split: &Split,
    truth: &[f64],
) -> Result<LeaderboardEntry> {
    let start = std::time::Instant::now();
    let mut params = DecisionTreeRegressorParameters::default().with_max_depth(TREE_MAX_DEPTH);
    params.seed = Some(SMARTCORE_SEED);
    let model = DecisionTreeRegressor::fit(x_train, &y_train.to_vec(), params)
        .map_err(|e| anyhow::anyhow!("smartcore tree regressor fit failed: {e}"))?;
    let (rmse, r2) = score_regressor(enc, split, truth, |x| {
        model
            .predict(x)
            .map_err(|e| anyhow::anyhow!("smartcore tree regressor predict failed: {e}"))
    })?;
    Ok(LeaderboardEntry {
        model_name: "Drzewo decyzyjne".to_string(),
        framework: SMARTCORE_FRAMEWORK.to_string(),
        accuracy: None,
        f1_macro: None,
        rmse: Some(rmse),
        r2: Some(r2),
        train_secs: start.elapsed().as_secs_f64(),
    })
}

fn fit_forest_regressor(
    x_train: &DenseMatrix<f64>,
    y_train: &[f64],
    enc: &Encoded,
    split: &Split,
    truth: &[f64],
) -> Result<LeaderboardEntry> {
    let start = std::time::Instant::now();
    let params = RandomForestRegressorParameters::default()
        .with_n_trees(RF_N_TREES as usize)
        .with_max_depth(TREE_MAX_DEPTH)
        .with_seed(SMARTCORE_SEED);
    let model = RandomForestRegressor::fit(x_train, &y_train.to_vec(), params)
        .map_err(|e| anyhow::anyhow!("smartcore forest regressor fit failed: {e}"))?;
    let (rmse, r2) = score_regressor(enc, split, truth, |x| {
        model
            .predict(x)
            .map_err(|e| anyhow::anyhow!("smartcore forest regressor predict failed: {e}"))
    })?;
    Ok(LeaderboardEntry {
        model_name: "Las losowy".to_string(),
        framework: SMARTCORE_FRAMEWORK.to_string(),
        accuracy: None,
        f1_macro: None,
        rmse: Some(rmse),
        r2: Some(r2),
        train_secs: start.elapsed().as_secs_f64(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(cols: &[&str]) -> Vec<String> {
        cols.iter().map(|s| s.to_string()).collect()
    }

    fn row(cells: &[&str]) -> Vec<String> {
        cells.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn logreg_learns_linearly_separable_data() {
        // Two numeric features; label is 1 when x1 + x2 > 0 — perfectly
        // separable. A real gradient learner must beat the majority baseline
        // and reach high holdout accuracy. A stub/random model could not.
        let headers = h(&["x1", "x2", "label"]);
        let mut rows = Vec::new();
        // Deterministic grid of points around the decision boundary.
        for a in -10..10 {
            for b in -10..10 {
                let x1 = a as f64 * 0.5;
                let x2 = b as f64 * 0.5;
                let label = if x1 + x2 > 0.0 { "yes" } else { "no" };
                rows.push(row(&[&x1.to_string(), &x2.to_string(), label]));
            }
        }
        let out = train_tabular(&headers, &rows, "label", Task::Classification).unwrap();
        let logreg = out
            .leaderboard
            .iter()
            .find(|e| e.framework == "rust-logreg")
            .unwrap();
        let baseline = out
            .leaderboard
            .iter()
            .find(|e| e.framework == "rust-baseline")
            .unwrap();
        // Real learning: high accuracy and clearly above the majority baseline.
        assert!(
            logreg.accuracy.unwrap() > 0.9,
            "logreg accuracy too low: {:?}",
            logreg.accuracy
        );
        assert!(
            logreg.f1_macro.unwrap() > 0.9,
            "logreg macro-F1 too low: {:?}",
            logreg.f1_macro
        );
        assert!(logreg.accuracy.unwrap() > baseline.accuracy.unwrap());
        assert_eq!(out.best_model_name, "Regresja logistyczna");
        // Loss must strictly decrease overall (gradient is converging).
        assert!(out.best_loss_curve.first().unwrap() > out.best_loss_curve.last().unwrap());
    }

    #[test]
    fn linreg_fits_linear_target() {
        // y = 3*x + 2 (+ a categorical that is irrelevant). Linear regression
        // must drive RMSE far below the mean baseline and R^2 near 1.
        let headers = h(&["x", "city", "y"]);
        let mut rows = Vec::new();
        for i in 0..120 {
            let x = i as f64 * 0.1;
            let y = 3.0 * x + 2.0;
            let city = if i % 2 == 0 { "A" } else { "B" };
            rows.push(row(&[&x.to_string(), city, &y.to_string()]));
        }
        let out = train_tabular(&headers, &rows, "y", Task::Regression).unwrap();
        let lin = out
            .leaderboard
            .iter()
            .find(|e| e.framework == "rust-linreg")
            .unwrap();
        let baseline = out
            .leaderboard
            .iter()
            .find(|e| e.framework == "rust-baseline")
            .unwrap();
        assert!(lin.r2.unwrap() > 0.95, "linreg R^2 too low: {:?}", lin.r2);
        assert!(lin.rmse.unwrap() < baseline.rmse.unwrap());
        assert_eq!(out.best_model_name, "Regresja liniowa");
    }

    #[test]
    fn numeric_id_column_excluded_from_features() {
        // `row_id` runs 1..N (integer, near-unique) and must be treated as an
        // identifier, not a feature. `score` is the only real signal.
        let headers = h(&["row_id", "score", "label"]);
        let mut rows = Vec::new();
        for i in 0..200 {
            let score = (i % 20) as f64;
            let label = if score > 9.0 { "high" } else { "low" };
            rows.push(row(&[&(i + 1).to_string(), &score.to_string(), label]));
        }
        let out = train_tabular(&headers, &rows, "label", Task::Classification).unwrap();
        assert!(
            !out.feature_names.iter().any(|f| f == "row_id"),
            "row_id leaked into features: {:?}",
            out.feature_names
        );
        assert!(out.feature_names.iter().any(|f| f == "score"));
    }

    #[test]
    fn too_many_target_classes_errors() {
        // Many distinct classes that each repeat (low distinct/row ratio, so the
        // identifier guard stays off) must still be rejected by the harder
        // MAX_TARGET_CLASSES cap rather than building a giant softmax.
        let headers = h(&["x", "bucket"]);
        let mut rows = Vec::new();
        let n_classes = MAX_TARGET_CLASSES + 20;
        for i in 0..(n_classes * 4) {
            let bucket = format!("B{}", i % n_classes);
            rows.push(row(&[&(i as f64 * 0.1).to_string(), &bucket]));
        }
        let err = train_tabular(&headers, &rows, "bucket", Task::Classification)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("too many target classes"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn target_identifier_column_rejected() {
        // The target `id` runs 1..N (one unique value per row). On a non-tiny
        // table this is an identifier, not a label: training would "classify"
        // each row into its own class at ~0 accuracy, so it must be rejected.
        let headers = h(&["id", "score"]);
        let mut rows = Vec::new();
        for i in 0..40 {
            rows.push(row(&[&(i + 1).to_string(), &((i % 5) as f64).to_string()]));
        }
        let err = train_tabular(&headers, &rows, "id", Task::Classification)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("identifier"),
            "expected identifier rejection, got: {err}"
        );

        // A genuine categorical target (a handful of classes, each repeated many
        // times) must still train fine — the guard only fires on near-unique
        // targets.
        let headers = h(&["score", "tier"]);
        let mut rows = Vec::new();
        for i in 0..40 {
            let tier = match i % 3 {
                0 => "low",
                1 => "mid",
                _ => "high",
            };
            rows.push(row(&[&((i % 7) as f64).to_string(), tier]));
        }
        let out = train_tabular(&headers, &rows, "tier", Task::Classification)
            .expect("categorical target with repeats must train");
        assert_eq!(out.class_labels.len(), 3);
    }

    #[test]
    fn smartcore_classification_models_on_leaderboard() {
        // Te same separowalne dane co logreg — sprawdzamy ze smartcore drzewo i
        // las trafiaja na leaderboard z poprawnym frameworkiem i sensownymi
        // metrykami (accuracy/F1 w [0,1], drzewa ucza sie wzorca > baseline).
        let headers = h(&["x1", "x2", "label"]);
        let mut rows = Vec::new();
        for a in -10..10 {
            for b in -10..10 {
                let x1 = a as f64 * 0.5;
                let x2 = b as f64 * 0.5;
                let label = if x1 + x2 > 0.0 { "yes" } else { "no" };
                rows.push(row(&[&x1.to_string(), &x2.to_string(), label]));
            }
        }
        let out = train_tabular(&headers, &rows, "label", Task::Classification).unwrap();

        let tree = out
            .leaderboard
            .iter()
            .find(|e| e.model_name == "Drzewo decyzyjne")
            .expect("decision tree entry missing");
        let forest = out
            .leaderboard
            .iter()
            .find(|e| e.model_name == "Las losowy")
            .expect("random forest entry missing");
        assert_eq!(tree.framework, "smartcore (Rust)");
        assert_eq!(forest.framework, "smartcore (Rust)");
        for e in [tree, forest] {
            let acc = e.accuracy.expect("classification entry needs accuracy");
            assert!((0.0..=1.0).contains(&acc), "accuracy out of range: {acc}");
            let f1 = e.f1_macro.expect("classification entry needs f1");
            assert!((0.0..=1.0).contains(&f1), "f1 out of range: {f1}");
            assert!(e.rmse.is_none() && e.r2.is_none());
        }
        // Na separowalnych danych drzewa musza realnie sie uczyc (powyzej losowego).
        assert!(tree.accuracy.unwrap() > 0.7, "tree acc: {:?}", tree.accuracy);
        assert!(
            forest.accuracy.unwrap() > 0.7,
            "forest acc: {:?}",
            forest.accuracy
        );
    }

    #[test]
    fn smartcore_regression_models_on_leaderboard() {
        // y = 3*x + 2 — drzewo i las regresyjny trafiaja na leaderboard z
        // metrykami RMSE/R^2 i frameworkiem smartcore.
        let headers = h(&["x", "city", "y"]);
        let mut rows = Vec::new();
        for i in 0..120 {
            let x = i as f64 * 0.1;
            let y = 3.0 * x + 2.0;
            let city = if i % 2 == 0 { "A" } else { "B" };
            rows.push(row(&[&x.to_string(), city, &y.to_string()]));
        }
        let out = train_tabular(&headers, &rows, "y", Task::Regression).unwrap();

        let tree = out
            .leaderboard
            .iter()
            .find(|e| e.model_name == "Drzewo decyzyjne")
            .expect("decision tree regressor missing");
        let forest = out
            .leaderboard
            .iter()
            .find(|e| e.model_name == "Las losowy")
            .expect("random forest regressor missing");
        assert_eq!(tree.framework, "smartcore (Rust)");
        assert_eq!(forest.framework, "smartcore (Rust)");
        for e in [tree, forest] {
            let rmse = e.rmse.expect("regression entry needs rmse");
            assert!(rmse.is_finite() && rmse >= 0.0, "bad rmse: {rmse}");
            assert!(e.r2.is_some(), "regression entry needs r2");
            assert!(e.accuracy.is_none() && e.f1_macro.is_none());
        }
        // Najlepszy wpis (po sortowaniu wg RMSE) musi byc spojny z best_model_name.
        assert_eq!(out.best_model_name, out.leaderboard[0].model_name);
    }

    #[test]
    fn missing_target_column_errors() {
        let headers = h(&["a", "b"]);
        let rows = vec![row(&["1", "2"]), row(&["3", "4"])];
        assert!(train_tabular(&headers, &rows, "nope", Task::Classification).is_err());
    }
}
