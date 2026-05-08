// =============================================================================
// Plik: deploy/process_ctl.rs
// Opis: Cross-platform kontrola cyklu zycia subprocesow zdeployowanych przez
//       python-bundle (vllm, vllm-metal, sglang, xtts itd.). Zabija proces
//       grzecznie (SIGTERM/WM_CLOSE), po timeout twardo (SIGKILL/TerminateProcess).
//       Uzywane przez dispatch::handlers::service_delete gdy user klika "Usun"
//       na natywnym silniku w GUI.
// =============================================================================

use anyhow::Result;

/// Probuje zatrzymac proces `pid`. Zwraca `Ok(true)` gdy proces zostal zabity,
/// `Ok(false)` gdy PID juz byl martwy. Na Unixie wysyla SIGTERM, czeka do 3s,
/// potem SIGKILL jesli nadal zyje. Na Windowsie uzywa TerminateProcess.
pub fn terminate(pid: u32) -> Result<bool> {
    if !is_alive(pid) {
        return Ok(false);
    }

    terminate_impl(pid)?;

    // Do 3s grace period na grzeczne zakonczenie.
    for _ in 0..30 {
        std::thread::sleep(std::time::Duration::from_millis(100));
        if !is_alive(pid) {
            return Ok(true);
        }
    }

    // Nadal zyje — twardy kill.
    force_kill(pid)?;
    Ok(true)
}

/// Sprawdza czy PID wciaz istnieje w systemie (bez zabijania).
pub fn is_alive(pid: u32) -> bool {
    is_alive_impl(pid)
}

// errno cross-platform: macOS uzywa __error(), Linux __errno_location().
// std::io::Error::last_os_error() opakowuje to przenosnie.
#[cfg(unix)]
fn last_errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

#[cfg(unix)]
fn is_alive_impl(pid: u32) -> bool {
    // kill(pid, 0) zwraca 0 gdy proces istnieje (TAKZE zombie!) i mamy
    // do niego dostep, -1 z ESRCH gdy go nie ma. EPERM tez sygnalizuje
    // ze zyje (ale nie jest nasz).
    //
    // KRYTYCZNE: zombie process (defunct, zakonczony ale nie reaped)
    // tez zwraca kill(pid, 0) == 0. Bez wykrywania zombie nasz
    // smart_health_probe loops forever czekajac na readiness URL od
    // martwego procesu. Sprawdzamy `/proc/<pid>/status` i wyciagamy
    // pole `State: Z` -> traktujemy jako not-alive (process logicznie
    // umarl, supervisor flag'uje Failed). Linux-only path; macOS / BSD
    // nie maja /proc, fall back na klasyczny kill check.
    unsafe {
        let rc = libc::kill(pid as libc::pid_t, 0);
        if rc != 0 {
            return last_errno() == libc::EPERM;
        }
    }
    #[cfg(target_os = "linux")]
    {
        if let Ok(status) = std::fs::read_to_string(format!("/proc/{}/status", pid)) {
            for line in status.lines() {
                if let Some(rest) = line.strip_prefix("State:") {
                    let trimmed = rest.trim();
                    // Format: "State:\tZ (zombie)" → first non-space char.
                    if trimmed.starts_with('Z') {
                        return false;
                    }
                    break;
                }
            }
        }
    }
    true
}

#[cfg(unix)]
fn terminate_impl(pid: u32) -> Result<()> {
    // Wysylamy do CALEJ process group (negative PID). Kazdy native
    // python-bundle / binary spawn ustawia setsid() w pre_exec, wiec
    // child jest liderem grupy z pgid == pid. SIGTERM na -pid trafia
    // we wszystkie potomne procesy (np. vLLM EngineCore workers
    // trzymajace GPU memory). ESRCH z group-wide kill = grupa juz
    // pusta, fallback na klasyczny pid-only kill na wypadek gdyby
    // proces nie utworzyl wlasnej grupy (legacy spawn).
    unsafe {
        if libc::kill(-(pid as libc::pid_t), libc::SIGTERM) == 0 {
            return Ok(());
        }
        let errno = last_errno();
        if errno == libc::ESRCH {
            return Ok(());
        }
        if libc::kill(pid as libc::pid_t, libc::SIGTERM) != 0 {
            let errno2 = last_errno();
            if errno2 == libc::ESRCH {
                return Ok(());
            }
            anyhow::bail!("SIGTERM pid={} group_errno={} pid_errno={}", pid, errno, errno2);
        }
    }
    Ok(())
}

#[cfg(unix)]
fn force_kill(pid: u32) -> Result<()> {
    unsafe {
        if libc::kill(-(pid as libc::pid_t), libc::SIGKILL) == 0 {
            return Ok(());
        }
        let errno = last_errno();
        if errno == libc::ESRCH {
            return Ok(());
        }
        if libc::kill(pid as libc::pid_t, libc::SIGKILL) != 0 {
            let errno2 = last_errno();
            if errno2 == libc::ESRCH {
                return Ok(());
            }
            anyhow::bail!("SIGKILL pid={} group_errno={} pid_errno={}", pid, errno, errno2);
        }
    }
    Ok(())
}

#[cfg(windows)]
fn is_alive_impl(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return false;
        }
        let mut code: u32 = 0;
        let ok = GetExitCodeProcess(handle, &mut code);
        CloseHandle(handle);
        ok != 0 && code as i32 == STILL_ACTIVE
    }
}

#[cfg(windows)]
fn terminate_impl(pid: u32) -> Result<()> {
    // Windows nie ma SIGTERM — wysylamy od razu TerminateProcess jako
    // pierwszy strzal (tak samo w force_kill). Grace period i tak nic nie da.
    force_kill(pid)
}

#[cfg(windows)]
fn force_kill(pid: u32) -> Result<()> {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenProcess, TerminateProcess, PROCESS_TERMINATE};
    unsafe {
        let handle = OpenProcess(PROCESS_TERMINATE, 0, pid);
        if handle.is_null() {
            anyhow::bail!("OpenProcess pid={} zwrocil null", pid);
        }
        let rc = TerminateProcess(handle, 1);
        CloseHandle(handle);
        if rc == 0 {
            anyhow::bail!("TerminateProcess pid={} nieudane", pid);
        }
    }
    Ok(())
}
