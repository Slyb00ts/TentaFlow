// =============================================================================
// File: collectors/mod.rs — Public API for ProfileCollector trait and plumbing.
// =============================================================================

use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;

use tentaflow_protocol::profiling::{
    ClockSamples, ElevationRequirement, EventCategory, GpuVendor, ProfileScope, TimelineEvent,
};

pub mod elevation;
pub mod intern;
pub mod linux;
pub mod macos;
pub mod nvidia_nsys;
pub mod nvidia_nsys_parser;
pub mod registry;
pub mod windows;

pub use elevation::{ElevationKind, ElevationToken};
pub use intern::{FrameInterner, FrameKey, NameInterner};
pub use nvidia_nsys::{NvidiaNsysCollector, NvidiaNsysRunning};
pub use nvidia_nsys_parser::NvidiaNsysParser;
pub use registry::CollectorRegistry;

// =============================================================================
// Trait surface.
// =============================================================================

/// A factory + capability descriptor for a single profiling data source
/// (e.g. NVIDIA Nsight, Linux `/proc`, AMD rocm-smi).
///
/// One concrete `ProfileCollector` is registered per source; it produces
/// per-session `RunningCollector` handles via `start`.
pub trait ProfileCollector: Send + Sync {
    /// Stable ascii id (e.g. `"nvidia.nsys.gpu"`). Must satisfy
    /// `tentaflow_protocol::profiling::validate_collector_id`.
    fn id(&self) -> &str;

    fn capability(&self) -> &CollectorCapability;

    /// Cheap, side-effect-free probe — checks for binaries, kernel features,
    /// device files, etc. Used at registry build time and by GUI to gray out
    /// unavailable sources.
    fn probe(&self) -> ProbeResult;

    /// Spawn the underlying tool / open the kernel interface for one session.
    fn start(&self, ctx: SessionCtx) -> Result<Box<dyn RunningCollector>, CollectorError>;
}

/// Live handle to a collector instance owned by an active profiling session.
pub trait RunningCollector: Send {
    fn collector_id(&self) -> &str;

    /// Cooperative stop: signal the underlying tool to flush + stop. Consumes
    /// the handle and yields raw artifacts for the parser stage.
    fn stop(self: Box<Self>) -> Result<RawCapture, CollectorError>;

    /// Hard abort. Cleans up artifacts on best-effort basis. Consumes the
    /// handle; no `RawCapture` is produced.
    fn abort(self: Box<Self>);
}

/// Parser that converts a `RawCapture` from one collector into normalized
/// `TimelineEvent`s using the session-scoped interners. Concrete collectors
/// typically pair a `ProfileCollector` impl with a `CollectorParser` impl.
pub trait CollectorParser: Send + Sync {
    fn parse(
        &self,
        raw: RawCapture,
        ctx: &SessionCtx,
        names: &mut NameInterner,
        frames: &mut FrameInterner,
    ) -> Result<Vec<TimelineEvent>, CollectorError>;
}

// =============================================================================
// Capability descriptor.
// =============================================================================

/// Static capability descriptor advertised by every collector.
pub struct CollectorCapability {
    pub categories: Vec<EventCategory>,
    pub elevation: ElevationRequirement,
    pub platforms: PlatformSet,
    /// `Some` for vendor-specific GPU collectors; `None` for vendor-neutral
    /// collectors (CPU samplers, generic disk I/O, ...).
    pub vendor: Option<GpuVendor>,
    /// Human description shown in the GUI source picker.
    pub description: &'static str,
}

/// Bitset of supported (os, arch) combinations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlatformSet(u32);

impl PlatformSet {
    pub const LINUX_X64: u32 = 1 << 0;
    pub const LINUX_ARM64: u32 = 1 << 1;
    pub const MACOS_X64: u32 = 1 << 2;
    pub const MACOS_ARM64: u32 = 1 << 3;
    pub const WINDOWS_X64: u32 = 1 << 4;
    pub const WINDOWS_ARM64: u32 = 1 << 5;
    pub const ANDROID_ARM64: u32 = 1 << 6;

    const ALL_LINUX: u32 = Self::LINUX_X64 | Self::LINUX_ARM64;
    const ALL_MACOS: u32 = Self::MACOS_X64 | Self::MACOS_ARM64;
    const ALL_WINDOWS: u32 = Self::WINDOWS_X64 | Self::WINDOWS_ARM64;
    const ALL_MASK: u32 =
        Self::ALL_LINUX | Self::ALL_MACOS | Self::ALL_WINDOWS | Self::ANDROID_ARM64;

    pub fn empty() -> Self {
        Self(0)
    }

    pub fn from_flag(flag: u32) -> Self {
        Self(flag)
    }

    pub fn from_flags(flags: u32) -> Self {
        Self(flags)
    }

    pub fn all_linux() -> Self {
        Self(Self::ALL_LINUX)
    }

    pub fn all_macos() -> Self {
        Self(Self::ALL_MACOS)
    }

    pub fn all_windows() -> Self {
        Self(Self::ALL_WINDOWS)
    }

    pub fn all() -> Self {
        Self(Self::ALL_MASK)
    }

