// =============================================================================
// File: tentanas/rdma.rs — what this node can do over RDMA (plan-02 §5.5a,
//       the "RDMA" row of the Environment tab).
//
//       Two facts decide everything downstream: does the node have an RDMA
//       device with a port that is actually up, and can its kernel speak RPC
//       over RDMA. Both are unprivileged sysfs reads, so the probe runs in the
//       same pass as every other feature.
//
//       The device list comes from `/sys/class/infiniband`, which is the
//       authoritative one — a card whose netdev is down still has an entry
//       there. The netdev and its addresses are joined in from
//       `mesh::roce_config`, the existing RoCE enumerator, rather than walking
//       `/sys/class/net` a second time.
//
//       WHY NOT `rdma link`: the iproute2 tool reads exactly these files, so
//       shelling out would add a package dependency to a question sysfs
//       already answers, and would make the probe fail on a node where RDMA
//       works but the tool is not installed.
// =============================================================================

use std::path::Path;

use tentaflow_protocol::features::FeatureState;

/// The Environment row's feature id (`FEATURES` in environment.rs) and the id
/// the package install uses.
pub const FEATURE_ID: &str = "rdma";

/// The kernel module that carries RPC over RDMA — both the client
/// (`xprtrdma`) and the server (`svcrdma`) side.
///
/// TRAP: `svcrdma` and `xprtrdma` are module ALIASES of this single module,
/// not modules of their own, so `/sys/module/svcrdma` never exists. Probing
/// for it would report "kernel module missing" on every node where NFS over
/// RDMA works perfectly well.
pub const RPCRDMA_MODULE: &str = "rpcrdma";

const INFINIBAND_CLASS: &str = "/sys/class/infiniband";

/// One RDMA device of this node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RdmaDevice {
    /// `mlx5_0`, `rocep1s0f0`, … — the name sysfs indexes the device by.
    pub device: String,
    /// The best port state of the device: `ACTIVE`, `DOWN`, `INIT`, `POLLING`.
    pub state: String,
    /// Whether at least one port of the device is ACTIVE.
    pub active: bool,
    /// The netdev bound to the device, when one is (RoCE always, IB usually).
    pub netdev: String,
    /// The IPv4 addresses of that netdev — what a peer would mount from.
    pub addresses: Vec<String>,
}

