// ===== File: gpu_telemetry/windows.rs — DXGI + PDH GPU sampler for Windows =====
//
// DXGI lists the adapters (name, PCI vendor/device, dedicated VRAM, LUID); PDH
// supplies usage through three wildcard counters, registered by their English
// names so a localized Windows works too:
//   \GPU Adapter Memory(luid_…_phys_N)\Dedicated Usage       per adapter, bytes
//   \GPU Process Memory(pid_P_luid_…_phys_N)\Dedicated Usage per process, bytes
//   \GPU Engine(pid_P_luid_…_phys_N_eng_E_engtype_T)\Utilization Percentage
// Utilization follows Task Manager: an engine's load is the sum over processes,
// the adapter's load is its busiest engine.

use std::collections::HashMap;
use std::sync::{Arc, Once};
use std::time::{Duration, Instant};

use parking_lot::RwLock;
use tracing::warn;
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIFactory1, DXGI_ADAPTER_DESC1, DXGI_ADAPTER_FLAG_SOFTWARE,
};

use super::{AdapterUsage, GpuTelemetry, ProcessUsage};
use crate::profiling::collectors::windows::pdh_sys::PdhQuery;

const SAMPLE_INTERVAL: Duration = Duration::from_secs(1);
/// Adapters change only on driver install or hot-plug.
const ADAPTER_REFRESH: Duration = Duration::from_secs(60);
/// PCI vendor of Microsoft's software and remote-display adapters.
const MICROSOFT_VENDOR_ID: u32 = 0x1414;

static LATEST: RwLock<Option<Arc<GpuTelemetry>>> = RwLock::new(None);
static START: Once = Once::new();

pub(super) fn snapshot() -> Option<Arc<GpuTelemetry>> {
    START.call_once(|| {
        if let Err(e) = std::thread::Builder::new()
            .name("gpu-telemetry".into())
            .spawn(run_sampler)
        {
            warn!("gpu telemetry: cannot start the sampler thread: {e}");
        }
    });
    LATEST.read().clone()
}

struct Adapter {
    luid: u64,
    name: String,
    vendor_id: u32,
    device_id: u32,
    dedicated_total_bytes: u64,
}

fn run_sampler() {
    let mut adapters = enumerate_adapters();
    let mut adapters_at = Instant::now();

    // The PDH handles live in this thread only (they are not Send).
    let query = match PdhQuery::open() {
        Ok(q) => q,
        Err(e) => {
            warn!("gpu telemetry: {e}; VRAM totals only");
            publish(&adapters, &[], &[], &[]);
            return;
        }
    };
    let counter = |path: &str| {
        query
            .add_english_counter(path)
            .map_err(|e| warn!("gpu telemetry: {path}: {e}"))
            .ok()
    };
    let adapter_memory = counter(r"\GPU Adapter Memory(*)\Dedicated Usage");
    let process_memory = counter(r"\GPU Process Memory(*)\Dedicated Usage");
    let engines = counter(r"\GPU Engine(*)\Utilization Percentage");

    loop {
        if adapters_at.elapsed() >= ADAPTER_REFRESH {
            adapters = enumerate_adapters();
            adapters_at = Instant::now();
        }
        if let Err(e) = query.collect() {
            warn!("gpu telemetry: {e}");
        }
        let adapter_mem = adapter_memory
            .as_ref()
            .map(|c| c.instances_large())
            .unwrap_or_default();
        let process_mem = process_memory
            .as_ref()
            .map(|c| c.instances_large())
            .unwrap_or_default();
        let engine_util = engines
            .as_ref()
            .map(|c| c.instances_double())
            .unwrap_or_default();
        publish(&adapters, &adapter_mem, &process_mem, &engine_util);
        std::thread::sleep(SAMPLE_INTERVAL);
    }
}

fn publish(
    adapters: &[Adapter],
    adapter_mem: &[(String, i64)],
    process_mem: &[(String, i64)],
    engine_util: &[(String, f64)],
) {
    let telemetry = build_telemetry(adapters, adapter_mem, process_mem, engine_util);
    *LATEST.write() = Some(Arc::new(telemetry));
}

