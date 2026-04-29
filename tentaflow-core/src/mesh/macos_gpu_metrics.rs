// =============================================================================
// Plik: macos_gpu_metrics.rs
// Opis: Odczyt temperatury i poboru mocy GPU na Apple Silicon przez private,
//       ale niewymagajace sudo API: IOReport (libIOReport.dylib) +
//       IOHIDEventSystemClient (IOKit). Symbole rozwiazujemy w runtime przez
//       dlsym, dzieki czemu binarka linkuje sie bez prywatnego frameworka,
//       a brak ktoregokolwiek symbolu nie jest fatalny — funkcja po prostu
//       nic nie ustawia.
// =============================================================================

#![cfg(target_os = "macos")]
#![allow(non_upper_case_globals)]

use std::ffi::CString;
use std::os::raw::{c_char, c_void};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use core_foundation::array::{CFArray, CFArrayRef};
use core_foundation::base::{CFRelease, CFRetain, CFType, CFTypeRef, TCFType};
use core_foundation::dictionary::{CFDictionary, CFDictionaryRef, CFMutableDictionaryRef};
use core_foundation::number::CFNumber;
use core_foundation::string::{CFString, CFStringRef};

use crate::mesh::peer_store::PeerGpuInfo;

// ---------------------------------------------------------------------------
// dlopen / dlsym — minimalny binding (libc nie eksponuje tego natywnie tu)
// ---------------------------------------------------------------------------

const RTLD_NOW: i32 = 0x2;

unsafe extern "C" {
    fn dlopen(filename: *const c_char, flag: i32) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
}

fn load_lib(path: &str) -> Option<*mut c_void> {
    let c = CString::new(path).ok()?;
    let h = unsafe { dlopen(c.as_ptr(), RTLD_NOW) };
    if h.is_null() {
        None
    } else {
        Some(h)
    }
}

unsafe fn load_sym(handle: *mut c_void, name: &str) -> Option<*mut c_void> {
    let c = CString::new(name).ok()?;
    let s = unsafe { dlsym(handle, c.as_ptr()) };
    if s.is_null() {
        None
    } else {
        Some(s)
    }
}

// ---------------------------------------------------------------------------
// IOReport — sygnatury i tabela symboli
// ---------------------------------------------------------------------------

type FnIOReportCopyChannelsInGroup = unsafe extern "C" fn(
    group: CFStringRef,
    subgroup: CFStringRef,
    a: u64,
    b: u64,
    c: u64,
) -> CFMutableDictionaryRef;

type FnIOReportCreateSubscription = unsafe extern "C" fn(
    a: *mut c_void,
    channels: CFMutableDictionaryRef,
    subscribed: *mut CFMutableDictionaryRef,
    channel_id: u64,
    b: CFTypeRef,
) -> CFTypeRef;

type FnIOReportCreateSamples = unsafe extern "C" fn(
    subscription: CFTypeRef,
    channels: CFMutableDictionaryRef,
    a: CFTypeRef,
) -> CFDictionaryRef;

type FnIOReportCreateSamplesDelta = unsafe extern "C" fn(
    prev: CFDictionaryRef,
    now: CFDictionaryRef,
    a: CFTypeRef,
) -> CFDictionaryRef;

type FnIOReportChannelGetCFString = unsafe extern "C" fn(sample: CFDictionaryRef) -> CFStringRef;
type FnIOReportSimpleGetIntegerValue = unsafe extern "C" fn(sample: CFDictionaryRef, a: i32) -> i64;

struct IOReportSyms {
    copy_channels: FnIOReportCopyChannelsInGroup,
    create_subscription: FnIOReportCreateSubscription,
    create_samples: FnIOReportCreateSamples,
    create_samples_delta: FnIOReportCreateSamplesDelta,
    channel_get_group: FnIOReportChannelGetCFString,
    channel_get_name: FnIOReportChannelGetCFString,
    simple_get_int: FnIOReportSimpleGetIntegerValue,
}