impl RdmaDevice {
    /// One line for the Environment row's detail column.
    fn describe(&self) -> String {
        let mut out = format!("{} {}", self.device, self.state);
        if !self.netdev.is_empty() {
            out.push_str(&format!(" ({}", self.netdev));
            if let Some(ip) = self.addresses.first() {
                out.push(' ');
                out.push_str(ip);
            }
            out.push(')');
        }
        out
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Probe {
    pub devices: Vec<RdmaDevice>,
    /// `rpcrdma` is loaded right now.
    pub module_loaded: bool,
    /// `rpcrdma` exists in this kernel's module tree, so the kernel loads it
    /// on demand when nfsd opens the RDMA listener.
    pub module_available: bool,
}

impl Probe {
    /// Whether this node can serve or mount NFS over RDMA right now.
    pub fn ready(&self) -> bool {
        self.devices.iter().any(|d| d.active) && (self.module_loaded || self.module_available)
    }

    /// The addresses a peer can reach this node's RDMA listener on: the ones
    /// bound to a device whose port is up. A DOWN device's address would only
    /// produce mounts that hang until they time out.
    pub fn addresses(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .devices
            .iter()
            .filter(|d| d.active)
            .flat_map(|d| d.addresses.iter().cloned())
            .collect();
        out.sort();
        out.dedup();
        out
    }
}

/// `"4: ACTIVE"` → `"ACTIVE"`. The numeric prefix is the enum value and says
/// nothing the name does not.
fn parse_port_state(raw: &str) -> String {
    let text = raw.trim();
    match text.split_once(':') {
        Some((_, name)) => name.trim().to_string(),
        None => text.to_string(),
    }
}

/// The state of one device, folded over its ports: ACTIVE when any port is,
/// otherwise the first port's state. A dual-port card with one link up is a
/// usable card.
fn device_state(device: &str) -> String {
    let ports = format!("{INFINIBAND_CLASS}/{device}/ports");
    let Ok(entries) = std::fs::read_dir(&ports) else {
        return String::new();
    };
    let mut names: Vec<String> = entries
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    let mut first = String::new();
    for port in names {
        let Ok(raw) = std::fs::read_to_string(format!("{ports}/{port}/state")) else {
            continue;
        };
        let state = parse_port_state(&raw);
        if state == "ACTIVE" {
            return state;
        }
        if first.is_empty() {
            first = state;
        }
    }
    first
}

/// This node's kernel release. Shared with the ksmbd probe, which reports it
/// next to the EXPERIMENTAL note of its own Environment row.
pub fn kernel_release() -> String {
    std::fs::read_to_string("/proc/sys/kernel/osrelease")
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

/// Whether a module is in this kernel's module tree. `modules.dep` is the
/// index depmod writes for exactly this question, and reading it needs no
/// privilege and no `modinfo`.
pub fn module_in_tree(module: &str) -> bool {
    let release = kernel_release();
    if release.is_empty() {
        return false;
    }
    let Ok(text) = std::fs::read_to_string(format!("/lib/modules/{release}/modules.dep")) else {
        return false;
    };
    text.lines()
        .filter_map(|l| l.split(':').next())
        .any(|path| {
            path.rsplit('/')
                .next()
                .is_some_and(|file| file.starts_with(&format!("{module}.ko")))
        })
}

/// Reads the node's RDMA state. Linux-only: `/sys/class/infiniband` does not
/// exist anywhere else, and the probe answers "no devices" there, which is
/// what the limited mode of §3.3 already says about those platforms.
pub fn probe() -> Probe {
    let mut devices = Vec::new();
    if let Ok(entries) = std::fs::read_dir(INFINIBAND_CLASS) {
        // The RoCE enumerator already maps netdev → RDMA device and collects
        // every IPv4 of the netdev, including the secondary ones a storage
        // VLAN may live on.
        let interfaces = crate::mesh::roce_config::enumerate_roce_interfaces();
        for entry in entries.flatten() {
            let device = entry.file_name().to_string_lossy().into_owned();
            let state = device_state(&device);
            let iface = interfaces.iter().find(|i| i.roce_device == device);
            let addresses = iface
                .map(|i| {
                    i.ipv4
                        .iter()
                        .cloned()
                        .chain(i.ipv4_aliases.iter().cloned())
                        .collect()
                })
                .unwrap_or_default();
            devices.push(RdmaDevice {
                active: state == "ACTIVE",
                device,
                state,
                netdev: iface.map(|i| i.netdev.clone()).unwrap_or_default(),
                addresses,
            });
        }
    }
    devices.sort_by(|a, b| a.device.cmp(&b.device));
    Probe {
        devices,
        module_loaded: module_loaded(RPCRDMA_MODULE),
        module_available: module_in_tree(RPCRDMA_MODULE),
    }
}

/// Whether a kernel module is loaded right now. Shared with the ksmbd probe.
pub fn module_loaded(module: &str) -> bool {
    Path::new(&format!("/sys/module/{module}")).is_dir()
}

/// Turns a probe into the Environment row (n16). Split from `probe` so the
/// wording is testable against a fixture instead of against this host.
pub fn describe(probe: &Probe) -> (&'static str, String) {
    let module = if probe.module_loaded {
        format!("{RPCRDMA_MODULE} loaded (provides svcrdma/xprtrdma)")
    } else if probe.module_available {
        format!("{RPCRDMA_MODULE} available, loaded when the listener opens")
    } else {
        format!("{RPCRDMA_MODULE} is not in this kernel's module tree")
    };
    if probe.devices.is_empty() {
        return (
            "no_device",
            format!("no RDMA device under {INFINIBAND_CLASS} · {module}"),
        );
    }
    let devices = probe
        .devices
        .iter()
        .map(RdmaDevice::describe)
        .collect::<Vec<_>>()
        .join(", ");
    let status = if !probe.devices.iter().any(|d| d.active) {
        "no_device"
    } else if !(probe.module_loaded || probe.module_available) {
        "missing_module"
    } else {
        "ok"
    };
    (status, format!("{devices} · {module}"))
}

/// Replaces the generic feature probe's answer for the RDMA row.
///
/// WHY: every other feature is "is this binary there, is this module loaded".
/// RDMA is neither — a node can have `rdma-core` installed and no card, or a
/// card with every port down, and both must read as "not available" rather
/// than as an installed feature. So the generic probe supplies the id and the
/// package list and this supplies the verdict.
pub fn refine(feature: &mut FeatureState) {
    let probe = probe();
    let (status, detail) = describe(&probe);
    feature.status = status.to_string();
    feature.detail = detail;
    feature.version = None;
    feature.kernel_module = Some(RPCRDMA_MODULE.to_string());
}

/// Whether the RDMA row of an environment says this node can use RDMA. The
/// one place the "is it offerable" question is answered, so the share wizard,
/// the config writer and the mount reconcile cannot drift apart.
pub fn available(features: &[FeatureState]) -> bool {
    features
        .iter()
        .any(|f| f.id == FEATURE_ID && f.status == "ok")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(name: &str, state: &str, netdev: &str, ip: &str) -> RdmaDevice {
        RdmaDevice {
            device: name.to_string(),
            active: state == "ACTIVE",
            state: state.to_string(),
            netdev: netdev.to_string(),
            addresses: if ip.is_empty() {
                Vec::new()
            } else {
                vec![ip.to_string()]
            },
        }
    }

    #[test]
    fn port_state_drops_the_enum_prefix_sysfs_prints() {
        // The exact strings /sys/class/infiniband/<dev>/ports/1/state holds.
        assert_eq!(parse_port_state("4: ACTIVE\n"), "ACTIVE");
        assert_eq!(parse_port_state("1: DOWN\n"), "DOWN");
        assert_eq!(parse_port_state("2: INIT\n"), "INIT");
        assert_eq!(parse_port_state("ACTIVE"), "ACTIVE");
        assert_eq!(parse_port_state(""), "");
    }

    #[test]
    fn a_card_with_an_active_port_and_the_module_is_the_only_ok_state() {
        let ready = Probe {
            devices: vec![device("mlx5_0", "ACTIVE", "enp1s0f0np0", "10.10.0.5")],
            module_loaded: true,
            module_available: true,
        };
        let (status, detail) = describe(&ready);
        assert_eq!(status, "ok");
        assert_eq!(
            detail,
            "mlx5_0 ACTIVE (enp1s0f0np0 10.10.0.5) · rpcrdma loaded (provides svcrdma/xprtrdma)"
        );
        assert!(ready.ready());
        assert_eq!(ready.addresses(), vec!["10.10.0.5".to_string()]);

        // A card that is present but whose link is down is not a card the
        // wizard may offer, and installing a package would not change that.
        let down = Probe {
            devices: vec![device("mlx5_0", "DOWN", "enp1s0f0np0", "10.10.0.5")],
            module_loaded: true,
            module_available: true,
        };
        assert_eq!(describe(&down).0, "no_device");
        assert!(!down.ready());
        assert!(down.addresses().is_empty());

        // Hardware without the kernel side: a different problem, said so.
        let no_module = Probe {
            devices: vec![device("mlx5_0", "ACTIVE", "enp1s0f0np0", "10.10.0.5")],
            module_loaded: false,
            module_available: false,
        };
        let (status, detail) = describe(&no_module);
        assert_eq!(status, "missing_module");
        assert!(detail.contains("not in this kernel's module tree"), "{detail}");
        assert!(!no_module.ready());

        // Not loaded but present in the tree: nfsd loads it when it opens the
        // listener, so this is a usable node.
        let on_demand = Probe {
            devices: vec![device("mlx5_0", "ACTIVE", "enp1s0f0np0", "10.10.0.5")],
            module_loaded: false,
            module_available: true,
        };
        assert_eq!(describe(&on_demand).0, "ok");
        assert!(on_demand.ready());
    }

    #[test]
    fn a_node_without_a_card_says_so_instead_of_blaming_the_module() {
        let empty = Probe {
            devices: Vec::new(),
            module_loaded: false,
            module_available: true,
        };
        let (status, detail) = describe(&empty);
        assert_eq!(status, "no_device");
        assert!(detail.starts_with("no RDMA device under /sys/class/infiniband"), "{detail}");
        assert!(!empty.ready());
    }

    #[test]
    fn a_dual_port_card_reports_every_port_and_only_the_up_addresses() {
        let probe = Probe {
            devices: vec![
                device("mlx5_0", "ACTIVE", "enp1s0f0np0", "10.10.0.5"),
                device("mlx5_1", "DOWN", "enp1s0f1np1", "10.20.0.5"),
            ],
            module_loaded: true,
            module_available: true,
        };
        let (status, detail) = describe(&probe);
        assert_eq!(status, "ok");
        assert_eq!(
            detail,
            "mlx5_0 ACTIVE (enp1s0f0np0 10.10.0.5), mlx5_1 DOWN (enp1s0f1np1 10.20.0.5) \
             · rpcrdma loaded (provides svcrdma/xprtrdma)"
        );
        // The down port's address must never be published: a peer mounting it
        // would hang instead of falling back.
        assert_eq!(probe.addresses(), vec!["10.10.0.5".to_string()]);
    }

    #[test]
    fn a_card_with_no_netdev_still_counts_as_a_device() {
        // A pure InfiniBand HCA with no IPoIB interface configured.
        let probe = Probe {
            devices: vec![device("mlx5_2", "ACTIVE", "", "")],
            module_loaded: true,
            module_available: true,
        };
        assert_eq!(describe(&probe).0, "ok");
        assert_eq!(
            describe(&probe).1,
            "mlx5_2 ACTIVE · rpcrdma loaded (provides svcrdma/xprtrdma)"
        );
        // It can serve nothing to the fleet, though: nobody has an address.
        assert!(probe.addresses().is_empty());
    }

    #[test]
    fn availability_reads_the_environment_row_and_nothing_else() {
        let row = |status: &str| FeatureState {
            id: FEATURE_ID.to_string(),
            status: status.to_string(),
            ..Default::default()
        };
        assert!(available(&[row("ok")]));
        assert!(!available(&[row("no_device")]));
        assert!(!available(&[row("missing_module")]));
        assert!(!available(&[]));
        assert!(!available(&[FeatureState {
            id: "nfs".to_string(),
            status: "ok".to_string(),
            ..Default::default()
        }]));
    }
}
