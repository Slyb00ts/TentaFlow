// ===== File: rules.rs — the mechanical gates, one function per rule =====
//
// Every rule here exists because the repository already paid for its absence.
// The `why` field carries that price so nobody has to guess whether a gate is
// worth keeping; it is printed with the report.

use crate::scan::{block_len, strip_comment, Scope, SourceFile};

/// Per-file result of one rule.
pub struct Measure {
    pub metric: usize,
    /// Up to a few example locations, for the report only.
    pub samples: Vec<(usize, String)>,
}

pub struct Rule {
    pub id: &'static str,
    pub title: &'static str,
    pub why: &'static str,
    /// Unit of `metric`, printed in the report ("lines" / "occurrences").
    pub unit: &'static str,
    /// Value a clean file must not exceed when it has no baseline entry.
    pub default_allowance: usize,
    pub scopes: &'static [Scope],
    pub eval: fn(&SourceFile) -> Measure,
    /// Heuristic rules flag code for human review instead of proving a defect.
    pub review_only: bool,
}

/// The single module allowed to read the environment once the configuration
/// contract lands. It does not exist yet, so today every read is a violation —
/// that is the intended starting position, not a mistake.
const CONFIG_MODULE: &str = "crates/forge-types/src/config/load.rs";

/// Crates that PLAN_NAPRAWY 5.1 lets touch the hardware. Everything above this
/// line describes a model, a format or a schedule, and must not know whether a
/// GPU is underneath — that is the whole point of the layering, and the reason
/// one checkpoint reads the same on every machine.
const HAL_IS_ALLOWED_IN: &[&str] = &[
    "crates/forge-kernels/",
    "crates/forge-state/",
    "crates/forge-cli/",
    "crates/forge-hal/",
];

pub const RULES: &[Rule] = &[
    Rule {
        id: "hal_boundary",
        title: "forge-hal reached from a layer that must not know the hardware",
        why: "a model that imports the HAL grows one loader per platform, and the second loader is the one that quietly misses a format quirk",
        unit: "occurrences",
        default_allowance: 0,
        scopes: &[Scope::RustAll],
        eval: eval_hal_boundary,
        review_only: false,
    },
    Rule {
        id: "size",
        title: "File size limit",
        why: "model.rs 21430 and launchers.rs 20430 lines were a real brake on every change",
        unit: "lines",
        default_allowance: 1500,
        scopes: &[Scope::RustAll],
        eval: eval_size,
        review_only: false,
    },
    Rule {
        id: "env",
        title: "Environment reads outside the configuration module",
        why: "83 FORGE_* variables meant 83 separately untested execution paths",
        unit: "occurrences",
        default_allowance: 0,
        scopes: &[Scope::RustSrc],
        eval: eval_env,
        review_only: false,
    },
    Rule {
        id: "placeholder",
        title: "Placeholders in production code",
        why: "a placeholder that reaches a release is a silent wrong answer, not a compile error",
        unit: "occurrences",
        default_allowance: 0,
        scopes: &[Scope::RustSrc],
        eval: eval_placeholder,
        review_only: false,
    },
    Rule {
        id: "vendor_gate",
        title: "Vendor gate wider than 20 lines without a justification comment",
        why: "three over-wide NVIDIA-only gates in one file cost 16.4x on MTP verify and 26x on hybrid prefill",
        unit: "occurrences",
        default_allowance: 0,
        scopes: &[Scope::RustSrc],
        eval: eval_vendor_gate,
        review_only: false,
    },
    Rule {
        id: "kernel_geometry",
        title: "Kernel variant name must carry its full tile geometry",
        why: "a name with _bm but no _bn lets the launcher size the grid for the wrong tile and silently skip half the rows",
        unit: "occurrences",
        default_allowance: 0,
        scopes: &[Scope::RustSrc, Scope::Manifest],
        eval: eval_kernel_geometry,
        review_only: false,
    },
    Rule {
        id: "batch_antipattern",
        title: "Single-token-in-a-batch kernel shapes (three criteria)",
        why: "the same defect was found four times: per-token grids, one workgroup per row, host loops issuing many launches",
        unit: "occurrences",
        default_allowance: 0,
        scopes: &[Scope::Mojo, Scope::RustSrc],
        eval: eval_batch_antipattern,
        review_only: true,
    },
];

fn sample(samples: &mut Vec<(usize, String)>, idx: usize, line: &str) {
    if samples.len() < 3 {
        samples.push((idx + 1, line.trim().chars().take(110).collect()));
    }
}

