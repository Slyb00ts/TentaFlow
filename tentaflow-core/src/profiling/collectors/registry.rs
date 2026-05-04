// =============================================================================
// File: collectors/registry.rs — Discovery and lookup of available ProfileCollectors.
// =============================================================================

use std::sync::Arc;

use tentaflow_protocol::profiling::{EventCategory, GpuTargets, ProfileSourceFlags};

use super::ProfileCollector;

/// Registry of every built-in collector. Concrete implementations register
/// themselves here in later phases (F5 onwards). In F2 the registry is empty
/// by default and exposed for tests / future-phase wiring.
pub struct CollectorRegistry {
    collectors: Vec<Arc<dyn ProfileCollector>>,
}

impl CollectorRegistry {
    pub fn new() -> Self {
        Self {
            collectors: Vec::new(),
        }
    }

    /// Discover all built-in collectors compiled into this binary.
    ///
    /// Each new collector source is added here. The registry is populated
    /// regardless of host platform support; `filter_for_scope` and
    /// `ProfileCollector::probe` decide at session-start time whether a given
    /// collector actually participates in a run.
    pub fn discover() -> Self {
        let mut r = Self::new();
        // NVIDIA Nsight Systems (kernels, API calls, NVTX, GPU metrics).
        r.register(Arc::new(super::NvidiaNsysCollector::new()));
        // Linux no-priv collectors (CPU/RAM/Disk/Power/GPU util via vendor CLIs).
        // Cross-compile target nie ma plikow linux/* — moduly sa cfg-gated do
        // target_os="linux" w mod.rs, wiec rejestracje musza byc tak samo gated.
        #[cfg(target_os = "linux")]
        {
            r.register(Arc::new(
                super::linux::cpu_util::LinuxProcCpuUtilCollector::new(),
            ));
            r.register(Arc::new(
                super::linux::perf_sampling::LinuxPerfSamplingCollector::new(),
            ));
            r.register(Arc::new(
                super::linux::perf_counters::LinuxPerfCountersCollector::new(),
            ));
            r.register(Arc::new(super::linux::ram::LinuxProcRamCollector::new()));
            r.register(Arc::new(super::linux::disk::LinuxIostatDiskCollector::new()));
            r.register(Arc::new(
                super::linux::rapl_power::LinuxRaplPowerCollector::new(),
            ));
            r.register(Arc::new(
                super::linux::nvsmi_gpu::LinuxNvsmiGpuCollector::new(),
            ));
            r.register(Arc::new(super::linux::netdev::LinuxNetdevCollector::new()));
            r.register(Arc::new(
                super::linux::top_processes::LinuxTopProcessesCollector::new(),
            ));
            r.register(Arc::new(
                super::linux::uncore_imc::LinuxUncoreImcCollector::new(),
            ));
            // Linux GPU vendor collectors (AMD ROCm, Intel iGPU).
            r.register(Arc::new(
                super::linux_gpu::rocsmi_util::LinuxRocmSmiGpuCollector::new(),
            ));
            r.register(Arc::new(
                super::linux_gpu::rocprof_kernels::LinuxRocprofKernelsCollector::new(),
            ));
            r.register(Arc::new(
                super::linux_gpu::intel_gpu_top::LinuxIntelGpuTopCollector::new(),
            ));
        }
        // macOS collectors (vm_stat, iostat, powermetrics power+gpu).
        #[cfg(target_os = "macos")]
        {
            r.register(Arc::new(
                super::macos::vm_stat_ram::MacosVmStatRamCollector::new(),
            ));
            r.register(Arc::new(
                super::macos::iostat_disk::MacosIostatDiskCollector::new(),
            ));
            r.register(Arc::new(
                super::macos::powermetrics_power::MacosPowermetricsPowerCollector::new(),
            ));
            r.register(Arc::new(
                super::macos::powermetrics_gpu::MacosPowermetricsGpuCollector::new(),
            ));
        }
        // Windows PDH no-Admin collectors (CPU/RAM/Disk/GPU).
        #[cfg(target_os = "windows")]
        {
            r.register(Arc::new(
                super::windows::pdh_cpu_util::WindowsPdhCpuUtilCollector::new(),
            ));
            r.register(Arc::new(
                super::windows::pdh_ram::WindowsPdhRamCollector::new(),
            ));
            r.register(Arc::new(
                super::windows::pdh_disk::WindowsPdhDiskCollector::new(),
            ));
            r.register(Arc::new(
                super::windows::pdh_gpu::WindowsPdhGpuCollector::new(),
            ));
        }
        r
    }

    pub fn register(&mut self, collector: Arc<dyn ProfileCollector>) {
        self.collectors.push(collector);
    }

