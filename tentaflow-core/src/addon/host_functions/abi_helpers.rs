// =============================================================================
// Plik: addon/host_functions/abi_helpers.rs
// Opis: Wspolne pomocnicze funkcje ABI uzywane przez host functions F1a:
//       - PayloadKind + enforce_payload_size — limity per kategoria API
//       - write_output_with_retry_semantics — ujednolicony out_cap retry
//         pattern wg planu v0.5.3 §6.2.Y.
// =============================================================================

use super::super::errors::AbiError;
use super::super::runtime::{AsContextMut, WasmMemory};

/// Kategoria payloadu host function — determinuje maksymalny rozmiar bajtow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PayloadKind {
    /// service_call: dane do mikroserwisu QUIC (max 8 MB).
    ServiceCall,
    /// sql_*: query + params zlozone (max 4 MB).
    SqlCombined,
    /// vector_upsert per item (max 1 MB).
    VectorItem,
    /// ui_render — drzewo komponentow (max 2 MB).
    UiRender,
    /// secret_set / secret_get — wartosc (max 64 KB).
    Secret,
}

impl PayloadKind {
    /// Maksymalna liczba bajtow payloadu dla danej kategorii.
    pub const fn max_bytes(self) -> usize {
        match self {
            Self::ServiceCall => 8 * 1024 * 1024,
            Self::SqlCombined => 4 * 1024 * 1024,
            Self::VectorItem => 1024 * 1024,
            Self::UiRender => 2 * 1024 * 1024,
            Self::Secret => 64 * 1024,
        }
    }
}

/// Sprawdza czy payload o danej dlugosci miesci sie w limicie kategorii.
#[inline]
pub fn enforce_payload_size(len: usize, kind: PayloadKind) -> Result<(), AbiError> {
    if len > kind.max_bytes() {
        Err(AbiError::PayloadTooLarge)
    } else {
        Ok(())
    }
}

/// Zapisuje liczbe `value` (u32 little-endian) pod adres `ptr` w pamieci guest.
/// Zwraca `false` gdy adres byl poza zakresem (memory overflow).
fn write_u32_le(memory: &WasmMemory, store: &mut impl AsContextMut, ptr: i32, value: u32) -> bool {
    if ptr < 0 {
        return false;
    }
    let start = ptr as usize;
    let mem = memory.data_mut(store);
    // checked_add chroni przed wrap-around na 32-bit hostach gdy guest podaje i32::MAX.
    let end = match start.checked_add(4) {
        Some(e) if e <= mem.len() => e,
        _ => return false,
    };
    mem[start..end].copy_from_slice(&value.to_le_bytes());
    true
}

/// Implementuje out_cap retry pattern z planu §6.2.Y.
///
/// Zachowanie:
/// - Jesli `actual_data.len() <= out_cap` → zapisuje dane do bufora pod `out_ptr`,
///   ustawia `*out_len_ptr = actual_data.len()`, zwraca `AbiError::Ok` (0).
/// - Jesli `actual_data.len() > out_cap` → NIE pisze do bufora, ustawia
///   `*out_len_ptr = actual_data.len()` (wymagany rozmiar), zwraca
///   `AbiError::OutputBufferTooSmall` (6). Caller realokuje bufor i powtarza.
///
/// Bledy memory (ptr poza zakresem) zwracaja `AbiError::Operation`.
pub fn write_output_with_retry_semantics(
    memory: &WasmMemory,
    store: &mut impl AsContextMut,
    actual_data: &[u8],
    out_ptr: i32,
    out_cap: i32,
    out_len_ptr: i32,
) -> i32 {
    if out_ptr < 0 || out_cap < 0 {
        return AbiError::Operation.as_i32();
    }

    let actual_len = actual_data.len();

    if actual_len > out_cap as usize {
        // Bufor za maly — zapisz wymagany rozmiar i zwroc retry.
        if !write_u32_le(memory, &mut *store, out_len_ptr, actual_len as u32) {
            return AbiError::Operation.as_i32();
        }
        return AbiError::OutputBufferTooSmall.as_i32();
    }

    // Bufor wystarczy — zapisz dane.
    let start = out_ptr as usize;
    // checked_add chroni przed wrap-around na 32-bit hostach (ARMv7 mobile).
    let end = match start.checked_add(actual_len) {
        Some(e) => e,
        None => return AbiError::Operation.as_i32(),
    };
    {
        let mem = memory.data_mut(&mut *store);
        if end > mem.len() {
            return AbiError::Operation.as_i32();
        }
        mem[start..end].copy_from_slice(actual_data);
    }

    if !write_u32_le(memory, &mut *store, out_len_ptr, actual_len as u32) {
        return AbiError::Operation.as_i32();
    }

    AbiError::Ok.as_i32()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_size_under_limit_ok() {
        assert!(enforce_payload_size(1024, PayloadKind::ServiceCall).is_ok());
        assert!(enforce_payload_size(8 * 1024 * 1024, PayloadKind::ServiceCall).is_ok());
        assert!(enforce_payload_size(0, PayloadKind::Secret).is_ok());
    }

    #[test]
    fn payload_size_over_limit_err() {
        assert_eq!(
            enforce_payload_size(9 * 1024 * 1024, PayloadKind::ServiceCall).unwrap_err(),
            AbiError::PayloadTooLarge
        );
        assert_eq!(
            enforce_payload_size(70_000, PayloadKind::Secret).unwrap_err(),
            AbiError::PayloadTooLarge
        );
    }

    #[test]
    fn payload_kind_limits_match_spec() {
        assert_eq!(PayloadKind::ServiceCall.max_bytes(), 8 * 1024 * 1024);
        assert_eq!(PayloadKind::SqlCombined.max_bytes(), 4 * 1024 * 1024);
        assert_eq!(PayloadKind::VectorItem.max_bytes(), 1024 * 1024);
        assert_eq!(PayloadKind::UiRender.max_bytes(), 2 * 1024 * 1024);
        assert_eq!(PayloadKind::Secret.max_bytes(), 64 * 1024);
    }

    // Test integracyjny out_cap retry z prawdziwa pamiecia wasmtime znajduje
    // sie w tests/sdk_boilerplate.rs (potrzebuje wasmtime::Store + Module).
}
