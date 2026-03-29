// =============================================================================
// Plik: mesh/node_info_collector.rs
// Opis: Zbieranie informacji o lokalnym systemie — hostname, OS, CPU, RAM, GPU,
//       sieci. Cross-platform: Linux, macOS, Windows, iOS, Android.
//       GPU detection WYLACZNIE przez wgpu (Metal, Vulkan, DX12, GL) —
//       jedna metoda, zero duplikatow. Live metryki GPU: nvidia-smi (NVIDIA),
//       ioreg (macOS Apple Silicon).
//       Wyniki uzywane do wymiany NodeInfo z peerami przez QUIC.
// =============================================================================

use std::collections::HashMap;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use sysinfo::{Networks, System};
use std::net::IpAddr;
use tracing::warn;
use crate::mesh::peer_store::{NodeInfo, PeerGpuInfo, PeerContainerInfo, PeerNetworkInfo};

lazy_static::lazy_static! {
    /// Wspoldzielona instancja System — trzymana miedzy wywolaniami
    /// zeby CPU usage byl obliczany na podstawie delty od ostatniego refresha
    static ref SYS: Mutex<System> = {
        let mut sys = System::new_all();
        sys.refresh_all();
        Mutex::new(sys)
    };

    /// Cache wynikow GPU — unika spawnowania procesow co 500ms
    static ref GPU_CACHE: Mutex<(Instant, Vec<PeerGpuInfo>)> = {
        Mutex::new((Instant::now() - Duration::from_secs(10), vec![]))
    };

    /// Instancja Networks z sysinfo — cross-platform (Linux, macOS, Windows)
    static ref NETWORKS: Mutex<Networks> = {
        let mut nets = Networks::new_with_refreshed_list();
        nets.refresh(false);
        Mutex::new(nets)
    };

    /// Poprzedni odczyt sieci — do liczenia delt rx/tx per second
    static ref NET_PREV: Mutex<(Instant, HashMap<String, (u64, u64)>)> = {
        Mutex::new((Instant::now(), HashMap::new()))
    };

}

/// GPU z wgpu — enumerowane w tle. NIE blokuje startu mesh.
/// None = jeszcze nie gotowe lub nie zainicjalizowane, Some = wynik.
static WGPU_RESULT: std::sync::OnceLock<Mutex<Option<Vec<PeerGpuInfo>>>> = std::sync::OnceLock::new();

/// Startuje wgpu enumeration w tle — wywolaj raz przy starcie aplikacji.
/// Nie blokuje — wynik bedzie dostepny pozniej.
fn start_wgpu_enumeration() {
    let store = WGPU_RESULT.get_or_init(|| Mutex::new(None));
    // Jesli juz mamy wynik, nie startuj ponownie
    if store.lock().is_some() {
        return;
    }

    std::thread::spawn(|| {
        let result = std::panic::catch_unwind(|| {
            detect_gpus_wgpu()
        });
        let gpus = result.unwrap_or_else(|_| {
            warn!("wgpu enumerate_adapters panic");
            vec![]
        });
        if let Some(store) = WGPU_RESULT.get() {
            *store.lock() = Some(gpus);
        }
    });
}

/// Pobiera wynik wgpu enumeration (None jesli jeszcze nie gotowe)
fn get_wgpu_gpus() -> Option<Vec<PeerGpuInfo>> {
    WGPU_RESULT.get().and_then(|store| store.lock().clone())
}