    pub fn all(&self) -> &[Arc<dyn ProfileCollector>] {
        &self.collectors
    }

    pub fn by_id(&self, id: &str) -> Option<&Arc<dyn ProfileCollector>> {
        self.collectors.iter().find(|c| c.id() == id)
    }

    /// Filter collectors by host platform support, requested source flags
    /// and GPU vendor selector.
    pub fn filter_for_scope(
        &self,
        requested_sources: u32,
        gpu_targets: &GpuTargets,
    ) -> Vec<Arc<dyn ProfileCollector>> {
        self.collectors
            .iter()
            .filter(|c| {
                let cap = c.capability();
                if !cap.platforms.supports_current() {
                    return false;
                }
                let matches_any_category = cap
                    .categories
                    .iter()
                    .any(|cat| category_matches_source(*cat, requested_sources));
                if !matches_any_category {
                    return false;
                }
                gpu_targets_admit_collector(gpu_targets, cap.vendor.as_ref(), &cap.categories)
            })
            .cloned()
            .collect()
    }

    pub fn len(&self) -> usize {
        self.collectors.len()
    }

    pub fn is_empty(&self) -> bool {
        self.collectors.is_empty()
    }

    /// Probe each registered collector and return the ids of every collector
    /// reporting `Available` or `NeedsElevation` (i.e. potentially runnable on
    /// this host). Used by heartbeat advertisement so peers know upfront which
    /// data sources a node can actually offer.
    pub fn probe_available_ids(registry: &Self) -> Vec<String> {
        registry
            .collectors
            .iter()
            .filter(|c| {
                if !c.capability().platforms.supports_current() {
                    return false;
                }
                matches!(
                    c.probe(),
                    super::ProbeResult::Available { .. }
                        | super::ProbeResult::NeedsElevation { .. }
                )
            })
            .map(|c| c.id().to_string())
            .collect()
    }
}

impl Default for CollectorRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Map a single `EventCategory` to the bitmask flag that requests it.
fn category_matches_source(cat: EventCategory, sources: u32) -> bool {
    let needed = match cat {
        EventCategory::CpuSample => ProfileSourceFlags::CPU_SAMPLING,
        EventCategory::CpuCounter => ProfileSourceFlags::CPU_COUNTERS,
        EventCategory::CpuUtil => ProfileSourceFlags::CPU_UTIL,
        EventCategory::RamSample => ProfileSourceFlags::RAM_USAGE,
        EventCategory::RamBandwidth => ProfileSourceFlags::RAM_BANDWIDTH,
        EventCategory::DiskIoBurst => ProfileSourceFlags::DISK_IO,
        EventCategory::GpuKernel
        | EventCategory::GpuApiCall
        | EventCategory::GpuUtilSample
        | EventCategory::GpuMemSample
        | EventCategory::GpuMemTransfer
        | EventCategory::NvtxRange => ProfileSourceFlags::GPU,
        EventCategory::PowerSample => ProfileSourceFlags::POWER,
        EventCategory::NetworkSample => ProfileSourceFlags::NETWORK,
        // Process-level sample - mapped pod RAM_USAGE i DISK_IO bo to są
        // workflow user'a wybierajacego "pamiec" lub "dysk" do trace.
        EventCategory::ProcessRssSample => ProfileSourceFlags::RAM_USAGE,
        EventCategory::ProcessIoSample => ProfileSourceFlags::DISK_IO,
        // `Custom` is admitted by any non-empty request so that user-defined
        // sources can attach to a session without a dedicated flag.
        EventCategory::Custom => return sources != 0,
    };
    (sources & needed) == needed
}

