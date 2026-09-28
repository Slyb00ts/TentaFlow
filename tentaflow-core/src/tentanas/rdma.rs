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
//
//       RDMA LISTENERS (`cm_ids`, measured on rig11 2026-09-27, "RDMA
//       listeners" in the MAJOR 27 measurements): an nvmet-rdma port or an
//       iSER portal listens through the RDMA connection manager, which `ss`
//       and `/proc/net/tcp` never see, and configfs keeps claiming a listener
//       the kernel has dropped (device removed and re-added: the port link and
//       `iser = 1` stay, the listener does not come back). The one place the
//       kernel publishes it is the RDMA netlink resource dump — the request
//       `rdma res show cm_id` makes — and it answers an UNPRIVILEGED reader
//       byte for byte like root. That question sysfs cannot answer, so this
//       file speaks the netlink itself rather than depending on the tool.
// =============================================================================

use std::path::Path;

use tentaflow_protocol::features::FeatureState;

use super::CodedText;

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
    /// Whether an RDMA portal (an nvmet-rdma port, an iSER flag) can listen on
    /// this device, as the Environment row says it per interface:
    ///
    /// - `no_netdev` / `no_address`: a portal binds an IPv4 ADDRESS, and the
    ///   kernel refuses one no RDMA device carries — MEASURED on rig11
    ///   2026-09-27: the nvmet port link and the `iser = 1` write both fail
    ///   with `ENODEV` on 192.168.11.11 while no RDMA device held it;
    /// - `port_down`: the device holds an address but its link is not
    ///   ACTIVE, so no client reaches it (whether the kernel would bind the
    ///   listener there is NOT measured — rig11's ports have no address);
    /// - `yes`: an ACTIVE port with an address.
    pub fn portal_verdict(&self) -> &'static str {
        if self.netdev.is_empty() {
            "no_netdev"
        } else if self.addresses.is_empty() {
            "no_address"
        } else if !self.active {
            "port_down"
        } else {
            "yes"
        }
    }

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

/// The netdev an RDMA port carries traffic on, from the port's own GID
/// attributes (`ports/<n>/gid_attrs/ndevs/0`).
///
/// MEASURED on rig11 (2026-09-27): `enp4s0np0` / `enp141s0np0` for the mlx5
/// ports (link DOWN, no address), `lo` / `enp5s0` for a Soft-RoCE link. The
/// RoCE enumerator finds a netdev through `/sys/class/net/<if>/device/
/// infiniband`, which a Soft-RoCE (`rdma_rxe`) device does not have — so
/// without this a software RDMA device read as "no interface", and the
/// portal picker never offered RDMA on the interface it runs on.
fn port_netdev(device: &str) -> String {
    let ports = format!("{INFINIBAND_CLASS}/{device}/ports");
    let Ok(entries) = std::fs::read_dir(&ports) else {
        return String::new();
    };
    let mut names: Vec<String> = entries
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
        .iter()
        .find_map(|port| {
            std::fs::read_to_string(format!("{ports}/{port}/gid_attrs/ndevs/0"))
                .ok()
                .map(|n| n.trim().to_string())
                .filter(|n| !n.is_empty())
        })
        .unwrap_or_default()
}

/// Every IPv4 address of one netdev, the loopback excluded (the portal
/// picker lists the same set).
fn netdev_ipv4(netdev: &str) -> Vec<String> {
    let networks = sysinfo::Networks::new_with_refreshed_list();
    let mut out = Vec::new();
    for (name, data) in networks.iter() {
        if name != netdev {
            continue;
        }
        for net in data.ip_networks() {
            if let std::net::IpAddr::V4(v4) = net.addr {
                if !v4.is_loopback() && !out.contains(&v4.to_string()) {
                    out.push(v4.to_string());
                }
            }
        }
    }
    out
}

/// Reads the node's RDMA state. Linux-only: `/sys/class/infiniband` does not
/// exist anywhere else, and the probe answers "no devices" there, which is
/// what the limited mode of §3.3 already says about those platforms.
pub fn probe() -> Probe {
    Probe {
        devices: devices(),
        module_loaded: module_loaded(RPCRDMA_MODULE),
        module_available: module_in_tree(RPCRDMA_MODULE),
    }
}

/// The RDMA devices of this node, without the `rpcrdma` module questions
/// `probe` also answers: a listener read only needs to know which device
/// holds an address, and the module check scans `modules.dep`.
pub fn devices() -> Vec<RdmaDevice> {
    devices_with_netdevs().0
}

/// `devices`, and the netdev → device map (`netdev_devices`) it was built
/// with, so a caller that needs both reads sysfs once (critic RDMA r3,
/// MINOR 1: `targets::interfaces` used to walk it a second time).
pub fn devices_with_netdevs() -> (Vec<RdmaDevice>, std::collections::BTreeMap<String, String>) {
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
            let (netdev, addresses) = match iface {
                Some(i) => (
                    i.netdev.clone(),
                    i.ipv4.iter().cloned().chain(i.ipv4_aliases.iter().cloned()).collect(),
                ),
                // A device the enumerator does not map (Soft-RoCE): its own
                // port says which netdev it runs on.
                None => {
                    let netdev = port_netdev(&device);
                    let addresses = if netdev.is_empty() { Vec::new() } else { netdev_ipv4(&netdev) };
                    (netdev, addresses)
                }
            };
            devices.push(RdmaDevice {
                active: state == "ACTIVE",
                device,
                state,
                netdev,
                addresses,
            });
        }
    }
    devices.sort_by(|a, b| a.device.cmp(&b.device));
    // The addresses of the netdevs stacked on a device (a VLAN, a bond:
    // `netdev_devices`) are the device's too: an RDMA listener binds there.
    let stacked = netdev_devices(&devices);
    if stacked.len() > devices.iter().filter(|d| !d.netdev.is_empty()).count() {
        let networks = sysinfo::Networks::new_with_refreshed_list();
        for (name, data) in networks.iter() {
            let Some(owner) = stacked.get(name) else { continue };
            let Some(device) = devices.iter_mut().find(|d| &d.device == owner) else { continue };
            if device.netdev == *name {
                continue;
            }
            for net in data.ip_networks() {
                if let std::net::IpAddr::V4(v4) = net.addr {
                    if !v4.is_loopback() && !device.addresses.contains(&v4.to_string()) {
                        device.addresses.push(v4.to_string());
                    }
                }
            }
        }
    }
    (devices, stacked)
}

const NET_CLASS: &str = "/sys/class/net";

