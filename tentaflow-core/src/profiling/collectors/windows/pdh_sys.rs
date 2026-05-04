// =============================================================================
// File: collectors/windows/pdh_sys.rs — Safe wrappers over Windows PDH FFI used
// by the Windows PDH-backed collectors. Hides `unsafe` and UTF-16 marshalling
// behind small typed helpers.
// =============================================================================

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;

use windows_sys::Win32::Foundation::ERROR_SUCCESS;
use windows_sys::Win32::System::Performance::{
    PdhAddCounterW, PdhCloseQuery, PdhCollectQueryData, PdhEnumObjectItemsW,
    PdhGetFormattedCounterValue, PdhOpenQueryW, PDH_FMT_COUNTERVALUE, PDH_FMT_DOUBLE,
    PERF_DETAIL_WIZARD,
};

// PDH_FMT_NOCAP100 nie jest exportowane przez windows-sys 0.59. Stala z PDH API
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
pub struct PdhQuery(isize);

impl PdhQuery {
    pub fn open() -> Result<Self, PdhError> {
        let mut h: isize = 0;
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

    pub fn add_counter(&self, path: &str) -> Result<PdhCounter, PdhError> {
        let wide = to_wide(path);
        let mut counter: isize = 0;
        // SAFETY: `self.0` is a valid query handle; `wide` is NUL-terminated;
        // counter slot is writable.
        let st = unsafe { PdhAddCounterW(self.0, wide.as_ptr(), 0, &mut counter) };
        if st as u32 == ERROR_SUCCESS {
            Ok(PdhCounter(counter))
        } else {
            Err(PdhError {
                op: "PdhAddCounterW",
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
        if self.0 != 0 {
            // SAFETY: `self.0` is a valid query handle owned by this struct.
            unsafe {
                PdhCloseQuery(self.0);
            }
        }
    }
}

/// Counter handle. Lifetime is bound to the parent query (which owns it).
#[derive(Copy, Clone)]
pub struct PdhCounter(isize);

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
}

/// Enumerate instance names of a PDH performance object (e.g. "PhysicalDisk",
/// "GPU Engine"). Returns the parsed UTF-16 instance list.
pub fn enum_instances(object_name: &str) -> Result<Vec<String>, PdhError> {
    let object_w = to_wide(object_name);
    let mut counter_buf_len: u32 = 0;
    let mut instance_buf_len: u32 = 0;

    // First call: query buffer sizes.
    // SAFETY: passing null buffers with zero lengths is the documented probe
    // pattern; PDH writes the required sizes back through the length pointers.
    let st = unsafe {
        PdhEnumObjectItemsW(
            std::ptr::null(),
            std::ptr::null(),
            object_w.as_ptr(),
            std::ptr::null_mut(),
            &mut counter_buf_len,
            std::ptr::null_mut(),
            &mut instance_buf_len,
            PERF_DETAIL_WIZARD,
            0,
        )
    };
    // PDH_MORE_DATA == 0x800007D2; ERROR_SUCCESS means object exists but is
    // empty. Anything else is a real failure.
    const PDH_MORE_DATA: u32 = 0x800007D2;
    let st_u = st as u32;
    if st_u != PDH_MORE_DATA && st_u != ERROR_SUCCESS {
        return Err(PdhError {
            op: "PdhEnumObjectItemsW(size)",
            status: st_u,
        });
    }

    if instance_buf_len == 0 {
        return Ok(Vec::new());
    }

    let mut counter_buf = vec![0u16; counter_buf_len.max(1) as usize];
    let mut instance_buf = vec![0u16; instance_buf_len as usize];

    // SAFETY: buffers are sized as reported by the previous call; lengths
    // passed by mutable pointer.
    let st = unsafe {
        PdhEnumObjectItemsW(
            std::ptr::null(),
            std::ptr::null(),
            object_w.as_ptr(),
            counter_buf.as_mut_ptr(),
            &mut counter_buf_len,
            instance_buf.as_mut_ptr(),
            &mut instance_buf_len,
            PERF_DETAIL_WIZARD,
            0,
        )
    };
    if st as u32 != ERROR_SUCCESS {
        return Err(PdhError {
            op: "PdhEnumObjectItemsW(read)",
            status: st as u32,
        });
    }

    Ok(parse_multi_sz(&instance_buf))
}

/// Parse a Windows MULTI_SZ block (sequence of NUL-terminated UTF-16 strings
/// terminated by an empty string) into owned `String`s.
fn parse_multi_sz(buf: &[u16]) -> Vec<String> {
    let mut out = Vec::new();
    let mut start = 0usize;
    for i in 0..buf.len() {
        if buf[i] == 0 {
            if i == start {
                // Empty string terminator.
                break;
            }
            out.push(String::from_utf16_lossy(&buf[start..i]));
            start = i + 1;
        }
    }
    out
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

    #[test]
    fn parse_multi_sz_handles_empty_buffer() {
        assert!(parse_multi_sz(&[0u16]).is_empty());
        assert!(parse_multi_sz(&[]).is_empty());
    }

    #[test]
    fn parse_multi_sz_two_entries() {
        // "abc\0de\0\0"
        let buf: Vec<u16> = "abc\0de\0\0".encode_utf16().collect();
        let v = parse_multi_sz(&buf);
        assert_eq!(v, vec!["abc".to_string(), "de".to_string()]);
    }
}