fn eval_size(f: &SourceFile) -> Measure {
    Measure {
        metric: f.line_count(),
        samples: Vec::new(),
    }
}

fn eval_env(f: &SourceFile) -> Measure {
    // A cargo build script reads the environment because that is the only
    // interface cargo gives it: OUT_DIR, CARGO_FEATURE_*, CARGO_CFG_TARGET_OS.
    // The rule exists to kill RUNTIME execution-path switches, and none of
    // those are one.
    if f.rel.ends_with("/build.rs") || f.rel == CONFIG_MODULE {
        return Measure {
            metric: 0,
            samples: Vec::new(),
        };
    }
    let mut metric = 0;
    let mut samples = Vec::new();
    for (idx, line) in f.lines.iter().enumerate() {
        let code = strip_comment(line, f.lang);
        if code.contains("env::var") || code.contains("env::var_os") {
            metric += 1;
            sample(&mut samples, idx, line);
        }
    }
    Measure { metric, samples }
}

fn eval_placeholder(f: &SourceFile) -> Measure {
    // `tests/` and `examples/` are out of scope via Scope::RustSrc, but unit
    // tests live inside src behind #[cfg(test)]; a placeholder there is still a
    // placeholder, so no extra exemption is granted.
    const NEEDLES: &[&str] = &[
        "todo!(",
        "unimplemented!(",
        "// TODO",
        "// FIXME",
        "not implemented",
    ];
    let mut metric = 0;
    let mut samples = Vec::new();
    for (idx, line) in f.lines.iter().enumerate() {
        if NEEDLES.iter().any(|n| line.contains(n)) {
            metric += 1;
            sample(&mut samples, idx, line);
        }
    }
    Measure { metric, samples }
}

fn eval_vendor_gate(f: &SourceFile) -> Measure {
    const VENDOR_TOKENS: &[&str] = &[
        "Vendor::",
        "is_nvidia",
        "is_amd",
        "is_apple",
        "arch ==",
        "target ==",
    ];
    let mut metric = 0;
    let mut samples = Vec::new();
    for (idx, line) in f.lines.iter().enumerate() {
        let code = strip_comment(line, f.lang);
        let is_condition =
            code.contains("if ") || code.contains("match ") || code.contains("matches!");
        if !is_condition || !VENDOR_TOKENS.iter().any(|t| code.contains(t)) {
            continue;
        }
        if block_len(&f.lines, idx, f.lang) <= 20 {
            continue;
        }
        // A justification within the three preceding lines makes the gate a
        // deliberate decision instead of an accident.
        let start = idx.saturating_sub(3);
        let justified = f.lines[start..idx]
            .iter()
            .any(|l| l.contains("justification:") || l.contains("uzasadnienie:"));
        if !justified {
            metric += 1;
            sample(&mut samples, idx, line);
        }
    }
    Measure { metric, samples }
}

fn eval_kernel_geometry(f: &SourceFile) -> Measure {
    // Counts DISTINCT variant names, not mentions: the debt is the number of
    // ill-named kernels, and one name repeated in forty launchers is still one
    // kernel to rename.
    let mut seen: Vec<String> = Vec::new();
    let mut samples = Vec::new();
    for (idx, line) in f.lines.iter().enumerate() {
        for token in tokens_with_tile_prefix(line, "_bm") {
            if token.contains("_bn") || seen.contains(&token) {
                continue;
            }
            sample(&mut samples, idx, &token);
            seen.push(token);
        }
    }
    Measure {
        metric: seen.len(),
        samples,
    }
}

/// Collects identifier-like tokens that contain `prefix` followed by a digit,
/// which is how tile geometry is spelled in kernel names (`gemm_..._bm256_bn128`).
fn tokens_with_tile_prefix(line: &str, prefix: &str) -> Vec<String> {
    let mut out = Vec::new();
    for raw in line.split(|c: char| !(c.is_alphanumeric() || c == '_')) {
        if raw.len() < prefix.len() + 1 {
            continue;
        }
        let Some(pos) = raw.find(prefix) else { continue };
        let after = &raw[pos + prefix.len()..];
        if after.chars().next().is_some_and(|c| c.is_ascii_digit()) {
            out.push(raw.to_string());
        }
    }
    out
}

