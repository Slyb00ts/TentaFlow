// =============================================================================
// Plik: addon/host_functions/image.rs
// Opis: Host function image_resize_rgb_v1 — udostepnia addonom WASM najszybszy
//       resizer RGB24 z vision::resize. Surowe bajty obrazu plyna bezposrednio
//       przez liniowa pamiec (bez CBOR), wymiary jako parametry i32.
// =============================================================================

use super::abi_helpers::write_output_with_retry_semantics;
use super::{audit_log, check_permission, get_memory, read_guest_bytes, AddonState, WasmCaller};
use crate::addon::errors::AbiError;
use crate::vision::resize::{resize_rgb, ResizeError};

/// Uprawnienie wymagane do uzycia resizera obrazow.
const PERM_IMAGE_RESIZE: &str = "image.resize";

/// Limit liczby pikseli wejscia/wyjscia — chroni przed alokacja DoS przez addon.
/// 64 MP pokrywa najwieksze zdjecia (np. 8K+), a wynik 3 bajty/px to ~192 MB max.
const MAX_PIXELS: u64 = 64 * 1024 * 1024;

/// Host function: resize obrazu RGB24.
///
/// ABI: `(src_ptr, src_len, src_w, src_h, dst_w, dst_h, out_ptr, out_cap, out_len_ptr) -> i32`.
/// Zwraca 0 (OK), 6 (out_cap za maly — out_len_ptr ma wymagany rozmiar) lub inny
/// kod AbiError. Wynik to RGB24 o rozmiarze `dst_w * dst_h * 3`.
#[allow(clippy::too_many_arguments)]
pub fn image_resize_rgb_v1(
    mut caller: WasmCaller<'_, AddonState>,
    src_ptr: i32,
    src_len: i32,
    src_w: i32,
    src_h: i32,
    dst_w: i32,
    dst_h: i32,
    out_ptr: i32,
    out_cap: i32,
    out_len_ptr: i32,
) -> i32 {
    if !check_permission(caller.data(), PERM_IMAGE_RESIZE, None) {
        audit_log(
            caller.data(),
            "image.resize",
            Some("image"),
            None,
            "denied",
            None,
        );
        return AbiError::Permission.as_i32();
    }

    // Walidacja wymiarow przed dotknieciem pamieci — odrzuc ujemne / za duze.
    if src_w <= 0 || src_h <= 0 || dst_w <= 0 || dst_h <= 0 {
        audit_log(
            caller.data(),
            "image.resize",
            Some("image"),
            None,
            "error",
            Some("bledne wymiary"),
        );
        return AbiError::Operation.as_i32();
    }
    let (src_w, src_h, dst_w, dst_h) = (src_w as u32, src_h as u32, dst_w as u32, dst_h as u32);

    // Skrot wymiarow do post-mortem DoS (ktory rozmiar zadania przeciazyl host).
    let dims = format!("{src_w}x{src_h}->{dst_w}x{dst_h}");

    let src_px = src_w as u64 * src_h as u64;
    let dst_px = dst_w as u64 * dst_h as u64;
    if src_px > MAX_PIXELS || dst_px > MAX_PIXELS {
        audit_log(
            caller.data(),
            "image.resize",
            Some("image"),
            None,
            "error",
            Some(&format!("przekroczono limit pikseli ({dims})")),
        );
        return AbiError::PayloadTooLarge.as_i32();
    }

    // Tani guard na rozmiar wejscia PRZED odczytem bufora — odrzuca bledne /
    // zlosliwe src_len bez przejsciowej alokacji (to_vec). Maks. RGB24 to
    // MAX_PIXELS * 3 bajty.
    if src_len as u64 > MAX_PIXELS * 3 {
        audit_log(
            caller.data(),
            "image.resize",
            Some("image"),
            None,
            "error",
            Some(&format!("src_len przekracza limit ({dims})")),
        );
        return AbiError::PayloadTooLarge.as_i32();
    }

    let memory = match get_memory(&mut caller) {
        Some(m) => m,
        None => return AbiError::Operation.as_i32(),
    };

    // Kopiujemy wejscie do wlasnego bufora: resize alokuje wynik, a my i tak
    // musimy oddac data_mut pamiec dla zapisu wyniku (borrow checker).
    let src = match read_guest_bytes(&memory, &caller, src_ptr, src_len) {
        Some(s) => s.to_vec(),
        None => return AbiError::Operation.as_i32(),
    };

    let resized = match resize_rgb(&src, src_w, src_h, dst_w, dst_h) {
        Ok(out) => out,
        Err(e) => {
            let msg = match e {
                ResizeError::ZeroDimension => "wymiar zerowy",
                ResizeError::BufferSizeMismatch { .. } => "zly rozmiar bufora src",
                ResizeError::DimensionOverflow => "overflow wymiarow",
            };
            audit_log(
                caller.data(),
                "image.resize",
                Some("image"),
                None,
                "error",
                Some(&format!("{msg} ({dims})")),
            );
            return AbiError::Operation.as_i32();
        }
    };

    let rc = write_output_with_retry_semantics(
        &memory,
        &mut caller,
        &resized,
        out_ptr,
        out_cap,
        out_len_ptr,
    );

    let result = if rc == AbiError::Ok.as_i32() {
        "ok"
    } else if rc == AbiError::OutputBufferTooSmall.as_i32() {
        "retry"
    } else {
        "error"
    };
    // Do sciezek error/retry dolaczamy wymiary; ok nie wymaga skrotu.
    let detail = if result == "ok" {
        None
    } else {
        Some(format!("{result} ({dims})"))
    };
    audit_log(
        caller.data(),
        "image.resize",
        Some("image"),
        None,
        result,
        detail.as_deref(),
    );

    rc
}
