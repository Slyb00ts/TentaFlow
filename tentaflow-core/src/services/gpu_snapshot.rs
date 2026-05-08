// =============================================================================
// Plik: services/gpu_snapshot.rs
// Opis: Lekki nvidia-smi snapshot per call. Używany przez Edit Service modal /
//       Deploy Wizard Advanced step do pokazywania user'owi co AKTUALNIE
//       zajmuje GPU (sunshine, chrome itp.) oraz do liczenia recommended
//       gpu_memory_utilization który *uwzględnia* external usage.
//
//       Nie persystuje, nie pollu w background — collect_vram_snapshot()
//       wywoływane on-demand przez handler `service.vram_hint`. Dla
//       refreshów per 2s frontend ponownie wywołuje request — koszt jednego
//       snapshot to ~2× exec nvidia-smi (~50-150ms).
// =============================================================================

use anyhow::Result;
use std::process::Command;
use tentaflow_protocol::{GpuProcessInfo, GpuVramSnapshot};

/// Desktop reserve — ile MiB trzymamy "z zapasem" dla X11/Wayland +
/// chrome compositor, żeby `recommended_utilization` nie zjadało
/// całego wolnego VRAM-u i nie crashowało desktopu user'a.
///
/// 1 GiB = bezpieczny default dla typowego dev hosta z kompozytorem
/// + okazjonalnym chrome tabem. Headless / serwerowy bare-metal nie
/// ma desktopu — tam całe `free_mib` pójdzie pod cap.
const DESKTOP_RESERVE_MIB: u64 = 1024;

/// Zbiera snapshot VRAM z `nvidia-smi`. Zwraca pustą listę gdy:
///   * nvidia-smi nie jest w PATH (host bez GPU lub bez sterownika),
///   * exec failed (brak permission, broken driver itp.).
///
/// `gpu_index` zawęża zwracaną listę do jednej karty; `None` = wszystkie.
/// `exclude_pids` to PIDs procesów do wykluczenia z `external_processes`
/// (np. własny serwis który już startuje — chcemy zobaczyć tylko *cudze*
/// zajęcie GPU).
pub async fn collect_vram_snapshot(
    gpu_index: Option<u32>,
    exclude_pids: &[u32],
) -> Vec<GpuVramSnapshot> {
    let exclude: Vec<u32> = exclude_pids.to_vec();
    let target_idx = gpu_index;
    tokio::task::spawn_blocking(move || collect_blocking(target_idx, &exclude))
        .await
        .unwrap_or_default()
}

/// Wylicza sugerowaną wartość `gpu_memory_utilization` (0.10..0.95) dla
/// GPU, biorąc pod uwagę ile VRAM-u jest aktualnie wolne MINUS desktop
/// reserve. Wzór: `(free_mib - DESKTOP_RESERVE_MIB) / total_mib`.
///
/// Gdy wolnego po reserve jest mniej niż 10% total, klampuje do 0.10
/// (minimum żeby slider się nie zerował) — wtedy GUI pokazuje czerwony
/// banner "za mało VRAM" i admin sam podejmuje decyzję.
pub fn recommended_utilization(snapshot: &GpuVramSnapshot) -> f32 {
    if snapshot.total_mib == 0 {
        return 0.9; // brak telemetrii — fallback na vLLM default
    }
    let free_after_reserve = snapshot.free_mib.saturating_sub(DESKTOP_RESERVE_MIB);
    let raw = free_after_reserve as f32 / snapshot.total_mib as f32;
    raw.clamp(0.10, 0.95)
}