/// Detekcja GPU WYLACZNIE przez wgpu — jedna metoda, zero duplikatow.
/// Dziala na: Metal (macOS/iOS), Vulkan (Linux/Android), DX12 (Windows), GL (fallback).
/// Deduplikacja po nazwie — wgpu moze zwrocic ten sam GPU na roznych backendach.
/// Timeout 3s — jesli wgpu wisi (headless server, brak drivera), zwraca pusty Vec.
fn detect_gpus_wgpu() -> Vec<PeerGpuInfo> {
    let (tx, rx) = std::sync::mpsc::channel();

    let handle = std::thread::spawn(move || {
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::all(),
            ..Default::default()
        });

        // Zbierz adaptery ze wszystkich backendow
        let all_adapters: Vec<wgpu::AdapterInfo> = instance.enumerate_adapters(wgpu::Backends::all())
            .into_iter()
            .map(|a| a.get_info())
            .filter(|info| info.device_type != wgpu::DeviceType::Cpu)
            .collect();

        // Wybierz preferowany backend (Vulkan > Metal > DX12 > GL)
        // Kazdy fizyczny GPU jest enumerowany raz per backend,
        // wiec bierzemy adaptery z jednego backendu — wtedy count = prawdziwa liczba GPU.
        let preferred_backend = [
            wgpu::Backend::Vulkan,
            wgpu::Backend::Metal,
            wgpu::Backend::Dx12,
            wgpu::Backend::Gl,
        ].iter().find(|&&b| all_adapters.iter().any(|a| a.backend == b)).copied();

        let gpus: Vec<PeerGpuInfo> = if let Some(backend) = preferred_backend {
            all_adapters.iter()
                .filter(|a| a.backend == backend)
                .map(|info| {
                    let clean_name = info.name.split('/').next().unwrap_or(&info.name).trim().to_string();
                    PeerGpuInfo {
                        name: clean_name,
                        vram_total_mb: 0,
                        vram_used_mb: 0,
                        usage_percent: 0.0,
                        temperature_c: 0,
                    }
                })
                .collect()
        } else {
            vec![]
        };

        // Apple Silicon — VRAM = RAM (unified memory)
        #[cfg(target_os = "macos")]
        {
            let total_ram_mb = {
                let sys = SYS.lock();
                sys.total_memory() / (1024 * 1024)
            };
            for gpu in &mut gpus {
                if gpu.vram_total_mb == 0 {
                    gpu.vram_total_mb = total_ram_mb;
                }
            }
        }

        let _ = tx.send(gpus);
    });

    // Czekaj max 3s — jesli wgpu wisi (headless server, brak drivera), odpuszczamy
    match rx.recv_timeout(Duration::from_secs(3)) {
        Ok(gpus) => {
            let _ = handle.join();
            gpus
        }
        Err(_) => {
            warn!("wgpu enumerate_adapters timeout (3s) — GPU detection niedostepne");
            vec![]
        }
    }
}

/// Wykrywa platforme na ktorej dziala nod
pub fn detect_platform() -> String {
    if cfg!(target_os = "linux") {
        // Android tez raportuje linux — sprawdz obecnosc /system/build.prop
        if std::path::Path::new("/system/build.prop").exists() {
            return "android".to_string();
        }
        "linux".to_string()
    } else if cfg!(target_os = "macos") {
        "macos".to_string()
    } else if cfg!(target_os = "windows") {
        "windows".to_string()
    } else if cfg!(target_os = "ios") {
        "ios".to_string()
    } else if cfg!(target_os = "android") {
        "android".to_string()
    } else {
        "unknown".to_string()
    }
}