    pub fn contains(&self, flag: u32) -> bool {
        flag != 0 && (self.0 & flag) == flag
    }

    pub fn bits(&self) -> u32 {
        self.0
    }

    /// Flag(s) for the host the binary runs on. Returns `Self(0)` for any
    /// host platform not present in the constants above; collectors then
    /// gracefully decline.
    pub fn current() -> Self {
        // Single expression: exactly one cfg arm evaluates per build.
        #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
        let bits = Self::LINUX_X64;
        #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
        let bits = Self::LINUX_ARM64;
        #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
        let bits = Self::MACOS_X64;
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        let bits = Self::MACOS_ARM64;
        #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
        let bits = Self::WINDOWS_X64;
        #[cfg(all(target_os = "windows", target_arch = "aarch64"))]
        let bits = Self::WINDOWS_ARM64;
        #[cfg(all(target_os = "android", target_arch = "aarch64"))]
        let bits = Self::ANDROID_ARM64;
        #[cfg(not(any(
            all(target_os = "linux", target_arch = "x86_64"),
            all(target_os = "linux", target_arch = "aarch64"),
            all(target_os = "macos", target_arch = "x86_64"),
            all(target_os = "macos", target_arch = "aarch64"),
            all(target_os = "windows", target_arch = "x86_64"),
            all(target_os = "windows", target_arch = "aarch64"),
            all(target_os = "android", target_arch = "aarch64"),
        )))]
        let bits = 0u32;
        Self(bits)
    }

    pub fn supports_current(&self) -> bool {
        let cur = Self::current();
        cur.0 != 0 && (self.0 & cur.0) != 0
    }
}

// =============================================================================
// Probe result.
// =============================================================================

/// Outcome of `ProfileCollector::probe`.
pub enum ProbeResult {
    Available { version: Option<String> },
    NeedsElevation { kind: ElevationKind, reason: String },
    Unavailable { reason: String },
}

// =============================================================================
// Session context + raw capture.
// =============================================================================

/// Session-scoped context handed to every collector at `start` time.
#[derive(Debug)]
pub struct SessionCtx {
    pub session_id: String,
    pub t0_monotonic_ns: u64,
    pub t0_wallclock_unix_ns: u64,
    /// Directory dedicated to this session; collectors write artifacts under
    /// `output_dir.join("raw").join(collector_id)/`.
    pub output_dir: PathBuf,
    pub scope: ProfileScope,
    pub target_pid: Option<u32>,
    pub elevation: Option<Arc<ElevationToken>>,
    /// Approximate planned duration; collectors may use this to pre-allocate
    /// buffers or set tool-side timeouts. Zero means manual stop.
    pub planned_duration_ns: u64,
}

/// Raw artifacts produced by a collector when it stops.
#[derive(Debug)]
pub struct RawCapture {
    pub artifacts: Vec<PathBuf>,
    pub metadata: HashMap<String, String>,
    pub clock_samples: ClockSamples,
    /// Collector-reported sample count for the manifest. Parser may override
    /// later when it knows the exact event count.
    pub samples_observed: u64,
}

// =============================================================================
// Collector errors.
// =============================================================================

#[derive(thiserror::Error, Debug)]
pub enum CollectorError {
    #[error("io: {0}")]
    Io(#[from] io::Error),
    #[error("spawn failed: {0}")]
    Spawn(String),
    #[error("parse failed: {0}")]
    Parse(String),
    #[error("timeout")]
    Timeout,
    #[error("aborted")]
    Aborted,
    #[error("elevation required: {0}")]
    ElevationRequired(String),
    #[error("{0}")]
    Custom(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_set_contains() {
        let s = PlatformSet::all_linux();
        assert!(s.contains(PlatformSet::LINUX_X64));
        assert!(s.contains(PlatformSet::LINUX_ARM64));
        assert!(!s.contains(PlatformSet::WINDOWS_X64));
        assert!(!s.contains(0));
    }

    #[test]
    fn platform_set_empty_contains_nothing() {
        let s = PlatformSet::empty();
        assert!(!s.contains(PlatformSet::LINUX_X64));
        assert!(!s.contains(PlatformSet::MACOS_ARM64));
    }

    #[test]
    fn platform_set_all_covers_each_variant() {
        let s = PlatformSet::all();
        assert!(s.contains(PlatformSet::LINUX_X64));
        assert!(s.contains(PlatformSet::LINUX_ARM64));
        assert!(s.contains(PlatformSet::MACOS_X64));
        assert!(s.contains(PlatformSet::MACOS_ARM64));
        assert!(s.contains(PlatformSet::WINDOWS_X64));
        assert!(s.contains(PlatformSet::WINDOWS_ARM64));
        assert!(s.contains(PlatformSet::ANDROID_ARM64));
    }

    #[test]
    fn platform_set_current_is_one_of_known() {
        let cur = PlatformSet::current();
        // On every CI target we exercise, the host should be one of the
        // recognised combinations; the bit must be non-zero.
        assert_ne!(cur.bits(), 0, "unrecognised host platform");
    }

    #[test]
    fn platform_set_supports_current_self_holds() {
        assert!(PlatformSet::all().supports_current());
        assert!(!PlatformSet::empty().supports_current());
    }
}