/// Hardware adapters in DXGI order, without the software rasterizer and the
/// remote-display adapter.
fn enumerate_adapters() -> Vec<Adapter> {
    // SAFETY: plain factory creation; no COM apartment is needed for DXGI.
    let factory: IDXGIFactory1 = match unsafe { CreateDXGIFactory1() } {
        Ok(f) => f,
        Err(e) => {
            warn!("gpu telemetry: CreateDXGIFactory1: {e}");
            return Vec::new();
        }
    };
    let mut out = Vec::new();
    for index in 0.. {
        // SAFETY: EnumAdapters1 returns DXGI_ERROR_NOT_FOUND past the last one.
        let Ok(adapter) = (unsafe { factory.EnumAdapters1(index) }) else {
            break;
        };
        // SAFETY: the adapter is alive for this call.
        let Ok(desc) = (unsafe { adapter.GetDesc1() }) else {
            continue;
        };
        if is_software(&desc) {
            continue;
        }
        out.push(Adapter {
            luid: luid_value(desc.AdapterLuid.HighPart as u32, desc.AdapterLuid.LowPart),
            name: utf16_until_nul(&desc.Description),
            vendor_id: desc.VendorId,
            device_id: desc.DeviceId,
            dedicated_total_bytes: desc.DedicatedVideoMemory as u64,
        });
    }
    out
}

fn is_software(desc: &DXGI_ADAPTER_DESC1) -> bool {
    desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0
        || (desc.VendorId == MICROSOFT_VENDOR_ID && desc.DedicatedVideoMemory == 0)
}

fn utf16_until_nul(buf: &[u16]) -> String {
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..len])
}

fn luid_value(high: u32, low: u32) -> u64 {
    (u64::from(high) << 32) | u64::from(low)
}

/// `…luid_0xHHHHHHHH_0xLLLLLLLL…` → LUID.
fn luid_in(instance: &str) -> Option<u64> {
    let rest = &instance[instance.find("luid_0x")? + "luid_0x".len()..];
    let (high, rest) = rest.split_once("_0x")?;
    let low = rest.get(..8)?;
    Some(luid_value(
        u32::from_str_radix(high, 16).ok()?,
        u32::from_str_radix(low, 16).ok()?,
    ))
}

/// `pid_1234_…` → 1234.
fn pid_in(instance: &str) -> Option<u32> {
    let digits = instance.strip_prefix("pid_")?;
    let end = digits.find('_').unwrap_or(digits.len());
    digits[..end].parse().ok()
}

/// `…_eng_3_engtype_…` → 3.
fn engine_in(instance: &str) -> Option<u32> {
    let rest = &instance[instance.find("_eng_")? + "_eng_".len()..];
    let end = rest.find('_').unwrap_or(rest.len());
    rest[..end].parse().ok()
}

