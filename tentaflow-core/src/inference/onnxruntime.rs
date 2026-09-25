// =============================================================================
// File: inference/onnxruntime.rs — the ONNX Runtime already mapped into the process
// =============================================================================
//
// On Linux sherpa-onnx links the shared ONNX Runtime (DT_NEEDED), so it is in
// the process before `ort` (load-dynamic) opens anything. `ort` must dlopen
// that very object: a second copy found by a directory search (another inode,
// or a system libonnxruntime) would run two runtimes side by side.

use std::path::PathBuf;

/// Path under which the loader mapped the process's ONNX Runtime, if one is
/// mapped. dlopen of this exact path returns the already-loaded object.
#[cfg(target_os = "linux")]
pub fn mapped_onnxruntime() -> Option<PathBuf> {
    use std::ffi::{CStr, OsStr};
    use std::os::unix::ffi::OsStrExt;

    // SAFETY: the symbol name is a NUL-terminated literal; RTLD_DEFAULT only
    // searches objects already in the global scope and loads nothing.
    let symbol = unsafe { libc::dlsym(libc::RTLD_DEFAULT, c"OrtGetApiBase".as_ptr()) };
    if symbol.is_null() {
        return None;
    }
    let mut info: libc::Dl_info = unsafe { std::mem::zeroed() };
    // SAFETY: `symbol` is an address inside a mapped object and `info` is a
    // valid out-parameter; dladdr only fills it in.
    if unsafe { libc::dladdr(symbol, &mut info) } == 0 || info.dli_fname.is_null() {
        return None;
    }
    // SAFETY: dli_fname points at the loader's NUL-terminated l_name, which
    // lives as long as the object stays mapped (it is never unloaded).
    let name = unsafe { CStr::from_ptr(info.dli_fname) };
    let path = PathBuf::from(OsStr::from_bytes(name.to_bytes()));
    let is_runtime = path
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with("libonnxruntime.so"));
    is_runtime.then_some(path)
}

/// Other platforms do not link a shared ONNX Runtime at start-up.
#[cfg(not(target_os = "linux"))]
pub fn mapped_onnxruntime() -> Option<PathBuf> {
    None
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::mapped_onnxruntime;
    use std::collections::HashSet;
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    use std::path::PathBuf;

    /// Distinct (device, inode) pairs of every libonnxruntime mapped into this process.
    fn mapped_runtime_files() -> HashSet<String> {
        std::fs::read_to_string("/proc/self/maps")
            .expect("read /proc/self/maps")
            .lines()
            .filter(|l| l.contains("/libonnxruntime.so"))
            .filter_map(|l| {
                let f: Vec<&str> = l.split_whitespace().collect();
                Some(format!("{}:{}", f.get(3)?, f.get(4)?))
            })
            .collect()
    }

    // Stands in for sherpa-onnx's DT_NEEDED: the runtime enters the global
    // scope before `ort` loads anything, and `ort` must end up on that object.
    #[test]
    fn ort_loads_the_runtime_the_loader_already_mapped() {
        let arch = if cfg!(target_arch = "aarch64") {
            "aarch64"
        } else {
            "x86_64"
        };
        let runtime = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../native-libs")
            .join(format!("linux-{arch}"))
            .join("lib-dynamic/libonnxruntime.so.1");
        if !runtime.exists() {
            eprintln!(
                "skip: {} not provisioned (build-onnxruntime.sh)",
                runtime.display()
            );
            return;
        }
        let c_path = CString::new(runtime.as_os_str().as_bytes()).expect("path without NUL");
        // SAFETY: valid NUL-terminated path; the handle stays open for the
        // life of the test process, like a DT_NEEDED dependency.
        let handle = unsafe { libc::dlopen(c_path.as_ptr(), libc::RTLD_NOW | libc::RTLD_GLOBAL) };
        assert!(!handle.is_null(), "dlopen {}", runtime.display());

        let mapped = mapped_onnxruntime().expect("the global-scope runtime is found");
        assert_eq!(mapped, runtime);

        ort::init_from(&mapped)
            .expect("ort loads the mapped runtime")
            .commit();
        assert!(ort::info().contains("ORT Build Info"));
        assert_eq!(
            1,
            mapped_runtime_files().len(),
            "exactly one ONNX Runtime is mapped"
        );
    }
}