fn eval_batch_antipattern(f: &SourceFile) -> Measure {
    let is_host_code = f.rel.starts_with("crates/");
    let mut metric = 0;
    let mut samples = Vec::new();
    for (idx, line) in f.lines.iter().enumerate() {
        let code = strip_comment(line, f.lang);

        if let Some(grid) = grid_tuple(code) {
            let axes = grid_axes(&grid);

            // (1) A grid whose token axis is used whole: one block per
            //     (row group, token) makes every token re-read the entire
            //     weight matrix. A token axis divided by a tile constant is the
            //     correct shape and must not be flagged.
            let untiled_tokens = axes
                .iter()
                .any(|a| mentions_token_axis(a) && !is_tiled(a));
            // (2) One workgroup per output row, no tiling at all.
            let untiled_rows = axes.len() == 1 && mentions_row_axis(&axes[0]) && !is_tiled(&axes[0]);

            if untiled_tokens || untiled_rows {
                metric += 1;
                sample(&mut samples, idx, line);
                continue;
            }
        }

        // (3) A host loop issuing one launch per iteration where a single GEMM
        //     would do. Host-side only: a loop around a launch in a Mojo
        //     benchmark is repetition for timing, not a shape defect.
        if is_host_code && code.trim_start().starts_with("for ") && launches_inside(f, idx) {
            metric += 1;
            sample(&mut samples, idx, line);
        }
    }
    Measure { metric, samples }
}

/// Extracts the grid expression from a launch site, so that kernel arguments
/// standing next to it are never mistaken for grid axes.
fn grid_tuple(code: &str) -> Option<String> {
    const MARKERS: &[&str] = &["grid_dim=", "grid_dim =", "grid=", "grid =", "blocks="];
    let lower = code.to_ascii_lowercase();
    let marker = MARKERS.iter().find_map(|m| lower.find(m).map(|p| (p, m.len())))?;
    let rest = lower[marker.0 + marker.1..].trim_start();

    // The grid expression runs to the first comma at depth zero, so that a
    // trailing operator survives: `grid = (rows + 7) // 8` is one tiled axis,
    // not a bare `rows` axis, and cutting at the closing parenthesis would
    // report every correctly tiled launch.
    let mut depth = 0i32;
    let mut end = rest.len();
    for (i, ch) in rest.char_indices() {
        match ch {
            '(' | '[' => depth += 1,
            ')' | ']' => {
                if depth == 0 {
                    end = i;
                    break;
                }
                depth -= 1;
            }
            ',' if depth == 0 => {
                end = i;
                break;
            }
            _ => {}
        }
    }
    Some(rest[..end].trim().to_string())
}

/// Splits a grid expression into axes. A parenthesised expression is a tuple
/// only when it holds a comma at its own nesting level.
fn grid_axes(expr: &str) -> Vec<String> {
    if expr.starts_with('(') && expr.ends_with(')') {
        let inner = &expr[1..expr.len() - 1];
        let parts = split_top_level(inner);
        if parts.len() > 1 || inner.trim_end().ends_with(',') {
            return parts;
        }
    }
    vec![expr.to_string()]
}

/// Splits a tuple body on commas that sit outside any nested parentheses.
fn split_top_level(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut current = String::new();
    for ch in body.chars() {
        match ch {
            '(' | '[' => {
                depth += 1;
                current.push(ch);
            }
            ')' | ']' => {
                depth -= 1;
                current.push(ch);
            }
            ',' if depth == 0 => {
                out.push(current.trim().to_string());
                current = String::new();
            }
            _ => current.push(ch),
        }
    }
    let last = current.trim();
    if !last.is_empty() {
        out.push(last.to_string());
    }
    out
}

fn mentions_token_axis(axis: &str) -> bool {
    ["n_tokens", "num_tokens", "ntokens", "n_tok", "tokens"]
        .iter()
        .any(|n| axis.contains(n))
}

fn mentions_row_axis(axis: &str) -> bool {
    ["n_rows", "rows", "out_features", "n_out"]
        .iter()
        .any(|n| axis.contains(n))
}

fn is_tiled(axis: &str) -> bool {
    axis.contains('/') || axis.contains("bm") || axis.contains("bn") || axis.contains("tile")
}