/// Every netdev of the node that an RDMA device carries, mapped to that
/// device: the device's own netdev, and the netdevs STACKED on it — a VLAN
/// over a RoCE NIC, a bond whose slaves are RoCE NICs (critic RDMA r1/r2,
/// MINOR 4: keyed by the netdev name alone, both were refused).
///
/// FROM THE KERNEL SOURCE (drivers/infiniband/core/roce_gid_mgmt.c and
/// cma.c), not measured — rig11 has neither a VLAN nor a bond on its RoCE
/// ports: the RoCE GID table of a port holds entries for the port's netdev
/// AND for its upper devices (a VLAN on it; a bond master while the port's
/// netdev is an active slave), each entry naming its netdev
/// (`ports/<n>/gid_attrs/ndevs/<i>`). An RDMA CM bind to an address checks
/// that table for a GID of that address ON the netdev holding it — which is
/// why a listener on a VLAN address works, and why the table is asked first
/// here: it is the kernel's own answer, and for an active-backup bond it
/// names the device of the slave that carries traffic.
///
/// The `lower_*` links of `/sys/class/net/<if>` are the fallback, walked
/// down to a netdev an RDMA device holds: they answer when the GID table
/// has no entry for the upper device yet (no address on it at the moment
/// of the read). They may name a bond's BACKUP slave; the kernel's ENODEV
/// at apply time stays the last word, and a portal this admits wrongly
/// fails there as it did before the refusal existed.
fn netdev_devices(devices: &[RdmaDevice]) -> std::collections::BTreeMap<String, String> {
    netdev_devices_in(devices, Path::new(NET_CLASS), Path::new(INFINIBAND_CLASS))
}

fn netdev_devices_in(devices: &[RdmaDevice], net_root: &Path, ib_root: &Path) -> std::collections::BTreeMap<String, String> {
    let mut out = std::collections::BTreeMap::new();
    for d in devices.iter().filter(|d| !d.netdev.is_empty()) {
        out.entry(d.netdev.clone()).or_insert_with(|| d.device.clone());
    }
    // The whole GID table is `gid_tbl_len` (256 on mlx5) reads per port,
    // almost all of them empty entries that fail — MEASURED on rig11
    // 2026-09-28: 25-28 ms per walk for 2 ports (critic RDMA r3, MINOR 1).
    // Every entry names the port's netdev or an UPPER device of it
    // (roce_gid_mgmt.c), and the kernel links every upper it knows as
    // `upper_<name>` in the lower one's directory. So a device none of whose
    // netdevs has an `upper_*` link has nothing in its table but those
    // netdevs themselves: its ports' first entries say which, and the table
    // is walked only for a device that has a stacked netdev.
    let mut stacked = false;
    for d in devices {
        let device_dir = ib_root.join(&d.device);
        let mut own = port_netdevs(&device_dir);
        if !d.netdev.is_empty() {
            own.insert(d.netdev.clone());
        }
        for netdev in &own {
            out.entry(netdev.clone()).or_insert_with(|| d.device.clone());
        }
        if !own.iter().any(|netdev| has_upper(net_root, netdev)) {
            continue;
        }
        stacked = true;
        for netdev in gid_netdevs(&device_dir) {
            out.entry(netdev).or_insert_with(|| d.device.clone());
        }
    }
    // The stacking links, for the netdevs still unresolved. A chain of
    // `lower_*` links that reaches an RDMA netdev ends in an `upper_*` link
    // on that netdev, so with none there is nothing to find.
    if !stacked {
        return out;
    }
    let Ok(entries) = sysfs_read_dir(net_root) else { return out };
    let names: Vec<String> = entries.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    for name in names {
        if out.contains_key(&name) {
            continue;
        }
        if let Some(device) = lower_device(&out, net_root, &name) {
            out.insert(name, device);
        }
    }
    out
}