unsafe impl Send for IOReportSyms {}
unsafe impl Sync for IOReportSyms {}

fn ioreport_syms() -> Option<&'static IOReportSyms> {
    static CELL: OnceLock<Option<IOReportSyms>> = OnceLock::new();
    CELL.get_or_init(|| {
        let lib = load_lib("/usr/lib/libIOReport.dylib")?;
        unsafe {
            Some(IOReportSyms {
                copy_channels: std::mem::transmute::<*mut c_void, FnIOReportCopyChannelsInGroup>(
                    load_sym(lib, "IOReportCopyChannelsInGroup")?,
                ),
                create_subscription: std::mem::transmute::<
                    *mut c_void,
                    FnIOReportCreateSubscription,
                >(load_sym(lib, "IOReportCreateSubscription")?),
                create_samples: std::mem::transmute::<*mut c_void, FnIOReportCreateSamples>(
                    load_sym(lib, "IOReportCreateSamples")?,
                ),
                create_samples_delta: std::mem::transmute::<
                    *mut c_void,
                    FnIOReportCreateSamplesDelta,
                >(load_sym(lib, "IOReportCreateSamplesDelta")?),
                channel_get_group: std::mem::transmute::<*mut c_void, FnIOReportChannelGetCFString>(
                    load_sym(lib, "IOReportChannelGetGroup")?,
                ),
                channel_get_name: std::mem::transmute::<*mut c_void, FnIOReportChannelGetCFString>(
                    load_sym(lib, "IOReportChannelGetChannelName")?,
                ),
                simple_get_int: std::mem::transmute::<
                    *mut c_void,
                    FnIOReportSimpleGetIntegerValue,
                >(load_sym(lib, "IOReportSimpleGetIntegerValue")?),
            })
        }
    })
    .as_ref()
}

// ---------------------------------------------------------------------------
// IOHIDEventSystemClient (z IOKit) — sygnatury i tabela symboli
// ---------------------------------------------------------------------------

type IOHIDEventSystemClientRef = CFTypeRef;
type IOHIDServiceClientRef = CFTypeRef;
type IOHIDEventRef = *mut c_void;

type FnHIDClientCreate = unsafe extern "C" fn(allocator: CFTypeRef) -> IOHIDEventSystemClientRef;
type FnHIDClientSetMatching =
    unsafe extern "C" fn(client: IOHIDEventSystemClientRef, matching: CFDictionaryRef);
type FnHIDClientCopyServices =
    unsafe extern "C" fn(client: IOHIDEventSystemClientRef) -> CFArrayRef;
type FnHIDServiceCopyProperty =
    unsafe extern "C" fn(service: IOHIDServiceClientRef, key: CFStringRef) -> CFTypeRef;
type FnHIDServiceCopyEvent = unsafe extern "C" fn(
    service: IOHIDServiceClientRef,
    event_type: i64,
    a: i32,
    b: i64,
) -> IOHIDEventRef;
type FnHIDEventGetFloat = unsafe extern "C" fn(event: IOHIDEventRef, field: i64) -> f64;

struct HidSyms {
    client_create: FnHIDClientCreate,
    client_set_matching: FnHIDClientSetMatching,
    client_copy_services: FnHIDClientCopyServices,
    service_copy_property: FnHIDServiceCopyProperty,
    service_copy_event: FnHIDServiceCopyEvent,
    event_get_float: FnHIDEventGetFloat,
}

unsafe impl Send for HidSyms {}
unsafe impl Sync for HidSyms {}