/// Zbiera statyczne informacje o nodzie (hostname, OS, CPU count, RAM total, GPU)
/// NIE blokuje na wgpu — jesli wgpu jeszcze nie skonczyl, GPU bedzie puste
/// i uzupelni sie przy nastepnym heartbeat (co 500ms).
pub fn collect_node_info(node_id: &str) -> NodeInfo {
    // Startuj wgpu enumeration w tle (fire-and-forget, nie blokuje)
    start_wgpu_enumeration();

    // Zbierz dane systemowe — KROTKO trzymaj lock na SYS, potem zwolnij
    #[allow(unused_mut)] // mut potrzebny na iOS (FFI device name) i Android
    let (hostname, os_info, cpu_count, ram_total_mb) = {
        let sys = SYS.lock();
        let mut hostname = System::host_name().unwrap_or_else(|| "unknown".to_string());

        // iOS: System::host_name() zwraca "localhost" — probuj pobrac nazwe urzadzenia przez FFI
        #[cfg(target_os = "ios")]
        {
            extern "C" { fn tentaflow_get_device_name() -> *mut std::ffi::c_char; }
            let ptr = unsafe { tentaflow_get_device_name() };
            if !ptr.is_null() {
                let name = unsafe { std::ffi::CStr::from_ptr(ptr) }.to_string_lossy().to_string();
                extern "C" { fn free(ptr: *mut std::ffi::c_void); }
                unsafe { free(ptr as *mut std::ffi::c_void); }
                if !name.is_empty() {
                    hostname = name;
                }
            }
        }

        #[allow(unused_mut)] // mut potrzebny na iOS/Android
        let mut os_name = System::name().unwrap_or_else(|| "unknown".to_string());
        let os_version = System::os_version().unwrap_or_default();
        let arch = std::env::consts::ARCH;

        // System::name() zwraca "Darwin" zarowno na macOS jak i iOS —
        // na iOS nadpisujemy na "iOS" zeby parse_platform poprawnie rozpoznal platforme
        #[cfg(target_os = "ios")]
        { os_name = "iOS".to_string(); }

        // Analogicznie dla Androida
        #[cfg(target_os = "android")]
        { os_name = "Android".to_string(); }

        let os_info = format!("{} {} ({})", os_name, os_version, arch);
        let cpu_count = sys.cpus().len() as u32;
        let ram_total_mb = sys.total_memory() / (1024 * 1024);
        (hostname, os_info, cpu_count, ram_total_mb)
        // SYS lock zwolniony tutaj — PRZED GPU detection
    };

    // GPU — uzyj cache jesli dostepny, jesli nie — puste (wgpu w tle)
    let gpu_info = get_wgpu_gpus().unwrap_or_default();

    NodeInfo {
        node_id: node_id.to_string(),
        hostname,
        os_info,
        cpu_count,
        ram_total_mb,
        gpu_info,
    }
}

/// Biezace metryki systemu — CPU usage, RAM used, GPU usage
pub struct CurrentMetrics {
    pub cpu_usage_percent: f32,
    pub ram_used_mb: u64,
    pub gpus: Vec<PeerGpuInfo>,
    pub containers: Vec<PeerContainerInfo>,
    pub networks: Vec<PeerNetworkInfo>,
}

/// Szybkie metryki (CPU/RAM/GPU/sieci) — bezpieczne do wywolywania co 500ms
pub fn collect_fast_metrics() -> CurrentMetrics {
    // KROTKO lockuj SYS — tylko na refresh CPU/RAM, potem zwolnij
    let (cpu_usage, ram_used_mb) = {
        let mut sys = SYS.lock();
        sys.refresh_cpu_usage();
        sys.refresh_memory();
        (sys.global_cpu_usage(), sys.used_memory() / (1024 * 1024))
    };

    let gpus = detect_gpus_cached();
    let networks = detect_networks();

    CurrentMetrics {
        cpu_usage_percent: cpu_usage,
        ram_used_mb,
        gpus,
        containers: vec![],
        networks,
    }
}

/// Wolne metryki — kontenery Docker (trwa ~1-2s, wywolywac co 5s)
pub fn collect_docker_containers() -> Vec<PeerContainerInfo> {
    detect_containers()
}

/// Sprawdza dostepnosc i wersje Docker serwera.
/// Zwraca (docker_available, docker_version).
pub fn collect_docker_info() -> (bool, String) {
    let available = std::process::Command::new("docker")
        .arg("info")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    let version = if available {
        std::process::Command::new("docker")
            .args(["version", "--format", "{{.Server.Version}}"])
            .output()
            .ok()
            .and_then(|o| {
                if o.status.success() {
                    Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
                } else {
                    None
                }
            })
            .unwrap_or_default()
    } else {
        String::new()
    };

    (available, version)
}

