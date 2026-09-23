// =============================================================================
// File: collectors/windows/pdh_sys.rs — Safe wrappers over Windows PDH FFI used
// by the Windows PDH-backed collectors. Hides `unsafe` and UTF-16 marshalling
// behind small typed helpers.
// =============================================================================

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;

use windows_sys::Win32::Foundation::ERROR_SUCCESS;
use windows_sys::Win32::System::Performance::{
    PdhAddEnglishCounterW, PdhCloseQuery, PdhCollectQueryData, PdhGetFormattedCounterArrayW,
    PdhGetFormattedCounterValue, PdhOpenQueryW, PDH_FMT_COUNTERVALUE, PDH_FMT_COUNTERVALUE_ITEM_W,
    PDH_FMT_DOUBLE, PDH_FMT_LARGE, PDH_HCOUNTER, PDH_HQUERY,
};

// PDH_FMT_NOCAP100 nie jest exportowane przez windows-sys. Stala z PDH API
// (pdhmsg.h): nie capnij wartosci na 100 dla licznikow procentowych.
const PDH_FMT_NOCAP100: u32 = 0x0000_8000;

/// PDH operation error: a Win32 status code surfaced from the PDH API.
#[derive(Debug)]
pub struct PdhError {
    pub op: &'static str,
    pub status: u32,
}

impl std::fmt::Display for PdhError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "PDH {} failed: 0x{:08X}", self.op, self.status)
    }
}

impl std::error::Error for PdhError {}

/// Encode a Rust string as a NUL-terminated UTF-16 buffer suitable for the
/// `*W` PDH entry points.
pub fn to_wide(s: &str) -> Vec<u16> {
    OsStr::new(s)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

/// Owned PDH query handle. Calls `PdhCloseQuery` on drop.
pub struct PdhQuery(PDH_HQUERY);

impl PdhQuery {
    pub fn open() -> Result<Self, PdhError> {
        let mut h: PDH_HQUERY = std::ptr::null_mut();
        // SAFETY: passing a NULL data source (live data), zero reserved arg
        // and a writable handle slot per PdhOpenQueryW contract.
        let st = unsafe { PdhOpenQueryW(std::ptr::null(), 0, &mut h) };
        if st as u32 == ERROR_SUCCESS {
            Ok(Self(h))
        } else {
            Err(PdhError {
                op: "PdhOpenQueryW",
                status: st as u32,
            })
        }
    }

    /// Adds a counter by its English path (`\GPU Engine(*)\Utilization Percentage`).
    /// `PdhAddCounterW` expects names in the UI language, so the same path
    /// silently fails on a Polish or German Windows; the English variant works
    /// everywhere. A `*` instance keeps matching instances that appear later.
    pub fn add_english_counter(&self, path: &str) -> Result<PdhCounter, PdhError> {
        let wide = to_wide(path);
        let mut counter: PDH_HCOUNTER = std::ptr::null_mut();
        // SAFETY: `self.0` is a valid query handle; `wide` is NUL-terminated;
        // counter slot is writable.
        let st = unsafe { PdhAddEnglishCounterW(self.0, wide.as_ptr(), 0, &mut counter) };
        if st as u32 == ERROR_SUCCESS {
            Ok(PdhCounter(counter))
        } else {
            Err(PdhError {
                op: "PdhAddEnglishCounterW",
                status: st as u32,
            })
        }
    }

    pub fn collect(&self) -> Result<(), PdhError> {
        // SAFETY: `self.0` is a live query handle.
        let st = unsafe { PdhCollectQueryData(self.0) };
        if st as u32 == ERROR_SUCCESS {
            Ok(())
        } else {
            Err(PdhError {
                op: "PdhCollectQueryData",
                status: st as u32,
            })
        }
    }
}

impl Drop for PdhQuery {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: `self.0` is a valid query handle owned by this struct.
            unsafe {
                PdhCloseQuery(self.0);
            }
        }
    }
}

/// Counter handle. Lifetime is bound to the parent query (which owns it).
#[derive(Copy, Clone)]
pub struct PdhCounter(PDH_HCOUNTER);

