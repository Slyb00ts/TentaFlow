// =============================================================================
// Plik: ffi_sensors.rs
// Opis: FFI dla natywnego przechwytywania czujników telefonu. Warstwa natywna
//       (Swift CoreMotion/CoreLocation/ARKit, Kotlin SensorManager/ARCore/Fused
//       Location) koduje KANONICZNĄ próbkę (ImuSample / GnssFix / BaroSample /
//       LidarFrame) i przekazuje bajty tutaj; bufurujemy je w hostowej
//       MobileSensorQueue. Addon `phone` opróżnia kolejkę co tick przez host-fn
//       `mobile_sensor_drain_v1` (sprawdza uprawnienia per-czujnik i podaje do
//       silnika fuzji ESKF + wspólnej mapy). Ten plik to TYLKO cienki transport —
//       żadnej logiki czujników (ta jest w rdzeniu, jednakowa dla obu platform).
//
//       `kind`: 1=IMU, 2=GNSS, 3=BARO, 4=DEPTH (LidarFrame) — zgodne ze stałymi
//       SENSOR_KIND_* w tentaflow-core/src/services/mobile_sensors.rs.
// =============================================================================

use tentaflow_core::services::mobile_sensors::MobileSensorQueue;

#[cfg(target_os = "android")]
use jni::objects::{JByteArray, JClass};
#[cfg(target_os = "android")]
use jni::sys::{jboolean, jint, JNI_FALSE, JNI_TRUE};
#[cfg(target_os = "android")]
use jni::JNIEnv;

/// Wspólna ścieżka: zbuforuj jedną zakodowaną próbkę. `false` gdy `kind` nieznany
/// lub bufor pusty (próbka odrzucona, nic nie trafia do silnika).
fn push_sensor_bytes(kind: i32, bytes: Vec<u8>) -> bool {
    if bytes.is_empty() || !(1..=5).contains(&kind) {
        return false;
    }
    MobileSensorQueue::global().push_vec(kind as u8, bytes);
    true
}

/// iOS / C ABI — Swift woła bezpośrednio z bufora bajtów kanonicznej próbki.
///
/// # Safety
/// `ptr` musi wskazywać na `len` ważnych bajtów (Swift gwarantuje to dla swojego
/// `Data`/`[UInt8]` w bloku `withUnsafeBytes`).
#[no_mangle]
pub unsafe extern "C" fn tentaflow_mobile_push_sensor(
    kind: i32,
    ptr: *const u8,
    len: i32,
) -> bool {
    if ptr.is_null() || len <= 0 {
        return false;
    }
    let bytes = std::slice::from_raw_parts(ptr, len as usize).to_vec();
    push_sensor_bytes(kind, bytes)
}

/// Wyczyść bufor czujników (rozłączenie / wstrzymanie aplikacji).
#[no_mangle]
pub extern "C" fn tentaflow_mobile_clear_sensors() {
    MobileSensorQueue::global().clear();
}

/// Android / JNI — Kotlin `NativeLib.pushSensor(kind, ByteArray)`.
#[cfg(target_os = "android")]
#[no_mangle]
pub extern "system" fn Java_ai_tentaflow_mobile_NativeLib_pushSensor(
    mut env: JNIEnv,
    _class: JClass,
    kind: jint,
    data: JByteArray,
) -> jboolean {
    let bytes = match env.convert_byte_array(&data) {
        Ok(b) => b,
        Err(_) => return JNI_FALSE,
    };
    if push_sensor_bytes(kind, bytes) {
        JNI_TRUE
    } else {
        JNI_FALSE
    }
}

/// Android / JNI — Kotlin `NativeLib.clearSensors()`.
#[cfg(target_os = "android")]
#[no_mangle]
pub extern "system" fn Java_ai_tentaflow_mobile_NativeLib_clearSensors(
    _env: JNIEnv,
    _class: JClass,
) {
    MobileSensorQueue::global().clear();
}