#[cfg(test)]
thread_local! {
    /// The sysfs reads `netdev_devices_in` made on this thread: a test counts
    /// them instead of timing them.
    static SYSFS_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn sysfs_read_dir(path: &Path) -> std::io::Result<std::fs::ReadDir> {
    #[cfg(test)]
    SYSFS_READS.with(|c| c.set(c.get() + 1));
    std::fs::read_dir(path)
}

fn sysfs_read(path: &Path) -> std::io::Result<String> {
    #[cfg(test)]
    SYSFS_READS.with(|c| c.set(c.get() + 1));
    std::fs::read_to_string(path)
}

/// The netdev each port of one RDMA device carries: the first entry of the
/// port's GID table (`ndevs/0`, what `port_netdev` reads). One read per port.
fn port_netdevs(device_dir: &Path) -> std::collections::BTreeSet<String> {
    let mut out = std::collections::BTreeSet::new();
    let Ok(ports) = sysfs_read_dir(&device_dir.join("ports")) else { return out };
    for port in ports.flatten() {
        if let Ok(name) = sysfs_read(&port.path().join("gid_attrs").join("ndevs").join("0")) {
            let name = name.trim();
            if !name.is_empty() {
                out.insert(name.to_string());
            }
        }
    }
    out
}

/// Whether some device is stacked on `netdev` (a VLAN, a bond master, a
/// macvlan): the kernel's `upper_<name>` link in its directory.
fn has_upper(net_root: &Path, netdev: &str) -> bool {
    let Ok(entries) = sysfs_read_dir(&net_root.join(netdev)) else { return false };
    entries.flatten().any(|e| e.file_name().to_string_lossy().starts_with("upper_"))
}

/// The netdev names the GID table of one RDMA device lists, over all its
/// ports. An empty entry reads as an error and is skipped.
fn gid_netdevs(device_dir: &Path) -> std::collections::BTreeSet<String> {
    let mut out = std::collections::BTreeSet::new();
    let Ok(ports) = sysfs_read_dir(&device_dir.join("ports")) else { return out };
    for port in ports.flatten() {
        let Ok(entries) = sysfs_read_dir(&port.path().join("gid_attrs").join("ndevs")) else { continue };
        for entry in entries.flatten() {
            if let Ok(name) = sysfs_read(&entry.path()) {
                let name = name.trim();
                if !name.is_empty() {
                    out.insert(name.to_string());
                }
            }
        }
    }
    out
}

/// The RDMA device under `netdev` through its `lower_<name>` links, breadth
/// first (a VLAN over a bond over two NICs is two levels), with a bound on
/// the walk so a malformed tree cannot loop.
fn lower_device(known: &std::collections::BTreeMap<String, String>, net_root: &Path, netdev: &str) -> Option<String> {
    let mut queue = std::collections::VecDeque::from([netdev.to_string()]);
    let mut seen = std::collections::BTreeSet::new();
    while let Some(name) = queue.pop_front() {
        if !seen.insert(name.clone()) || seen.len() > 32 {
            continue;
        }
        if name != netdev {
            if let Some(device) = known.get(&name) {
                return Some(device.clone());
            }
        }
        let Ok(entries) = sysfs_read_dir(&net_root.join(&name)) else { continue };
        let mut lowers: Vec<String> = entries
            .flatten()
            .filter_map(|e| e.file_name().to_string_lossy().strip_prefix("lower_").map(str::to_string))
            .collect();
        lowers.sort();
        queue.extend(lowers);
    }
    None
}

/// The RDMA device (any port state) whose netdev holds `address`, if one does.
/// The question the kernel asks before it binds an RDMA listener — no such
/// device is its `ENODEV` (measured). `0.0.0.0` is every device.
pub fn device_for_address(probe: &Probe, address: &str) -> Option<String> {
    if address == "0.0.0.0" {
        return probe.devices.first().map(|d| d.device.clone());
    }
    probe
        .devices
        .iter()
        .find(|d| d.addresses.iter().any(|a| a == address))
        .map(|d| d.device.clone())
}

/// Whether a kernel module is loaded right now. Shared with the ksmbd probe.
pub fn module_loaded(module: &str) -> bool {
    Path::new(&format!("/sys/module/{module}")).is_dir()
}

/// Turns a probe into the Environment row (n16). Split from `probe` so the
/// wording is testable against a fixture instead of against this host.
pub fn describe(probe: &Probe) -> (&'static str, String) {
    let (status, detail) = describe_coded(probe);
    (status, detail.text)
}

/// `describe` with the row's detail as codes beside the English, for the
/// Environment tab to word (`NasEnvironment::feature_reasons`). The one
/// place both are built, so the sentence and the codes cannot disagree.
pub fn describe_coded(probe: &Probe) -> (&'static str, CodedText) {
    let module_param = [("module", RPCRDMA_MODULE.to_string())];
    let module = if probe.module_loaded {
        CodedText::new("module_loaded", &module_param, format!("{RPCRDMA_MODULE} loaded (provides svcrdma/xprtrdma)"))
    } else if probe.module_available {
        CodedText::new("module_on_demand", &module_param, format!("{RPCRDMA_MODULE} available, loaded when the listener opens"))
    } else {
        CodedText::new("module_absent", &module_param, format!("{RPCRDMA_MODULE} is not in this kernel's module tree"))
    };
    if probe.devices.is_empty() {
        return (
            "no_device",
            CodedText::new(
                "rdma_no_device",
                &[("path", INFINIBAND_CLASS.to_string())],
                format!("no RDMA device under {INFINIBAND_CLASS}"),
            )
            .and(module),
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
    // One code per device, so the screen can say per INTERFACE whether it
    // can carry an RDMA portal; the English sentence stays the one list.
    let mut coded = CodedText { text: devices, reasons: Vec::new() };
    coded.reasons.extend(probe.devices.iter().map(|d| {
        super::disks::coded_reason(
            "rdma_device",
            &[
                ("device", d.device.clone()),
                ("state", d.state.clone()),
                ("netdev", d.netdev.clone()),
                ("addresses", d.addresses.join(", ")),
                ("portal", d.portal_verdict().to_string()),
            ],
        )
    }));
    (status, coded.and(module))
}

/// Replaces the generic feature probe's answer for the RDMA row.
///
/// WHY: every other feature is "is this binary there, is this module loaded".
/// RDMA is neither — a node can have `rdma-core` installed and no card, or a
/// card with every port down, and both must read as "not available" rather
/// than as an installed feature. So the generic probe supplies the id and the
/// package list and this supplies the verdict.
pub fn refine(feature: &mut FeatureState) {
    refine_coded(feature);
}

/// `refine`, answering the row's detail as codes too.
pub fn refine_coded(feature: &mut FeatureState) -> Vec<tentaflow_protocol::tentanas::NasHealthReason> {
    let probe = probe();
    let (status, detail) = describe_coded(&probe);
    feature.status = status.to_string();
    feature.detail = detail.text;
    feature.version = None;
    feature.kernel_module = Some(RPCRDMA_MODULE.to_string());
    detail.reasons
}

/// Whether the RDMA row of an environment says this node can use RDMA. The
/// one place the "is it offerable" question is answered, so the share wizard,
/// the config writer and the mount reconcile cannot drift apart.
pub fn available(features: &[FeatureState]) -> bool {
    features
        .iter()
        .any(|f| f.id == FEATURE_ID && f.status == "ok")
}

// =============================================================================
// RDMA CM ids: who listens and who is connected over RDMA (netlink `nldev`)
// =============================================================================

/// One RDMA connection-manager id as the kernel's resource tracker lists it.
///
/// Only what a screen may use is kept: the device (a kernel name), the state,
/// the addresses and the kernel module that owns it. The id number, the QP
/// number and the owning PID the dump also carries are dropped here, so they
/// cannot reach the wire by accident (no ids in the GUI).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CmId {
    /// `rocep4s0`, `m27rxe`, …
    pub device: String,
    /// The CM state as `rdma res show` names it: `LISTEN`, `CONNECT`, …
    pub state: String,
    pub src: Option<std::net::SocketAddr>,
    pub dst: Option<std::net::SocketAddr>,
    /// The kernel module that owns a kernel CM id (`nvmet_rdma`, `ib_isert`,
    /// `nvme_rdma`, `ib_iser`); empty for one a user-space process owns.
    pub owner: String,
}

/// The kernel module names that own a target's listeners and connections
/// (`KBUILD_MODNAME`), as measured on rig11.
pub const OWNER_NVMET: &str = "nvmet_rdma";
pub const OWNER_ISERT: &str = "ib_isert";

/// `enum rdma_cm_state` (kernel `cma_priv.h`), in the order iproute2 prints it.
const CM_STATES: [&str; 11] = [
    "IDLE",
    "ADDR_QUERY",
    "ADDR_RESOLVED",
    "ROUTE_QUERY",
    "ROUTE_RESOLVED",
    "CONNECT",
    "DISCONNECT",
    "ADDR_BOUND",
    "LISTEN",
    "DEVICE_REMOVAL",
    "DESTROYING",
];

// uapi `rdma/rdma_netlink.h` (values checked against the header; the dump
// below is the request `rdma res show cm_id` makes).
const NETLINK_RDMA: i32 = 20;
// Address families in the Linux netlink ABI, fixed regardless of the OS the
// parser is built on (libc::AF_INET6 is 30 on macOS, 23 on Windows).
const LINUX_AF_INET: u16 = 2;
const LINUX_AF_INET6: u16 = 10;
const RDMA_NL_NLDEV: u16 = 5;
const NLDEV_CMD_GET: u16 = 1;
const NLDEV_CMD_RES_CM_ID_GET: u16 = 11;
const ATTR_DEV_INDEX: u16 = 1;
const ATTR_DEV_NAME: u16 = 2;
const ATTR_RES_STATE: u16 = 27;
const ATTR_RES_KERN_NAME: u16 = 29;
const ATTR_RES_CM_ID: u16 = 30;
const ATTR_RES_CM_ID_ENTRY: u16 = 31;
const ATTR_RES_SRC_ADDR: u16 = 33;
const ATTR_RES_DST_ADDR: u16 = 34;
const NLMSG_ERROR: u16 = 2;
const NLMSG_DONE: u16 = 3;
/// `nla_type` without `NLA_F_NESTED` / `NLA_F_NET_BYTEORDER`.
const NLA_TYPE_MASK: u16 = 0x3fff;

const fn nldev_type(cmd: u16) -> u16 {
    (RDMA_NL_NLDEV << 10) + cmd
}

/// The attributes of one netlink payload: `(type, value)`, in order.
fn attributes(mut buf: &[u8]) -> Vec<(u16, &[u8])> {
    let mut out = Vec::new();
    while buf.len() >= 4 {
        let len = u16::from_ne_bytes([buf[0], buf[1]]) as usize;
        let kind = u16::from_ne_bytes([buf[2], buf[3]]) & NLA_TYPE_MASK;
        if len < 4 || len > buf.len() {
            break;
        }
        out.push((kind, &buf[4..len]));
        let aligned = (len + 3) & !3;
        buf = &buf[aligned.min(buf.len())..];
    }
    out
}

/// The data messages of one dump reply for `kind`: `Ok(payloads)`, or the
/// kernel's errno when it answered `NLMSG_ERROR`. `done` says whether the
/// dump's end marker was in these bytes.
fn messages(mut buf: &[u8], kind: u16) -> (std::result::Result<Vec<&[u8]>, i32>, bool) {
    let mut out = Vec::new();
    while buf.len() >= 16 {
        let len = u32::from_ne_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
        let msg_type = u16::from_ne_bytes([buf[4], buf[5]]);
        if len < 16 || len > buf.len() {
            break;
        }
        let payload = &buf[16..len];
        match msg_type {
            NLMSG_DONE => return (Ok(out), true),
            NLMSG_ERROR => {
                let errno = payload.get(..4).map(|b| i32::from_ne_bytes([b[0], b[1], b[2], b[3]])).unwrap_or(0);
                return if errno == 0 { (Ok(out), true) } else { (Err(-errno), true) };
            }
            t if t == kind => out.push(payload),
            _ => {}
        }
        buf = &buf[((len + 3) & !3).min(buf.len())..];
    }
    (Ok(out), false)
}

fn attr_u32(value: &[u8]) -> Option<u32> {
    value.get(..4).map(|b| u32::from_ne_bytes([b[0], b[1], b[2], b[3]]))
}

fn attr_str(value: &[u8]) -> String {
    let end = value.iter().position(|b| *b == 0).unwrap_or(value.len());
    String::from_utf8_lossy(&value[..end]).into_owned()
}

/// A `__kernel_sockaddr_storage` as the dump writes it: the family in host
/// order, the port in network order.
fn sockaddr(value: &[u8]) -> Option<std::net::SocketAddr> {
    let family = u16::from_ne_bytes([*value.first()?, *value.get(1)?]);
    let port = u16::from_be_bytes([*value.get(2)?, *value.get(3)?]);
    match family {
        LINUX_AF_INET => {
            let ip: [u8; 4] = value.get(4..8)?.try_into().ok()?;
            Some(std::net::SocketAddr::from((ip, port)))
        }
        LINUX_AF_INET6 => {
            let ip: [u8; 16] = value.get(8..24)?.try_into().ok()?;
            Some(std::net::SocketAddr::from((ip, port)))
        }
        _ => None,
    }
}

/// The devices of an `RDMA_NLDEV_CMD_GET` dump: `(index, name)`.
fn parse_devices(bytes: &[u8]) -> std::result::Result<Vec<(u32, String)>, i32> {
    let (msgs, _) = messages(bytes, nldev_type(NLDEV_CMD_GET));
    let mut out = Vec::new();
    for payload in msgs? {
        let attrs = attributes(payload);
        let index = attrs.iter().find(|(k, _)| *k == ATTR_DEV_INDEX).and_then(|(_, v)| attr_u32(v));
        let name = attrs.iter().find(|(k, _)| *k == ATTR_DEV_NAME).map(|(_, v)| attr_str(v));
        if let (Some(index), Some(name)) = (index, name) {
            out.push((index, name));
        }
    }
    Ok(out)
}

/// The CM ids of an `RDMA_NLDEV_CMD_RES_CM_ID_GET` dump of one device.
fn parse_cm_ids(bytes: &[u8]) -> std::result::Result<Vec<CmId>, i32> {
    let (msgs, _) = messages(bytes, nldev_type(NLDEV_CMD_RES_CM_ID_GET));
    let mut out = Vec::new();
    for payload in msgs? {
        let attrs = attributes(payload);
        let device = attrs.iter().find(|(k, _)| *k == ATTR_DEV_NAME).map(|(_, v)| attr_str(v)).unwrap_or_default();
        for (_, table) in attrs.iter().filter(|(k, _)| *k == ATTR_RES_CM_ID) {
            for (_, entry) in attributes(table).into_iter().filter(|(k, _)| *k == ATTR_RES_CM_ID_ENTRY) {
                let mut id = CmId { device: device.clone(), state: String::new(), src: None, dst: None, owner: String::new() };
                for (kind, value) in attributes(entry) {
                    match kind {
                        ATTR_RES_STATE => {
                            id.state = value
                                .first()
                                .map(|s| CM_STATES.get(*s as usize).map(|n| n.to_string()).unwrap_or_else(|| format!("STATE_{s}")))
                                .unwrap_or_default();
                        }
                        ATTR_RES_SRC_ADDR => id.src = sockaddr(value),
                        ATTR_RES_DST_ADDR => id.dst = sockaddr(value),
                        ATTR_RES_KERN_NAME => id.owner = attr_str(value),
                        _ => {}
                    }
                }
                out.push(id);
            }
        }
    }
    Ok(out)
}

/// One `NLM_F_DUMP` request on a fresh NETLINK_RDMA socket; every byte of
/// the reply up to its end marker. A 2 s receive timeout: a screen read
/// must never hang on a kernel that does not answer.
#[cfg(target_os = "linux")]
fn nldev_dump(cmd: u16, device_index: Option<u32>) -> std::io::Result<Vec<u8>> {
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    // SAFETY: plain socket syscalls on a descriptor this function owns; every
    // pointer handed to the kernel points at a live, correctly sized local.
    unsafe {
        let raw = libc::socket(libc::AF_NETLINK, libc::SOCK_RAW | libc::SOCK_CLOEXEC, NETLINK_RDMA);
        if raw < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let fd = OwnedFd::from_raw_fd(raw);
        let timeout = libc::timeval { tv_sec: 2, tv_usec: 0 };
        libc::setsockopt(
            fd.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_RCVTIMEO,
            (&timeout as *const libc::timeval).cast(),
            std::mem::size_of::<libc::timeval>() as libc::socklen_t,
        );
        let mut local: libc::sockaddr_nl = std::mem::zeroed();
        local.nl_family = libc::AF_NETLINK as libc::sa_family_t;
        if libc::bind(
            fd.as_raw_fd(),
            (&local as *const libc::sockaddr_nl).cast(),
            std::mem::size_of::<libc::sockaddr_nl>() as libc::socklen_t,
        ) < 0
        {
            return Err(std::io::Error::last_os_error());
        }
        let mut request = Vec::with_capacity(24);
        let len: u32 = if device_index.is_some() { 24 } else { 16 };
        request.extend_from_slice(&len.to_ne_bytes());
        request.extend_from_slice(&nldev_type(cmd).to_ne_bytes());
        request.extend_from_slice(&((libc::NLM_F_REQUEST | libc::NLM_F_DUMP) as u16).to_ne_bytes());
        request.extend_from_slice(&1u32.to_ne_bytes());
        request.extend_from_slice(&0u32.to_ne_bytes());
        if let Some(index) = device_index {
            request.extend_from_slice(&8u16.to_ne_bytes());
            request.extend_from_slice(&ATTR_DEV_INDEX.to_ne_bytes());
            request.extend_from_slice(&index.to_ne_bytes());
        }
        if libc::send(fd.as_raw_fd(), request.as_ptr().cast(), request.len(), 0) < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let mut out = Vec::new();
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = libc::recv(fd.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len(), 0);
            if n < 0 {
                return Err(std::io::Error::last_os_error());
            }
            if n == 0 {
                return Ok(out);
            }
            let chunk = &buf[..n as usize];
            out.extend_from_slice(chunk);
            if messages(chunk, nldev_type(cmd)).1 {
                return Ok(out);
            }
        }
    }
}

/// What one read of the CM table found: the ids of every device that
/// answered, and the devices whose own dump failed.
///
/// PER DEVICE, not all-or-nothing (critic RDMA r1, MINOR 8): the device list
/// and each device's ids are separate dumps, so a device removed between the
/// two answers `ENODEV` for its own — it is gone, and so are its ids, which
/// is exactly what "no ids from it" says. Any OTHER failure of one device's
/// dump leaves that device `unread`: a listener that is not among the ids
/// read may be on it, so "lost" cannot be claimed while one is (see
/// `targets::rdma_listener`), but every other listener still reads as what it
/// is instead of the whole table turning into "Nie zmierzono".
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CmTable {
    pub ids: Vec<CmId>,
    /// Devices whose dump failed for a reason other than being gone.
    pub unread: Vec<String>,
}

/// The kernel's "no such device" — what a device removed since the device
/// list answers for its own dump.
const ENODEV: i32 = 19;

/// One device's dump failed: its errno when there is one.
#[derive(Debug)]
enum DumpError {
    Io(std::io::Error),
    Errno(i32),
}

impl DumpError {
    fn gone(&self) -> bool {
        match self {
            DumpError::Io(e) => e.raw_os_error() == Some(ENODEV),
            DumpError::Errno(errno) => *errno == ENODEV,
        }
    }
}

/// The CM ids of `devices`, each read by `dump` (the netlink dump in
/// production, fixtures in the tests), isolated per device.
fn collect_cm_ids(
    devices: Vec<(u32, String)>,
    mut dump: impl FnMut(u32) -> std::io::Result<Vec<u8>>,
) -> CmTable {
    let mut table = CmTable::default();
    for (index, name) in devices {
        let read = dump(index)
            .map_err(DumpError::Io)
            .and_then(|bytes| parse_cm_ids(&bytes).map_err(DumpError::Errno));
        match read {
            Ok(ids) => table.ids.extend(ids),
            Err(e) if e.gone() => {
                tracing::debug!("RDMA netlink {name}: gone since the device list");
            }
            Err(e) => {
                tracing::debug!("RDMA netlink {name}: {e:?}");
                table.unread.push(name);
            }
        }
    }
    table
}

/// Every RDMA CM id of this node, across all devices. `Err` when not even
/// the device list could be read — "nobody listens" and "could not look" are
/// different answers, and the screen says "Nie zmierzono" for the second.
/// One device that fails is isolated (`CmTable::unread`).
///
/// MEASURED on rig11 (2026-09-27): an unprivileged reader gets exactly what
/// root gets; a node without RDMA devices answers an empty list.
///
/// BLOCKING: up to one netlink dump per device, each with a 2 s receive
/// timeout. Async callers run it on `spawn_blocking`.
#[cfg(target_os = "linux")]
pub fn cm_ids() -> std::result::Result<CmTable, String> {
    let bytes = nldev_dump(NLDEV_CMD_GET, None).map_err(|e| format!("RDMA netlink: {e}"))?;
    let devices = parse_devices(&bytes).map_err(|errno| format!("RDMA netlink device list: errno {errno}"))?;
    Ok(collect_cm_ids(devices, |index| nldev_dump(NLDEV_CMD_RES_CM_ID_GET, Some(index))))
}

#[cfg(not(target_os = "linux"))]
pub fn cm_ids() -> std::result::Result<CmTable, String> {
    Err("RDMA netlink exists on Linux only".to_string())
}

/// The devices on which `owner` LISTENs on `address:port`: the address
/// itself, or a wildcard listener of that port. MEASURED: a listener bound
/// to a device's address is ONE entry on that device; one bound to a
/// loopback address is one entry per RDMA device.
pub fn listening_devices(ids: &[CmId], owner: &str, address: &str, port: u32) -> Vec<String> {
    let wanted: Option<std::net::IpAddr> = address.parse().ok();
    let mut out: Vec<String> = ids
        .iter()
        .filter(|id| id.state == "LISTEN" && id.owner == owner)
        .filter(|id| {
            id.src.is_some_and(|src| {
                u32::from(src.port()) == port && (Some(src.ip()) == wanted || src.ip().is_unspecified())
            })
        })
        .map(|id| id.device.clone())
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Whether `id` is on this portal's `address:port` (or the portal is every
/// address, `0.0.0.0`, and only the port has to match).
fn on_portal(id: &CmId, address: &str, port: u32) -> bool {
    let wanted: Option<std::net::IpAddr> = address.parse().ok();
    let every = wanted.is_some_and(|ip| ip.is_unspecified());
    id.src.is_some_and(|src| u32::from(src.port()) == port && (every || Some(src.ip()) == wanted))
}

/// The connected peers of `owner` on `address:port`: one entry per peer
/// address with how many CM ids in `CONNECT` it holds there, sorted by
/// address.
///
/// MEASURED on rig11 (2026-09-27, E3): an NVMe-oF host connected over RDMA
/// is one target-side `nvmet_rdma` CM id PER QUEUE (49 for one connect, the
/// admin queue included), each with the port as `src` and the host's
/// address as `dst`. The table names no host — no NQN, no controller — so a
/// count of connections per address is all it can honestly say.
pub fn connected_peers(ids: &[CmId], owner: &str, address: &str, port: u32) -> Vec<(String, u32)> {
    let mut peers: std::collections::BTreeMap<String, u32> = std::collections::BTreeMap::new();
    for id in ids.iter().filter(|id| id.state == "CONNECT" && id.owner == owner && on_portal(id, address, port)) {
        if let Some(dst) = id.dst.filter(|d| !d.ip().is_unspecified()) {
            *peers.entry(dst.ip().to_string()).or_default() += 1;
        }
    }
    peers.into_iter().collect()
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
        // Wave 7: the same row as codes, one per part of the sentence.
        let (_, coded) = describe_coded(&Probe { devices: Vec::new(), module_loaded: false, module_available: true });
        assert_eq!(
            coded.reasons.iter().map(|r| r.code.as_str()).collect::<Vec<_>>(),
            vec!["rdma_no_device", "module_on_demand"]
        );
        assert_eq!(coded.reasons[1].params.get("module").map(String::as_str), Some(RPCRDMA_MODULE));
        let (_, coded) = describe_coded(&Probe {
            devices: vec![device("mlx5_0", "ACTIVE", "enp1s0f0np0", "10.10.0.5")],
            module_loaded: true,
            module_available: true,
        });
        // One code per device, so the row can say per interface whether an
        // RDMA portal can listen there.
        assert_eq!(coded.reasons[0].code, "rdma_device");
        let p = &coded.reasons[0].params;
        assert_eq!(p.get("device").map(String::as_str), Some("mlx5_0"));
        assert_eq!(p.get("state").map(String::as_str), Some("ACTIVE"));
        assert_eq!(p.get("netdev").map(String::as_str), Some("enp1s0f0np0"));
        assert_eq!(p.get("addresses").map(String::as_str), Some("10.10.0.5"));
        assert_eq!(p.get("portal").map(String::as_str), Some("yes"));
        assert_eq!(coded.reasons[1].code, "module_loaded");
        // The sentence is still the one list.
        assert_eq!(coded.text, "mlx5_0 ACTIVE (enp1s0f0np0 10.10.0.5) · rpcrdma loaded (provides svcrdma/xprtrdma)");

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

    /// Critic RDMA r2, MINOR 4: a VLAN over a RoCE NIC and a bond of RoCE
    /// NICs resolve to the RDMA device under them — first from the kernel's
    /// GID table (which names the upper netdev), else down the `lower_*`
    /// links; an interface with no RDMA underneath stays without one.
    #[test]
    fn a_vlan_or_a_bond_on_a_roce_nic_resolves_to_its_rdma_device() {
        let root = tempfile::tempdir().expect("tmp");
        let net = root.path().join("net");
        let ib = root.path().join("infiniband");
        let mkdir = |p: std::path::PathBuf| std::fs::create_dir_all(p).expect("dir");
        let file = |p: std::path::PathBuf, text: &str| {
            std::fs::create_dir_all(p.parent().expect("parent")).expect("dir");
            std::fs::write(p, text).expect("write");
        };
        for nic in ["enp4s0np0", "enp141s0np0", "eno1", "veth0"] {
            mkdir(net.join(nic));
        }
        // The kernel links both ends: `lower_<x>` in the upper device,
        // `upper_<y>` in the lower one.
        let stack = |upper: &str, lower: &str| {
            mkdir(net.join(upper).join(format!("lower_{lower}")));
            mkdir(net.join(lower).join(format!("upper_{upper}")));
        };
        // storage.100 on enp4s0np0 (8021q links its lower device).
        stack("storage.100", "enp4s0np0");
        // bond0 over both mlx5 ports; storage.200 on bond0; a VLAN on a
        // NIC without RDMA.
        stack("bond0", "enp4s0np0");
        stack("bond0", "enp141s0np0");
        stack("storage.200", "bond0");
        stack("lan.5", "eno1");
        let devices = vec![
            device("rocep4s0", "DOWN", "enp4s0np0", ""),
            device("rocep141s0", "DOWN", "enp141s0np0", ""),
        ];

        let map = netdev_devices_in(&devices, &net, &ib);
        let of = |map: &std::collections::BTreeMap<String, String>, n: &str| map.get(n).cloned();
        assert_eq!(of(&map, "enp4s0np0").as_deref(), Some("rocep4s0"));
        assert_eq!(of(&map, "storage.100").as_deref(), Some("rocep4s0"), "a VLAN on the NIC");
        assert_eq!(of(&map, "bond0").as_deref(), Some("rocep141s0"), "the first slave (sorted) of the bond");
        assert_eq!(of(&map, "storage.200").as_deref(), Some("rocep141s0"), "a VLAN on the bond");
        assert_eq!(of(&map, "lan.5"), None);
        assert_eq!(of(&map, "eno1"), None);
        assert_eq!(of(&map, "veth0"), None);

        // The GID table is the kernel's own answer and wins: the active
        // slave of an active-backup bond is the one whose table lists it.
        file(ib.join("rocep4s0/ports/1/gid_attrs/ndevs/0"), "enp4s0np0\n");
        file(ib.join("rocep4s0/ports/1/gid_attrs/ndevs/3"), "bond0\n");
        std::fs::create_dir_all(ib.join("rocep141s0/ports/1/gid_attrs/ndevs")).expect("dir");
        // An unused entry reads as an error in sysfs; a directory stands in.
        mkdir(ib.join("rocep141s0/ports/1/gid_attrs/ndevs/0"));
        let map = netdev_devices_in(&devices, &net, &ib);
        assert_eq!(of(&map, "bond0").as_deref(), Some("rocep4s0"));
        assert_eq!(of(&map, "storage.200").as_deref(), Some("rocep4s0"), "through the bond the table named");
        // An upper the table names with no lower link to follow here (a
        // macvlan: the gate still sees its `upper_mv0`).
        mkdir(net.join("enp141s0np0/upper_mv0"));
        file(ib.join("rocep141s0/ports/1/gid_attrs/ndevs/5"), "mv0\n");
        assert_eq!(of(&netdev_devices_in(&devices, &net, &ib), "mv0").as_deref(), Some("rocep141s0"));

        // A loop in a malformed tree ends.
        mkdir(net.join("a/lower_b"));
        mkdir(net.join("b/lower_a"));
        assert_eq!(of(&netdev_devices_in(&devices, &net, &ib), "a"), None);
    }

    /// Critic RDMA r3, MINOR 1: with nothing stacked on an RDMA netdev the
    /// GID table (256 entries per port, almost all empty) is not walked —
    /// the map comes from the ports' first entries — and with a VLAN on one
    /// NIC only that device's table is.
    #[test]
    fn the_gid_table_is_walked_only_for_a_device_with_a_stacked_netdev() {
        let root = tempfile::tempdir().expect("tmp");
        let net = root.path().join("net");
        let ib = root.path().join("infiniband");
        let mkdir = |p: std::path::PathBuf| std::fs::create_dir_all(p).expect("dir");
        let file = |p: std::path::PathBuf, text: &str| {
            std::fs::create_dir_all(p.parent().expect("parent")).expect("dir");
            std::fs::write(p, text).expect("write");
        };
        for nic in ["enp4s0np0", "enp141s0np0", "eno1", "docker0", "veth0"] {
            mkdir(net.join(nic));
        }
        // Stacking elsewhere (a bridge port) does not open any GID table.
        mkdir(net.join("veth0/upper_docker0"));
        mkdir(net.join("docker0/lower_veth0"));
        // rig11's shape: one port per device, entry 0 names the NIC, the
        // other 255 are empty (an error to read; a directory stands in).
        for (device, nic) in [("rocep4s0", "enp4s0np0"), ("rocep141s0", "enp141s0np0")] {
            let ndevs = ib.join(device).join("ports/1/gid_attrs/ndevs");
            file(ndevs.join("0"), &format!("{nic}\n"));
            for i in 1..256 {
                mkdir(ndevs.join(i.to_string()));
            }
        }
        let devices = vec![
            device("rocep4s0", "DOWN", "enp4s0np0", ""),
            device("rocep141s0", "DOWN", "enp141s0np0", ""),
        ];
        let counted = |devices: &[RdmaDevice]| {
            SYSFS_READS.with(|c| c.set(0));
            let map = netdev_devices_in(devices, &net, &ib);
            (map, SYSFS_READS.with(|c| c.get()))
        };

        let (map, reads) = counted(&devices);
        let expected: std::collections::BTreeMap<String, String> =
            [("enp141s0np0", "rocep141s0"), ("enp4s0np0", "rocep4s0")].map(|(n, d)| (n.to_string(), d.to_string())).into();
        assert_eq!(map, expected);
        // Per device: its ports, one entry per port, its netdev's directory.
        assert_eq!(reads, 6, "no GID table walked");
        // A device the enumerator gave no netdev (Soft-RoCE) is still found
        // from its port.
        let rxe = vec![device("rocep4s0", "DOWN", "", "")];
        assert_eq!(counted(&rxe).0.get("enp4s0np0").map(String::as_str), Some("rocep4s0"));

        // A VLAN on one NIC: that device's table is walked, the other's not.
        mkdir(net.join("enp4s0np0/upper_storage.100"));
        mkdir(net.join("storage.100/lower_enp4s0np0"));
        std::fs::remove_dir(ib.join("rocep4s0/ports/1/gid_attrs/ndevs/1")).expect("rm");
        file(ib.join("rocep4s0/ports/1/gid_attrs/ndevs/1"), "storage.100\n");
        let (map, reads) = counted(&devices);
        assert_eq!(map.get("storage.100").map(String::as_str), Some("rocep4s0"));
        assert!(reads > 256 && reads < 512, "one table of 256 entries, not two: {reads}");
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

    #[test]
    fn the_portal_verdict_per_device_is_what_the_kernel_measured() {
        // rig11's mlx5 ports: a netdev, no address, link DOWN.
        assert_eq!(device("rocep4s0", "DOWN", "enp4s0np0", "").portal_verdict(), "no_address");
        assert_eq!(device("mlx5_2", "ACTIVE", "", "").portal_verdict(), "no_netdev");
        assert_eq!(device("mlx5_0", "DOWN", "enp1s0f0np0", "10.10.0.5").portal_verdict(), "port_down");
        assert_eq!(device("m27rxe", "ACTIVE", "enp5s0", "192.168.11.11").portal_verdict(), "yes");
        // The address → device question the kernel asks before binding.
        let probe = Probe {
            devices: vec![device("rocep4s0", "DOWN", "enp4s0np0", ""), device("m27rxe", "ACTIVE", "enp5s0", "192.168.11.11")],
            module_loaded: true,
            module_available: true,
        };
        assert_eq!(device_for_address(&probe, "192.168.11.11").as_deref(), Some("m27rxe"));
        assert_eq!(device_for_address(&probe, "10.10.0.5"), None);
        assert_eq!(device_for_address(&probe, "0.0.0.0").as_deref(), Some("rocep4s0"));
        assert_eq!(device_for_address(&Probe::default(), "0.0.0.0"), None);
    }

    /// The netlink bytes rig11 answered, verbatim (`cmid.py --raw`, RDMA
    /// listeners 2026-09-27): every message of one dump, end marker included.
    const DEVICES: &str = include_str!("../../tests/fixtures/rdma-nldev-devices.hex");
    const LAN_LISTEN: &str = include_str!("../../tests/fixtures/rdma-nldev-cmid-lan-listen.hex");
    const ISER_CONNECTED: &str = include_str!("../../tests/fixtures/rdma-nldev-cmid-iser-connected.hex");
    const LOOPBACK_LISTEN: &str = include_str!("../../tests/fixtures/rdma-nldev-cmid-loopback-listen.hex");
    // Round 2 E3 (`cmid-raw-nvmet-connected.json`): the m27rxe dump, two
    // data messages and the end marker, while one host was connected.
    const NVMET_CONNECTED: &str = include_str!("../../tests/fixtures/rdma-nldev-cmid-nvmet-connected.hex");

    fn hex(text: &str) -> Vec<u8> {
        let text = text.trim();
        (0..text.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&text[i..i + 2], 16).expect("hex"))
            .collect()
    }

    #[test]
    fn sockaddr_decodes_linux_ipv4_family_and_network_order_port() {
        let mut bytes = [0u8; 16];
        bytes[..2].copy_from_slice(&2u16.to_ne_bytes());
        bytes[2..8].copy_from_slice(&[0x11, 0x44, 192, 168, 11, 11]);

        assert_eq!(sockaddr(&bytes), Some("192.168.11.11:4420".parse().unwrap()));
    }

    #[test]
    fn sockaddr_decodes_linux_ipv6_family_and_network_order_port() {
        let mut bytes = [0u8; 28];
        bytes[..2].copy_from_slice(&10u16.to_ne_bytes());
        bytes[2..4].copy_from_slice(&[0x0c, 0xbc]);
        bytes[8..24].copy_from_slice(&[0x20, 0x01, 0x0d, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]);

        assert_eq!(sockaddr(&bytes), Some("[2001:db8::1]:3260".parse().unwrap()));
    }

    #[test]
    fn sockaddr_rejects_truncated_addresses_and_non_linux_families() {
        for (family, required_len) in [(2u16, 8), (10u16, 24)] {
            let mut bytes = [0u8; 28];
            bytes[..2].copy_from_slice(&family.to_ne_bytes());
            for len in 0..required_len {
                assert_eq!(sockaddr(&bytes[..len]), None);
            }
        }
        for family in [0u16, 23, 30, u16::MAX] {
            let mut bytes = [0u8; 28];
            bytes[..2].copy_from_slice(&family.to_ne_bytes());
            assert_eq!(sockaddr(&bytes), None);
        }
    }

    #[test]
    fn the_measured_device_dump_names_every_rdma_device() {
        let devices = parse_devices(&hex(DEVICES)).expect("no errno");
        assert_eq!(
            devices,
            vec![(0, "rocep4s0".to_string()), (1, "rocep141s0".to_string()), (4, "m27rxe".to_string())]
        );
    }

    #[test]
    fn a_listener_on_an_address_is_one_entry_on_the_device_that_holds_it() {
        let ids = parse_cm_ids(&hex(LAN_LISTEN)).expect("no errno");
        assert_eq!(ids.len(), 2, "{ids:?}");
        assert_eq!(ids[0].device, "m27rxe");
        assert_eq!(ids[0].state, "LISTEN");
        assert_eq!(ids[0].owner, OWNER_NVMET);
        assert_eq!(ids[0].src, Some("192.168.11.11:4420".parse().unwrap()));
        assert_eq!(ids[0].dst, Some("0.0.0.0:0".parse().unwrap()));
        assert_eq!(ids[1].owner, OWNER_ISERT);
        assert_eq!(ids[1].src, Some("192.168.11.11:3260".parse().unwrap()));
        assert_eq!(listening_devices(&ids, OWNER_NVMET, "192.168.11.11", 4420), vec!["m27rxe"]);
        assert_eq!(listening_devices(&ids, OWNER_ISERT, "192.168.11.11", 3260), vec!["m27rxe"]);
        // The other module's listener, another port or another address is
        // not this portal's listener.
        assert!(listening_devices(&ids, OWNER_ISERT, "192.168.11.11", 4420).is_empty());
        assert!(listening_devices(&ids, OWNER_NVMET, "192.168.11.11", 3260).is_empty());
        assert!(listening_devices(&ids, OWNER_NVMET, "10.10.0.5", 4420).is_empty());
    }

    #[test]
    fn an_iser_session_is_a_connected_cm_id_of_ib_isert_with_the_initiator_as_peer() {
        let ids = parse_cm_ids(&hex(ISER_CONNECTED)).expect("no errno");
        let target_side: Vec<&CmId> = ids.iter().filter(|i| i.state == "CONNECT" && i.owner == OWNER_ISERT).collect();
        assert_eq!(target_side.len(), 1, "{ids:?}");
        assert_eq!(target_side[0].src.map(|a| a.port()), Some(3260));
        assert_eq!(target_side[0].dst.map(|a| a.ip().to_string()).as_deref(), Some("192.168.11.11"));
        // The initiator's own half (the test ran on one host) is `ib_iser`.
        assert_eq!(ids.iter().filter(|i| i.state == "CONNECT" && i.owner == "ib_iser").count(), 1);
        // The listeners are still there while the session runs.
        assert_eq!(listening_devices(&ids, OWNER_ISERT, "192.168.11.11", 3260), vec!["m27rxe"]);
    }

    #[test]
    fn a_loopback_listener_is_listed_on_every_device_without_a_port() {
        // Round 1: 127.0.0.1 bound on rocep4s0 although its link is DOWN.
        let ids = parse_cm_ids(&hex(LOOPBACK_LISTEN)).expect("no errno");
        assert_eq!(ids.len(), 2, "{ids:?}");
        assert!(ids.iter().all(|i| i.device == "rocep4s0" && i.state == "LISTEN"));
        assert_eq!(listening_devices(&ids, OWNER_NVMET, "127.0.0.1", 4420), vec!["rocep4s0"]);
        assert_eq!(listening_devices(&ids, OWNER_ISERT, "127.0.0.1", 3260), vec!["rocep4s0"]);
    }

    /// An `NLMSG_ERROR` reply carrying `-errno`.
    fn nl_error(errno: i32) -> Vec<u8> {
        let mut error = Vec::new();
        error.extend_from_slice(&36u32.to_ne_bytes());
        error.extend_from_slice(&NLMSG_ERROR.to_ne_bytes());
        error.extend_from_slice(&0u16.to_ne_bytes());
        error.extend_from_slice(&[0u8; 8]);
        error.extend_from_slice(&(-errno).to_ne_bytes());
        error.extend_from_slice(&[0u8; 16]);
        error
    }

    #[test]
    fn a_netlink_error_is_an_errno_and_short_bytes_are_not_a_panic() {
        // NLMSG_ERROR carrying -EPERM (1).
        let error = nl_error(1);
        assert_eq!(parse_cm_ids(&error), Err(1));
        assert_eq!(parse_devices(&error), Err(1));
        let bytes = hex(LAN_LISTEN);
        for cut in [0, 3, 15, 16, 40, 200, bytes.len() - 1] {
            let _ = parse_cm_ids(&bytes[..cut]);
        }
        // A cut inside the only data message yields nothing, never garbage.
        assert_eq!(parse_cm_ids(&bytes[..200]), Ok(Vec::new()));
    }

    /// Critic RDMA r1, MINOR 8: a device removed between the device list and
    /// its own dump answers ENODEV — as a netlink errno or as the socket's
    /// error — and takes only ITS ids with it; any other failure of one
    /// device marks that device unread and leaves the rest of the table.
    #[test]
    fn one_device_failing_its_dump_does_not_blank_the_other_devices() {
        let devices = parse_devices(&hex(DEVICES)).expect("no errno");
        let table = collect_cm_ids(devices.clone(), |index| match index {
            0 => Err(std::io::Error::from_raw_os_error(ENODEV)),
            1 => Ok(nl_error(ENODEV)),
            _ => Ok(hex(LAN_LISTEN)),
        });
        assert_eq!(table.unread, Vec::<String>::new(), "a device that is gone is not 'unread'");
        assert_eq!(listening_devices(&table.ids, OWNER_NVMET, "192.168.11.11", 4420), vec!["m27rxe"]);

        let table = collect_cm_ids(devices.clone(), |index| match index {
            0 => Ok(nl_error(1)),
            1 => Err(std::io::Error::from_raw_os_error(11)),
            _ => Ok(hex(LAN_LISTEN)),
        });
        assert_eq!(table.unread, vec!["rocep4s0".to_string(), "rocep141s0".to_string()]);
        assert_eq!(listening_devices(&table.ids, OWNER_ISERT, "192.168.11.11", 3260), vec!["m27rxe"]);

        // And every device answering is the whole table.
        let table = collect_cm_ids(devices, |index| if index == 4 { Ok(hex(LAN_LISTEN)) } else { Ok(Vec::new()) });
        assert_eq!((table.ids.len(), table.unread.len()), (2, 0));
    }

    /// Round 2 E3 (measured): one `nvme connect -t rdma` is 49 target-side
    /// `nvmet_rdma` CONNECT ids from the host's address, one per queue; the
    /// host's own `nvme_rdma` half and the iSER listener are not counted.
    #[test]
    fn an_nvmet_rdma_host_is_one_connection_per_queue_counted_by_its_address() {
        let ids = parse_cm_ids(&hex(NVMET_CONNECTED)).expect("no errno");
        assert_eq!(ids.iter().filter(|i| i.state == "CONNECT").count(), 98, "both halves were dumped");
        assert_eq!(connected_peers(&ids, OWNER_NVMET, "192.168.11.11", 4420), vec![("192.168.11.11".to_string(), 49)]);
        assert_eq!(connected_peers(&ids, OWNER_NVMET, "0.0.0.0", 4420), vec![("192.168.11.11".to_string(), 49)]);
        assert!(connected_peers(&ids, OWNER_NVMET, "192.168.11.11", 4421).is_empty());
        assert!(connected_peers(&ids, OWNER_NVMET, "10.10.0.5", 4420).is_empty());
        assert!(connected_peers(&ids, OWNER_ISERT, "192.168.11.11", 3260).is_empty());
        assert_eq!(listening_devices(&ids, OWNER_NVMET, "192.168.11.11", 4420), vec!["m27rxe"]);
    }

    /// The live netlink path on this host: it must answer, and answer whole,
    /// wherever an RDMA device exists. Skips (passes without asserting) on a
    /// host without one — there NETLINK_RDMA may not even be registered.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_live_cm_table_reads_on_a_host_with_rdma_devices() {
        let present = std::fs::read_dir(INFINIBAND_CLASS).map(|d| d.count()).unwrap_or(0);
        if present == 0 {
            eprintln!("skipped: no RDMA device in {INFINIBAND_CLASS}");
            return;
        }
        let table = cm_ids().expect("the CM table reads where RDMA devices exist");
        assert!(table.unread.is_empty(), "every device answered: {table:?}");
        assert!(table.ids.iter().all(|id| !id.device.is_empty() && !id.state.is_empty()), "{table:?}");
    }
}