fn hid_syms() -> Option<&'static HidSyms> {
    static CELL: OnceLock<Option<HidSyms>> = OnceLock::new();
    CELL.get_or_init(|| {
        let lib = load_lib("/System/Library/Frameworks/IOKit.framework/IOKit")?;
        unsafe {
            Some(HidSyms {
                client_create: std::mem::transmute::<*mut c_void, FnHIDClientCreate>(load_sym(
                    lib,
                    "IOHIDEventSystemClientCreate",
                )?),
                client_set_matching: std::mem::transmute::<*mut c_void, FnHIDClientSetMatching>(
                    load_sym(lib, "IOHIDEventSystemClientSetMatching")?,
                ),
                client_copy_services: std::mem::transmute::<*mut c_void, FnHIDClientCopyServices>(
                    load_sym(lib, "IOHIDEventSystemClientCopyServices")?,
                ),
                service_copy_property: std::mem::transmute::<*mut c_void, FnHIDServiceCopyProperty>(
                    load_sym(lib, "IOHIDServiceClientCopyProperty")?,
                ),
                service_copy_event: std::mem::transmute::<*mut c_void, FnHIDServiceCopyEvent>(
                    load_sym(lib, "IOHIDServiceClientCopyEvent")?,
                ),
                event_get_float: std::mem::transmute::<*mut c_void, FnHIDEventGetFloat>(load_sym(
                    lib,
                    "IOHIDEventGetFloatValue",
                )?),
            })
        }
    })
    .as_ref()
}

// Stale HID — z IOHIDEventTypes.h.
const kIOHIDEventTypeTemperature: i64 = 15;
// kIOHIDEventFieldTemperatureLevel = (kIOHIDEventTypeTemperature << 16)
const kIOHIDEventFieldTemperatureLevel: i64 = 15 << 16;

// ---------------------------------------------------------------------------
// RAII handle dla CFType — zwalnia refcount przy Drop.
// ---------------------------------------------------------------------------

struct CFHandle {
    inner: CFTypeRef,
}

unsafe impl Send for CFHandle {}

impl CFHandle {
    fn from_create(ptr: CFTypeRef) -> Option<Self> {
        if ptr.is_null() {
            None
        } else {
            Some(Self { inner: ptr })
        }
    }

    fn as_ref(&self) -> CFTypeRef {
        self.inner
    }
}

impl Drop for CFHandle {
    fn drop(&mut self) {
        if !self.inner.is_null() {
            unsafe { CFRelease(self.inner) };
        }
    }
}

// ---------------------------------------------------------------------------
// Stan globalny
// ---------------------------------------------------------------------------

struct EnergyState {
    subscription: CFHandle,
    channels: CFHandle,
    prev_samples: Option<CFHandle>,
    prev_instant: Option<Instant>,
    has_gpu_channel: bool,
}

unsafe impl Send for EnergyState {}

static ENERGY_STATE: OnceLock<Mutex<Option<EnergyState>>> = OnceLock::new();

fn energy_state() -> &'static Mutex<Option<EnergyState>> {
    ENERGY_STATE.get_or_init(|| Mutex::new(None))
}

struct ThermalState {
    /// Trzymany dla refcountu — uslugi HID zaleza od zycia tego klienta.
    #[allow(dead_code)]
    client: CFHandle,
    gpu_services: Vec<CFHandle>,
}

unsafe impl Send for ThermalState {}

static THERMAL_STATE: OnceLock<Mutex<Option<ThermalState>>> = OnceLock::new();

fn thermal_state() -> &'static Mutex<Option<ThermalState>> {
    THERMAL_STATE.get_or_init(|| Mutex::new(None))
}

static INIT_FAIL_LOGGED: OnceLock<Mutex<bool>> = OnceLock::new();

fn debug_once(msg: &str) {
    let lock = INIT_FAIL_LOGGED.get_or_init(|| Mutex::new(false));
    let mut flag = lock.lock().unwrap_or_else(|e| e.into_inner());
    if !*flag {
        tracing::debug!(target: "mesh::macos_gpu_metrics", "{msg}");
        *flag = true;
    }
}

// ---------------------------------------------------------------------------
// Publiczne API
// ---------------------------------------------------------------------------

