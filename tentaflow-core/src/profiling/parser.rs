// =============================================================================
// Plik: profiling/parser.rs
// Opis: Parsowanie wyniku `nsys stats --format json` do struktury ParsedStats.
//       Defensywny — kazda sekcja moze byc nieobecna; pola w wierszach to
//       Option<>, brakujace traktujemy jako 0.
// =============================================================================

use std::path::Path;

use serde::Deserialize;
use tentaflow_protocol::profiling::{ProfileKpi, ProfileTopRow};
use super::nsys::{nsys_command, ProfilingError};

/// Domyslny limit wierszy per tabela top — stat reports zwracaja czesto >1000
/// linii a w GUI pokazujemy max ~50.
const DEFAULT_TOP_N: usize = 50;

/// Wynik parsowania `nsys stats` — agregaty potrzebne do `ProfileReport`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ParsedStats {
    pub kpi: ProfileKpi,
    pub gpu_kernels_top: Vec<ProfileTopRow>,
    pub cuda_api_top: Vec<ProfileTopRow>,
    pub gpu_mem_ops: Vec<ProfileTopRow>,
    pub cpu_samples_top: Vec<ProfileTopRow>,
    pub nvtx_ranges_top: Vec<ProfileTopRow>,
}

/// Surowy wiersz z JSON-a `nsys stats`. Format zmienia sie miedzy wersjami,
/// wiec wszystkie pola sa Option i probujemy kilku wariantow nazewnictwa.
#[derive(Debug, Deserialize, Default)]
struct RawRow {
    #[serde(default, alias = "Time(ns)", alias = "Total Time (ns)", alias = "Total Time")]
    time_ns: Option<f64>,
    #[serde(default, alias = "Total Time (ms)")]
    time_ms: Option<f64>,
    #[serde(default, alias = "Calls", alias = "Num Calls", alias = "Instances")]
    calls: Option<u64>,
    #[serde(default, alias = "Avg(ns)", alias = "Avg (ns)", alias = "Avg")]
    avg_ns: Option<f64>,
    #[serde(default, alias = "Avg(ms)", alias = "Avg (ms)")]
    avg_ms: Option<f64>,
    #[serde(default, alias = "Name", alias = "Operation", alias = "Range", alias = "Symbol")]
    name: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct RawSection {
    #[serde(default, alias = "reportName", alias = "report")]
    report_name: Option<String>,
    #[serde(default)]
    data: Vec<RawRow>,
}

fn ns_to_ms(ns: f64) -> f64 {
    ns / 1_000_000.0
}

fn row_total_ms(r: &RawRow) -> f64 {
    if let Some(ms) = r.time_ms {
        ms
    } else if let Some(ns) = r.time_ns {
        ns_to_ms(ns)
    } else {
        0.0
    }
}

fn row_avg_ms(r: &RawRow) -> f64 {
    if let Some(ms) = r.avg_ms {
        ms
    } else if let Some(ns) = r.avg_ns {
        ns_to_ms(ns)
    } else {
        0.0
    }
}

fn convert_rows(rows: &[RawRow]) -> Vec<ProfileTopRow> {
    rows.iter()
        .filter_map(|r| {
            let name = r.name.clone()?;
            Some(ProfileTopRow {
                name,
                total_ms: row_total_ms(r),
                calls: r.calls.unwrap_or(0),
                avg_ms: row_avg_ms(r),
                pct: 0.0,
            })
        })
        .collect()
}

/// Sortuje desc po `total_ms`, przycina do `n`, liczy pct.
fn top_n_with_pct(mut rows: Vec<ProfileTopRow>, n: usize) -> Vec<ProfileTopRow> {
    rows.sort_by(|a, b| {
        b.total_ms
            .partial_cmp(&a.total_ms)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    rows.truncate(n);
    let total: f64 = rows.iter().map(|r| r.total_ms).sum();
    if total > 0.0 {
        for r in &mut rows {
            r.pct = ((r.total_ms / total) * 100.0) as f32;
        }
    }
    rows
}

fn report_kind(name: &str) -> &str {
    let n = name.to_lowercase();
    if n.contains("gpukern") || n.contains("cuda_gpu_kern") {
        "gpu_kernels"
    } else if n.contains("cudaapi") || n.contains("cuda_api") {
        "cuda_api"
    } else if n.contains("gpumem") || n.contains("cuda_gpu_mem") {
        "gpu_mem"
    } else if n.contains("osrt") || n.contains("cpusample") || n.contains("os_runtime") {
        "cpu_samples"
    } else if n.contains("nvtx") {
        "nvtx"
    } else {
        ""
    }
}

/// Parsuje JSON jako string — pure function, testowalna bez subprocesu nsys.
pub fn parse_stats_json_str(s: &str) -> Result<ParsedStats, ProfilingError> {
    // Format `nsys stats --format json`: tablica obiektow {reportName, data: [...]}.
    // W niektorych wersjach output zawiera obiekt z polem `reports`. Probujemy oba.
    let sections: Vec<RawSection> = if let Ok(arr) = serde_json::from_str::<Vec<RawSection>>(s) {
        arr
    } else if let Ok(wrap) = serde_json::from_str::<serde_json::Value>(s) {
        if let Some(reports) = wrap.get("reports").and_then(|v| v.as_array()) {
            reports
                .iter()
                .filter_map(|v| serde_json::from_value::<RawSection>(v.clone()).ok())
                .collect()
        } else {
            return Err(ProfilingError::Parse(
                "unexpected nsys stats JSON shape".to_string(),
            ));
        }
    } else {
        return Err(ProfilingError::Parse(
            "invalid JSON from nsys stats".to_string(),
        ));
    };

    let mut out = ParsedStats::default();

    for section in sections {
        let name = section.report_name.unwrap_or_default();
        let kind = report_kind(&name);
        let rows = convert_rows(&section.data);

        match kind {
            "gpu_kernels" => {
                out.kpi.kernel_count = rows.iter().map(|r| r.calls).sum();
                out.kpi.total_gpu_active_ms += rows.iter().map(|r| r.total_ms).sum::<f64>();
                out.gpu_kernels_top = top_n_with_pct(rows, DEFAULT_TOP_N);
            }
            "cuda_api" => {
                out.kpi.cuda_api_count = rows.iter().map(|r| r.calls).sum();
                out.cuda_api_top = top_n_with_pct(rows, DEFAULT_TOP_N);
            }
            "gpu_mem" => {
                out.gpu_mem_ops = top_n_with_pct(rows, DEFAULT_TOP_N);
            }
            "cpu_samples" => {
                out.kpi.samples_collected += rows.iter().map(|r| r.calls).sum::<u64>();
                out.kpi.total_cpu_active_ms += rows.iter().map(|r| r.total_ms).sum::<f64>();
                out.cpu_samples_top = top_n_with_pct(rows, DEFAULT_TOP_N);
            }
            "nvtx" => {
                out.nvtx_ranges_top = top_n_with_pct(rows, DEFAULT_TOP_N);
            }
            _ => {
                // Nieznana sekcja — ignorujemy zeby nie wywalac calego parsowania.
            }
        }
    }

    Ok(out)
}

/// Wywoluje `nsys stats --format json` na pliku `.nsys-rep` i parsuje wynik.
/// Cienki wrapper na `parse_stats_json_str` — pure parsing siedzi tam.
pub async fn parse_nsys_stats_json(rep_path: &Path) -> Result<ParsedStats, ProfilingError> {
    let output = nsys_command()?
        .args([
            "stats",
            "--report",
            "cudaapisum,gpukernsum,gpumemsizesum,osrtsum,cpusample,nvtxsum",
            "--format",
            "json",
            "--output",
            "-",
        ])
        .arg(rep_path)
        .output()
        .await?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        return Err(ProfilingError::ProcessFailed(format!(
            "nsys stats failed: {stderr}"
        )));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_stats_json_str(&stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> String {
        let p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/nsys")
            .join(name);
        std::fs::read_to_string(p).expect("fixture present")
    }

    #[test]
    fn top_n_with_pct_sorts_desc() {
        let rows = vec![
            ProfileTopRow {
                name: "a".into(),
                total_ms: 10.0,
                calls: 1,
                avg_ms: 10.0,
                pct: 0.0,
            },
            ProfileTopRow {
                name: "b".into(),
                total_ms: 100.0,
                calls: 2,
                avg_ms: 50.0,
                pct: 0.0,
            },
            ProfileTopRow {
                name: "c".into(),
                total_ms: 30.0,
                calls: 3,
                avg_ms: 10.0,
                pct: 0.0,
            },
        ];
        let out = top_n_with_pct(rows, 5);
        assert_eq!(out[0].name, "b");
        assert_eq!(out[1].name, "c");
        assert_eq!(out[2].name, "a");
        let sum_pct: f32 = out.iter().map(|r| r.pct).sum();
        assert!((sum_pct - 100.0).abs() < 0.5);
    }

    #[test]
    fn top_n_with_pct_caps_at_n() {
        let rows: Vec<_> = (0..10)
            .map(|i| ProfileTopRow {
                name: format!("k{i}"),
                total_ms: i as f64,
                calls: 1,
                avg_ms: i as f64,
                pct: 0.0,
            })
            .collect();
        let out = top_n_with_pct(rows, 5);
        assert_eq!(out.len(), 5);
    }

    #[test]
    fn parse_stats_basic() {
        let json = fixture("stats_full.json");
        let parsed = parse_stats_json_str(&json).unwrap();
        assert!(!parsed.gpu_kernels_top.is_empty());
        assert!(!parsed.cuda_api_top.is_empty());
        assert!(!parsed.cpu_samples_top.is_empty());
        let sum_pct: f32 = parsed.gpu_kernels_top.iter().map(|r| r.pct).sum();
        assert!((sum_pct - 100.0).abs() < 0.5);
    }

    #[test]
    fn parse_stats_empty_cpu() {
        let json = fixture("stats_gpu_only.json");
        let parsed = parse_stats_json_str(&json).unwrap();
        assert!(parsed.cpu_samples_top.is_empty());
        assert_eq!(parsed.kpi.total_cpu_active_ms, 0.0);
        assert!(!parsed.gpu_kernels_top.is_empty());
    }

    #[test]
    fn parse_stats_malformed() {
        let json = fixture("stats_malformed.json");
        let err = parse_stats_json_str(&json).unwrap_err();
        assert!(matches!(err, ProfilingError::Parse(_)));
    }
}
