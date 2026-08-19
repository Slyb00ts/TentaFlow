// ===== File: code_studio/exec/windows.rs — job objects on Windows =====
//
// Windows has no process groups that survive the parent, so the equivalent of
// `setsid` + `killpg` is a Job Object with `KILL_ON_JOB_CLOSE`: every process
// the command starts is inside the job, terminating the job removes the whole
// tree, and even a crashed Core cannot leave the tree running — closing the
// last handle to the job kills it.
//
// The bindings are declared here rather than taken from `windows-sys`, because
// the crate is pulled in without its `Win32_System_JobObjects` feature and this
// module must not change the dependency set. The declarations are the five
// kernel32 entry points a job needs and the two structures they read.
//
// One honest gap: the child is assigned to the job immediately AFTER
// `CreateProcess` returns, not while suspended, because `std::process::Command`
// does not hand out the main thread handle needed to resume a suspended
// process. A child that spawns a grandchild in the first microseconds could
// place it outside the job. Closing that window needs a spawn path that does
// not go through `Command`.

use std::io;
use std::os::windows::io::AsRawHandle;
use std::os::windows::process::CommandExt;
use std::process::{Child, Command};

type Handle = *mut core::ffi::c_void;

const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
const JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE: u32 = 0x0000_2000;
const JOB_OBJECT_EXTENDED_LIMIT_INFORMATION_CLASS: i32 = 9;
const JOB_OBJECT_BASIC_ACCOUNTING_INFORMATION_CLASS: i32 = 1;
const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x0000_1000;
const STILL_ACTIVE: u32 = 259;

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct JobObjectBasicLimitInformation {
    per_process_user_time_limit: i64,
    per_job_user_time_limit: i64,
    limit_flags: u32,
    minimum_working_set_size: usize,
    maximum_working_set_size: usize,
    active_process_limit: u32,
    affinity: usize,
    priority_class: u32,
    scheduling_class: u32,
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct IoCounters {
    read_operation_count: u64,
    write_operation_count: u64,
    other_operation_count: u64,
    read_transfer_count: u64,
    write_transfer_count: u64,
    other_transfer_count: u64,
}

/// `JOBOBJECT_BASIC_ACCOUNTING_INFORMATION`. Only `active_processes` is read;
/// the rest is here because the structure is queried whole.
#[repr(C)]
#[derive(Default, Clone, Copy)]
struct JobObjectBasicAccountingInformation {
    total_user_time: i64,
    total_kernel_time: i64,
    this_period_total_user_time: i64,
    this_period_total_kernel_time: i64,
    total_page_fault_count: u32,
    total_processes: u32,
    active_processes: u32,
    total_terminated_processes: u32,
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct JobObjectExtendedLimitInformation {
    basic_limit_information: JobObjectBasicLimitInformation,
    io_info: IoCounters,
    process_memory_limit: usize,
    job_memory_limit: usize,
    peak_process_memory_used: usize,
    peak_job_memory_used: usize,
}

#[link(name = "kernel32")]
extern "system" {
    fn CreateJobObjectW(attributes: *mut core::ffi::c_void, name: *const u16) -> Handle;
    fn SetInformationJobObject(
        job: Handle,
        info_class: i32,
        info: *const core::ffi::c_void,
        info_len: u32,
    ) -> i32;
    fn AssignProcessToJobObject(job: Handle, process: Handle) -> i32;
    fn QueryInformationJobObject(
        job: Handle,
        info_class: i32,
        info: *mut core::ffi::c_void,
        info_len: u32,
        returned_len: *mut u32,
    ) -> i32;
    fn TerminateJobObject(job: Handle, exit_code: u32) -> i32;
    fn CloseHandle(object: Handle) -> i32;
    fn OpenProcess(access: u32, inherit_handle: i32, process_id: u32) -> Handle;
    fn GetExitCodeProcess(process: Handle, exit_code: *mut u32) -> i32;
}

/// Owner of one job object. Dropping it closes the last handle, which — with
/// `KILL_ON_JOB_CLOSE` — is itself a kill of everything still inside.
#[derive(Debug)]
pub struct Guard {
    job: Handle,
}

// The handle is an opaque kernel object; every use below goes through the
// kernel32 calls, which are themselves thread-safe.
unsafe impl Send for Guard {}
unsafe impl Sync for Guard {}

/// Gives the child its own console process group, so a Ctrl-C in Core's console
/// is not delivered to a session's build.
pub fn configure(cmd: &mut Command) {
    cmd.creation_flags(CREATE_NEW_PROCESS_GROUP);
}

/// Creates the job and puts the freshly spawned child in it.
pub fn adopt(child: &Child) -> io::Result<Guard> {
    let job = unsafe { CreateJobObjectW(std::ptr::null_mut(), std::ptr::null()) };
    if job.is_null() {
        return Err(io::Error::last_os_error());
    }
    let guard = Guard { job };

    let mut limits = JobObjectExtendedLimitInformation::default();
    limits.basic_limit_information.limit_flags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    let ok = unsafe {
        SetInformationJobObject(
            guard.job,
            JOB_OBJECT_EXTENDED_LIMIT_INFORMATION_CLASS,
            &limits as *const _ as *const core::ffi::c_void,
            std::mem::size_of::<JobObjectExtendedLimitInformation>() as u32,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    let assigned = unsafe { AssignProcessToJobObject(guard.job, child.as_raw_handle() as Handle) };
    if assigned == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(guard)
}

impl Guard {
    /// Windows has no group-wide "please stop" for a process without a shared
    /// console, so asking and insisting are the same call. Stated rather than
    /// simulated: pretending a graceful stop happened would be worse than the
    /// abrupt one that actually happens.
    pub fn terminate(&self) {
        self.kill();
    }

    pub fn kill(&self) {
        unsafe {
            TerminateJobObject(self.job, 1);
        }
    }

    /// Whether any process is still inside the job.
    ///
    /// This is the Windows answer to `killpg(pgid, 0)`, and it has to be a real
    /// answer: the caller uses it to decide whether a command that already
    /// exited still has descendants running against the sandbox. A job that
    /// cannot be queried is reported as alive, because "the kernel would not
    /// tell us" must not read as "everything is gone".
    pub fn is_alive(&self) -> bool {
        if self.job.is_null() {
            return false;
        }
        let mut info = JobObjectBasicAccountingInformation::default();
        let mut returned: u32 = 0;
        let ok = unsafe {
            QueryInformationJobObject(
                self.job,
                JOB_OBJECT_BASIC_ACCOUNTING_INFORMATION_CLASS,
                &mut info as *mut _ as *mut core::ffi::c_void,
                std::mem::size_of::<JobObjectBasicAccountingInformation>() as u32,
                &mut returned,
            )
        };
        if ok == 0 {
            return true;
        }
        info.active_processes > 0
    }

    pub fn id(&self) -> i32 {
        0
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.job);
        }
    }
}

// There is deliberately NO pseudo-terminal in this module. A Windows PTY is
// ConPTY (`CreatePseudoConsole` plus a `STARTUPINFOEX` attribute list), which
// cannot be driven through `std::process::Command` and needs the `portable-pty`
// crate this build does not link. `terminal.rs` refuses to open a terminal on
// Windows with that exact reason, instead of handing back a handle that would
// never produce output.

/// Whether a recorded process id is still running. Used when reaping what a
/// crash left behind, so a pid that cannot even be opened counts as gone.
pub fn process_alive(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid as u32) };
    if handle.is_null() {
        return false;
    }
    let mut code: u32 = 0;
    let queried = unsafe { GetExitCodeProcess(handle, &mut code) };
    unsafe {
        CloseHandle(handle);
    }
    queried != 0 && code == STILL_ACTIVE
}