/// Wzbogaca pola `temperature_c` i `power_draw_w` w `gpus`. Nie nadpisuje
/// wartosci ktore juz zostaly ustawione przez wczesniejszy enricher.
pub fn enrich_macos_thermal_power(gpus: &mut [PeerGpuInfo]) {
    if gpus.is_empty() {
        return;
    }

    let temp = read_gpu_temperature_c();
    let power = read_gpu_power_w();

    for gpu in gpus.iter_mut() {
        if gpu.temperature_c == 0 {
            if let Some(t) = temp {
                gpu.temperature_c = t;
            }
        }
        if gpu.power_draw_w.is_none() {
            if let Some(p) = power {
                gpu.power_draw_w = Some(p);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Temperatura — IOHIDEventSystemClient
// ---------------------------------------------------------------------------

fn read_gpu_temperature_c() -> Option<u32> {
    let syms = hid_syms()?;
    let mut guard = thermal_state().lock().ok()?;

    if guard.is_none() {
        match init_thermal_state(syms) {
            Some(s) => *guard = Some(s),
            None => {
                debug_once("IOHIDEventSystemClient init failed — brak temperatur GPU");
                return None;
            }
        }
    }

    let state = guard.as_ref()?;
    if state.gpu_services.is_empty() {
        return None;
    }

    let mut sum = 0.0_f64;
    let mut count = 0_u32;

    for svc in state.gpu_services.iter() {
        unsafe {
            let event = (syms.service_copy_event)(svc.as_ref(), kIOHIDEventTypeTemperature, 0, 0);
            if event.is_null() {
                continue;
            }
            let value = (syms.event_get_float)(event, kIOHIDEventFieldTemperatureLevel);
            CFRelease(event as CFTypeRef);

            // Sensowny zakres: 0..125 °C.
            if value > 0.0 && value < 125.0 {
                sum += value;
                count += 1;
            }
        }
    }

    if count == 0 {
        return None;
    }

    Some((sum / count as f64).round() as u32)
}

fn init_thermal_state(syms: &HidSyms) -> Option<ThermalState> {
    unsafe {
        let client_ref = (syms.client_create)(std::ptr::null_mut());
        let client = CFHandle::from_create(client_ref)?;

        // Matching dictionary: kHIDPage_AppleVendor (0xff00),
        // kHIDUsage_AppleVendor_TemperatureSensor (0x0005).
        let key_page = CFString::from_static_string("PrimaryUsagePage");
        let key_usage = CFString::from_static_string("PrimaryUsage");
        let val_page = CFNumber::from(0xff00_i32);
        let val_usage = CFNumber::from(0x0005_i32);

        let matching = CFDictionary::from_CFType_pairs(&[
            (key_page.as_CFType(), val_page.as_CFType()),
            (key_usage.as_CFType(), val_usage.as_CFType()),
        ]);

        (syms.client_set_matching)(client.as_ref(), matching.as_concrete_TypeRef());

        let services_ref = (syms.client_copy_services)(client.as_ref());
        if services_ref.is_null() {
            return Some(ThermalState {
                client,
                gpu_services: Vec::new(),
            });
        }
        let services: CFArray<CFTypeRef> = CFArray::wrap_under_create_rule(services_ref);

        let product_key = CFString::from_static_string("Product");
        let mut gpu_services: Vec<CFHandle> = Vec::new();

        for i in 0..services.len() {
            let Some(svc_ptr) = services.get(i) else {
                continue;
            };
            let svc_ref: CFTypeRef = *svc_ptr;
            if svc_ref.is_null() {
                continue;
            }

            let name_ref = (syms.service_copy_property)(svc_ref, product_key.as_concrete_TypeRef());
            if name_ref.is_null() {
                continue;
            }
            let name_cf: CFType = CFType::wrap_under_create_rule(name_ref);
            let Some(name_str) = name_cf.downcast::<CFString>() else {
                continue;
            };
            let name = name_str.to_string();
            if !name.to_ascii_lowercase().contains("gpu") {
                continue;
            }

            let retained = CFRetain(svc_ref);
            if let Some(handle) = CFHandle::from_create(retained) {
                gpu_services.push(handle);
            }
        }

        Some(ThermalState {
            client,
            gpu_services,
        })
    }
}

// ---------------------------------------------------------------------------
// Pobor mocy — IOReport (Energy Model -> GPU Energy)
// ---------------------------------------------------------------------------

fn read_gpu_power_w() -> Option<f32> {
    let syms = ioreport_syms()?;
    let mut guard = energy_state().lock().ok()?;

    if guard.is_none() {
        match init_energy_state(syms) {
            Some(s) => *guard = Some(s),
            None => {
                debug_once("IOReport init failed — brak power GPU");
                return None;
            }
        }
    }

    let state = guard.as_mut()?;
    if !state.has_gpu_channel {
        return None;
    }

    let now = Instant::now();
    let samples_ref = unsafe {
        (syms.create_samples)(
            state.subscription.as_ref(),
            state.channels.as_ref() as CFMutableDictionaryRef,
            std::ptr::null(),
        )
    };
    let Some(samples) = CFHandle::from_create(samples_ref as CFTypeRef) else {
        return None;
    };

    // Pierwsza probka — nie ma jeszcze delty.
    let Some(prev) = state.prev_samples.take() else {
        state.prev_samples = Some(samples);
        state.prev_instant = Some(now);
        return None;
    };
    let Some(prev_instant) = state.prev_instant.take() else {
        state.prev_samples = Some(samples);
        state.prev_instant = Some(now);
        return None;
    };

    let dt_secs = now.duration_since(prev_instant).as_secs_f64();
    if dt_secs <= 0.0 {
        state.prev_samples = Some(samples);
        state.prev_instant = Some(now);
        return None;
    }

    let delta_ref = unsafe {
        (syms.create_samples_delta)(
            prev.as_ref() as CFDictionaryRef,
            samples.as_ref() as CFDictionaryRef,
            std::ptr::null(),
        )
    };
    let delta = CFHandle::from_create(delta_ref as CFTypeRef);

    state.prev_samples = Some(samples);
    state.prev_instant = Some(now);

    let delta = delta?;

    let total_nj = sum_gpu_energy_nj(syms, delta.as_ref() as CFDictionaryRef)?;
    if total_nj == 0 {
        return Some(0.0);
    }

    // 1 nJ = 1e-9 J. Power [W] = J / s.
    let power_w = (total_nj as f64) * 1e-9 / dt_secs;
    if !power_w.is_finite() || power_w < 0.0 || power_w > 1000.0 {
        return None;
    }
    Some(power_w as f32)
}

fn init_energy_state(syms: &IOReportSyms) -> Option<EnergyState> {
    unsafe {
        let group = CFString::from_static_string("Energy Model");
        let channels_ref =
            (syms.copy_channels)(group.as_concrete_TypeRef(), std::ptr::null(), 0, 0, 0);
        if channels_ref.is_null() {
            return None;
        }
        let channels = CFHandle::from_create(channels_ref as CFTypeRef)?;

        // Sprawdzenie czy w channels jest jakikolwiek GPU* Energy. Na Intel Macach
        // nie ma — wtedy stan zostaje, ale flaga blokuje czytanie.
        let has_gpu_channel = channels_have_gpu_energy(syms, channels.as_ref() as CFDictionaryRef);

        let mut subscribed: CFMutableDictionaryRef = std::ptr::null_mut();
        let sub_ref = (syms.create_subscription)(
            std::ptr::null_mut(),
            channels.as_ref() as CFMutableDictionaryRef,
            &mut subscribed,
            0,
            std::ptr::null(),
        );
        let subscription = CFHandle::from_create(sub_ref)?;

        if !subscribed.is_null() {
            CFRelease(subscribed as CFTypeRef);
        }

        Some(EnergyState {
            subscription,
            channels,
            prev_samples: None,
            prev_instant: None,
            has_gpu_channel,
        })
    }
}

fn channels_have_gpu_energy(syms: &IOReportSyms, channels: CFDictionaryRef) -> bool {
    iterate_energy_channels(syms, channels, |group, name| {
        is_energy_group(&group) && is_gpu_energy_name(&name)
    })
}

fn sum_gpu_energy_nj(syms: &IOReportSyms, samples: CFDictionaryRef) -> Option<i64> {
    let mut total: i64 = 0;
    let mut found = false;

    iterate_samples(samples, |sample| {
        let group = unsafe { cf_string_to_rust((syms.channel_get_group)(sample)) };
        let name = unsafe { cf_string_to_rust((syms.channel_get_name)(sample)) };
        if is_energy_group(&group) && is_gpu_energy_name(&name) {
            let v = unsafe { (syms.simple_get_int)(sample, 0) };
            if v >= 0 {
                total = total.saturating_add(v);
                found = true;
            }
        }
    });

    if found {
        Some(total)
    } else {
        None
    }
}

fn is_energy_group(group: &str) -> bool {
    group.eq_ignore_ascii_case("Energy Model")
}

fn is_gpu_energy_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    // M1: "GPU Energy"; M2/M3 multi-die: "GPU0 Energy", "GPU1 Energy".
    lower.starts_with("gpu") && lower.contains("energy")
}

fn iterate_energy_channels<F>(
    syms: &IOReportSyms,
    channels: CFDictionaryRef,
    mut predicate: F,
) -> bool
where
    F: FnMut(String, String) -> bool,
{
    let mut found = false;
    iterate_samples(channels, |sample| {
        if found {
            return;
        }
        let group = unsafe { cf_string_to_rust((syms.channel_get_group)(sample)) };
        let name = unsafe { cf_string_to_rust((syms.channel_get_name)(sample)) };
        if predicate(group, name) {
            found = true;
        }
    });
    found
}

/// Wyciaga z root dict klucz "IOReportChannels" -> CFArray<CFDictionary> i wywoluje
/// `f` dla kazdego elementu.
fn iterate_samples<F>(root: CFDictionaryRef, mut f: F)
where
    F: FnMut(CFDictionaryRef),
{
    if root.is_null() {
        return;
    }
    unsafe {
        let key = CFString::from_static_string("IOReportChannels");
        let root_dict: CFDictionary<CFType, CFType> = CFDictionary::wrap_under_get_rule(root);
        let Some(arr_value) = root_dict.find(key.as_CFType()) else {
            return;
        };
        let arr_ref: CFTypeRef = arr_value.as_CFTypeRef();
        if arr_ref.is_null() {
            return;
        }
        let array: CFArray<CFType> = CFArray::wrap_under_get_rule(arr_ref as CFArrayRef);
        for i in 0..array.len() {
            let Some(item) = array.get(i) else { continue };
            let item_ref = item.as_CFTypeRef();
            if item_ref.is_null() {
                continue;
            }
            f(item_ref as CFDictionaryRef);
        }
    }
}

unsafe fn cf_string_to_rust(s: CFStringRef) -> String {
    if s.is_null() {
        return String::new();
    }
    let cf: CFString = unsafe { CFString::wrap_under_get_rule(s) };
    cf.to_string()
}

// ---------------------------------------------------------------------------
// Smoke test — empty slice nie panikuje. Realny odczyt wymaga sprzetu i nie
// nadaje sie na unit-test (FFI do prywatnego API).
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_slice_is_noop() {
        let mut empty: Vec<PeerGpuInfo> = Vec::new();
        enrich_macos_thermal_power(&mut empty);
        assert!(empty.is_empty());
    }
}