fn build_telemetry(
    adapters: &[Adapter],
    adapter_mem: &[(String, i64)],
    process_mem: &[(String, i64)],
    engine_util: &[(String, f64)],
) -> GpuTelemetry {
    // Dedicated usage per adapter; linked adapters report one instance per phys.
    let mut used_by_luid: HashMap<u64, u64> = HashMap::new();
    for (instance, bytes) in adapter_mem {
        if let Some(luid) = luid_in(instance) {
            *used_by_luid.entry(luid).or_default() += (*bytes).max(0) as u64;
        }
    }

    // Engine load: summed over processes per engine; per process, its busiest engine.
    let mut engine_load: HashMap<(u64, u32), f64> = HashMap::new();
    let mut process_util: HashMap<(u32, u64), f64> = HashMap::new();
    for (instance, value) in engine_util {
        let (Some(pid), Some(luid), Some(engine)) =
            (pid_in(instance), luid_in(instance), engine_in(instance))
        else {
            continue;
        };
        *engine_load.entry((luid, engine)).or_default() += value;
        let entry = process_util.entry((pid, luid)).or_default();
        *entry = entry.max(*value);
    }
    let mut util_by_luid: HashMap<u64, f64> = HashMap::new();
    for ((luid, _), load) in &engine_load {
        let entry = util_by_luid.entry(*luid).or_default();
        *entry = entry.max(*load);
    }

    let mut process_bytes: HashMap<(u32, u64), u64> = HashMap::new();
    for (instance, bytes) in process_mem {
        if let (Some(pid), Some(luid)) = (pid_in(instance), luid_in(instance)) {
            *process_bytes.entry((pid, luid)).or_default() += (*bytes).max(0) as u64;
        }
    }

    let sampled_rates = !engine_util.is_empty();
    let sampled_memory = !adapter_mem.is_empty();
    let adapters = adapters
        .iter()
        .map(|a| AdapterUsage {
            luid: a.luid,
            name: a.name.clone(),
            vendor_id: a.vendor_id,
            device_id: a.device_id,
            dedicated_total_bytes: a.dedicated_total_bytes,
            dedicated_used_bytes: sampled_memory
                .then(|| used_by_luid.get(&a.luid).copied().unwrap_or(0)),
            utilization_percent: sampled_rates.then(|| {
                util_by_luid
                    .get(&a.luid)
                    .copied()
                    .unwrap_or(0.0)
                    .clamp(0.0, 100.0) as f32
            }),
        })
        .collect();

    let mut keys: Vec<(u32, u64)> = process_bytes.keys().copied().collect();
    keys.extend(process_util.keys().copied());
    keys.sort_unstable();
    keys.dedup();
    let processes = keys
        .into_iter()
        .map(|(pid, luid)| ProcessUsage {
            pid,
            luid,
            dedicated_bytes: process_bytes.get(&(pid, luid)).copied().unwrap_or(0),
            utilization_percent: process_util
                .get(&(pid, luid))
                .copied()
                .unwrap_or(0.0)
                .clamp(0.0, 100.0) as f32,
        })
        .filter(|p| p.dedicated_bytes > 0 || p.utilization_percent > 0.0)
        .collect();

    GpuTelemetry {
        adapters,
        processes,
        sampled_at: Instant::now(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LUID: &str = "luid_0x00000000_0x0000D1B7";

    /// Live check of the whole chain on this host: DXGI must name the card and
    /// PDH must fill its dedicated usage. A machine with no hardware adapter
    /// (a VM with the Microsoft rasterizer, which `enumerate_adapters` drops)
    /// has nothing to measure, so there the test only asserts that.
    #[test]
    fn samples_the_local_adapter() {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut latest = None;
        while Instant::now() < deadline {
            latest = super::snapshot();
            if latest
                .as_ref()
                .is_some_and(|t| t.adapters.iter().any(|a| a.dedicated_used_bytes.is_some()))
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        let telemetry = latest.expect("the sampler publishes a snapshot");
        if telemetry.adapters.is_empty() {
            assert!(enumerate_adapters().is_empty(), "no adapter to sample");
            return;
        }
        for adapter in &telemetry.adapters {
            assert!(!adapter.name.trim().is_empty(), "{adapter:?}");
            assert!(adapter.dedicated_total_bytes > 0, "{adapter:?}");
            let used = adapter
                .dedicated_used_bytes
                .expect("PDH reports dedicated usage");
            assert!(used <= adapter.dedicated_total_bytes, "{adapter:?}");
        }
    }

    fn adapter() -> Adapter {
        Adapter {
            luid: 0xD1B7,
            name: "NVIDIA GeForce RTX 2080 Ti".into(),
            vendor_id: 0x10DE,
            device_id: 0x1E07,
            dedicated_total_bytes: 11 << 30,
        }
    }

    #[test]
    fn parses_instance_names() {
        let engine = format!("pid_4424_{LUID}_phys_0_eng_3_engtype_Compute");
        assert_eq!(pid_in(&engine), Some(4424));
        assert_eq!(luid_in(&engine), Some(0xD1B7));
        assert_eq!(engine_in(&engine), Some(3));
        assert_eq!(pid_in(&format!("{LUID}_phys_0")), None);
        assert_eq!(
            luid_in("luid_0x00000001_0x00000002_phys_0"),
            Some((1 << 32) | 2)
        );
    }

    #[test]
    fn utilization_is_the_busiest_engine_summed_over_processes() {
        let util = vec![
            (format!("pid_10_{LUID}_phys_0_eng_0_engtype_3D"), 30.0),
            (format!("pid_20_{LUID}_phys_0_eng_0_engtype_3D"), 25.0),
            (format!("pid_20_{LUID}_phys_0_eng_3_engtype_Compute"), 40.0),
        ];
        let t = build_telemetry(&[adapter()], &[], &[], &util);
        assert_eq!(t.adapters[0].utilization_percent, Some(55.0));
        let p20 = t.processes.iter().find(|p| p.pid == 20).unwrap();
        assert_eq!(p20.utilization_percent, 40.0);
    }

    #[test]
    fn memory_is_attributed_per_process_and_adapter() {
        let adapter_mem = vec![(format!("{LUID}_phys_0"), 3 << 30)];
        let process_mem = vec![
            (format!("pid_4424_{LUID}_phys_0"), 2 << 30),
            (format!("pid_99_{LUID}_phys_0"), 0),
        ];
        let t = build_telemetry(&[adapter()], &adapter_mem, &process_mem, &[]);
        assert_eq!(t.adapters[0].dedicated_used_bytes, Some(3 << 30));
        assert_eq!(t.adapters[0].utilization_percent, None);
        assert_eq!(t.process_dedicated_bytes(4424), 2 << 30);
        assert!(t.processes.iter().all(|p| p.pid != 99));
    }
}