impl PdhCounter {
    /// Read the current formatted value as a double. Returns `None` when the
    /// PDH layer cannot format (e.g. divide-by-zero, calc-negative-denominator,
    /// invalid data on first sample) — collectors should treat as a missing
    /// reading and continue.
    pub fn value_double(&self) -> Option<f64> {
        let mut value: PDH_FMT_COUNTERVALUE = unsafe { std::mem::zeroed() };
        // SAFETY: counter handle is valid; `value` is properly sized for the
        // PDH_FMT_DOUBLE format flag.
        let st = unsafe {
            PdhGetFormattedCounterValue(
                self.0,
                PDH_FMT_DOUBLE | PDH_FMT_NOCAP100,
                std::ptr::null_mut(),
                &mut value,
            )
        };
        if st as u32 == ERROR_SUCCESS {
            // SAFETY: status indicates the doubleValue arm of the union is
            // populated.
            Some(unsafe { value.Anonymous.doubleValue })
        } else {
            None
        }
    }

    /// Every instance of a wildcard counter as `(instance name, value)`, read as
    /// f64. Empty when the counter has no instances yet or no valid data (the
    /// first sample of a rate counter).
    pub fn instances_double(&self) -> Vec<(String, f64)> {
        self.instances(PDH_FMT_DOUBLE | PDH_FMT_NOCAP100, |v| {
            // SAFETY: PDH_FMT_DOUBLE fills the doubleValue arm.
            unsafe { v.Anonymous.doubleValue }
        })
    }

    /// Every instance of a wildcard counter as `(instance name, value)`, read as
    /// i64 — byte counters such as `GPU Process Memory\Dedicated Usage`.
    pub fn instances_large(&self) -> Vec<(String, i64)> {
        self.instances(PDH_FMT_LARGE, |v| {
            // SAFETY: PDH_FMT_LARGE fills the largeValue arm.
            unsafe { v.Anonymous.largeValue }
        })
    }

    fn instances<T>(
        &self,
        format: u32,
        read: impl Fn(&PDH_FMT_COUNTERVALUE) -> T,
    ) -> Vec<(String, T)> {
        let mut buffer_size: u32 = 0;
        let mut item_count: u32 = 0;
        // SAFETY: a null buffer with size 0 is the documented size probe.
        let st = unsafe {
            PdhGetFormattedCounterArrayW(
                self.0,
                format,
                &mut buffer_size,
                &mut item_count,
                std::ptr::null_mut(),
            )
        };
        const PDH_MORE_DATA: u32 = 0x800007D2;
        if st as u32 != PDH_MORE_DATA || buffer_size == 0 {
            return Vec::new();
        }
        // u64 storage keeps the item array (which starts the buffer) aligned.
        let mut buffer = vec![0u64; (buffer_size as usize).div_ceil(8)];
        let items = buffer.as_mut_ptr() as *mut PDH_FMT_COUNTERVALUE_ITEM_W;
        // SAFETY: the buffer holds `buffer_size` bytes as PDH asked for.
        let st = unsafe {
            PdhGetFormattedCounterArrayW(self.0, format, &mut buffer_size, &mut item_count, items)
        };
        if st as u32 != ERROR_SUCCESS {
            return Vec::new();
        }
        let mut out = Vec::with_capacity(item_count as usize);
        for i in 0..item_count as usize {
            // SAFETY: PDH wrote `item_count` items; each name points into the
            // same buffer and is NUL-terminated.
            let item = unsafe { &*items.add(i) };
            if item.FmtValue.CStatus != ERROR_SUCCESS {
                continue;
            }
            let name = unsafe { wide_ptr_to_string(item.szName) };
            out.push((name, read(&item.FmtValue)));
        }
        out
    }
}

/// Reads a NUL-terminated UTF-16 string.
///
/// # Safety
/// `ptr` must be null or point at a NUL-terminated UTF-16 string.
unsafe fn wide_ptr_to_string(ptr: *const u16) -> String {
    if ptr.is_null() {
        return String::new();
    }
    let mut len = 0usize;
    while *ptr.add(len) != 0 {
        len += 1;
    }
    String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_wide_terminates_with_nul() {
        let w = to_wide("abc");
        assert_eq!(w.last(), Some(&0u16));
        assert_eq!(w.len(), 4);
    }
}
