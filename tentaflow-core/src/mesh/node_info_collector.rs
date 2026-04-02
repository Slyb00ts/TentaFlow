// =============================================================================
// Plik: mesh/node_info_collector.rs
// Opis: Zbieranie informacji o lokalnym systemie — hostname, OS, CPU, RAM, GPU,
//       sieci. Cross-platform: Linux, macOS, Windows, iOS, Android.
//       GPU detection WYLACZNIE przez wgpu (Metal, Vulkan, DX12, GL) —
//       jedna metoda, zero duplikatow. Live metryki GPU: nvidia-smi (NVIDIA),
//       ioreg (macOS Apple Silicon), amd-smi/sysfs (AMD), sysfs hwmon (Intel),
//       sysfs kgsl/mali (Android), Metal (iOS).
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
                        power_draw_w: None,
                        power_limit_w: None,
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
    pub cpu_temperature_c: Option<f32>,
    pub swap_total_mb: u64,
    pub swap_used_mb: u64,
}

/// Szybkie metryki (CPU/RAM/GPU/sieci) — bezpieczne do wywolywania co 500ms
pub fn collect_fast_metrics() -> CurrentMetrics {
    // KROTKO lockuj SYS — tylko na refresh CPU/RAM/swap, potem zwolnij
    let (cpu_usage, ram_used_mb, swap_total_mb, swap_used_mb) = {
        let mut sys = SYS.lock();
        sys.refresh_cpu_usage();
        sys.refresh_memory();
        (
            sys.global_cpu_usage(),
            sys.used_memory() / (1024 * 1024),
            sys.total_swap() / (1024 * 1024),
            sys.used_swap() / (1024 * 1024),
        )
    };

    let cpu_temperature_c = detect_cpu_temperature();
    let gpus = detect_gpus_cached();
    let networks = detect_networks();

    CurrentMetrics {
        cpu_usage_percent: cpu_usage,
        ram_used_mb,
        gpus,
        containers: vec![],
        networks,
        cpu_temperature_c,
        swap_total_mb,
        swap_used_mb,
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
/// Kolejnosc enrichmentu: NVIDIA -> AMD -> Intel -> macOS -> Android -> iOS.
/// Kazda funkcja wzbogaca tylko GPU ktore jeszcze nie maja metryk (vram_total_mb == 0 && usage_percent == 0.0).
fn detect_gpus_with_live_metrics() -> Vec<PeerGpuInfo> {
    let mut gpus = get_wgpu_gpus().unwrap_or_default();

    if gpus.is_empty() {
        return gpus;
    }

    // NVIDIA — nvidia-smi (Linux, Windows)
    enrich_nvidia_live(&mut gpus);

    // AMD — amd-smi / sysfs (Linux)
    #[cfg(target_os = "linux")]
    enrich_amd_live(&mut gpus);

    // Intel — sysfs hwmon (Linux)
    #[cfg(target_os = "linux")]
    enrich_intel_live(&mut gpus);

    // macOS — ioreg (Apple Silicon)
    #[cfg(target_os = "macos")]
    enrich_macos_live(&mut gpus);

    // Android — sysfs kgsl/mali/thermal
    #[cfg(target_os = "linux")]
    if detect_platform() == "android" {
        enrich_android_live(&mut gpus);
    }

    // iOS — unified memory
    #[cfg(target_os = "ios")]
    enrich_ios_live(&mut gpus);

    gpus
}

/// NVIDIA live metryki — nvidia-smi CLI. Wzbogaca istniejace GPU o VRAM, usage, temp.
fn enrich_nvidia_live(gpus: &mut [PeerGpuInfo]) {
    let output = match std::process::Command::new("nvidia-smi")
        .args([
            "--query-gpu=name,memory.total,memory.used,utilization.gpu,temperature.gpu,power.draw,power.limit",
            "--format=csv,noheader,nounits",
        ])
        .output()
    {
        Ok(o) if o.status.success() => o,
        _ => return,
    };

    let stdout = String::from_utf8_lossy(&output.stdout);

    struct NvidiaEntry<'a> {
        name: &'a str,
        vram_total: u64,
        vram_used: u64,
        usage: f32,
        temp: u32,
        power_draw: Option<f32>,
        power_limit: Option<f32>,
    }

    let nvidia_entries: Vec<NvidiaEntry> = stdout
        .lines()
        .filter_map(|line| {
            let parts: Vec<&str> = line.split(',').map(|s| s.trim()).collect();
            if parts.len() >= 5 {
                Some(NvidiaEntry {
                    name: parts[0],
                    vram_total: parts[1].parse().unwrap_or(0),
                    vram_used: parts[2].parse().unwrap_or(0),
                    usage: parts[3].parse().unwrap_or(0.0),
                    temp: parts[4].parse().unwrap_or(0),
                    power_draw: parts.get(5).and_then(|s| s.parse().ok()),
                    power_limit: parts.get(6).and_then(|s| s.parse().ok()),
                })
            } else {
                None
            }
        })
        .collect();

    // RAM systemowy — potrzebny dla unified memory GPU (GB10, przyszle Blackwell)
    let system_ram_mb = {
        let sys = SYS.lock();
        sys.total_memory() / (1024 * 1024)
    };

    // Dopasowanie po indeksie — nvidia-smi zwraca GPU w kolejności 0, 1, 2...
    for (i, gpu) in gpus.iter_mut().enumerate() {
        if let Some(nv) = nvidia_entries.get(i) {
            gpu.vram_total_mb = nv.vram_total;
            gpu.vram_used_mb = nv.vram_used;
            gpu.usage_percent = nv.usage;
            gpu.temperature_c = nv.temp;
            gpu.power_draw_w = nv.power_draw;
            gpu.power_limit_w = nv.power_limit;

            // Unified memory GPU (np. DGX Spark GB10) — nvidia-smi zwraca [N/A] dla VRAM
            // Wtedy VRAM = RAM systemowy (wspoldzielona pamiec CPU/GPU)
            if gpu.vram_total_mb == 0 {
                gpu.vram_total_mb = system_ram_mb;
            }
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
// GPU enrichment — AMD (Linux: amd-smi / sysfs)
// =============================================================================

/// Pomocnik do odczytu wartosci z sysfs — zwraca None jesli plik nie istnieje lub parse sie nie uda
#[cfg(target_os = "linux")]
fn read_sysfs_value<T: std::str::FromStr>(path: &str) -> Option<T> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

/// AMD live metryki — amd-smi CLI (modern) z fallbackiem na sysfs.
/// Wzbogaca tylko GPU ktore jeszcze nie maja metryk (nie wzbogacone przez nvidia-smi).
#[cfg(target_os = "linux")]
fn enrich_amd_live(gpus: &mut [PeerGpuInfo]) {
    // Zbierz indeksy GPU AMD ktore jeszcze nie maja metryk
    let amd_indices: Vec<usize> = gpus
        .iter()
        .enumerate()
        .filter(|(_, g)| g.vram_total_mb == 0 && g.usage_percent == 0.0)
        .filter(|(_, g)| {
            let name = g.name.to_lowercase();
            name.contains("amd") || name.contains("radeon") || name.contains("navi") || name.contains("vega")
        })
        .map(|(i, _)| i)
        .collect();

    if amd_indices.is_empty() {
        return;
    }

    // Probuj amd-smi (modern CLI)
    if enrich_amd_from_amdsmi(gpus, &amd_indices) {
        return;
    }

    // Fallback — sysfs
    enrich_amd_from_sysfs(gpus, &amd_indices);
}

/// Probuje wzbogacic AMD GPU przez amd-smi CLI. Zwraca true jesli udalo sie odczytac dane.
#[cfg(target_os = "linux")]
fn enrich_amd_from_amdsmi(gpus: &mut [PeerGpuInfo], amd_indices: &[usize]) -> bool {
    let output = match std::process::Command::new("amd-smi")
        .args(["monitor", "--json"])
        .output()
    {
        Ok(o) if o.status.success() => o,
        _ => return false,
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value = match serde_json::from_str(&stdout) {
        Ok(v) => v,
        Err(e) => {
            warn!("amd-smi JSON parse error: {}", e);
            return false;
        }
    };

    // amd-smi monitor --json zwraca tablice GPU
    let gpu_array = match json.as_array() {
        Some(arr) => arr,
        None => return false,
    };

    for (entry_idx, gpu_idx) in amd_indices.iter().enumerate() {
        let entry = match gpu_array.get(entry_idx) {
            Some(e) => e,
            None => break,
        };

        let gpu = &mut gpus[*gpu_idx];

        if let Some(usage) = entry.get("GFX_ACTIVITY").and_then(|v| v.as_f64()) {
            gpu.usage_percent = usage as f32;
        }
        if let Some(vram_total) = entry.get("VRAM_TOTAL").and_then(|v| v.as_u64()) {
            gpu.vram_total_mb = vram_total;
        }
        if let Some(vram_used) = entry.get("VRAM_USED").and_then(|v| v.as_u64()) {
            gpu.vram_used_mb = vram_used;
        }
        if let Some(temp) = entry.get("TEMPERATURE_HOTSPOT").and_then(|v| v.as_u64())
            .or_else(|| entry.get("TEMPERATURE_EDGE").and_then(|v| v.as_u64()))
        {
            gpu.temperature_c = temp as u32;
        }
        if let Some(power) = entry.get("POWER").and_then(|v| v.as_f64()) {
            gpu.power_draw_w = Some(power as f32);
        }
    }

    true
}

/// Wzbogaca AMD GPU przez sysfs — /sys/class/drm/card*/device/
#[cfg(target_os = "linux")]
fn enrich_amd_from_sysfs(gpus: &mut [PeerGpuInfo], amd_indices: &[usize]) {
    // Znajdz karty DRM z vendorem AMD (0x1002)
    let mut amd_cards: Vec<String> = Vec::new();
    let drm_dir = match std::fs::read_dir("/sys/class/drm") {
        Ok(d) => d,
        Err(_) => return,
    };

    for entry in drm_dir.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        // Tylko card0, card1, ... (nie card0-HDMI-A-1 itp.)
        if !name.starts_with("card") || name.contains('-') {
            continue;
        }
        let device_path = format!("/sys/class/drm/{}/device", name);
        let vendor_path = format!("{}/vendor", device_path);
        if let Some(vendor) = read_sysfs_value::<String>(&vendor_path) {
            if vendor.trim() == "0x1002" {
                amd_cards.push(device_path);
            }
        }
    }

    amd_cards.sort();

    // Dopasowanie po indeksie — sysfs karty AMD w kolejnosci odpowiadaja GPU AMD w liscie
    for (card_idx, gpu_idx) in amd_indices.iter().enumerate() {
        let device_path = match amd_cards.get(card_idx) {
            Some(p) => p,
            None => break,
        };

        let gpu = &mut gpus[*gpu_idx];

        // Usage %
        if let Some(usage) = read_sysfs_value::<f32>(&format!("{}/gpu_busy_percent", device_path)) {
            gpu.usage_percent = usage;
        }

        // VRAM total/used (bajty -> MB)
        if let Some(vram_total) = read_sysfs_value::<u64>(&format!("{}/mem_info_vram_total", device_path)) {
            gpu.vram_total_mb = vram_total / (1024 * 1024);
        }
        if let Some(vram_used) = read_sysfs_value::<u64>(&format!("{}/mem_info_vram_used", device_path)) {
            gpu.vram_used_mb = vram_used / (1024 * 1024);
        }

        // Temperatura — hwmon/hwmon*/temp1_input (mili-stopnie Celsjusza)
        if let Some(temp) = find_hwmon_value::<u64>(device_path, "temp1_input") {
            gpu.temperature_c = (temp / 1000) as u32;
        }

        // Moc — hwmon/hwmon*/power1_average (mikrowaty)
        if let Some(power_uw) = find_hwmon_value::<u64>(device_path, "power1_average") {
            gpu.power_draw_w = Some(power_uw as f32 / 1_000_000.0);
        }
    }
}

/// Szuka wartosci w hwmon danego urzadzenia — iteruje hwmon*/plik
#[cfg(target_os = "linux")]
fn find_hwmon_value<T: std::str::FromStr>(device_path: &str, filename: &str) -> Option<T> {
    let hwmon_dir = format!("{}/hwmon", device_path);
    let entries = std::fs::read_dir(&hwmon_dir).ok()?;
    for entry in entries.flatten() {
        let path = format!("{}/{}", entry.path().display(), filename);
        if let Some(val) = read_sysfs_value::<T>(&path) {
            return Some(val);
        }
    }
    None
}

// =============================================================================
// GPU enrichment — Intel (Linux: sysfs hwmon)
// =============================================================================

/// Intel live metryki — sysfs hwmon. Intel GPU rzadko udostepnia usage% przez sysfs,
/// wiec wzbogacamy tylko temperature i moc (jesli dostepne).
#[cfg(target_os = "linux")]
fn enrich_intel_live(gpus: &mut [PeerGpuInfo]) {
    // Zbierz indeksy GPU Intel ktore jeszcze nie maja metryk
    let intel_indices: Vec<usize> = gpus
        .iter()
        .enumerate()
        .filter(|(_, g)| g.vram_total_mb == 0 && g.usage_percent == 0.0)
        .filter(|(_, g)| {
            let name = g.name.to_lowercase();
            name.contains("intel") || name.contains("iris") || name.contains("uhd") || name.contains("arc")
        })
        .map(|(i, _)| i)
        .collect();

    if intel_indices.is_empty() {
        return;
    }

    // Znajdz karty DRM z vendorem Intel (0x8086)
    let mut intel_cards: Vec<String> = Vec::new();
    let drm_dir = match std::fs::read_dir("/sys/class/drm") {
        Ok(d) => d,
        Err(_) => return,
    };

    for entry in drm_dir.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.starts_with("card") || name.contains('-') {
            continue;
        }
        let device_path = format!("/sys/class/drm/{}/device", name);
        let vendor_path = format!("{}/vendor", device_path);
        if let Some(vendor) = read_sysfs_value::<String>(&vendor_path) {
            if vendor.trim() == "0x8086" {
                intel_cards.push(device_path);
            }
        }
    }

    intel_cards.sort();

    for (card_idx, gpu_idx) in intel_indices.iter().enumerate() {
        let device_path = match intel_cards.get(card_idx) {
            Some(p) => p,
            None => break,
        };

        let gpu = &mut gpus[*gpu_idx];

        // Temperatura — hwmon/hwmon*/temp1_input (mili-stopnie)
        if let Some(temp) = find_hwmon_value::<u64>(device_path, "temp1_input") {
            gpu.temperature_c = (temp / 1000) as u32;
        }

        // Moc — hwmon/hwmon*/power1_average (mikrowaty, jesli eksponowane)
        if let Some(power_uw) = find_hwmon_value::<u64>(device_path, "power1_average") {
            gpu.power_draw_w = Some(power_uw as f32 / 1_000_000.0);
        }
    }
}

// =============================================================================
// GPU enrichment — Android (sysfs: Adreno kgsl, Mali, thermal)
// =============================================================================

/// Android live metryki — sysfs dla Qualcomm Adreno i ARM Mali.
/// Sciezki sysfs czesto blokowane przez SELinux na nowszych wersjach Androida.
#[cfg(target_os = "linux")]
fn enrich_android_live(gpus: &mut [PeerGpuInfo]) {
    // Wzbogacaj tylko GPU bez metryk
    let unenriched: Vec<usize> = gpus
        .iter()
        .enumerate()
        .filter(|(_, g)| g.vram_total_mb == 0 && g.usage_percent == 0.0)
        .map(|(i, _)| i)
        .collect();

    if unenriched.is_empty() {
        return;
    }

    // Adreno (Qualcomm) — /sys/class/kgsl/kgsl-3d0/
    let adreno_path = "/sys/class/kgsl/kgsl-3d0";
    let has_adreno = std::path::Path::new(adreno_path).exists();

    if has_adreno {
        if let Some(&gpu_idx) = unenriched.first() {
            let gpu = &mut gpus[gpu_idx];

            // GPU busy — format "X Y", usage = X/Y * 100
            if let Ok(content) = std::fs::read_to_string(format!("{}/gpubusy", adreno_path)) {
                let parts: Vec<&str> = content.trim().split_whitespace().collect();
                if parts.len() >= 2 {
                    if let (Ok(busy), Ok(total)) = (
                        parts[0].parse::<f64>(),
                        parts[1].parse::<f64>(),
                    ) {
                        if total > 0.0 {
                            gpu.usage_percent = (busy / total * 100.0) as f32;
                        }
                    }
                }
            }
        }
    }

    // Mali (ARM) — rozne sciezki w zaleznosci od SoC
    if !has_adreno {
        // Probuj mali utilization
        let mali_paths = [
            "/sys/kernel/gpu/gpu_clock",
            "/sys/devices/platform/mali.0/utilization",
        ];

        if let Some(&gpu_idx) = unenriched.first() {
            let gpu = &mut gpus[gpu_idx];

            for path in &mali_paths {
                if let Ok(content) = std::fs::read_to_string(path) {
                    if path.contains("utilization") {
                        if let Ok(usage) = content.trim().parse::<f32>() {
                            gpu.usage_percent = usage;
                        }
                    }
                    break;
                }
            }
        }
    }

    // Temperatura GPU — skanuj thermal_zone szukajac typu "gpu"
    let gpu_temp = detect_android_gpu_temperature();
    if let Some(temp) = gpu_temp {
        for &gpu_idx in &unenriched {
            if gpus[gpu_idx].temperature_c == 0 {
                gpus[gpu_idx].temperature_c = temp;
            }
        }
    }
}

/// Szuka temperatury GPU w thermal_zone na Androidzie
#[cfg(target_os = "linux")]
fn detect_android_gpu_temperature() -> Option<u32> {
    let thermal_dir = match std::fs::read_dir("/sys/class/thermal") {
        Ok(d) => d,
        Err(_) => return None,
    };

    for entry in thermal_dir.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.starts_with("thermal_zone") {
            continue;
        }

        let type_path = format!("/sys/class/thermal/{}/type", name);
        if let Ok(zone_type) = std::fs::read_to_string(&type_path) {
            if zone_type.trim().to_lowercase().contains("gpu") {
                let temp_path = format!("/sys/class/thermal/{}/temp", name);
                if let Some(temp_millideg) = read_sysfs_value::<u64>(&temp_path) {
                    return Some((temp_millideg / 1000) as u32);
                }
            }
        }
    }

    None
}

// =============================================================================
// GPU enrichment — iOS (unified memory)
// =============================================================================

/// iOS live metryki — bardzo ograniczone. iOS nie udostepnia GPU usage/temp/power.
/// VRAM = RAM systemowy (unified memory, jak Apple Silicon Mac).
#[cfg(target_os = "ios")]
fn enrich_ios_live(gpus: &mut [PeerGpuInfo]) {
    if gpus.is_empty() {
        return;
    }

    let total_ram_mb = {
        let sys = SYS.lock();
        sys.total_memory() / (1024 * 1024)
    };

    for gpu in gpus.iter_mut() {
        if gpu.vram_total_mb == 0 && gpu.usage_percent == 0.0 {
            gpu.vram_total_mb = total_ram_mb;
        }
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
// Temperatura CPU — sysinfo::Components
// =============================================================================

/// Odczytuje temperature CPU z sensorow systemowych.
/// Zwraca srednia temperature sensorow CPU lub None jesli brak danych.
fn detect_cpu_temperature() -> Option<f32> {
    let components = sysinfo::Components::new_with_refreshed_list();
    let mut sum = 0.0f32;
    let mut count = 0u32;
    for c in &components {
        let label = c.label().to_lowercase();
        if label.contains("cpu") || label.contains("core") || label.contains("package") || label.contains("tctl") || label.contains("tdie") {
            if let Some(temp) = c.temperature() {
                if temp > 0.0 {
                    sum += temp;
                    count += 1;
                }
            }
        }
    }
    if count > 0 {
        Some(sum / count as f32)
    } else {
        None
    }
}

// =============================================================================
// Informacje o interfejsach sieciowych — Linux-specific
// =============================================================================

/// Odczytuje stan linku interfejsu sieciowego (Linux: /sys/class/net/{name}/operstate)
fn detect_link_up(name: &str) -> bool {
    #[cfg(target_os = "linux")]
    {
        // carrier = fizyczny kabel wpiety (1/0), niezalezny od konfiguracji IP
        let carrier_path = format!("/sys/class/net/{}/carrier", name);
        if let Ok(val) = std::fs::read_to_string(&carrier_path) {
            return val.trim() == "1";
        }
        // Fallback na operstate jesli carrier nie dostepny (np. WiFi)
        let operstate_path = format!("/sys/class/net/{}/operstate", name);
        std::fs::read_to_string(&operstate_path)
            .map(|s| s.trim() == "up")
            .unwrap_or(false)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = name;
        false
    }
}

/// Odczytuje adres MAC interfejsu (Linux: /sys/class/net/{name}/address)
fn detect_mac_address(name: &str) -> String {
    #[cfg(target_os = "linux")]
    {
        let path = format!("/sys/class/net/{}/address", name);
        std::fs::read_to_string(&path)
            .map(|s| s.trim().to_string())
            .unwrap_or_default()
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = name;
        String::new()
    }
}

/// Wykrywa typ interfejsu sieciowego
fn detect_interface_type(name: &str) -> String {
    if name.starts_with("lo") {
        return "loopback".to_string();
    }
    if name.starts_with("docker") || name.starts_with("br-") || name.starts_with("veth") {
        return "virtual".to_string();
    }

    #[cfg(target_os = "linux")]
    {
        // Thunderbolt
        let subsystem_path = format!("/sys/class/net/{}/device/subsystem", name);
        if let Ok(target) = std::fs::read_link(&subsystem_path) {
            if let Some(s) = target.to_str() {
                if s.contains("thunderbolt") {
                    return "thunderbolt".to_string();
                }
            }
        }

        // Wi-Fi
        let wireless_path = format!("/sys/class/net/{}/wireless", name);
        if std::path::Path::new(&wireless_path).exists() {
            return "wifi".to_string();
        }
    }

    "ethernet".to_string()
}

/// Wykrywa sciezke PCIe interfejsu: 0 = CPU path, 1 = GPU path
/// Metoda 1: NUMA node (standardowe serwery z wieloma CPU/GPU)
/// Metoda 2: PCIe domain (Grace-Blackwell SoC: domain 0000=CPU, >0000=GPU)
fn detect_numa_node(name: &str) -> Option<i32> {
    #[cfg(target_os = "linux")]
    {
        // Metoda 1: NUMA node
        let numa_path = format!("/sys/class/net/{}/device/numa_node", name);
        if let Ok(val) = std::fs::read_to_string(&numa_path) {
            let numa: i32 = val.trim().parse().unwrap_or(-1);
            if numa >= 0 {
                return Some(numa);
            }
        }
        // Metoda 2: PCIe domain — TYLKO dla kart Mellanox/NVIDIA (vendor 0x15b3)
        // Domain 0000 = CPU path, domain > 0000 = GPU bridge path
        let vendor_path = format!("/sys/class/net/{}/device/vendor", name);
        let is_mellanox = std::fs::read_to_string(&vendor_path)
            .map(|v| v.trim() == "0x15b3")
            .unwrap_or(false);
        if is_mellanox {
            let device_link = format!("/sys/class/net/{}/device", name);
            if let Ok(target) = std::fs::read_link(&device_link) {
                let target_str = target.to_string_lossy();
                if let Some(pci_addr) = target_str.rsplit('/').next() {
                    if let Some(domain_str) = pci_addr.split(':').next() {
                        if let Ok(domain) = u32::from_str_radix(domain_str, 16) {
                            if domain > 0 {
                                return Some(1); // GPU path
                            } else {
                                return Some(0); // CPU path
                            }
                        }
                    }
                }
            }
        }
        None
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = name;
        None
    }
}

/// Odczytuje predkosc linku w Mbps (Linux: /sys/class/net/{name}/speed)
fn detect_link_speed(name: &str) -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let path = format!("/sys/class/net/{}/speed", name);
        if let Ok(val) = std::fs::read_to_string(&path) {
            let speed: i64 = val.trim().parse().unwrap_or(-1);
            if speed > 0 {
                return Some(speed as u64);
            }
        }
        None
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = name;
        None
    }
}

/// Sprawdza czy RDMA jest dostepne dla interfejsu (Linux: /sys/class/infiniband/)
fn detect_rdma_available(name: &str) -> bool {
    #[cfg(target_os = "linux")]
    {
        let path = format!("/sys/class/infiniband/{}", name);
        std::path::Path::new(&path).exists()
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = name;
        false
    }
}

/// Pobiera PIERWSZY adres IPv4 i maske interfejsu z sysinfo::Networks
fn detect_ipv4_info(name: &str, nets: &Networks) -> (String, String) {
    let all = detect_all_ipv4_info(name, nets);
    all.into_iter().next().unwrap_or((String::new(), String::new()))
}

/// Pobiera WSZYSTKIE adresy IPv4 interfejsu (bridge moze miec wiele adresow)
fn detect_all_ipv4_info(name: &str, nets: &Networks) -> Vec<(String, String)> {
    let mut result = Vec::new();
    for (iface_name, data) in nets.iter() {
        if iface_name == name {
            for net in data.ip_networks() {
                if let IpAddr::V4(v4) = net.addr {
                    let prefix = net.prefix;
                    let mask = if prefix >= 32 {
                        u32::MAX
                    } else {
                        u32::MAX << (32 - prefix)
                    };
                    let netmask = std::net::Ipv4Addr::from(mask);
                    result.push((v4.to_string(), netmask.to_string()));
                }
            }
        }
    }
    result
}

// Cache bramek domyslnych per interfejs (parsowane z `ip route show`)
lazy_static::lazy_static! {
    static ref GATEWAY_CACHE: Mutex<(Instant, HashMap<String, String>)> = {
        Mutex::new((Instant::now() - Duration::from_secs(120), HashMap::new()))
    };
}

/// Pobiera bramke domyslna dla interfejsu
fn detect_gateway(name: &str) -> String {
    let mut cache = GATEWAY_CACHE.lock();
    // Odswiezaj co 30s
    if cache.0.elapsed() > Duration::from_secs(30) {
        let mut gateways = HashMap::new();
        #[cfg(target_os = "linux")]
        {
            if let Ok(output) = std::process::Command::new("ip")
                .args(["route", "show"])
                .output()
            {
                if output.status.success() {
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    for line in stdout.lines() {
                        // "default via 192.168.1.1 dev eth0 ..."
                        if line.starts_with("default") {
                            let parts: Vec<&str> = line.split_whitespace().collect();
                            if let (Some(via_idx), Some(dev_idx)) = (
                                parts.iter().position(|&p| p == "via"),
                                parts.iter().position(|&p| p == "dev"),
                            ) {
                                if let (Some(gw), Some(dev)) = (parts.get(via_idx + 1), parts.get(dev_idx + 1)) {
                                    gateways.insert(dev.to_string(), gw.to_string());
                                }
                            }
                        }
                    }
                }
            }
        }
        *cache = (Instant::now(), gateways);
    }
    cache.1.get(name).cloned().unwrap_or_default()
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
        .filter_map(|(name, data)| -> Option<Vec<PeerNetworkInfo>> {
            // Pomijaj loopback i Docker bridge
            if name == "lo" || name == "lo0" || name.starts_with("docker") || name.starts_with("br-") || name.starts_with("veth") {
                return None;
            }

            let rx = data.total_received();
            let tx = data.total_transmitted();

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

            let iface_name = name.to_string();
            let link_up = detect_link_up(&iface_name);
            let all_ips = detect_all_ipv4_info(&iface_name, &nets);
            let ipv4_gateway = detect_gateway(&iface_name);
            let mac_address = detect_mac_address(&iface_name);
            let interface_type = detect_interface_type(&iface_name);
            let rdma_available = detect_rdma_available(&iface_name);
            let speed_mbps = detect_link_speed(&iface_name);
            let numa_node = detect_numa_node(&iface_name);

            // Jesli interfejs ma wiele IP (np. bridge), tworz osobny wpis per IP
            if all_ips.len() <= 1 {
                let (ipv4_address, ipv4_netmask) = all_ips.into_iter().next()
                    .unwrap_or((String::new(), String::new()));
                Some(vec![PeerNetworkInfo {
                    name: iface_name,
                    rx_bytes: rx,
                    tx_bytes: tx,
                    rx_bytes_per_sec: rx_per_sec,
                    tx_bytes_per_sec: tx_per_sec,
                    link_up,
                    ipv4_address,
                    ipv4_netmask,
                    ipv4_gateway,
                    mac_address,
                    interface_type,
                    rdma_available,
                    speed_mbps,
                    numa_node,
                }])
            } else {
                Some(all_ips.into_iter().map(|(ip, mask)| {
                    PeerNetworkInfo {
                        name: iface_name.clone(),
                        rx_bytes: rx,
                        tx_bytes: tx,
                        rx_bytes_per_sec: rx_per_sec,
                        tx_bytes_per_sec: tx_per_sec,
                        link_up,
                        ipv4_address: ip,
                        ipv4_netmask: mask,
                        ipv4_gateway: ipv4_gateway.clone(),
                        mac_address: mac_address.clone(),
                        interface_type: interface_type.clone(),
                        rdma_available,
                        speed_mbps,
                        numa_node,
                    }
                }).collect())
            }
        })
        .flatten()
        .collect();

    prev.0 = now;
    prev.1 = current_values;

    let mut sorted = results;
    sorted.sort_by(|a, b| a.name.cmp(&b.name));
    sorted
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