// =============================================================================
// GPU — wgpu base + live metryki (nvidia-smi, ioreg)
// =============================================================================

/// Wykrywanie GPU z cache (co najwyzej raz na 2s) — wgpu base + live enrichment
fn detect_gpus_cached() -> Vec<PeerGpuInfo> {
    {
        let cache = GPU_CACHE.lock();
        if cache.0.elapsed() < Duration::from_secs(2) {
            return cache.1.clone();
        }
    }

    let result = detect_gpus_with_live_metrics();

    {
        let mut cache = GPU_CACHE.lock();
        *cache = (Instant::now(), result.clone());
    }

    result
}

/// Bazowa lista GPU z wgpu + live metryki z platform-specific narzedzi.
/// wgpu daje nazwy GPU (bez duplikatow), nvidia-smi/ioreg daja live metryki.
fn detect_gpus_with_live_metrics() -> Vec<PeerGpuInfo> {
    let mut gpus = get_wgpu_gpus().unwrap_or_default();

    if gpus.is_empty() {
        return gpus;
    }

    // Live metryki — nvidia-smi (NVIDIA GPU)
    enrich_nvidia_live(&mut gpus);

    // Live metryki — ioreg (macOS Apple Silicon)
    #[cfg(target_os = "macos")]
    enrich_macos_live(&mut gpus);

    gpus
}