fn launches_inside(f: &SourceFile, start: usize) -> bool {
    let len = block_len(&f.lines, start, f.lang).min(40);
    if len <= 1 {
        return false;
    }
    f.lines[start..start + len]
        .iter()
        .any(|l| l.contains(".launch(") || l.contains("launch_kernel(") || l.contains(".dispatch("))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::Lang;

    fn measure(rel: &str, src: &str) -> usize {
        let file = SourceFile {
            rel: rel.to_string(),
            lang: if rel.ends_with(".mojo") {
                Lang::Mojo
            } else {
                Lang::Rust
            },
            lines: src.lines().map(|l| l.to_string()).collect(),
        };
        eval_batch_antipattern(&file).metric
    }

    #[test]
    fn tiled_two_axis_grid_is_clean() {
        // Both axes divided by a tile constant: this is the correct shape and
        // must never be reported, or the rule drowns the real defects.
        assert_eq!(
            measure(
                "kernels/mojo/a.mojo",
                "grid_dim=((n_rows + 127) // 128, (n_tokens + 63) // 64), block_dim=128,"
            ),
            0
        );
    }

    #[test]
    fn tiled_single_axis_with_trailing_divide_is_clean() {
        assert_eq!(measure("kernels/mojo/a.mojo", "grid = (rows + 7) // 8"), 0);
    }

    #[test]
    fn untiled_token_axis_is_reported() {
        // The measured defect: one block per (row group, token), so every token
        // re-reads the whole weight matrix.
        assert_eq!(
            measure("kernels/mojo/a.mojo", "grid_dim=(hidden // 8, n_tokens),"),
            1
        );
    }

    #[test]
    fn one_workgroup_per_row_is_reported() {
        assert_eq!(measure("kernels/mojo/a.mojo", "grid_dim=rows, block_dim=256)"), 1);
        assert_eq!(measure("kernels/mojo/a.mojo", "grid_dim=(n_rows,), block_dim=256,"), 1);
    }

    #[test]
    fn host_loop_around_launch_is_reported_only_in_host_code() {
        let src = "for r in 0..rep {\n    self.device.launch(k, cfg)?;\n}\n";
        assert_eq!(measure("crates/forge-engine/src/model.rs", src), 1);
        assert_eq!(measure("kernels/mojo/a.mojo", src), 0);
    }

    #[test]
    fn kernel_name_without_bn_is_reported_once_per_name() {
        let file = SourceFile {
            rel: "crates/forge-kernels/src/launchers.rs".to_string(),
            lang: Lang::Rust,
            lines: vec![
                "\"gemm_nvfp4_gguf_wmma_f16_bm256\",".to_string(),
                "\"gemm_nvfp4_gguf_wmma_f16_bm256\",".to_string(),
                "\"gemm_fp8_wmma_bm256_bn128\",".to_string(),
            ],
        };
        assert_eq!(eval_kernel_geometry(&file).metric, 1);
    }

    #[test]
    fn vendor_gate_needs_justification_only_when_wide() {
        let narrow = SourceFile {
            rel: "crates/a/src/b.rs".to_string(),
            lang: Lang::Rust,
            lines: vec!["if caps.vendor == Vendor::Nvidia {".to_string(), "}".to_string()],
        };
        assert_eq!(eval_vendor_gate(&narrow).metric, 0);

        let mut lines = vec!["if caps.vendor == Vendor::Nvidia {".to_string()];
        lines.extend((0..25).map(|i| format!("    step({i});")));
        lines.push("}".to_string());
        let wide = SourceFile {
            rel: "crates/a/src/b.rs".to_string(),
            lang: Lang::Rust,
            lines: lines.clone(),
        };
        assert_eq!(eval_vendor_gate(&wide).metric, 1);

        let mut justified = vec!["// justification: measured 26x on this path".to_string()];
        justified.extend(lines);
        let ok = SourceFile {
            rel: "crates/a/src/b.rs".to_string(),
            lang: Lang::Rust,
            lines: justified,
        };
        assert_eq!(eval_vendor_gate(&ok).metric, 0);
    }
}

/// Counts imports of the hardware layer from crates PLAN_NAPRAWY 5.1 forbids it in.
fn eval_hal_boundary(file: &SourceFile) -> Measure {
    // Regula opisuje graf crate'ow, a `xtask` w nim nie lezy — bez tego lapie
    // wlasny kod wykrywajacy.
    if !file.rel.starts_with("crates/") || HAL_IS_ALLOWED_IN.iter().any(|c| file.rel.starts_with(c)) {
        return Measure { metric: 0, samples: Vec::new() };
    }
    let mut hits = Vec::new();
    for (i, line) in file.lines.iter().enumerate() {
        let code = strip_comment(line, file.lang);
        if code.contains("forge_hal::") || code.contains("use forge_hal") {
            hits.push((i + 1, line.trim().to_string()));
        }
    }
    Measure {
        metric: hits.len(),
        samples: hits,
    }
}
