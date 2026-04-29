// =============================================================================
// Plik: profiling/timeline.rs
// Opis: Ekstrakcja timeline'u GPU z `.nsys-rep` przez `nsys export --type sqlite`
//       i kwantyzacja probek do binow 100ms (mniej danych w GUI niz raw 10ms
//       sampling z Nsight).
// =============================================================================

use std::collections::HashMap;
use std::path::Path;

use super::nsys::{nsys_command, ProfilingError};
use rusqlite::Connection;
use tentaflow_protocol::profiling::{GpuUtilSample, GpuUtilSeries, NsightGpuTarget};

/// Surowa probka odczytana z SQLite — 1 wiersz `GPU_METRICS`.
#[derive(Debug, Clone)]
struct RawGpuSample {
    timestamp_ns: i64,
    device_id: u8,
    sm_active: f32,
    dram_active: f32,
    fb_used_mb: u32,
    power_w: f32,
}

const BIN_MS: u32 = 100;

/// Eksportuje `.nsys-rep` do SQLite i wyciaga timeline GPU jako serie probek
/// po 100ms. Brak tabeli `GPU_METRICS` (sesja bez `--gpu-metrics-device`)
/// zwraca pusty Vec — to nie jest blad.
pub(crate) async fn extract_gpu_timeline(
    rep_path: &Path,
    gpu_targets: &[NsightGpuTarget],
    power_limits: &HashMap<u8, f32>,
) -> Result<Vec<GpuUtilSeries>, ProfilingError> {
    // Refuse symlinks: rep_path must be a regular file inside our storage tree.
    let meta = tokio::fs::symlink_metadata(rep_path).await?;
    if meta.file_type().is_symlink() {
        return Err(ProfilingError::Parse("rep_path is symlink".into()));
    }

    let sqlite_path = rep_path.with_extension("sqlite");

    if !sqlite_path.exists() {
        let output = nsys_command()?
            .args(["export", "--type", "sqlite", "--output"])
            .arg(&sqlite_path)
            .arg(rep_path)
            .output()
            .await?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            return Err(ProfilingError::ProcessFailed(format!(
                "nsys export failed: {stderr}"
            )));
        }
    }

    let conn = Connection::open(&sqlite_path).map_err(|e| ProfilingError::Db(e.to_string()))?;

    // Sprawdz czy tabela GPU_METRICS istnieje — niektore sesje nie nagrywaja metryk.
    let table_exists: bool = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name='GPU_METRICS' LIMIT 1",
            [],
            |_| Ok(true),
        )
        .unwrap_or(false);
    if !table_exists {
        return Ok(Vec::new());
    }

    let mut stmt = conn
        .prepare(
            "SELECT timestamp, deviceId, smActive, dramActive, fbUsed, gpuPower \
             FROM GPU_METRICS ORDER BY timestamp",
        )
        .map_err(|e| ProfilingError::Db(e.to_string()))?;

    let rows = stmt
        .query_map([], |row| {
            Ok(RawGpuSample {
                timestamp_ns: row.get::<_, i64>(0)?,
                device_id: row.get::<_, i64>(1)? as u8,
                sm_active: row.get::<_, f64>(2)? as f32,
                dram_active: row.get::<_, f64>(3)? as f32,
                fb_used_mb: (row.get::<_, i64>(4)? / (1024 * 1024)) as u32,
                power_w: row.get::<_, f64>(5)? as f32,
            })
        })
        .map_err(|e| ProfilingError::Db(e.to_string()))?;

    let mut per_device: std::collections::BTreeMap<u8, Vec<RawGpuSample>> =
        std::collections::BTreeMap::new();
    for r in rows {
        let r = r.map_err(|e| ProfilingError::Db(e.to_string()))?;
        per_device.entry(r.device_id).or_default().push(r);
    }

    let mut out: Vec<GpuUtilSeries> = Vec::new();
    for (device_id, samples) in per_device {
        // Mute unused-warning while keeping API stable for future per-target metadata.
        let _ = gpu_targets;
        let power_limit_w = power_limits.get(&device_id).copied().unwrap_or(0.0);
        out.push(GpuUtilSeries {
            gpu_idx: device_id,
            power_limit_w,
            samples: bin_samples_100ms(samples),
        });
    }

    Ok(out)
}

/// Agreguje raw probki nsys (typowo 10ms) do binow 100ms — srednia sm/mem,
/// max vram/power w binie. VRAM uzywa max bo to peak occupancy w okienku jest
/// istotny dla diagnostyki OOM, nie srednia. Pierwsza probka definiuje t=0.
fn bin_samples_100ms(raw: Vec<RawGpuSample>) -> Vec<GpuUtilSample> {
    if raw.is_empty() {
        return Vec::new();
    }
    let t0 = raw[0].timestamp_ns;
    let mut bins: std::collections::BTreeMap<u32, Vec<RawGpuSample>> =
        std::collections::BTreeMap::new();
    for s in raw {
        let off_ms = ((s.timestamp_ns - t0).max(0) / 1_000_000) as u32;
        let bin = (off_ms / BIN_MS) * BIN_MS;
        bins.entry(bin).or_default().push(s);
    }
    let mut out: Vec<GpuUtilSample> = Vec::with_capacity(bins.len());
    for (bin_t, group) in bins {
        let n = group.len() as f32;
        let sm_avg: f32 = group.iter().map(|s| s.sm_active).sum::<f32>() / n;
        let dram_avg: f32 = group.iter().map(|s| s.dram_active).sum::<f32>() / n;
        let vram_max: u32 = group.iter().map(|s| s.fb_used_mb).max().unwrap_or(0);
        let power_max: f32 = group
            .iter()
            .map(|s| s.power_w)
            .fold(f32::NEG_INFINITY, f32::max);
        out.push(GpuUtilSample {
            t_ms: bin_t,
            sm_pct: sm_avg.clamp(0.0, 100.0) as u8,
            mem_pct: dram_avg.clamp(0.0, 100.0) as u8,
            vram_used_mb: vram_max,
            power_w: if power_max.is_finite() {
                power_max
            } else {
                0.0
            },
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(timestamp_ns: i64, device: u8, sm: f32) -> RawGpuSample {
        RawGpuSample {
            timestamp_ns,
            device_id: device,
            sm_active: sm,
            dram_active: 50.0,
            fb_used_mb: 1024,
            power_w: 200.0,
        }
    }

    #[test]
    fn bin_samples_100ms_empty() {
        assert!(bin_samples_100ms(Vec::new()).is_empty());
    }

    #[test]
    fn bin_samples_100ms_basic() {
        // 1000 probek po 1ms (timestamp 0..1_000_000_000ns) -> 10 binow.
        let raws: Vec<RawGpuSample> = (0..1000).map(|i| raw(i * 1_000_000, 0, 50.0)).collect();
        let out = bin_samples_100ms(raws);
        assert_eq!(out.len(), 10);
        assert_eq!(out[0].t_ms, 0);
        assert_eq!(out[1].t_ms, 100);
        assert!(out.iter().all(|s| s.sm_pct == 50));
    }

    #[test]
    fn bin_samples_100ms_partial_bin() {
        // 150 probek po 1ms -> bin 0 (100 probek) + bin 100 (50 probek) = 2 biny.
        let raws: Vec<RawGpuSample> = (0..150).map(|i| raw(i * 1_000_000, 0, 30.0)).collect();
        let out = bin_samples_100ms(raws);
        assert_eq!(out.len(), 2);
    }
}