/// NVIDIA live metryki — nvidia-smi CLI. Wzbogaca istniejace GPU o VRAM, usage, temp.
fn enrich_nvidia_live(gpus: &mut [PeerGpuInfo]) {
    let output = match std::process::Command::new("nvidia-smi")
        .args([
            "--query-gpu=name,memory.total,memory.used,utilization.gpu,temperature.gpu",
            "--format=csv,noheader,nounits",
        ])
        .output()
    {
        Ok(o) if o.status.success() => o,
        _ => return,
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let nvidia_entries: Vec<(&str, u64, u64, f32, u32)> = stdout
        .lines()
        .filter_map(|line| {
            let parts: Vec<&str> = line.split(',').map(|s| s.trim()).collect();
            if parts.len() >= 5 {
                Some((
                    parts[0],
                    parts[1].parse().unwrap_or(0),
                    parts[2].parse().unwrap_or(0),
                    parts[3].parse().unwrap_or(0.0),
                    parts[4].parse().unwrap_or(0),
                ))
            } else {
                None
            }
        })
        .collect();

    for gpu in gpus.iter_mut() {
        if let Some(nv) = nvidia_entries.iter().find(|(name, ..)| {
            gpu.name.contains(name) || name.contains(&gpu.name.as_str())
        }) {
            gpu.vram_total_mb = nv.1;
            gpu.vram_used_mb = nv.2;
            gpu.usage_percent = nv.3;
            gpu.temperature_c = nv.4;
        }
    }
}

/// macOS live metryki — ioreg dla Apple GPU (usage %, VRAM used)
#[cfg(target_os = "macos")]
fn enrich_macos_live(gpus: &mut [PeerGpuInfo]) {
    if gpus.is_empty() {
        return;
    }

    // Apple Silicon unified memory — VRAM total = RAM total
    let total_ram_mb = {
        let sys = SYS.lock();
        sys.total_memory() / (1024 * 1024)
    };

    for gpu in gpus.iter_mut() {
        if gpu.vram_total_mb == 0 {
            gpu.vram_total_mb = total_ram_mb;
        }
    }

    // ioreg — live metryki z IOAccelerator
    let output = match std::process::Command::new("ioreg")
        .args(["-r", "-d", "1", "-c", "IOAccelerator"])
        .output()
    {
        Ok(o) if o.status.success() => o,
        _ => return,
    };

    let stdout = String::from_utf8_lossy(&output.stdout);

    for line in stdout.lines() {
        let trimmed = line.trim();
        if !trimmed.contains("PerformanceStatistics") {
            continue;
        }

        let dict_content = match (trimmed.find('{'), trimmed.rfind('}')) {
            (Some(start), Some(end)) if start < end => &trimmed[start + 1..end],
            _ => continue,
        };

        let gpu = match gpus.first_mut() {
            Some(g) => g,
            None => return,
        };

        for pair in dict_content.split(',') {
            let pair = pair.trim();
            let parts: Vec<&str> = pair.rsplitn(2, '=').collect();
            if parts.len() != 2 {
                continue;
            }
            let value_str = parts[0].trim();
            let key = parts[1].trim().trim_matches('"');

            match key {
                "Device Utilization %" => {
                    if let Ok(v) = value_str.parse::<f32>() {
                        gpu.usage_percent = v;
                    }
                }
                "Renderer Utilization %" => {
                    if gpu.usage_percent == 0.0 {
                        if let Ok(v) = value_str.parse::<f32>() {
                            gpu.usage_percent = v;
                        }
                    }
                }
                "In use system memory" => {
                    if let Ok(v) = value_str.parse::<u64>() {
                        gpu.vram_used_mb = v / (1024 * 1024);
                    }
                }
                _ => {}
            }
        }

        break;
    }
}

// =============================================================================
// Docker containers
// =============================================================================

fn detect_containers() -> Vec<PeerContainerInfo> {
    let ps_output = match std::process::Command::new("docker")
        .args(["ps", "-a", "--format", "{{.ID}}\t{{.Names}}\t{{.Image}}\t{{.Status}}"])
        .output()
    {
        Ok(o) if o.status.success() => o,
        _ => return vec![],
    };

    let stats_output = std::process::Command::new("docker")
        .args(["stats", "--no-stream", "--format", "{{.ID}}\t{{.CPUPerc}}\t{{.MemUsage}}"])
        .output()
        .ok();

    let mut stats_map: HashMap<String, (f32, u64, u64)> = HashMap::new();
    if let Some(ref output) = stats_output {
        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            for line in stdout.lines() {
                let parts: Vec<&str> = line.split('\t').collect();
                if parts.len() >= 3 {
                    let id = parts[0].to_string();
                    let cpu_str = parts[1].trim_end_matches('%');
                    let cpu_percent: f32 = cpu_str.parse().unwrap_or(0.0);
                    let (memory_mb, memory_limit_mb) = parse_mem_usage(parts[2]);
                    stats_map.insert(id, (cpu_percent, memory_mb, memory_limit_mb));
                }
            }
        }
    }

    let ps_stdout = String::from_utf8_lossy(&ps_output.stdout);
    ps_stdout
        .lines()
        .filter_map(|line| {
            let parts: Vec<&str> = line.split('\t').collect();
            if parts.len() >= 4 {
                let id = parts[0].to_string();
                let (cpu_percent, memory_mb, memory_limit_mb) = stats_map
                    .get(&id)
                    .copied()
                    .unwrap_or((0.0, 0, 0));

                Some(PeerContainerInfo {
                    id,
                    name: parts[1].to_string(),
                    image: parts[2].to_string(),
                    status: parts[3].to_string(),
                    cpu_percent,
                    memory_mb,
                    memory_limit_mb,
                })
            } else {
                None
            }
        })
        .collect()
}

fn parse_mem_usage(s: &str) -> (u64, u64) {
    let parts: Vec<&str> = s.split('/').collect();
    if parts.len() != 2 {
        return (0, 0);
    }
    (parse_mem_value(parts[0].trim()), parse_mem_value(parts[1].trim()))
}

