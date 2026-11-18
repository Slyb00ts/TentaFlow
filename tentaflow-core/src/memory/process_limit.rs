// =============================================================================
// File: memory/process_limit.rs — the memory ceiling iOS grants this process
// =============================================================================
// iOS terminates an app (jetsam) once its physical footprint reaches a per-app
// limit that sits far below physical RAM and depends on the device and on the
// `com.apple.developer.kernel.increased-memory-limit` entitlement. Every iOS
// memory budget (reported unified-memory GPU, MemoryGuard eviction, MLX guard)
// is sized from this one reading, never from physical RAM.
//
// Example:
//   if let Some(mem) = crate::memory::process_memory() {
//       let room_for_models = mem.model_headroom_bytes();
//   }
// =============================================================================

use mach2::kern_return::KERN_SUCCESS;
use mach2::message::mach_msg_type_number_t;
use mach2::task::task_info;
use mach2::task_info::{task_vm_info, TASK_VM_INFO};
use mach2::traps::mach_task_self;
use mach2::vm_types::{integer_t, natural_t};

extern "C" {
    // <os/proc.h>, libSystem, iOS 13+. The kernel does not expose the limit
    // itself, only the headroom left below it.
    fn os_proc_available_memory() -> usize;
}

/// Kept free below the jetsam limit for what is not a model: request and TLS
/// buffers, SQLite caches, the Metal driver's own allocations. They grow
/// during a generation as well, and nothing stops them at the limit.
pub const JETSAM_RESERVE_BYTES: u64 = 256 * 1024 * 1024;

/// Memory accounting of this process against its jetsam limit, in bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessMemory {
    /// `phys_footprint` — the figure jetsam compares against the limit.
    pub footprint_bytes: u64,
    /// The per-app limit: current footprint plus the remaining headroom.
    pub limit_bytes: u64,
}

impl ProcessMemory {
    /// The footprint the app may reach with its models loaded: the limit less
    /// [`JETSAM_RESERVE_BYTES`].
    pub fn model_ceiling_bytes(&self) -> u64 {
        self.limit_bytes.saturating_sub(JETSAM_RESERVE_BYTES)
    }

    /// What models may still add to the current footprint.
    pub fn model_headroom_bytes(&self) -> u64 {
        self.model_ceiling_bytes()
            .saturating_sub(self.footprint_bytes)
    }
}

/// Reads the footprint and the limit, or `None` when either is unavailable.
///
/// `os_proc_available_memory` returns 0 both for a process without a limit and
/// for one already past it; neither yields a usable ceiling, so 0 is unknown
/// rather than "no memory".
pub fn process_memory() -> Option<ProcessMemory> {
    let footprint_bytes = phys_footprint()?;
    // SAFETY: takes no arguments and only reads the calling task's accounting.
    let available = unsafe { os_proc_available_memory() } as u64;
    if available == 0 {
        return None;
    }
    Some(ProcessMemory {
        footprint_bytes,
        limit_bytes: footprint_bytes.saturating_add(available),
    })
}

fn phys_footprint() -> Option<u64> {
    const WORD: usize = std::mem::size_of::<natural_t>();
    let mut info = task_vm_info::default();
    let mut count = (std::mem::size_of::<task_vm_info>() / WORD) as mach_msg_type_number_t;
    // SAFETY: `info` is a live `task_vm_info` and `count` holds its size in
    // `natural_t` words, so the kernel never writes past the buffer.
    let rc = unsafe {
        task_info(
            mach_task_self(),
            TASK_VM_INFO,
            (&mut info as *mut task_vm_info).cast::<integer_t>(),
            &mut count,
        )
    };
    // The kernel shrinks `count` to the revision it filled; `phys_footprint`
    // only exists from revision 1 on.
    let needed =
        (std::mem::offset_of!(task_vm_info, phys_footprint) + std::mem::size_of::<u64>()) / WORD;
    if rc != KERN_SUCCESS || (count as usize) < needed {
        return None;
    }
    Some(info.phys_footprint)
}