/// Returns `true` if the GPU target selector permits a collector with the
/// given vendor to participate in the session.
fn gpu_targets_admit_collector(
    targets: &GpuTargets,
    collector_vendor: Option<&tentaflow_protocol::profiling::GpuVendor>,
    categories: &[EventCategory],
) -> bool {
    let is_gpu_collector = categories.iter().any(|c| {
        matches!(
            c,
            EventCategory::GpuKernel
                | EventCategory::GpuApiCall
                | EventCategory::GpuUtilSample
                | EventCategory::GpuMemSample
                | EventCategory::GpuMemTransfer
                | EventCategory::NvtxRange
        )
    });

    if !is_gpu_collector {
        // Non-GPU collectors are not gated by gpu_targets.
        return true;
    }

    match targets {
        GpuTargets::None => false,
        GpuTargets::All | GpuTargets::Indices(_) => true,
        GpuTargets::ByVendor(v) => match collector_vendor {
            Some(cv) => cv == v,
            None => false,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profiling::collectors::{
        CollectorCapability, CollectorError, PlatformSet, ProbeResult, ProfileCollector,
        RunningCollector, SessionCtx,
    };
    use tentaflow_protocol::profiling::{ElevationRequirement, GpuVendor};

    struct MockCollector {
        id: String,
        cap: CollectorCapability,
    }

    impl MockCollector {
        fn new(
            id: &str,
            categories: Vec<EventCategory>,
            platforms: PlatformSet,
            vendor: Option<GpuVendor>,
        ) -> Arc<Self> {
            Arc::new(Self {
                id: id.into(),
                cap: CollectorCapability {
                    categories,
                    elevation: ElevationRequirement::None,
                    platforms,
                    vendor,
                    description: "mock",
                },
            })
        }
    }

    impl ProfileCollector for MockCollector {
        fn id(&self) -> &str {
            &self.id
        }
        fn capability(&self) -> &CollectorCapability {
            &self.cap
        }
        fn probe(&self) -> ProbeResult {
            ProbeResult::Available { version: None }
        }
        fn start(&self, _ctx: SessionCtx) -> Result<Box<dyn RunningCollector>, CollectorError> {
            Err(CollectorError::Custom("mock cannot start".into()))
        }
    }

    #[test]
    fn registry_new_is_empty() {
        let r = CollectorRegistry::new();
        assert!(r.is_empty());
        assert_eq!(r.len(), 0);
    }

    #[test]
    fn discover_includes_nsys() {
        let r = CollectorRegistry::discover();
        assert!(r.by_id("nvidia.nsys.gpu").is_some());
    }

    #[test]
    fn registry_register_lookup() {
        let mut r = CollectorRegistry::new();
        let c = MockCollector::new(
            "mock.cpu",
            vec![EventCategory::CpuSample],
            PlatformSet::all(),
            None,
        );
        r.register(c);
        assert_eq!(r.len(), 1);
        assert!(r.by_id("mock.cpu").is_some());
        assert!(r.by_id("nope").is_none());
        assert_eq!(r.all().len(), 1);
    }

    #[test]
    fn registry_filter_by_source() {
        let mut r = CollectorRegistry::new();
        r.register(MockCollector::new(
            "mock.cpu",
            vec![EventCategory::CpuSample],
            PlatformSet::all(),
            None,
        ));

        let with = r.filter_for_scope(ProfileSourceFlags::CPU_SAMPLING, &GpuTargets::None);
        assert_eq!(with.len(), 1);

        let without = r.filter_for_scope(ProfileSourceFlags::POWER, &GpuTargets::None);
        assert!(without.is_empty());
    }

    #[test]
    fn registry_filter_gpu_vendor() {
        let mut r = CollectorRegistry::new();
        r.register(MockCollector::new(
            "mock.nvidia",
            vec![EventCategory::GpuKernel],
            PlatformSet::all(),
            Some(GpuVendor::Nvidia),
        ));
        r.register(MockCollector::new(
            "mock.amd",
            vec![EventCategory::GpuKernel],
            PlatformSet::all(),
            Some(GpuVendor::Amd),
        ));

        let nv = r.filter_for_scope(
            ProfileSourceFlags::GPU,
            &GpuTargets::ByVendor(GpuVendor::Nvidia),
        );
        assert_eq!(nv.len(), 1);
        assert_eq!(nv[0].id(), "mock.nvidia");

        let none = r.filter_for_scope(ProfileSourceFlags::GPU, &GpuTargets::None);
        assert!(none.is_empty());

        let all = r.filter_for_scope(ProfileSourceFlags::GPU, &GpuTargets::All);
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn registry_filter_platform() {
        let mut r = CollectorRegistry::new();
        // Pick a platform that is NOT the host.
        let host = PlatformSet::current().bits();
        let foreign_bit = if host == PlatformSet::WINDOWS_X64 {
            PlatformSet::LINUX_X64
        } else {
            PlatformSet::WINDOWS_X64
        };
        r.register(MockCollector::new(
            "mock.foreign",
            vec![EventCategory::CpuSample],
            PlatformSet::from_flag(foreign_bit),
            None,
        ));
        let out = r.filter_for_scope(ProfileSourceFlags::CPU_SAMPLING, &GpuTargets::None);
        assert!(
            out.is_empty(),
            "foreign-platform collector must be filtered out"
        );
    }

    #[test]
    fn registry_filter_gpu_collector_not_gated_by_non_gpu_request() {
        // GPU collector should not appear when only CPU sources requested.
        let mut r = CollectorRegistry::new();
        r.register(MockCollector::new(
            "mock.nvidia",
            vec![EventCategory::GpuKernel],
            PlatformSet::all(),
            Some(GpuVendor::Nvidia),
        ));
        let out = r.filter_for_scope(ProfileSourceFlags::CPU_SAMPLING, &GpuTargets::All);
        assert!(out.is_empty());
    }
}