fn parse_mem_value(s: &str) -> u64 {
    let s = s.trim();
    if let Some(v) = s.strip_suffix("GiB") {
        (v.trim().parse::<f64>().unwrap_or(0.0) * 1024.0) as u64
    } else if let Some(v) = s.strip_suffix("GB") {
        (v.trim().parse::<f64>().unwrap_or(0.0) * 1024.0) as u64
    } else if let Some(v) = s.strip_suffix("MiB") {
        v.trim().parse::<f64>().unwrap_or(0.0) as u64
    } else if let Some(v) = s.strip_suffix("MB") {
        v.trim().parse::<f64>().unwrap_or(0.0) as u64
    } else if let Some(v) = s.strip_suffix("KiB") {
        (v.trim().parse::<f64>().unwrap_or(0.0) / 1024.0) as u64
    } else if let Some(v) = s.strip_suffix("KB") {
        (v.trim().parse::<f64>().unwrap_or(0.0) / 1024.0) as u64
    } else if let Some(v) = s.strip_suffix("B") {
        (v.trim().parse::<f64>().unwrap_or(0.0) / (1024.0 * 1024.0)) as u64
    } else {
        0
    }
}

// =============================================================================
// Sieci — cross-platform przez sysinfo::Networks
// =============================================================================

fn detect_networks() -> Vec<PeerNetworkInfo> {
    let mut nets = NETWORKS.lock();
    nets.refresh(false);

    let now = Instant::now();
    let mut prev = NET_PREV.lock();
    let elapsed_secs = prev.0.elapsed().as_secs_f64();

    let mut current_values: HashMap<String, (u64, u64)> = HashMap::new();

    let results: Vec<PeerNetworkInfo> = nets
        .iter()
        .filter_map(|(name, data)| {
            if name == "lo" || name == "lo0" {
                return None;
            }

            let rx = data.total_received();
            let tx = data.total_transmitted();

            if rx == 0 && tx == 0 {
                return None;
            }

            current_values.insert(name.to_string(), (rx, tx));

            let (rx_per_sec, tx_per_sec) = if elapsed_secs > 0.01 {
                if let Some(&(prev_rx, prev_tx)) = prev.1.get(name.as_str()) {
                    let drx = rx.saturating_sub(prev_rx) as f64 / elapsed_secs;
                    let dtx = tx.saturating_sub(prev_tx) as f64 / elapsed_secs;
                    (drx as u64, dtx as u64)
                } else {
                    (0, 0)
                }
            } else {
                (0, 0)
            };

            Some(PeerNetworkInfo {
                name: name.to_string(),
                rx_bytes: rx,
                tx_bytes: tx,
                rx_bytes_per_sec: rx_per_sec,
                tx_bytes_per_sec: tx_per_sec,
            })
        })
        .collect();

    prev.0 = now;
    prev.1 = current_values;

    results
}

// =============================================================================
// Lokalne adresy IP — do uzupelniania danych lokalnego noda
// =============================================================================

/// Pobiera lokalne adresy IP z interfejsow sieciowych (bez loopback)
pub fn collect_local_addresses() -> Vec<IpAddr> {
    let mut nets = NETWORKS.lock();
    nets.refresh(true);

    let mut addrs: Vec<IpAddr> = Vec::new();
    for (name, data) in nets.iter() {
        if name == "lo" || name == "lo0" {
            continue;
        }
        for net in data.ip_networks() {
            let ip = net.addr;
            if !ip.is_loopback() {
                addrs.push(ip);
            }
        }
    }
    addrs
}

/// Nazwa dystrybucji OS (np. "Arch Linux", "Ubuntu 22.04", "macOS 15.2")
pub fn collect_os_distro() -> String {
    let os_name = System::name().unwrap_or_else(|| "unknown".to_string());
    let os_version = System::os_version().unwrap_or_default();
    let distribution = System::distribution_id();

    // Na Linuxie System::name() zwraca "Linux" — dystrybucja jest w distribution_id()
    #[cfg(target_os = "linux")]
    {
        let long_os_version = System::long_os_version().unwrap_or_default();
        if !long_os_version.is_empty() {
            return long_os_version;
        }
        if !distribution.is_empty() && distribution != "linux" {
            return format!("{} {}", distribution, os_version).trim().to_string();
        }
    }

    let _ = distribution;

    if os_version.is_empty() {
        os_name
    } else {
        format!("{} {}", os_name, os_version)
    }
}