/// Sync wnętrze (uruchamiane przez spawn_blocking). Dwa wywołania
/// nvidia-smi:
///   1. `--query-gpu=index,name,memory.total,memory.free,memory.used` —
///      per-GPU agregaty.
///   2. `--query-compute-apps=gpu_uuid,pid,process_name,used_memory` —
///      lista procesów. Łączymy z (1) po `gpu_uuid` (zmieniamy mapowanie
///      UUID → index przez dodatkowe `--query-gpu=uuid`).
fn collect_blocking(target_idx: Option<u32>, exclude_pids: &[u32]) -> Vec<GpuVramSnapshot> {
    let gpus = match query_gpu_inventory() {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let procs_by_uuid = query_compute_apps().unwrap_or_default();

    gpus.into_iter()
        .filter(|g| target_idx.map_or(true, |i| g.index == i))
        .map(|g| {
            let mut external_processes: Vec<GpuProcessInfo> = procs_by_uuid
                .iter()
                .filter(|(uuid, _)| uuid == &g.uuid)
                .flat_map(|(_, procs)| procs.iter())
                .filter(|p| !exclude_pids.contains(&p.pid))
                .cloned()
                .collect();
            external_processes.sort_by(|a, b| b.used_mib.cmp(&a.used_mib));
            GpuVramSnapshot {
                gpu_index: g.index,
                gpu_name: g.name,
                total_mib: g.total_mib,
                free_mib: g.free_mib,
                used_mib: g.used_mib,
                external_processes,
            }
        })
        .collect()
}

struct GpuInventoryRow {
    index: u32,
    uuid: String,
    name: String,
    total_mib: u64,
    free_mib: u64,
    used_mib: u64,
}

fn query_gpu_inventory() -> Result<Vec<GpuInventoryRow>> {
    let out = Command::new("nvidia-smi")
        .args([
            "--query-gpu=index,uuid,name,memory.total,memory.free,memory.used",
            "--format=csv,noheader,nounits",
        ])
        .output()?;
    if !out.status.success() {
        anyhow::bail!("nvidia-smi --query-gpu exit {:?}", out.status.code());
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let mut rows = Vec::new();
    for line in stdout.lines() {
        let cols: Vec<&str> = line.split(',').map(|s| s.trim()).collect();
        if cols.len() < 6 {
            continue;
        }
        let index: u32 = cols[0].parse().unwrap_or(0);
        let uuid = cols[1].to_string();
        let name = cols[2].to_string();
        let total_mib: u64 = cols[3].parse().unwrap_or(0);
        let free_mib: u64 = cols[4].parse().unwrap_or(0);
        let used_mib: u64 = cols[5].parse().unwrap_or(0);
        rows.push(GpuInventoryRow {
            index,
            uuid,
            name,
            total_mib,
            free_mib,
            used_mib,
        });
    }
    Ok(rows)
}

/// Zwraca mapę `gpu_uuid → Vec<GpuProcessInfo>`. nvidia-smi może raportować
/// procesy z process_name jako pełna ścieżka (np. `/usr/bin/sunshine`); my
/// trzymamy raw — frontend obetnie sobie do basename gdy potrzeba.
fn query_compute_apps() -> Result<Vec<(String, Vec<GpuProcessInfo>)>> {
    let out = Command::new("nvidia-smi")
        .args([
            "--query-compute-apps=gpu_uuid,pid,process_name,used_memory",
            "--format=csv,noheader,nounits",
        ])
        .output()?;
    if !out.status.success() {
        anyhow::bail!("nvidia-smi --query-compute-apps exit {:?}", out.status.code());
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let mut map: std::collections::HashMap<String, Vec<GpuProcessInfo>> =
        std::collections::HashMap::new();
    for line in stdout.lines() {
        let cols: Vec<&str> = line.split(',').map(|s| s.trim()).collect();
        if cols.len() < 4 {
            continue;
        }
        let uuid = cols[0].to_string();
        let pid: u32 = cols[1].parse().unwrap_or(0);
        if pid == 0 {
            continue;
        }
        let process_name = cols[2].to_string();
        let used_mib: u64 = cols[3].parse().unwrap_or(0);
        map.entry(uuid).or_default().push(GpuProcessInfo {
            pid,
            process_name,
            used_mib,
        });
    }
    Ok(map.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Recommendation logic: full free GPU → ~95% cap (clamp), zatłoczone
    /// GPU → 0.10 floor.
    #[test]
    fn recommended_clamp_extremes() {
        let empty = GpuVramSnapshot {
            gpu_index: 0,
            gpu_name: "RTX 4090".into(),
            total_mib: 24_000,
            free_mib: 23_500,
            used_mib: 500,
            external_processes: Vec::new(),
        };
        let r = recommended_utilization(&empty);
        assert!(r >= 0.9 && r <= 0.95, "free GPU should give ≥0.9, got {r}");

        let crowded = GpuVramSnapshot {
            gpu_index: 0,
            gpu_name: "RTX 4090".into(),
            total_mib: 24_000,
            free_mib: 1_500, // tylko ~500 MiB po reserve
            used_mib: 22_500,
            external_processes: Vec::new(),
        };
        let r2 = recommended_utilization(&crowded);
        assert!(r2 <= 0.05 + 0.10, "crowded GPU should clamp to 0.10, got {r2}");
        assert!(r2 >= 0.10);
    }

    /// Typical desktop scenario: 23.5GB total, 1.9GB used by sunshine+chrome,
    /// 21.6GB free. Reserve 1GB → 20.6/23.5 ≈ 0.876.
    #[test]
    fn recommended_typical_desktop() {
        let snap = GpuVramSnapshot {
            gpu_index: 0,
            gpu_name: "RTX 4090".into(),
            total_mib: 23_500,
            free_mib: 21_600,
            used_mib: 1_900,
            external_processes: vec![
                GpuProcessInfo {
                    pid: 2080,
                    process_name: "/usr/bin/sunshine".into(),
                    used_mib: 395,
                },
                GpuProcessInfo {
                    pid: 9999,
                    process_name: "chrome".into(),
                    used_mib: 1_538,
                },
            ],
        };
        let r = recommended_utilization(&snap);
        assert!(
            (r - 0.876).abs() < 0.01,
            "typical desktop should give ~0.876, got {r}"
        );
    }

    /// Zerowy total (brak telemetrii / GPU nie wykryte) → fallback 0.9.
    #[test]
    fn recommended_zero_total_falls_back() {
        let snap = GpuVramSnapshot {
            gpu_index: 0,
            gpu_name: String::new(),
            total_mib: 0,
            free_mib: 0,
            used_mib: 0,
            external_processes: Vec::new(),
        };
        assert_eq!(recommended_utilization(&snap), 0.9);
    }
}
