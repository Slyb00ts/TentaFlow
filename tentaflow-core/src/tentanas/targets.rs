// =============================================================================
// File: tentanas/targets.rs — block export: iSCSI targets and NVMe-oF
//       subsystems (plan-02 §5.5, transports §5.5a, ALUA/ANA from the first
//       version per research R8).
//
// Same shape as shares.rs: `tentanas.db` holds the DESIRED state, `apply`
// rebuilds the whole node from it, and nothing is ever read back out of the
// kernel to decide what should exist. The difference from the file protocols
// is that configfs is EMPTY after a reboot, so `apply` is also the restore —
// §3.4 fixes this app as the only thing that recreates targets, with no
// `targetcli saveconfig` and no `target.service` as a second source of truth.
//
// The rendering itself lives in `tentanas_helper::block`, next to the catalog
// entry that executes it, so the preview this module shows and the tree the
// root side writes come out of one function.
//
// SECURITY MODEL, stated once here because every function below assumes it:
// an IQN/NQN is a string the CLIENT sends about itself. The allowlist is a
// convenience filter and NOT authentication; only (mutual) CHAP and
// DH-HMAC-CHAP authenticate. A block export also hands the client a raw disk
// with no file ACLs on it, and two clients on one LUN without a cluster
// filesystem destroy each other's data. The wizard says all of this; this
// module refuses the cases it can decide on its own (a portal on 0.0.0.0
// without a deliberate confirmation, a zvol two targets would share).
// =============================================================================

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use anyhow::{anyhow, Result};
use tentaflow_protocol::features::FeatureState;
use tentaflow_protocol::tentanas::{
    NasBlockCapabilities, NasBlockInterface, NasBlockVolume, NasDataset, NasShareService,
    NasShareSession, NasTarget, NasTargetAuth, NasTargetLun, NasTargetPortGroup, NasTargetPortal,
};
use tentanas_helper::block::{
    self, BlockLun, BlockPortGroup, BlockPortal, IscsiAuth, IscsiTargetSpec, NvmetHost,
    NvmetSubsystemSpec,
};
use tentanas_helper::HelperCommand;
use zeroize::Zeroizing;

use super::db::{self as store, TargetRow};
use crate::crypto::SettingsCipher;
use crate::db::DbPool;
use crate::profiling::collectors::elevation::ElevationToken;

const APPLY_TIMEOUT: Duration = Duration::from_secs(120);

/// The `Nazwa` column of n12 becomes the last component of the IQN/NQN, so it
/// has to survive being a configfs directory name: lowercase letters, digits,
/// `-` and `.`.
const NAME_MAX: usize = 64;

/// The naming authority of the WWNs this node generates. `iqn.`/`nqn.` +
/// `<year>-<month>` + a reverse domain is what both specifications ask for;
/// the app owns `tentaflow.local` because a node cannot know the operator's
/// domain and a made-up one would collide with a real company's.
const WWN_AUTHORITY: &str = "2026-09.local.tentaflow";

/// Where a kernel keeps the configuration it was built with, when it keeps it
/// at all. Reading either is unprivileged.
const KCONFIG_PATHS: &[&str] = &["/proc/config.gz", "/boot/config"];

// =============================================================================
// What this node can serve
// =============================================================================

/// The verdict of one `CONFIG_*` line, or None when the file does not mention
/// the symbol at all.
fn kconfig_verdict(text: &str, symbol: &str) -> Option<bool> {
    for line in text.lines() {
        let line = line.trim();
        if line == format!("{symbol}=y") || line == format!("{symbol}=m") {
            return Some(true);
        }
        if line == format!("# {symbol} is not set") {
            return Some(false);
        }
    }
    None
}

/// This kernel's build configuration, from whichever place it publishes it.
///
/// `/proc/config.gz` (CONFIG_IKCONFIG_PROC) is the authoritative one — it comes
/// from the running kernel rather than from a file some distribution may or may
/// not install — so it is read FIRST and it is gzip, which is why `flate2` is
/// used here. Arch and its derivatives ship only this one; Debian and Fedora
/// ship only `/boot/config-<release>`.
fn kernel_config() -> Option<(String, String)> {
    let release = super::rdma::kernel_release();
    for path in KCONFIG_PATHS {
        if *path == "/proc/config.gz" {
            let Ok(raw) = std::fs::read(path) else {
                continue;
            };
            let mut text = String::new();
            if std::io::Read::read_to_string(
                &mut flate2::read::GzDecoder::new(&raw[..]),
                &mut text,
            )
            .is_ok()
                && !text.is_empty()
            {
                return Some((path.to_string(), text));
            }
            continue;
        }
        let candidates = if release.is_empty() {
            vec![path.to_string()]
        } else {
            vec![format!("{path}-{release}"), path.to_string()]
        };
        for candidate in candidates {
            if let Ok(text) = std::fs::read_to_string(&candidate) {
                return Some((candidate, text));
            }
        }
    }
    None
}

/// Whether this kernel has `CONFIG_NVME_TARGET_AUTH`, and the sentence the
/// Environment row shows when it does not.
///
/// TRAP: there is no unprivileged RUNTIME probe for this. nvmet only grows
/// `hosts/<nqn>/dhchap_key` INSIDE a host object, and creating one needs root,
/// so a read-only check cannot see the attribute. The kernel's own build
/// configuration is the honest source — and when the kernel publishes none at
/// all, the answer is "unknown", which the UI shows as such instead of
/// guessing that the feature is there.
/// The Environment row id of the DH-HMAC-CHAP probe (n16, §5.5).
pub const DHCHAP_FEATURE_ID: &str = "dhchap";

pub fn dhchap_support() -> (bool, String) {
    match kernel_config() {
        Some((path, text)) => match kconfig_verdict(&text, "CONFIG_NVME_TARGET_AUTH") {
            Some(true) => (
                true,
                format!("CONFIG_NVME_TARGET_AUTH in {path} — DH-HMAC-CHAP available"),
            ),
            Some(false) => (
                false,
                format!("this kernel was built without CONFIG_NVME_TARGET_AUTH ({path})"),
            ),
            None => (
                false,
                format!("{path} does not mention CONFIG_NVME_TARGET_AUTH"),
            ),
        },
        None => (
            false,
            "this kernel publishes no configuration (no /proc/config.gz, no /boot/config-*), \
             so DH-HMAC-CHAP support cannot be confirmed — the wizard does not offer what it \
             cannot confirm"
                .to_string(),
        ),
    }
}

/// Whether the module that carries iSER on the target side is available.
/// `ib_isert` is what turns the `iser` attribute of a portal into something
/// the kernel accepts.
fn iser_module() -> bool {
    super::rdma::module_loaded("ib_isert") || super::rdma::module_in_tree("ib_isert")
}

fn nvmet_rdma_module() -> bool {
    super::rdma::module_loaded("nvmet_rdma") || super::rdma::module_in_tree("nvmet-rdma")
}

/// The kernel modules each block protocol needs, in load order.
///
/// Delegated to the HELPER's list, which is the one `modprobe` actually runs,
/// so the question this side asks ("are these modules in the kernel's tree?")
/// and the answer the root side gives ("these are the modules I loaded")
/// cannot drift apart. Two copies of this list with nothing pinning them
/// together is how a node ends up judged appliable on one module set while the
/// apply loads another — round 3's cold-boot blocker, arriving by a different
/// door.
pub fn modules_for(protocol: &str) -> &'static [&'static str] {
    block::modules_for(protocol)
}

/// Whether this node can serve a block protocol, and why not when it cannot.
///
/// Asked of the KERNEL, never of `targetcli`/`nvmetcli`. This app talks to
/// configfs directly and its catalog has no entry that runs either tool
/// (there is a test pinning that), so gating the feature on those binaries
/// would refuse a node whose LIO works perfectly and demand a package §3.4
/// does not want installed. What matters is: the configfs tree is already
/// there, or the modules exist and can be loaded.
/// The whole decision, as a pure function of the two facts it is made of, so
/// the cold-boot case can be pinned by a test on any host.
///
/// The second disjunct is the one that matters and the one that was missing:
/// `can_serve(false, 0)` — "configfs is NOT there and every module this
/// protocol needs exists in the kernel's tree" — is exactly a node that has
/// just rebooted, and it has to answer YES. Answering no closes a loop with no
/// entrance, because the only thing that ever loads those modules is an apply,
/// and an apply only reaches a target this answered yes for.
fn can_serve(configfs_present: bool, missing_modules: usize) -> bool {
    configfs_present || missing_modules == 0
}

pub fn kernel_support(protocol: &str) -> (bool, String) {
    // FIRST, before the configfs check: an unknown protocol used to fall
    // through to LIO's tree, which exists on any node that ever served iSCSI —
    // so "is `tentanas-nonsense` supported?" answered yes. An empty module
    // list would then also read as "nothing is missing" in `can_serve`.
    let modules = modules_for(protocol);
    if modules.is_empty() {
        return (
            false,
            format!("'{protocol}' is not a block protocol this node serves"),
        );
    }
    let root = if protocol == "nvmet" {
        block::NVMET_CONFIGFS
    } else {
        block::TARGET_CONFIGFS
    };
    if Path::new(root).is_dir() {
        return (true, format!("{root} present"));
    }
    let missing: Vec<&str> = modules
        .iter()
        .copied()
        .filter(|m| !super::rdma::module_loaded(m) && !super::rdma::module_in_tree(m))
        .collect();
    if can_serve(false, missing.len()) {
        // Not loaded yet, and nothing else on this node will load them: no
        // `target.service`, by §3.4. The apply does it through the catalog.
        return (
            true,
            format!("{} available, loaded on the first target", modules.join(", ")),
        );
    }
    (
        false,
        format!(
            "this kernel has no {} — the {protocol} target is not built for it",
            missing.join(", ")
        ),
    )
}

/// Replaces the generic feature probe's answer for the `iscsi` and `nvmet`
/// rows of the Environment tab (n16).
///
/// WHY: the generic probe decides on a BINARY (`targetcli`, `nvmetcli`), and
/// this app runs neither — it writes configfs itself, and the catalog has a
/// test pinning that no entry ever calls those tools. A node with a working LIO
/// and no `targetcli-fb` package would otherwise read as "missing" and the
/// wizard would demand a package §3.4 does not want. The kernel is the honest
/// source, so it is the one asked.
///
/// The nvmet row also carries the DH-HMAC-CHAP verdict, which is where §5.5
/// asks for it ("sonda w Środowisku"), and the iSCSI row carries the iSER
/// module state (§5.5a).
pub fn refine(feature: &mut FeatureState) {
    // Nothing is installable for any of these rows: the modules come with the
    // kernel, the build option comes with the kernel, and the userspace tools
    // are exactly what this app does not use.
    feature.version = None;
    feature.binaries = Vec::new();
    feature.packages = Vec::new();

    // DH-HMAC-CHAP is its own row (n16, §5.5), because it is its own
    // question: not "is nvmet here" but "can this kernel authenticate the
    // hosts that connect to it". It has no module and no binary — the answer
    // is a build option — so it never reads `kernel_support`.
    //
    // `missing_module` is the status for "no" because that is the vocabulary
    // n16's chip understands, and it is honest: what is missing IS in the
    // kernel build. `dhchap_support` already carries the distinction between
    // "built without it" and "this kernel publishes no configuration at all",
    // and the detail string says which.
    if feature.id == DHCHAP_FEATURE_ID {
        let (ok, detail) = dhchap_support();
        feature.status = if ok { "ok" } else { "missing_module" }.to_string();
        feature.detail = detail;
        feature.kernel_module = None;
        return;
    }

    let (ok, mut detail) = kernel_support(&feature.id);
    if feature.id == "nvmet" {
        detail.push_str(&format!(
            " · nvmet-rdma {}",
            module_state(nvmet_rdma_module())
        ));
    } else {
        detail.push_str(&format!(" · ib_isert {}", module_state(iser_module())));
    }
    feature.status = if ok { "ok" } else { "missing_module" }.to_string();
    feature.detail = detail;
    feature.kernel_module = modules_for(&feature.id).first().map(|m| m.to_string());
}

fn module_state(available: bool) -> &'static str {
    if available {
        "available"
    } else {
        "not in this kernel's module tree"
    }
}

fn feature_detail(features: &[FeatureState], id: &str) -> String {
    features
        .iter()
        .find(|f| f.id == id)
        .map(|f| f.detail.clone())
        .unwrap_or_default()
}

/// The interfaces the portal picker offers (n14 step 2).
///
/// `shared` marks the one carrying the default route: that is the LAN, and a
/// target without authentication on it is reachable from wherever the LAN
/// reaches — which is the warning §5.5(c) asks for. It is computed here, from
/// the routing table, rather than guessed from a name.
pub fn interfaces() -> Vec<NasBlockInterface> {
    let rdma = super::rdma::probe();
    let rdma_netdevs: Vec<String> = rdma
        .devices
        .iter()
        .filter(|d| d.active && !d.netdev.is_empty())
        .map(|d| d.netdev.clone())
        .collect();
    let default_route = default_route_interface();
    let mut out = Vec::new();
    let networks = sysinfo::Networks::new_with_refreshed_list();
    for (name, data) in networks.iter() {
        for net in data.ip_networks() {
            if net.addr.is_loopback() {
                continue;
            }
            // IPv6 is listed and marked UNSUPPORTED rather than dropped. Both
            // kernels can do it (LIO takes `[addr]:port`, nvmet has
            // `addr_adrfam=ipv6`); this slice does not, and a node whose
            // storage network is IPv6-only would otherwise be handed an empty
            // portal picker with no explanation at all.
            let supported = net.addr.is_ipv4();
            out.push(NasBlockInterface {
                rdma: rdma_netdevs.iter().any(|n| n == name),
                shared: default_route.as_deref() == Some(name.as_str()),
                supported,
                name: name.clone(),
                address: net.addr.to_string(),
            });
        }
    }
    out.sort_by(|a, b| (&a.name, &a.address).cmp(&(&b.name, &b.address)));
    out
}

/// THE definition of "the addresses interface `name` holds that a portal can
/// actually bind", in the order the wizard's picker shows them.
///
/// One function, because there used to be two and they disagreed. The drift
/// check compared a portal against the LIST of an interface's addresses (so an
/// alias was correctly not drift), while the update handler took the FIRST
/// address of the first matching entry — and an interface carrying both
/// `10.10.0.9` (the one the target was bound to) and a newly added `10.10.0.5`
/// would therefore pass the drift check and then be silently rewritten onto
/// `10.10.0.5` by the next save, with `prune_iscsi` removing the live portal
/// underneath every initiator logged in on `10.10.0.9`. Two definitions of one
/// fact is what made that possible, so there is one.
///
/// "Bindable" means IPv4 here (`supported`): this slice does not do IPv6
/// portals, and an interface that has only an IPv6 address must resolve to NO
/// address rather than to an empty string that would produce a portal on
/// nothing.
pub fn bindable_addresses(interfaces: &[NasBlockInterface], name: &str) -> Vec<String> {
    interfaces
        .iter()
        .filter(|i| i.name == name && i.supported)
        .map(|i| i.address.clone())
        .collect()
}

/// The address a NEW portal on this interface gets — the first bindable one,
/// which is the one the picker shows next to the interface name. Used by the
/// create path and by an explicit re-pick, and by nothing else: an existing
/// portal keeps the address it was created with until somebody asks for it to
/// move (owner decision 2026-09-04, §5.5).
pub fn primary_address(interfaces: &[NasBlockInterface], name: &str) -> Option<String> {
    bindable_addresses(interfaces, name).into_iter().next()
}

/// The interface of the node's default route, read from `/proc/net/route`:
/// the destination of `00000000` with the gateway flag. Empty on a node with
/// no default route, and on every platform that has no such file — where the
/// mutating half of this app is off anyway (§3.3).
fn default_route_interface() -> Option<String> {
    parse_default_route(&std::fs::read_to_string("/proc/net/route").ok()?)
}

fn parse_default_route(text: &str) -> Option<String> {
    for line in text.lines().skip(1) {
        let mut fields = line.split_whitespace();
        let iface = fields.next()?;
        let destination = fields.next()?;
        let _gateway = fields.next()?;
        let flags = fields.next().and_then(|f| u32::from_str_radix(f, 16).ok())?;
        // RTF_UP | RTF_GATEWAY on the 0.0.0.0/0 route.
        if destination == "00000000" && flags & 0x0003 == 0x0003 {
            return Some(iface.to_string());
        }
    }
    None
}

/// The zvols the wizard may export, marked with the target that already does.
pub fn volumes(datasets: &[NasDataset], targets: &[TargetRow]) -> Vec<NasBlockVolume> {
    datasets
        .iter()
        .filter(|d| d.kind == "volume")
        .map(|d| NasBlockVolume {
            pool: d.name.split('/').next().unwrap_or_default().to_string(),
            size_bytes: d.volsize_bytes.unwrap_or(0),
            thin: d.thin,
            device_path: device_path(&d.name),
            // A second target on the same zvol is two clients writing one raw
            // disk. The wizard shows the row disabled with the name of the
            // target that holds it (n14 step 2).
            exported_by: targets
                .iter()
                .find(|t| t.luns.iter().any(|l| l.source == d.name))
                .map(|t| t.name.clone())
                .unwrap_or_default(),
            name: d.name.clone(),
        })
        .collect()
}

pub fn device_path(zvol: &str) -> String {
    format!("/dev/zvol/{zvol}")
}

/// Everything the wizard needs to offer exactly what this node can serve.
pub fn capabilities(
    features: &[FeatureState],
    datasets: &[NasDataset],
    targets: &[TargetRow],
) -> NasBlockCapabilities {
    let rdma_ok = super::rdma::available(features);
    let (dhchap, dhchap_detail) = dhchap_support();
    let rdma_detail = if rdma_ok {
        let mut parts = Vec::new();
        if !iser_module() {
            parts.push("ib_isert is not in this kernel's module tree (no iSER)");
        }
        if !nvmet_rdma_module() {
            parts.push("nvmet-rdma is not in this kernel's module tree (no NVMe-oF over RDMA)");
        }
        parts.join("; ")
    } else {
        feature_detail(features, super::rdma::FEATURE_ID)
    };
    // The two protocol rows come from the KERNEL, not from the `targetcli` /
    // `nvmetcli` Environment rows: this app never runs either tool, so a node
    // without those packages can still serve block storage perfectly well.
    let (iscsi, iscsi_detail) = kernel_support("iscsi");
    let (nvmet, nvmet_detail) = kernel_support("nvmet");
    NasBlockCapabilities {
        iscsi,
        nvmet,
        iser: rdma_ok && iser_module(),
        nvme_rdma: rdma_ok && nvmet_rdma_module(),
        dhchap,
        iscsi_detail,
        nvmet_detail,
        rdma_detail,
        dhchap_detail,
        interfaces: interfaces(),
        volumes: volumes(datasets, targets),
    }
}

/// The LIO and nvmet rows of the service table, next to smbd and nfsd.
pub fn services() -> Vec<NasShareService> {
    [
        ("iscsi", block::TARGET_CONFIGFS),
        ("nvmet", block::NVMET_CONFIGFS),
    ]
    .into_iter()
    .map(|(id, configfs)| {
        // "Installed" is whether the KERNEL can serve it — the same question
        // `capabilities` answers, so the two halves of the UI cannot disagree.
        let (installed, detail) = kernel_support(id);
        // "Running" is whether the configfs tree is there: there is no daemon
        // to look for, and the modules are loaded by the first apply.
        let running = Path::new(configfs).is_dir();
        NasShareService {
            protocol: id.to_string(),
            installed,
            running,
            version: None,
            config_path: configfs.to_string(),
            detail,
        }
    })
    .collect()
}

// =============================================================================
// Naming and state
// =============================================================================

pub fn name_valid(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= NAME_MAX
        && name.bytes().next().is_some_and(|b| b.is_ascii_lowercase())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"-.".contains(&b))
}

/// The IQN or NQN this node publishes for a target of the given name.
pub fn wwn_for(protocol: &str, node: &str, name: &str) -> String {
    let prefix = if protocol == "nvmet" { "nqn" } else { "iqn" };
    let node = node
        .to_ascii_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect::<String>();
    format!("{prefix}.{WWN_AUTHORITY}:{node}.{name}")
}

/// What `apply` must DO with a target once `target_state` has judged it.
///
/// The state alone is not enough, because two different errors want opposite
/// actions: a target whose backing zvol is gone has to leave the kernel, and a
/// target whose portal address drifted has to stay exactly as it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disposition {
    /// Make the kernel serve it.
    Apply,
    /// Take it out of the kernel. The admin disabled it, the backing volume is
    /// gone, or the subsystem is not on this node at all — in every one of
    /// those a live export would be handing out something the row no longer
    /// describes.
    Remove,
    /// Report it and touch NOTHING: neither re-plumb it nor take it out.
    ///
    /// OWNER DECISION (2026-09-04), the portal-drift policy. Neither LIO nor
    /// nvmet can bind a portal to an interface NAME — LIO's portal is
    /// `<address>:<port>`, nvmet's is `addr_traddr` — so "portal on storage0"
    /// only ever means "portal on the address storage0 had when it was
    /// picked". When that address moves, the app does not follow it: a block
    /// export that re-binds itself could surface on a network the admin never
    /// chose (the LAN with the default route, say), and a DHCP lease renewal
    /// is not a reason to hand a raw disk to a different segment. It also does
    /// not tear the target down, because a live initiator writing to it must
    /// not lose its disk over an address change either. The admin gets the
    /// error, the alert, and the decision.
    Freeze,
}

/// Whether a target reaches the kernel, why not when it does not, and what
/// `apply` should do about it.
///
/// `installed` is injected the way `shares::share_state` injects it, so the
/// table below is testable on a host with neither LIO nor nvmet.
/// `in_kernel` is whether the configfs object of this target is there RIGHT
/// NOW. It changes nothing about the verdict; it decides whether the drift
/// message may claim the export is still reachable somewhere. After a reboot
/// configfs is empty and the target was never created, so "the export is
/// reachable there" would be a security claim about a NIC that is serving
/// nothing.
pub fn target_state(
    target: &TargetRow,
    volume_exists: bool,
    installed: &dyn Fn(&str) -> bool,
    addresses: &BTreeMap<String, Vec<String>>,
    in_kernel: bool,
) -> (&'static str, String, Disposition) {
    if !target.enabled {
        // A row that ARRIVED disabled keeps the sentence it arrived with.
        //
        // The config import switches targets off for reasons the admin has to
        // read — the secret is not in an export, the portal was "every
        // interface" on somebody else's node — and writes them into exactly
        // this field. Blanking it here meant the very import that wrote the
        // sentence erased it on its next breath, leaving a grey chip and no
        // explanation anywhere but the job log.
        //
        // A target the admin STOPPED is the other case, and it does not keep
        // anything: its previous detail described a target that was running,
        // and leaving that under a stopped row is stale rather than useful.
        // The two are told apart by what the row already says — an imported
        // row is written `disabled` with its reason, an active one is not.
        let carried = if target.state == "disabled" {
            target.state_detail.clone()
        } else {
            String::new()
        };
        return ("disabled", carried, Disposition::Remove);
    }
    if !installed(&target.protocol) {
        let missing = if target.protocol == "nvmet" {
            "the nvmet kernel target is not available on this node"
        } else {
            "the LIO kernel target is not available on this node"
        };
        return ("error", missing.to_string(), Disposition::Remove);
    }
    if !volume_exists {
        return (
            "error",
            "the backing volume does not exist — the target stays out of the kernel".to_string(),
            Disposition::Remove,
        );
    }
    // A portal bound to a named interface follows that interface's ADDRESS,
    // and neither kernel subsystem can bind a netdev (see NasTargetPortal).
    // An address that moved is REPORTED — never re-bound, never torn out; see
    // `Disposition::Freeze` for why the owner chose that over following it.
    //
    // ALL of the interface's addresses count. A storage VLAN commonly lives on
    // a secondary address, and treating an alias as drift would push a
    // perfectly healthy target into an error it never had.
    for portal in &target.portals {
        if portal.interface.is_empty() {
            continue;
        }
        let held = addresses.get(&portal.interface);
        if held.is_some_and(|current| current.iter().any(|a| *a == portal.address)) {
            continue;
        }
        // WHERE the address went matters more than that it went. If another
        // interface of this node holds it now, the kernel is still serving —
        // the portal is bound to the ADDRESS — and the export is reachable on
        // a NIC nobody picked for it, which is the case the owner's policy is
        // written against. The app still does not act on it (that would be the
        // automatic re-plumbing the decision rules out, and tearing a live
        // export down over a moved address is worse), so the sentence has to
        // carry the whole picture.
        let elsewhere = addresses
            .iter()
            .find(|(name, addrs)| {
                name.as_str() != portal.interface && addrs.contains(&portal.address)
            })
            .map(|(name, _)| name.clone());
        let on_the_interface = match held {
            Some(current) => format!("{} now has {}", portal.interface, current.join(", ")),
            None => format!("interface {} is gone from this node", portal.interface),
        };
        // "The export is reachable there" is a claim about what this node is
        // SERVING, so it is only made when the object is actually in the
        // kernel. After a reboot configfs is empty and the freeze keeps it
        // that way, so the address having moved means nothing is listening on
        // it at all — telling an admin a raw disk is exposed on a NIC that
        // serves nothing is the same kind of lie as a measured zero.
        let where_now = match (&elsewhere, in_kernel) {
            (Some(name), true) => format!(
                "the address moved to {name}, which nobody picked for this target — the export \
                 is reachable there"
            ),
            (Some(name), false) => format!(
                "the address moved to {name}, which nobody picked for this target — nothing is \
                 exported on it, because this target is not in the kernel"
            ),
            (None, _) => "no interface of this node has that address".to_string(),
        };
        return (
            "error",
            format!(
                "portal {} is not on {} any more — {on_the_interface}, and {where_now}; the \
                 target stays as it is until an admin re-picks the interface",
                portal.address, portal.interface
            ),
            Disposition::Freeze,
        );
    }
    // Worth saying out loud on every list: this target hands a raw disk to
    // whoever gets past the portal, and nothing past that point is
    // authenticated.
    let open = if target.auth_method == "none" {
        "no authentication — the IQN/NQN allowlist is a filter, not a login"
    } else {
        ""
    };
    // "Active" is a claim about what this node is SERVING, so it is not made
    // about a target the node is not serving. A row can be judged appliable
    // and not be in the kernel for perfectly ordinary reasons — it was saved
    // seconds ago, the pool is still importing, the reconcile has not come
    // round yet — and every one of them used to render as a green chip over an
    // empty kernel, which is how a target lost to a transient could sit
    // "active" forever with nothing behind it.
    //
    // It is `pending`, not `error`: nothing is wrong, the node simply has not
    // done it yet, and the apply sweep on the next tick is what will.
    if !in_kernel {
        let mut detail =
            "saved, but this node is not exporting it yet — the next reconcile applies it"
                .to_string();
        if !open.is_empty() {
            detail.push_str(" · ");
            detail.push_str(open);
        }
        return ("pending", detail, Disposition::Apply);
    }
    ("active", open.to_string(), Disposition::Apply)
}

/// The dedupe key of one target's portal-drift alert. One open row per target,
/// closed again by the same `apply` that finds the portal back where it was.
fn drift_alert_key(target_id: &str) -> String {
    format!("target:{target_id}:portal")
}

/// Closes every alert a target owns, for the delete path: `apply` resolves the
/// alerts of the rows it iterates, and a deleted row is not one of them, so
/// without this the drift alert would stay open on n02/n15 forever with a
/// drill-down to a target that no longer exists.
pub fn forget_alerts(db: &DbPool, target_id: &str) -> Result<()> {
    store::resolve_alert(db, &drift_alert_key(target_id))
}

/// Whether the configfs tree of a protocol is there RIGHT NOW.
///
/// This is "is the module loaded", not "can this node serve the protocol", and
/// the difference decides whether anything comes back after a reboot. It is
/// the right question for exactly one caller — `remove_one`, which must not
/// try to take a target out of a tree that does not exist — and the WRONG
/// question for the verdict: see `kernel_can_serve`.
fn configfs_present(protocol: &str) -> bool {
    let configfs = if protocol == "nvmet" {
        block::NVMET_CONFIGFS
    } else {
        block::TARGET_CONFIGFS
    };
    Path::new(configfs).is_dir()
}

/// The predicate the VERDICT asks: can this node serve the protocol at all?
///
/// TRAP, and it cost this slice a full round. Asking `configfs_present` here
/// closes a loop that has no entrance. §3.4 forbids enabling `target.service`,
/// so on a cold boot nothing has loaded `target_core_mod` and
/// `/sys/kernel/config/target` does not exist. Every row then judges as
/// "the LIO kernel target is not available on this node" with
/// `Disposition::Remove`; `apply`'s applying loop only visits rows judged
/// `Apply`; and the ONE call site of `BlockModulesLoad` lives inside
/// `apply_one`, which only those rows reach. The modules load only once the
/// modules are loaded, so after a reboot no target is ever restored, the
/// restore loop reports success over an empty apply and stops trying, and the
/// only way out is a hand-typed `modprobe` on the node — which is the exact
/// thing §5.5 promises never to need.
///
/// `kernel_support` answers the question the verdict actually has: the tree is
/// there, OR the modules exist in this kernel's tree and the apply can load
/// them. On a node that genuinely cannot serve the protocol it still answers
/// no, and the row still leaves the kernel. On a node that can, the target
/// gets an `Apply` verdict, `apply_one` runs `modprobe`, and configfs appears.
fn kernel_can_serve(protocol: &str) -> bool {
    kernel_support(protocol).0
}

// =============================================================================
// Secrets
// =============================================================================

/// The plaintext credentials of one target on their way to the helper's
/// stdin. `Zeroizing` for the same reason the sudo password is: this is the
/// only place in the core where they exist in the clear.
struct Secrets {
    password: Zeroizing<String>,
    mutual_password: Zeroizing<String>,
}

/// The context an encrypted secret is bound to. A ciphertext moved to another
/// target's row then fails to decrypt instead of authenticating there.
fn secret_context(target_id: &str, field: &str) -> Vec<u8> {
    format!("tentanas/target/{target_id}/{field}").into_bytes()
}

pub fn encrypt_secret(
    cipher: &SettingsCipher,
    target_id: &str,
    field: &str,
    value: &str,
) -> Result<String> {
    Ok(cipher.encrypt_bound(value, &secret_context(target_id, field))?)
}

fn decrypt(cipher: &SettingsCipher, target_id: &str, field: &str, stored: &str) -> Result<String> {
    if stored.is_empty() {
        return Ok(String::new());
    }
    Ok(cipher
        .decrypt_bound(stored, &secret_context(target_id, field))
        .map_err(|e| anyhow!("the stored secret of this target cannot be read: {e}"))?
        .value)
}

fn secrets(cipher: &SettingsCipher, target: &TargetRow) -> Result<Secrets> {
    Ok(Secrets {
        password: Zeroizing::new(decrypt(
            cipher,
            &target.target_id,
            "secret",
            &target.auth_secret,
        )?),
        mutual_password: Zeroizing::new(decrypt(
            cipher,
            &target.target_id,
            "mutual_secret",
            &target.auth_mutual_secret,
        )?),
    })
}

/// Credentials that are the right SHAPE and carry no information — what the
/// preview and the validation are run against, so neither needs a key and
/// neither can leak one. `block::render` prints `***` for them anyway; the
/// shape only has to get them past the catalog's rules, which differ per
/// protocol: iSCSI wants 12 printable characters, nvmet a `DHHC-1:` key.
fn placeholder_secrets(protocol: &str) -> Secrets {
    if protocol == "nvmet" {
        return Secrets {
            password: Zeroizing::new("DHHC-1:00:cGxhY2Vob2xkZXItaG9zdA==:".to_string()),
            mutual_password: Zeroizing::new("DHHC-1:00:cGxhY2Vob2xkZXItY3RybA==:".to_string()),
        };
    }
    Secrets {
        password: Zeroizing::new("xxxxxxxxxxxx".to_string()),
        mutual_password: Zeroizing::new("yyyyyyyyyyyy".to_string()),
    }
}

// =============================================================================
// Desired state -> helper spec
// =============================================================================

fn lun_specs(target: &TargetRow) -> Vec<BlockLun> {
    target
        .luns
        .iter()
        .map(|lun| BlockLun {
            index: lun.index,
            name: format!("tentanas_{}_lun{}", target.name.replace(['.', '-'], "_"), lun.index),
            device_path: lun.device_path.clone(),
            uuid: lun.uuid.clone(),
            group_id: lun.group_id,
        })
        .collect()
}

fn group_specs(target: &TargetRow) -> Vec<BlockPortGroup> {
    target
        .port_groups
        .iter()
        .map(|g| BlockPortGroup {
            group_id: g.group_id,
            state: g.state.clone(),
            preferred: g.preferred,
        })
        .collect()
}

fn portal_specs(target: &TargetRow) -> Vec<BlockPortal> {
    target
        .portals
        .iter()
        .map(|p| BlockPortal {
            address: p.address.clone(),
            port: p.port,
            transport: p.transport.clone(),
        })
        .collect()
}

fn iscsi_spec(target: &TargetRow, secrets: &Secrets) -> IscsiTargetSpec {
    IscsiTargetSpec {
        iqn: target.wwn.clone(),
        luns: lun_specs(target),
        portals: portal_specs(target),
        port_groups: group_specs(target),
        auth: IscsiAuth {
            enabled: target.auth_method != "none",
            mutual: target.auth_method == "mutual-chap",
            userid: target.auth_username.clone(),
            password: secrets.password.to_string(),
            mutual_userid: target.auth_mutual_username.clone(),
            mutual_password: secrets.mutual_password.to_string(),
        },
        initiators: target.initiators.clone(),
    }
}

/// The nvmet subsystem of a target.
///
/// TRAP that decides the shape of this function: nvmet keeps DH-HMAC-CHAP keys
/// on the HOST object, which exists only because the NQN is on the allowlist.
/// So a subsystem with authentication is always allowlisted, and one without
/// an allowlist sets `attr_allow_any_host` — there is no third combination,
/// and iSCSI's TPG-level CHAP has no counterpart. `validate_options` refuses
/// the impossible one before a row is ever written.
fn nvmet_spec(target: &TargetRow, secrets: &Secrets) -> NvmetSubsystemSpec {
    let authenticated = target.auth_method != "none";
    NvmetSubsystemSpec {
        nqn: target.wwn.clone(),
        // nvmet caps `attr_serial` at 20 characters; the target id is a UUID,
        // and its first block is unique enough for `nvme list` to tell two
        // namespaces of one node apart.
        serial: target
            .target_id
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .take(20)
            .collect(),
        namespaces: lun_specs(target),
        portals: portal_specs(target),
        port_groups: group_specs(target),
        hosts: target
            .initiators
            .iter()
            .map(|nqn| NvmetHost {
                nqn: nqn.clone(),
                dhchap_key: if authenticated {
                    secrets.password.to_string()
                } else {
                    String::new()
                },
                dhchap_ctrl_key: if target.auth_method == "dhchap-bidi" {
                    secrets.mutual_password.to_string()
                } else {
                    String::new()
                },
                dhchap_hash: target.dhchap_hash.clone(),
                dhchap_dhgroup: target.dhchap_dhgroup.clone(),
            })
            .collect(),
        allow_any_host: target.initiators.is_empty(),
    }
}

/// The configfs the node would write for this target, rendered with
/// placeholder credentials. Safe to log, to show and to put on the wire:
/// `block::render` prints `***` for every secret and these are not the real
/// ones anyway.
pub fn preview(target: &TargetRow) -> Result<String> {
    preview_in(
        target,
        Path::new(block::NVMET_CONFIGFS),
        Path::new(block::TARGET_CONFIGFS),
    )
}

/// The same, with the two configfs roots injected.
///
/// They used to be hard-coded, which meant every `preview` test ran against
/// the build machine's empty `/sys/kernel/config` — so the branch that decides
/// what a SHARED host renders as, the one thing about this function that was
/// wrong, was unreachable from any test.
fn preview_in(target: &TargetRow, nvmet_root: &Path, iscsi_root: &Path) -> Result<String> {
    let secrets = placeholder_secrets(&target.protocol);
    // Observed the same way the wrapper observes before applying, so the
    // preview shows the steps this node would REALLY take on the NEXT apply —
    // the writes it would skip because the kernel already holds them, and the
    // objects it would REMOVE because the row no longer names them — rather
    // than a first-install plan. configfs is world-readable, so this needs no
    // privilege.
    let steps = if target.protocol == "nvmet" {
        let spec = nvmet_spec(target, &secrets);
        // NOTHING is adjusted here any more. The preview used to force every
        // shared host to `SharedAndAgrees`, which made `plan_nvmet` print
        // "already holds exactly this key" — a claim about a 0600 attribute
        // this process cannot read, on the one screen an admin opens when a
        // target is in `error`. The observation now reports what it could not
        // read (`hosts_unreadable`), the authority answers `SharedAndUnknown`,
        // and the plan says so. One authority, one honest answer, no local
        // exemption.
        let observed = block::observe_nvmet(nvmet_root, &spec);
        block::plan_nvmet(&spec, &observed).map_err(|e| anyhow!("{e}"))?
    } else {
        let spec = iscsi_spec(target, &secrets);
        let observed = block::observe_iscsi(iscsi_root, &spec);
        block::plan_iscsi(&spec, &observed).map_err(|e| anyhow!("{e}"))?
    };
    Ok(block::render(&steps))
}

// =============================================================================
// Validation
// =============================================================================

/// What the node refuses before a row is written, from the desired state alone
/// plus what this node can serve.
///
/// The security rules of §5.5 that a machine can check live here; the ones only
/// a human can decide (is this network really dedicated?) are warnings in the
/// wizard.
pub fn validate_options(
    target: &TargetRow,
    siblings: &[TargetRow],
    caps: &NasBlockCapabilities,
    confirm_all_interfaces: bool,
) -> Result<()> {
    validate_options_with(
        target,
        siblings,
        caps,
        confirm_all_interfaces,
        &object_in_kernel,
    )
}

/// The same with `in_kernel` injected, for the reason `installed` is injected
/// everywhere else in this file: the cross-target rule below turns on whether
/// a sibling's configfs object EXISTS, and a build machine has no configfs at
/// all — so without this seam the only test of that rule would be a test of
/// "nothing is ever in the kernel".
fn validate_options_with(
    target: &TargetRow,
    siblings: &[TargetRow],
    caps: &NasBlockCapabilities,
    confirm_all_interfaces: bool,
    in_kernel: &dyn Fn(&TargetRow) -> bool,
) -> Result<()> {
    if !name_valid(&target.name) {
        return Err(anyhow!(
            "a target name is 1..={NAME_MAX} characters of lowercase letters, digits, '-' and '.', starting with a letter"
        ));
    }
    match target.protocol.as_str() {
        "iscsi" if !caps.iscsi => {
            return Err(anyhow!(
                "this node cannot serve iSCSI: {}",
                if caps.iscsi_detail.is_empty() {
                    "the LIO target is not installed".to_string()
                } else {
                    caps.iscsi_detail.clone()
                }
            ))
        }
        "nvmet" if !caps.nvmet => {
            return Err(anyhow!(
                "this node cannot serve NVMe-oF: {}",
                if caps.nvmet_detail.is_empty() {
                    "the nvmet target is not installed".to_string()
                } else {
                    caps.nvmet_detail.clone()
                }
            ))
        }
        "iscsi" | "nvmet" => {}
        other => return Err(anyhow!("'{other}' is not a block protocol")),
    }
    if target.portals.is_empty() {
        return Err(anyhow!("a target needs at least one portal"));
    }
    for portal in &target.portals {
        // §5.5(a): the portal is bound to a chosen interface by default, and
        // every interface is a deliberate decision the request has to carry.
        if portal.address == "0.0.0.0" && !confirm_all_interfaces {
            return Err(anyhow!(
                "a portal on every interface (0.0.0.0) needs an explicit confirmation — \
                 pick a storage interface instead"
            ));
        }
        match (target.protocol.as_str(), portal.transport.as_str()) {
            ("iscsi", "tcp") => {}
            ("iscsi", "iser") if caps.iser => {}
            ("iscsi", "iser") => {
                return Err(anyhow!("this node cannot serve iSER: {}", caps.rdma_detail))
            }
            ("nvmet", "tcp") => {}
            ("nvmet", "rdma") if caps.nvme_rdma => {}
            ("nvmet", "rdma") => {
                return Err(anyhow!(
                    "this node cannot serve NVMe-oF over RDMA: {}",
                    caps.rdma_detail
                ))
            }
            (_, other) => {
                return Err(anyhow!(
                    "'{other}' is not a transport of {}",
                    target.protocol
                ))
            }
        }
    }
    if target.protocol == "iscsi" && target.portals.len() > 1 {
        // iSER is a flag ON the iSCSI portal, not a second one: the login is
        // TCP either way. Two portals on one target would be two addresses,
        // which the model allows but this slice's wizard does not build.
        return Err(anyhow!("an iSCSI target carries one portal in this version"));
    }
    match target.protocol.as_str() {
        "iscsi" => match target.auth_method.as_str() {
            "none" | "chap" | "mutual-chap" => {}
            other => return Err(anyhow!("'{other}' is not an iSCSI authentication method")),
        },
        _ => match target.auth_method.as_str() {
            "none" => {}
            "dhchap" | "dhchap-bidi" if !caps.dhchap => {
                return Err(anyhow!(
                    "this node cannot serve DH-HMAC-CHAP: {}",
                    caps.dhchap_detail
                ))
            }
            "dhchap" | "dhchap-bidi" => {
                // The whole reason the allowlist stops being optional here.
                if target.initiators.is_empty() {
                    return Err(anyhow!(
                        "nvmet keeps DH-HMAC-CHAP keys on the host objects of the NQN allowlist — \
                         an authenticated subsystem needs at least one allowed host NQN"
                    ));
                }
            }
            other => return Err(anyhow!("'{other}' is not an NVMe-oF authentication method")),
        },
    }
    if target.port_groups.is_empty() {
        return Err(anyhow!("a target needs at least one ALUA/ANA port group"));
    }
    if target.protocol == "nvmet" && target.port_groups.iter().any(|g| g.preferred) {
        return Err(anyhow!(
            "NVMe ANA has no preferred-path flag — express the preference with the group state"
        ));
    }
    host_allowlist_conflict(target, siblings, in_kernel)?;
    // The catalog's own rules judge the rendered spec, so a request cannot get
    // past the core with something the root side would refuse.
    let secrets = placeholder_secrets(&target.protocol);
    if target.protocol == "nvmet" {
        block::validate_nvmet(&nvmet_spec(target, &secrets)).map_err(|e| anyhow!("{e}"))?;
    } else {
        block::validate_iscsi(&iscsi_spec(target, &secrets)).map_err(|e| anyhow!("{e}"))?;
    }
    Ok(())
}

/// Every pair of nvmet rows in `targets` that cannot both be applied, one
/// sentence per pair — for config import, which needs a WARNING rather than a
/// refusal.
///
/// Three things separate this from `host_allowlist_conflict`, and each one was
/// a real defect:
///
///   * it is **not** a refusal. None of these rows is in the kernel at import
///     time, so the node would accept every one of these saves and it is the
///     second APPLY that fails. Refusing would break the subset property that
///     lets a second check exist beside `block::host_verdict` at all.
///   * it does **not** exempt a keyless `dhchap` row. The save-time check must
///     (such a row cannot reach the kernel yet), but an EXPORT carries no
///     secrets at all (§5.8), so at import every authenticated row is keyless
///     — exempting them made the import silent about exactly the pairs it
///     exists to describe.
///   * it is **order-independent**. Asked per row against the rows already
///     written, the pair was announced only when the authenticated row
///     happened to come second; in the other order — the one an export of that
///     same pair produces — nothing was said. This walks the finished set.
///
/// Each pair is reported once, against the later row, and named by both.
pub(crate) fn host_conflicts_in(targets: &[TargetRow]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for (i, row) in targets.iter().enumerate() {
        if row.protocol != "nvmet" || row.initiators.is_empty() {
            continue;
        }
        let ours: Vec<String> = row
            .initiators
            .iter()
            .map(|n| n.trim().to_lowercase())
            .collect();
        for other in &targets[..i] {
            if other.protocol != "nvmet" || other.auth_method == row.auth_method {
                continue;
            }
            let Some(shared) = other
                .initiators
                .iter()
                .find(|n| ours.contains(&n.trim().to_lowercase()))
            else {
                continue;
            };
            out.push((
                row.name.clone(),
                format!(
                    "shares host {shared} with '{}', which asks for different DH-HMAC-CHAP \
                     settings. nvmet keeps those on the host object, which is shared by the whole \
                     node, so only one of these two targets can be applied — the other is refused \
                     until they agree or one of them uses a different host NQN.",
                    other.name
                ),
            ));
        }
    }
    out
}

/// The part of `block::host_verdict`'s question that the DATABASE alone can
/// answer, asked one step earlier so the refusal lands before a zvol is
/// created rather than after.
///
/// THE authority on an nvmet host object is `block::host_verdict`, and it must
/// stay that way: the object is node-wide, it carries the DH-HMAC-CHAP
/// settings, and the only source of truth about what is on it is the kernel.
/// This is not a second authority — it is a strict SUBSET, and the subset
/// property is the entire reason a second check is allowed to exist at all.
/// It is therefore stated here as a claim with its conditions, not as a
/// slogan:
///
/// **It refuses only pairs the node would also refuse.** For that to hold, a
/// sibling has to be able to reach the kernel with settings of its own, so
/// three kinds of sibling are skipped, and each one was a real false refusal:
///
///   * one whose configfs object is NOT there (`in_kernel`). A disabled or
///     frozen target holds no host object, so the node answers `Sole` and
///     accepts the save. This check used to refuse it;
///   * one asking for `dhchap`/`dhchap-bidi` with NO stored secret. That is
///     what an imported row looks like before the admin retypes the key
///     (§5.8); the catalog refuses to render it, so it never creates a host
///     object either. This check used to refuse the *other* target because of
///     it, with a message about an object that does not exist;
///   * one with the same `auth_method` as ours. The methods agreeing does not
///     make the KEYS agree, but the core cannot compare keys — they are
///     ciphertext bound to each target's own id, so equal plaintexts are
///     unequal strings. That case belongs to the node and stays there.
///
/// What is left is the case the node provably refuses either way round: two
/// live nvmet targets sharing a host NQN where one wants a key on the object
/// and the other wants none.
///
/// Comparison is case-insensitive. Not because a capital could otherwise hide
/// a collision — the sibling rows already passed `validate_nqn`, and a row of
/// our own with a capital fails a few lines below — but so that the message
/// the admin gets names the real problem instead of the alphabet.
///
/// `in_kernel` is injected for the same reason `installed` is: this must be
/// testable on a host with no configfs at all.
fn host_allowlist_conflict(
    target: &TargetRow,
    siblings: &[TargetRow],
    in_kernel: &dyn Fn(&TargetRow) -> bool,
) -> Result<()> {
    if target.protocol != "nvmet" || target.initiators.is_empty() {
        return Ok(());
    }
    let ours: Vec<String> = target
        .initiators
        .iter()
        .map(|n| n.trim().to_lowercase())
        .collect();
    for other in siblings {
        if other.target_id == target.target_id || other.protocol != "nvmet" {
            continue;
        }
        if other.auth_method == target.auth_method {
            continue;
        }
        // A sibling that cannot put anything on the object cannot disagree
        // with us about it — see the doc above for why each of these was a
        // refusal the node would not have made.
        if !in_kernel(other) {
            continue;
        }
        if matches!(other.auth_method.as_str(), "dhchap" | "dhchap-bidi")
            && other.auth_secret.is_empty()
        {
            continue;
        }
        let Some(shared) = other
            .initiators
            .iter()
            .find(|n| ours.contains(&n.trim().to_lowercase()))
        else {
            continue;
        };
        let (with_key, without_key) = if target.auth_method == "none" {
            (other.name.as_str(), target.name.as_str())
        } else if other.auth_method == "none" {
            (target.name.as_str(), other.name.as_str())
        } else {
            // Two different authenticated methods (`dhchap` vs `dhchap-bidi`)
            // differ in the CONTROLLER key, which is the same object and the
            // same conflict.
            (target.name.as_str(), other.name.as_str())
        };
        return Err(anyhow!(
            "'{}' already allows host {shared}. nvmet keeps the DH-HMAC-CHAP settings on the host \
             object, which is shared by the whole node, so '{with_key}' and '{without_key}' cannot \
             disagree about it. Give this target its own host NQN, or use the same authentication \
             on both.",
            other.name
        ));
    }
    Ok(())
}

// =============================================================================
// evaluate — the unprivileged half, and the only thing that runs on a tick
// =============================================================================

/// Every interface of this node with the addresses it holds.
///
/// One interface can carry SEVERAL IPv4 addresses — a storage VLAN often lives
/// on a secondary one, which is why the RDMA probe collects aliases too.
/// Folding them into one address per name would make a portal bound to an
/// alias look like it had drifted.
fn interface_addresses() -> BTreeMap<String, Vec<String>> {
    let interfaces = interfaces();
    let mut addresses: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for iface in &interfaces {
        // Through the one shared definition, so "this interface's address"
        // means the same thing here, in the wizard and in `target_update`.
        addresses
            .entry(iface.name.clone())
            .or_insert_with(|| bindable_addresses(&interfaces, &iface.name));
    }
    addresses
}

/// Whether this node's kernel holds the configfs object of this target right
/// now. An unprivileged `is_dir` — configfs is 0755.
fn object_in_kernel(target: &TargetRow) -> bool {
    let path = if target.protocol == "nvmet" {
        format!("{}/subsystems/{}", block::NVMET_CONFIGFS, target.wwn)
    } else {
        format!("{}/iscsi/{}", block::TARGET_CONFIGFS, target.wwn)
    };
    Path::new(&path).is_dir()
}

/// Judges every row, persists what changed and raises or resolves the
/// portal-drift alert. Returns what `apply` should DO with each row.
///
/// It touches NOTHING outside the database: the whole judgement is the node's
/// interface list plus a `stat` of each backing device node plus a `stat` of
/// the configfs object. No privilege, no helper, no `zfs` — which is why it
/// can run on every tick of the restore loop, and why it must.
///
/// `installed` is injected for the same reason `target_state` takes it: the
/// verdict must be testable on a host with neither LIO nor nvmet.
///
/// OWNER DECISION (2026-09-04) #1 depends on exactly that. "Report the drift
/// and wait for an admin" is not a report if the only thing that ever
/// recomputes the state is a mutation: a DHCP lease renewal at 03:00 moves the
/// address, the portal stops answering, and the row stays green until somebody
/// happens to save an unrelated target. Splitting the judging out of `apply`
/// is what turns the decision into something that actually happens.
fn evaluate_rows(
    db: &DbPool,
    targets: &mut [TargetRow],
    installed: &dyn Fn(&str) -> bool,
    // Injected for the same reason `installed` is, and for one more: this
    // function SWEEPS the retry memory on every judgement, so with a global
    // one any test that judged rows quietly emptied the set another test was
    // asserting against. That was the whole reason the three production entry
    // points had no test — the seam is one parameter wide.
    retries: &RetryMemory,
    log: &mut Vec<String>,
) -> Result<BTreeMap<String, Disposition>> {
    let addresses = interface_addresses();
    let mut disposition: BTreeMap<String, Disposition> = BTreeMap::new();
    for target in targets.iter_mut() {
        // Asked of the DEVICE NODE, not of `zfs list`. A zvol the kernel can
        // open is a zvol that exists, and this cannot report "gone" because a
        // `zfs` invocation timed out, lost a race with an import, or vanished
        // in an upgrade — which would otherwise tear every live target out of
        // the kernel and persist `error` in the database.
        let volume_exists = target
            .luns
            .iter()
            .all(|lun| Path::new(&lun.device_path).exists());
        let (state, detail, verdict) = target_state(
            target,
            volume_exists,
            installed,
            &addresses,
            object_in_kernel(target),
        );
        if target.state != state || target.state_detail != detail {
            store::set_target_state(db, &target.target_id, state, &detail)?;
        }
        // The drift alert (§5.5, owner decision 2026-09-04). The admin who
        // picked an interface has to HEAR that the portal is no longer on it,
        // and a state chip on a tab nobody is looking at is not hearing it —
        // the export is silently unreachable until someone opens n12. The
        // alert closes itself when the address comes back, the same way a
        // disk health alert does.
        //
        // The errors are LOGGED, never swallowed: an alert that could not be
        // raised is the difference between an admin hearing about an exposed
        // raw disk and not.
        let key = drift_alert_key(&target.target_id);
        let outcome = if verdict == Disposition::Freeze {
            store::raise_alert(
                db,
                &key,
                "warning",
                "target",
                &target.name,
                &format!("Target {}: the portal address moved", target.name),
                &detail,
            )
            .map(|_| ())
        } else {
            store::resolve_alert(db, &key)
        };
        if let Err(e) = outcome {
            tracing::warn!("tentanas targets: alert {key} not written: {e}");
            log.push(format!("{}: the drift alert was not written: {e}", target.name));
        }
        // Since WHEN the backing volume has been missing. Recorded here
        // because this is the one place that already asks — and recorded as a
        // TIMESTAMP rather than a tick count so that judging the same row
        // twice in one tick (the sweep re-judges under the lock) cannot make
        // the answer arrive sooner. See `removal_is_due`.
        GraceClock::global().note(&target.target_id, volume_exists);
        disposition.insert(target.target_id.clone(), verdict);
        // Logged only when it CHANGED. `state_detail` is non-empty for every
        // target without authentication — the most common configuration there
        // is — so an unconditional line here was 4320 identical INFO lines a
        // day per such target, 43 000 on a ten-target node, burying every line
        // that meant something. `set_target_state` above is already gated this
        // way and so is `resolve_alert`; the log was the one that was not.
        let changed = target.state != state || target.state_detail != detail;
        target.state = state.to_string();
        target.state_detail = detail;
        if changed && !target.state_detail.is_empty() {
            log.push(format!("{}: {}", target.name, target.state_detail));
        }
    }
    // Rows that are gone take their grace clock and their remembered apply
    // failure with them, so a target recreated with a fresh id never inherits
    // an old one's countdown or an old one's retry.
    GraceClock::global().forget_missing(targets);
    retries.forget_missing(targets);
    Ok(disposition)
}

/// How long a backing volume must be CONTINUOUSLY absent before a live export
/// is taken out of the kernel.
///
/// WHY there is a grace period at all: the verdict behind it is one
/// `Path::exists` on `/dev/zvol/...` at one instant, and `sweep_removals` acts
/// on it by cutting a client off from a raw disk mid-write. A udev link that
/// has not appeared yet, a pool re-importing, a `zpool export` an admin is
/// about to undo — all of them read exactly like "this volume is gone
/// forever" to a single `stat`. The drift decision already says a live
/// initiator must not lose its disk over a transient; a device node is the
/// same kind of fact as an address.
///
/// It does NOT delay the report. The row goes red and the detail says what
/// happened on the very first judgement — only the privileged, destructive
/// half waits.
///
/// NINETY SECONDS IS A JUDGEMENT, not a measurement. A udev link appears in
/// seconds; a pool import with a large ARC or dedup table can take minutes;
/// nobody has measured either on the machines this runs on. The failure
/// direction is what makes the guess acceptable: too short costs an
/// interruption the apply sweep closes within twenty seconds, too long leaves
/// a client with I/O errors instead of a disk that cleanly went away — and of
/// those two, the second is the one that does not destroy anything.
const VOLUME_GONE_GRACE: Duration = Duration::from_secs(90);


/// Whether a `Remove` verdict may be ACTED ON now.
///
/// The verdict is honest the moment it is made; this is about the destructive
/// half. Three things produce `Remove` and they do not deserve the same
/// treatment:
///
///   * `!enabled` — the admin pressed "stop". No confirmation needed; that IS
///     the confirmation.
///   * the kernel cannot serve the protocol — unreachable while the object is
///     in configfs, since the tree being there is what `kernel_can_serve`
///     answers yes to.
///   * the backing volume is gone — one `stat`, and the thing it triggers is
///     taking a raw disk away from a client that is writing to it. This one
///     waits for `VOLUME_GONE_GRACE` of continuous absence.
fn removal_is_due(target: &TargetRow) -> bool {
    removal_is_due_with(target, &GraceClock::global())
}

/// The same decision against a GIVEN grace clock.
///
/// The clock is a parameter for the reason `installed` and `in_kernel` are:
/// the global one is process state, and `cargo test` runs tests in parallel.
/// Three tests used to share it under the same target id, winding it backwards
/// and clearing it wholesale — so the two tests guarding the previous round's
/// blocker fix could go either way depending on thread order, in both
/// directions. A test that can pass while the code is broken is worse than no
/// test, and these were the ones watching a client's disk.
fn removal_is_due_with(target: &TargetRow, since: &GraceClock) -> bool {
    if !target.enabled {
        return true;
    }
    let volume_exists = target
        .luns
        .iter()
        .all(|lun| Path::new(&lun.device_path).exists());
    if volume_exists {
        return true;
    }
    since.waited(&target.target_id).is_some_and(|w| w >= VOLUME_GONE_GRACE)
}

/// When each target's backing volume was first seen missing.
///
/// One instance per caller-supplied clock; `global()` is the process-wide one
/// the tick and the sweeps share, and a test makes its own.
struct GraceClock(std::sync::Mutex<BTreeMap<String, std::time::Instant>>);

impl GraceClock {
    fn new() -> Self {
        Self(std::sync::Mutex::new(BTreeMap::new()))
    }

    fn global() -> &'static Self {
        static CLOCK: std::sync::OnceLock<GraceClock> = std::sync::OnceLock::new();
        CLOCK.get_or_init(GraceClock::new)
    }

    /// `or_insert_with`, never `insert`: judging the same row twice in one
    /// tick — which the sweeps do, re-judging under the lock — must not bring
    /// the deadline closer. That is why this is a timestamp and not a counter.
    fn note(&self, target_id: &str, volume_exists: bool) {
        let Ok(mut since) = self.0.lock() else {
            return;
        };
        if volume_exists {
            since.remove(target_id);
        } else {
            since
                .entry(target_id.to_string())
                .or_insert_with(std::time::Instant::now);
        }
    }

    fn waited(&self, target_id: &str) -> Option<Duration> {
        self.0
            .lock()
            .ok()
            .and_then(|since| since.get(target_id).map(|at| at.elapsed()))
    }

    fn forget_missing(&self, targets: &[TargetRow]) {
        if let Ok(mut since) = self.0.lock() {
            since.retain(|id, _| targets.iter().any(|t| t.target_id == *id));
        }
    }

    /// Moves an existing mark back in time, so a test can reach the far side
    /// of the grace period without waiting ninety seconds.
    ///
    /// It REQUIRES the mark to exist and says so if it does not: that is what
    /// makes the tests around it discriminating rather than decorative. A test
    /// that silently created the mark here would pass just as happily against
    /// a `note` that never recorded anything.
    #[cfg(test)]
    fn rewind_for_test(&self, target_id: &str, by: Duration) {
        let mut since = self.0.lock().expect("grace clock");
        let at = since
            .get_mut(target_id)
            .unwrap_or_else(|| panic!("{target_id} has no mark to rewind — `note` did not record"));
        *at = std::time::Instant::now() - by;
    }
}

/// What one judging pass found: the lines for the log, and the two things the
/// tick may have to ACT on.
///
/// The pair is deliberately symmetric. A verdict is a statement about what
/// this node should be serving, and it is worth exactly nothing until
/// something carries it out. Round 5 gave `Remove` an executor on the tick and
/// left `Apply` without one, which turned every wrong `Remove` into a
/// permanent, silent loss: the tick took a target out of the kernel on one
/// missed `stat` and nothing ever put it back, while the row read `active`.
pub struct Evaluation {
    pub log: Vec<String>,
    /// A row judged `Remove` whose object is STILL in this node's configfs,
    /// and whose removal is due (see `removal_is_due`).
    pub removals_pending: bool,
    /// A row judged `Apply` that this node is NOT currently exporting.
    pub applies_pending: bool,
}

/// The tick version: judge every row, persist, alert, and touch no kernel.
///
/// It deliberately does NOT take `apply_lock`. Taking it would make the
/// judgement wait behind a running apply — and the judgement is what raises the
/// drift alert, the one thing on this path that has to happen on a schedule
/// rather than when a mutation finishes. The cost of not taking it is bounded
/// and small: a judgement that races an apply can persist a `state_detail`
/// computed a moment before that apply's own, and the next tick recomputes it
/// twenty seconds later. Nothing downstream reads `state` to decide anything —
/// every verdict comes from `Disposition`, and the sweeps re-judge under the
/// lock precisely so that no ACTION rests on this reading.
pub fn evaluate(db: &DbPool) -> Result<Evaluation> {
    let mut targets = store::list_targets(db)?;
    let mut log = Vec::new();
    let disposition = evaluate_rows(db, &mut targets, &kernel_can_serve, RetryMemory::global(), &mut log)?;
    Ok(Evaluation {
        removals_pending: !rows_to_remove(
            &targets,
            &disposition,
            &object_in_kernel,
            &removal_is_due,
        )
        .is_empty(),
        applies_pending: !rows_to_apply(
            &targets,
            &disposition,
            &object_in_kernel,
            &apply_retry_pending,
        )
        .is_empty(),
        log,
    })
}

/// Targets whose last `apply_one` failed and that have not succeeded since.
///
/// WHY this exists next to `object_in_kernel`. The apply gate asks "is the
/// object there", and `apply_plan` STOPS AT THE FIRST FAILED STEP while the
/// object's own `mkdir` is one of the earliest steps in both plans — nvmet's
/// is the very first. So a plan that dies on the portal, on a namespace or on
/// `{tpg}/enable` leaves a directory that serves nothing, and a gate that only
/// looks for the directory reads that as "done": green chip, no retry, no
/// alert, forever.
///
/// Remembering the failure is the cheap half of the answer. The expensive half
/// would be observing what the kernel is actually SERVING (`{tpg}/enable`,
/// `{port}/subsystems/<nqn>`) — reads this app has never measured as
/// unprivileged, unlike the ones it already makes. This needs no new reading
/// and closes the loop deterministically: whatever failed is retried under the
/// same backoff until it stops failing.
///
/// In-memory on purpose: after a restart nothing is remembered, and the
/// one-shot restore re-applies every row anyway.
/// The set itself. One instance per caller-supplied memory; `global()` is the
/// process-wide one the tick and the sweeps share, and a test makes its own —
/// exactly the shape `GraceClock` has, and for exactly the same reason. This
/// is process state and `cargo test` runs tests in parallel: `evaluate_rows`
/// calls `forget_missing` on every judgement, so a test asserting against the
/// global set could have its entry swept away mid-test by an unrelated one.
struct RetryMemory(std::sync::Mutex<std::collections::BTreeSet<String>>);

impl RetryMemory {
    fn new() -> Self {
        Self(std::sync::Mutex::new(std::collections::BTreeSet::new()))
    }

    fn global() -> &'static Self {
        static FAILED: std::sync::OnceLock<RetryMemory> = std::sync::OnceLock::new();
        FAILED.get_or_init(RetryMemory::new)
    }

    /// Remembers a failed apply and forgets a successful one.
    fn note(&self, target_id: &str, ok: bool) {
        if let Ok(mut failed) = self.0.lock() {
            if ok {
                failed.remove(target_id);
            } else {
                failed.insert(target_id.to_string());
            }
        }
    }

    /// Whether this row's last apply left something unfinished.
    fn pending(&self, target: &TargetRow) -> bool {
        self.0
            .lock()
            .map(|failed| failed.contains(&target.target_id))
            .unwrap_or(false)
    }

    /// Drops the remembered failure of every target that is no longer in the
    /// database, so the set cannot grow for the life of the process and a
    /// target created later with a fresh id inherits nothing.
    fn forget_missing(&self, targets: &[TargetRow]) {
        if let Ok(mut failed) = self.0.lock() {
            failed.retain(|id| targets.iter().any(|t| t.target_id == *id));
        }
    }
}

fn note_apply_outcome(target_id: &str, ok: bool) {
    RetryMemory::global().note(target_id, ok);
}

/// Whether this row's last apply left something unfinished.
fn apply_retry_pending(target: &TargetRow) -> bool {
    RetryMemory::global().pending(target)
}

/// The alert key for "this node cannot reach its own kernel".
const ELEVATION_ALERT_KEY: &str = "elevation:channel";

/// Whether the privilege-channel alert should stand, and what it says.
///
/// THE `elevation` alert had a label in five locales, a row in n02 and no
/// producer anywhere. It is the fleet-visible half of the owner's decision of
/// 2026-09-04 (§3.4): a node without a provisioned channel does not count as
/// ready to serve, and its absence is a FAULT rather than a neutral state.
/// configfs is empty after every reboot and §3.4 forbids `target.service`, so
/// on such a node exports do not come back until a human arms the channel —
/// for SMB an inconvenience, for a raw disk handed to a hypervisor a datastore
/// with no disk.
///
/// It is a CONJUNCTION, and both halves must be able to clear it:
///   * the channel is not armed, and
///   * there is block work waiting.
///
/// The first version resolved only when the channel came back, so an admin who
/// instead deleted the row the alert was about — or whose vanished zvol
/// returned — kept a red alert about work that no longer exists, with no
/// action that could clear it. Same shape as `RetryClock`'s "nothing pending
/// is success".
///
/// A node with no block targets is not faulty for having no channel, which is
/// why this is not simply `!armed`.
fn channel_alert(
    armed: bool,
    pending: &Evaluation,
) -> Option<(&'static str, &'static str, &'static str, &'static str)> {
    if armed || !(pending.applies_pending || pending.removals_pending) {
        return None;
    }
    Some((
        "warning",
        "channel",
        "the privilege channel is not armed, so this node cannot reach the kernel",
        "block targets are waiting to be applied or removed. configfs is empty after a reboot \
         and only this app restores it, so exports stay down until the channel is armed \
         (Environment → provision).",
    ))
}

/// The rows the removal sweep would act on, given a judgement.
///
/// (Test note: `in_kernel` is injected for the same reason `installed` is —
/// so the selection can be exercised on a host with no configfs at all,
/// instead of a test rewriting the filter and agreeing with itself.)
///
/// ONE function, called both by the gate that decides whether the tick reaches
/// for the privilege channel and by the sweep that does the removing — so the
/// two can never disagree, and so a test can exercise the real selection
/// instead of a copy of it. `in_kernel` is injected for the same reason
/// `installed` is: the selection has to be testable on a host with no configfs
/// at all.
///
/// Three conditions, each carrying its own weight:
///   * the verdict is `Remove`. `Freeze` is a DIFFERENT variant and can never
///     appear here — that is the property the owner's drift decision rests on,
///     and widening this to "not Apply" would quietly break it;
///   * the object is actually in the kernel, so a row that was never applied
///     is not swept forever;
///   * the removal is DUE (`removal_is_due`) — a vanished volume waits out its
///     grace period before a client loses a disk over it.
fn rows_to_remove<'a>(
    targets: &'a [TargetRow],
    disposition: &BTreeMap<String, Disposition>,
    in_kernel: &dyn Fn(&TargetRow) -> bool,
    due: &dyn Fn(&TargetRow) -> bool,
) -> Vec<&'a TargetRow> {
    targets
        .iter()
        .filter(|t| disposition.get(&t.target_id) == Some(&Disposition::Remove))
        .filter(|t| in_kernel(t))
        .filter(|t| due(t))
        .collect()
}

/// The rows the apply sweep would act on: judged `Apply`, and either this node
/// has no configfs object for them or their last apply failed.
///
/// The wording matters because the two are not the same question. "Not
/// exporting" is what the gate WANTS to ask; "no directory" is what it can ask
/// cheaply and without a reading this app has never measured as unprivileged.
/// The gap between them — a plan that died after `mkdir` and before
/// `{tpg}/enable` — is what the remembered failure covers.
///
/// The missing half of the reconcile, and its absence was the blocker. Every
/// path that puts something INTO the kernel used to be a mutation of that one
/// row, a config import, or a one-shot restore whose latch closed on the first
/// `Ok` — including an `Ok` that applied nothing because every row had been
/// judged `Remove`. So: a zvol whose udev link had not appeared yet produced a
/// green job and no target; a pool that came back after a `zpool export`
/// produced a green row and no target; and a reboot where the pool imported
/// more slowly than the app produced no targets at all and never even loaded
/// the kernel modules.
///
/// `object_in_kernel` is an unprivileged `is_dir`, so asking this costs a
/// `stat` per row — the same price the removal gate pays. What that saves is
/// the CATALOG invocation, not the sudo: the tick reaches these gates from
/// inside `channel_available`, which runs `sudo -n -- tentanas-helper
/// --version` on every tick whatever they answer.
fn rows_to_apply<'a>(
    targets: &'a [TargetRow],
    disposition: &BTreeMap<String, Disposition>,
    in_kernel: &dyn Fn(&TargetRow) -> bool,
    retry: &dyn Fn(&TargetRow) -> bool,
) -> Vec<&'a TargetRow> {
    targets
        .iter()
        .filter(|t| disposition.get(&t.target_id) == Some(&Disposition::Apply))
        .filter(|t| !in_kernel(t) || retry(t))
        .collect()
}

/// Why a target's saved change did NOT reach the kernel, when that is what
/// happened.
///
/// The question it answers is "is this row now what the node judged it to be",
/// not "is it frozen". It used to be only the second, and the gap was a green
/// job for three different saves the kernel never heard:
///
///   * FROZEN (portal drift) — `apply` skips the row on purpose, so the most
///     likely reaction to a drift alert, taking an initiator off the allowlist
///     of the target the alert is about, got a green "saved" for a revocation
///     nothing performed. This was the original case.
///   * judged `Apply`, not in the kernel — a target created on a zvol whose
///     udev link had not appeared yet is judged `Remove` at that instant,
///     applies nothing, and reports success for a target that does not exist.
///     The tick's apply sweep will pick it up within twenty seconds, but the
///     job that claimed to have made it must not claim that.
///   * judged `Remove`, still in the kernel — "Stop target" where the removal
///     failed. The chip says stopped, the toast says stopped, and the client
///     keeps writing to a raw disk.
///
/// The sentence is the row's own `state_detail` where there is one, because
/// that is the sentence the admin is already reading on n12.
pub fn unapplied_reason(db: &DbPool, name: &str) -> Result<Option<String>> {
    // By name, which is UNIQUE in `nas_targets`: the job carries the subject
    // it was spawned with, and that is the name.
    let Some(target) = store::target_by_name(db, name)? else {
        return Ok(None);
    };
    let target = &target;
    let volume_exists = target
        .luns
        .iter()
        .all(|lun| Path::new(&lun.device_path).exists());
    let in_kernel = object_in_kernel(target);
    let (_, detail, verdict) = target_state(
        target,
        volume_exists,
        &kernel_can_serve,
        &interface_addresses(),
        in_kernel,
    );
    let because = |what: &str| {
        Some(if detail.is_empty() {
            what.to_string()
        } else {
            format!("{what}: {detail}")
        })
    };
    Ok(match (verdict, in_kernel) {
        (Disposition::Freeze, _) => because("this target was left exactly as it is"),
        (Disposition::Apply, false) => {
            because("this node is not exporting it yet — the next reconcile will try again")
        }
        (Disposition::Remove, true) => {
            because("it is still in this node's kernel and still serving clients")
        }
        _ => None,
    })
}

/// Serialises every apply on this node.
///
/// WHY a lock and not a job queue: two `apply` runs at once corrupt each
/// other's view of a NODE-WIDE resource. `observe_nvmet` picks the lowest free
/// port index out of the directories that exist; two concurrent runs pick the
/// SAME index, both `mkdir` it, and the second one's `addr_traddr` write lands
/// on a port the first has already enabled by linking a subsystem into it —
/// which is -EACCES (measured: "Disable port '247' before changing attribute
/// in nvmet_addr_traddr_store"), and `apply_plan` stops at the first failed
/// step, leaving a subsystem with no port. Two admins saving two targets is
/// enough; so is one save landing on the one-shot restore.
///
/// A `tokio::sync::Mutex` because the guard is held across `await` points, and
/// deliberately not `try_lock`: the second save must WAIT and then reconcile
/// against what the first one actually left in the kernel, not fail.
fn apply_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

/// Recomputes every target's state and makes the kernel match it.
///
/// Also the RESTORE: configfs is empty after a reboot, so the same function
/// that a wizard save runs is what the instance runs when it starts. There is
/// no second path and no `saveconfig` (§3.4).
///
/// `scope` is which target the KERNEL half is for. `None` is the whole node —
/// the restore, the config import and the delete, which also wants the orphan
/// sweep. `Some(target_id)` is one row and nothing else, which is what a
/// wizard save or a pause/resume gets.
///
/// WHY the scope exists (owner decision 2026-09-04): every mutation used to
/// re-render every target on the node. Nothing about that was unsafe —
/// re-applying a live target is measured not to drop a session — but it made
/// one edit write ~300 configfs attributes on a 20-target node, put twenty
/// full plans in the job log of a single save so the lines that mattered could
/// not be found, and gave two concurrent saves twenty chances at the port
/// collision above instead of one. The JUDGING half stays node-wide whatever
/// the scope is: the drift alert has to be right for every row, and it costs
/// no privilege at all.
pub async fn apply(
    db: &DbPool,
    cipher: &SettingsCipher,
    explicit: Option<&ElevationToken>,
    scope: Option<&str>,
) -> Result<Vec<String>> {
    let _guard = apply_lock().lock().await;
    let mut targets = store::list_targets(db)?;
    let mut log = Vec::new();
    // What to do with each row, keyed by target id. It cannot live on
    // `TargetRow` — the row is the DESIRED state and this is a verdict about
    // the node — and the two loops below need it after the judging loop ends.
    let disposition = evaluate_rows(db, &mut targets, &kernel_can_serve, RetryMemory::global(), &mut log)?;

    // A frozen target is left ALONE — not removed, not re-applied — so both
    // loops below have to skip it. Reading the verdict rather than the state
    // is what keeps that promise: `error` alone would send it to the removal
    // loop with everything else that is not active.
    let verdict_of = |target: &TargetRow| {
        disposition
            .get(&target.target_id)
            .copied()
            .unwrap_or(Disposition::Remove)
    };
    // The kernel half only ever touches the scoped row. A target nobody
    // changed is left exactly as it is — including a broken one, whose error
    // belongs to its own save and not to somebody else's.
    let in_scope = |target: &TargetRow| scope.is_none_or(|id| id == target.target_id);

    let mut failed: Vec<String> = Vec::new();

    // Removals FIRST, and every one of them attempted: stopping an export must
    // never be blocked by a target further down the list failing to apply. A
    // disabled or broken target that stays in the kernel keeps handing a client
    // a raw disk.
    //
    // Through `enact_removals`, which is the ONLY way a row-judged removal
    // reaches the kernel — so this path cannot drift away from the tick's the
    // way it did when the grace period was added to one of them and not the
    // other. That is not hypothetical: this loop used to have its own
    // predicate, and `apply(None)` is reached by the delete, the config import
    // and the restore, so a pool re-importing while an admin deleted an
    // unrelated target tore a live export out with no grace at all.
    failed.extend(enact_removals(db, &targets, &disposition, scope, explicit, &mut log).await);

    // Orphans: an object this app created whose row is gone. It happens when a
    // delete removed the row and its job then failed (no channel, mode B with
    // no password, a restart in between). Nothing else would ever take it out —
    // `apply` iterates rows, and a row is exactly what an orphan lacks — so it
    // would keep exporting until the next reboot, which is the orphan §5.8
    // forbids. Only names under this app's own WWN authority are touched.
    //
    // Node-wide by definition, so it belongs to a node-wide apply: an orphan
    // has no target id to be in scope of. The delete path, which is where
    // orphans come from, is one of the callers that passes `None`.
    if scope.is_none() {
        for (protocol, wwn) in orphans(&targets) {
            log.push(format!("{wwn}: no row behind it any more, removing"));
            let row = TargetRow {
                name: wwn.clone(),
                protocol,
                wwn,
                ..Default::default()
            };
            if let Err(e) = remove_one(db, &row, explicit, &mut log).await {
                failed.push(format!("{}: {e}", row.name));
            }
        }
    }

    // Then the applies, each independent of the others: one target with an
    // undecryptable secret must not stop the rest of the node reaching its
    // desired state. The errors are collected and reported together.
    //
    // NOT through `rows_to_apply`, and the difference is deliberate rather
    // than an oversight of the kind the removal side just had. `rows_to_apply`
    // selects rows the node is NOT serving — the right question for a periodic
    // sweep, whose whole job is to notice what is missing. `apply` is the
    // mutation path: re-rendering a target that IS live is precisely what a
    // wizard save has to do, and is measured to be safe. One condition, two
    // legitimately different answers.
    for target in targets
        .iter()
        .filter(|t| in_scope(t) && verdict_of(t) == Disposition::Apply)
    {
        let outcome = apply_one(db, cipher, target, explicit, &mut log).await;
        // Remembered either way — see `RetryMemory`. A plan that died
        // halfway leaves the target's configfs directory behind, so the
        // "is it in the kernel" gate would read it as finished and never come
        // back to it.
        note_apply_outcome(&target.target_id, outcome.is_ok());
        if let Err(e) = outcome {
            log.push(format!("{}: {e}", target.name));
            failed.push(format!("{}: {e}", target.name));
        }
    }
    if !failed.is_empty() {
        return Err(anyhow!("{}", failed.join("; ")));
    }
    Ok(log)
}

/// Takes out of the kernel every row the node has judged `Remove` and whose
/// removal is due.
///
/// WHY this exists as its own entry point. A `Remove` verdict used to have no
/// executor outside a mutation of that same row and the one-shot restore, and
/// narrowing `apply` to one target made that worse: before, any edit of any
/// target on the node swept the whole list. So at 03:00 a zvol's device node
/// disappears, the tick judges the row `error` with "the target stays out of
/// the kernel" — and the sentence is false. The target is still in configfs,
/// `{tpg}/enable` still reads 1, and the initiator gets I/O errors from a disk
/// that is gone instead of a disk that cleanly went away. Plan §5.5 says a
/// target whose backing volume is gone has to leave the kernel, and §5.8
/// forbids orphans; the verdict existed, the executor did not.
///
/// It runs the removal loop and nothing else — it never applies, so it cannot
/// resurrect anything. `Disposition::Freeze` is skipped by `rows_to_remove`,
/// which selects `Remove` and only `Remove`: a drifted portal is a different
/// variant, and that is what keeps this from becoming the automatic teardown
/// the owner's drift decision rules out.
///
/// It DOES re-judge every row on the way in (`evaluate_rows` persists states
/// and raises or resolves alerts as it goes) — deliberately, because acting on
/// the tick's judgement without re-taking it under the lock would mean
/// removing a target a mutation resurrected a millisecond ago.
///
/// The orphan sweep is deliberately NOT here. An orphan has no row and no
/// verdict, its detection is a `read_dir` of two directories, and the full
/// `apply(None)` that the restore, the import and the delete all run is where
/// it belongs. This is about rows the node has already judged.
///
/// Errors are RETURNED, not logged and forgotten: a removal the kernel refuses
/// is a target still handing a client a raw disk, and the caller backs off and
/// raises an alert about it.
pub async fn sweep_removals(
    db: &DbPool,
    explicit: Option<&ElevationToken>,
) -> Result<Vec<String>> {
    let _guard = apply_lock().lock().await;
    let mut targets = store::list_targets(db)?;
    let mut log = Vec::new();
    let disposition = evaluate_rows(db, &mut targets, &kernel_can_serve, RetryMemory::global(), &mut log)?;
    let mut out = Vec::new();
    let failed = enact_removals(db, &targets, &disposition, None, explicit, &mut out).await;
    if !failed.is_empty() {
        return Err(anyhow!("{}", failed.join("; ")));
    }
    Ok(out)
}

/// THE way a `Remove` verdict on a ROW reaches the kernel. Every executor of
/// that verdict goes through here, and there is nowhere else to go.
///
/// WHY it is one function and not a shared filter. Three times in this slice a
/// guard was attached to one of two executors of the same verdict and the
/// other one kept the old behaviour: round 5 gave `Remove` an executor on the
/// tick and left `Apply` without one; round 6 fixed that and left the 90 s
/// grace period living only in `sweep_removals`, so the removal loop inside
/// `apply` — reached by the delete, the config import and the one-shot restore,
/// all node-wide — tore a live export out on the first missed `stat`. Each time
/// the trigger was fixed and the shape was not.
///
/// So the selection and the doing are the same function. A caller that wants to
/// act on the `Remove` VERDICT cannot get the rows without also getting
/// `rows_to_remove`'s three conditions — verdict, `in_kernel`, and
/// `removal_is_due`: `rows_to_remove` has exactly two callers, the gate in
/// `evaluate` and this function, and this function has exactly two, `apply`
/// and `sweep_removals`.
///
/// `remove_one` itself has three OTHER callers, none of which is acting on a
/// verdict and none of which may be read as a way around this one:
///   * the orphan loop in `apply` — a configfs object with no row at all, so
///     there is no verdict to consult and a synthetic `TargetRow` is built
///     from the object's own name;
///   * `remove()` — the admin deleted the target; the row is already out of
///     the database when the kernel side runs;
///   * `remove_all()` — uninstall (§5.8), deliberately verdict-free and
///     deliberately including `Freeze`d rows: the point is to leave nothing of
///     this app in the kernel, and a frozen row is still an export.
///
/// `scope` narrows it to one target for the mutation path; `None` is the whole
/// node. It narrows WHICH rows, never WHETHER the conditions apply.
///
/// Returns the failures. A removal that did not happen is a target still
/// handing a client a raw disk, so it never ends as a log line alone.
async fn enact_removals(
    db: &DbPool,
    targets: &[TargetRow],
    disposition: &BTreeMap<String, Disposition>,
    scope: Option<&str>,
    explicit: Option<&ElevationToken>,
    log: &mut Vec<String>,
) -> Vec<String> {
    let mut failed = Vec::new();
    for target in rows_to_remove(targets, disposition, &object_in_kernel, &removal_is_due)
        .into_iter()
        .filter(|t| scope.is_none_or(|id| id == t.target_id))
    {
        log.push(format!(
            "{}: judged '{}', taking it out of the kernel",
            target.name,
            if target.state_detail.is_empty() {
                target.state.as_str()
            } else {
                target.state_detail.as_str()
            }
        ));
        if let Err(e) = remove_one(db, target, explicit, log).await {
            failed.push(format!("{}: {e}", target.name));
        }
    }
    failed
}

/// Puts into the kernel every row the node has judged `Apply` and is not
/// exporting — the other half of the reconcile, and the one that was missing.
///
/// Symmetric to `sweep_removals` in every way that matters: same lock, same
/// re-judgement under it, same unprivileged gate (`rows_to_apply`), errors
/// returned rather than swallowed. What it fixes is not a cosmetic asymmetry:
/// while only removal ran on the tick, a target taken out of the kernel by a
/// transient — a udev link that had not appeared, a pool briefly exported —
/// never came back, and the row said `active` the whole time.
///
/// It applies rows the node ALREADY judged appliable, so it cannot smuggle
/// anything past a verdict: a frozen target is `Freeze`, not `Apply`, and is
/// never selected here either.
pub async fn sweep_applies(
    db: &DbPool,
    cipher: &SettingsCipher,
    explicit: Option<&ElevationToken>,
) -> Result<Vec<String>> {
    let _guard = apply_lock().lock().await;
    let mut targets = store::list_targets(db)?;
    let mut log = Vec::new();
    let disposition = evaluate_rows(db, &mut targets, &kernel_can_serve, RetryMemory::global(), &mut log)?;
    let mut out = Vec::new();
    let mut failed: Vec<String> = Vec::new();
    for target in rows_to_apply(&targets, &disposition, &object_in_kernel, &apply_retry_pending) {
        out.push(format!(
            "{}: judged active but not in this node's kernel, applying it",
            target.name
        ));
        let outcome = apply_one(db, cipher, target, explicit, &mut out).await;
        note_apply_outcome(&target.target_id, outcome.is_ok());
        if let Err(e) = outcome {
            out.push(format!("{}: {e}", target.name));
            failed.push(format!("{}: {e}", target.name));
        }
    }
    if !failed.is_empty() {
        return Err(anyhow!("{}", failed.join("; ")));
    }
    Ok(out)
}

/// The block objects in the kernel that carry this app's WWN authority but no
/// longer have a row: `(protocol, wwn)`.
///
/// The authority prefix is what makes this safe — a target somebody made by
/// hand, or another tool's, never matches it and is never touched.
pub fn orphans(rows: &[TargetRow]) -> Vec<(String, String)> {
    let (iscsi, nvmet) = orphan_dirs();
    orphans_in(&iscsi, &nvmet, rows)
}

/// The two configfs directories `orphans` walks: LIO's target list and
/// nvmet's subsystem list.
///
/// Named, rather than composed inline, because that composition was the one
/// part of the orphan walk no test could reach — a typo in either string is
/// invisible on a developer machine with no configfs, and the walk would then
/// silently find nothing forever.
fn orphan_dirs() -> (String, String) {
    (
        format!("{}/iscsi", block::TARGET_CONFIGFS),
        format!("{}/subsystems", block::NVMET_CONFIGFS),
    )
}

/// The same, with the two directories injected — the way `installed` and
/// `in_kernel` are injected, and for the same reason: the property that makes
/// this safe is the AUTHORITY PREFIX check, and a test that cannot point the
/// walk at a directory it built has to restate that check instead of running
/// it. One did, for four rounds, and would have passed with the check deleted.
fn orphans_in(iscsi_dir: &str, nvmet_dir: &str, rows: &[TargetRow]) -> Vec<(String, String)> {
    let known: std::collections::BTreeSet<&str> = rows.iter().map(|t| t.wwn.as_str()).collect();
    let mut out = Vec::new();
    for (protocol, dir, prefix) in [
        ("iscsi", iscsi_dir.to_string(), format!("iqn.{WWN_AUTHORITY}:")),
        ("nvmet", nvmet_dir.to_string(), format!("nqn.{WWN_AUTHORITY}:")),
    ] {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut names: Vec<String> = entries
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with(&prefix) && !known.contains(name.as_str()))
            .collect();
        names.sort();
        out.extend(names.into_iter().map(|n| (protocol.to_string(), n)));
    }
    out
}

async fn apply_one(
    db: &DbPool,
    cipher: &SettingsCipher,
    target: &TargetRow,
    explicit: Option<&ElevationToken>,
    log: &mut Vec<String>,
) -> Result<()> {
    // Nothing else on this node loads the kernel target modules: §3.4 forbids
    // enabling `target.service`/`nvmet.service`, which is what normally would.
    // Without this a fresh node has no `/sys/kernel/config/target` at all and
    // no target could ever be created or restored after a reboot.
    run(
        db,
        &HelperCommand::BlockModulesLoad {
            protocol: target.protocol.clone(),
        },
        None,
        explicit,
        log,
    )
    .await?;
    let secrets = secrets(cipher, target)?;
    let (command, document) = if target.protocol == "nvmet" {
        let spec = nvmet_spec(target, &secrets);
        block::validate_nvmet(&spec).map_err(|e| anyhow!("{}: {e}", target.name))?;
        (
            HelperCommand::NvmetSubsystemApply {},
            serde_json::to_vec(&spec)?,
        )
    } else {
        let spec = iscsi_spec(target, &secrets);
        block::validate_iscsi(&spec).map_err(|e| anyhow!("{}: {e}", target.name))?;
        (
            HelperCommand::IscsiTargetApply {},
            serde_json::to_vec(&spec)?,
        )
    };
    // The document holds the plaintext secrets, so it goes on stdin and is
    // dropped here. It is never logged: what the job log gets is the helper's
    // own rendering, which redacts.
    let document = Zeroizing::new(document);
    run(db, &command, Some(&document), explicit, log).await
}

/// Takes one target out of the kernel, and REPORTS whether it went.
///
/// The Result is the point. A swallowed failure here means "Stop target" ends
/// green, the chip reads "stopped", the toast says so — and the kernel is
/// still handing that client a raw disk. That is the same class of lie
/// `unapplied_reason` was written for, on the other side of the reconcile, so
/// the error travels to whoever can act on it.
async fn remove_one(
    db: &DbPool,
    target: &TargetRow,
    explicit: Option<&ElevationToken>,
    log: &mut Vec<String>,
) -> Result<()> {
    // Keyed on the KERNEL, not on the row: a node whose configfs never had the
    // target answers "not present" and the reconcile stays quiet. Nothing to
    // remove is success, not silence about a failure.
    if !configfs_present(&target.protocol) {
        return Ok(());
    }
    let command = if target.protocol == "nvmet" {
        HelperCommand::NvmetSubsystemRemove {
            nqn: target.wwn.clone(),
        }
    } else {
        HelperCommand::IscsiTargetRemove {
            iqn: target.wwn.clone(),
        }
    };
    if let Err(e) = run(db, &command, None, explicit, log).await {
        log.push(format!("{}: not removed from the kernel: {e}", target.name));
        return Err(e);
    }
    Ok(())
}

/// Takes ONE target out of the kernel — the delete path, which runs after the
/// row is already gone from the database.
/// Takes ONE target out of the kernel for the delete path, and says whether it
/// went.
///
/// The bool is the whole point. It used to answer only a log, and the caller
/// had already dropped the database row — so a teardown the kernel refused
/// produced a LIVE EXPORT the app no longer knew about: the client keeps its
/// disk, the UI has nothing to press, and the only trace is a line in a job
/// log nobody reads after a green job. That is §5.8's orphan, made by the
/// error path rather than by forgetting to clean up.
pub async fn remove(
    db: &DbPool,
    protocol: &str,
    wwn: &str,
    explicit: Option<&ElevationToken>,
) -> (Vec<String>, bool) {
    // The same lock `apply` takes, for the same node-wide reason: an nvmet
    // PORT is shared, `remove_nvmet` reaps one the last subsystem just left,
    // and a concurrent apply may have already chosen that index for a port it
    // is about to create.
    let _guard = apply_lock().lock().await;
    let row = TargetRow {
        name: wwn.to_string(),
        protocol: protocol.to_string(),
        wwn: wwn.to_string(),
        ..Default::default()
    };
    let mut log = Vec::new();
    let outcome = remove_one(db, &row, explicit, &mut log).await;
    (log, outcome.is_ok())
}

/// Every target this app created, out of the kernel (§5.8 step 2).
///
/// Uninstalling must not leave a live target handing a client a raw disk, so
/// this runs on the rows rather than on the files: there is no file to key on,
/// the state lives in the kernel.
pub async fn remove_all(db: &DbPool, explicit: Option<&ElevationToken>) -> Vec<String> {
    let _guard = apply_lock().lock().await;
    let mut log = Vec::new();
    let Ok(targets) = store::list_targets(db) else {
        return log;
    };
    for target in &targets {
        // Every one attempted: the teardown must not stop at the first target
        // the kernel refuses, or the ones after it keep serving. Each failure
        // is already a line in the log this returns.
        let _ = remove_one(db, target, explicit, &mut log).await;
    }
    log
}

async fn run(
    db: &DbPool,
    command: &HelperCommand,
    payload: Option<&[u8]>,
    explicit: Option<&ElevationToken>,
    log: &mut Vec<String>,
) -> Result<()> {
    if let Ok(plan) = command.plan() {
        log.push(format!("$ {}", plan.display()));
    }
    let (out, channel) = match payload {
        Some(bytes) => {
            super::broker::run_privileged_with_key(db, command, bytes, explicit, APPLY_TIMEOUT)
                .await?
        }
        None => super::broker::run_privileged(db, command, explicit, APPLY_TIMEOUT).await?,
    };
    log.push(format!("channel: {}", channel.as_str()));
    for text in [&out.stdout, &out.stderr] {
        for l in text.lines().filter(|l| !l.trim().is_empty()) {
            log.push(l.trim_end().to_string());
        }
    }
    if !out.success() {
        return Err(anyhow!(
            "{}",
            out.stderr
                .trim()
                .lines()
                .next()
                .unwrap_or("the privileged step failed")
        ));
    }
    Ok(())
}

// =============================================================================
// restore at startup — the whole reason there is no `targetcli saveconfig`
// =============================================================================

/// How often the restore loop looks at the privilege channel AND recomputes
/// every target's state. A target is not urgent to the second; a portal whose
/// address moved is not urgent to the second either, but it must not wait for
/// the next time an admin happens to save something.
const RESTORE_TICK: Duration = Duration::from_secs(20);

/// How long the restore waits after a failed attempt, and the ceiling it backs
/// off to.
///
/// WHY there is a backoff at all: `apply` returns `Err` when ANY target failed,
/// and the loop only marks itself restored on `Ok` — so one permanently broken
/// row (a secret encrypted with a key that is gone, a zvol that will never come
/// back) turned the loop into an unbounded stream of privileged calls. Every
/// tick meant a `modprobe` pair per target through the privilege channel plus a
/// full apply for every healthy target: on a ten-target node roughly thirty
/// `run_privileged` invocations every twenty seconds, which is ~130 000
/// `authpriv` lines a day and the same number added to the invocation counter
/// n16 shows the admin as a measure of how much this app uses root. The retry
/// is worth making, but not at that rate: nothing about a broken row changes
/// between two ticks.
const RESTORE_RETRY_MIN: Duration = Duration::from_secs(20);
const RESTORE_RETRY_MAX: Duration = Duration::from_secs(300);

/// The alert the periodic reconcile raises when it cannot make the kernel
/// match the node's own decision.
///
/// One row for the node, not one per target: the failures that produce it are
/// almost always the same cause (the channel, the helper, a kernel that
/// refuses), and an admin needs to hear "this node is not doing what it
/// decided" once, not fifteen times.
const SWEEP_ALERT_KEY: &str = "targets:reconcile";

/// How many consecutive failures before that alert goes up. One failure is a
/// transient — the channel blinked, a job held the lock — and this is a
/// twenty-second tick.
const SWEEP_ALERT_AFTER: u32 = 3;

/// One reconcile's retry clock: when it may run again, how long it waits after
/// the next failure, and how many failures it has had IN A ROW.
///
/// A tiny struct rather than three loose variables because there are two of
/// these now (removals and applies keep their own), and because the counter
/// has one rule that three loose variables kept getting wrong: it means
/// CONSECUTIVE failures. A failure on Monday and two on Friday are not "three
/// in a row", and an alert that says they are is an alert an admin learns to
/// ignore. `succeeded()` is the only thing that resets it, and every attempt
/// ends in exactly one of the two.
struct RetryClock {
    at: Option<std::time::Instant>,
    wait: Duration,
    failures: u32,
}

impl RetryClock {
    fn new() -> Self {
        Self {
            at: None,
            wait: RESTORE_RETRY_MIN,
            failures: 0,
        }
    }

    fn due(&self, now: std::time::Instant) -> bool {
        self.at.is_none_or(|at| now >= at)
    }

    fn succeeded(&mut self) {
        *self = Self::new();
    }

    fn failed(&mut self, now: std::time::Instant) {
        self.at = Some(now + self.wait);
        self.wait = (self.wait * 2).min(RESTORE_RETRY_MAX);
        self.failures += 1;
    }

    /// Nothing is outstanding: no failure is waiting to be retried.
    fn settled(&self) -> bool {
        self.failures == 0
    }
}

fn stopped() -> &'static std::sync::atomic::AtomicBool {
    static FLAG: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    &FLAG
}

/// Stops the restore loop for good — the uninstall teardown, before it starts
/// taking targets out of a kernel this loop would put them back into.
pub fn stop() {
    stopped().store(true, std::sync::atomic::Ordering::Relaxed);
}

/// Recreates every target from `tentanas.db` when the instance starts, and
/// keeps recomputing every target's state for as long as the instance runs.
///
/// The two halves are deliberately separate. Applying needs the privilege
/// channel and happens once (plus on every mutation); JUDGING needs nothing at
/// all and happens on every tick, because a portal's address can move at any
/// hour and the drift policy is a report, not a repair — see `evaluate_rows`.
///
/// configfs is EMPTY after a reboot: no target serves anything until this
/// runs. That is the deal §3.4 makes — TentaNas is the only thing that
/// restores a target, with no `targetcli saveconfig` and no `target.service`
/// as a second source of truth, so there is nothing to drift against.
///
/// It waits for the privilege channel rather than failing once. Mode A has the
/// helper immediately; mode B can restore nothing until an admin arms a
/// session, and then the targets come back on the next tick — which is exactly
/// what the mode B screen promises. A channel that goes away and comes back
/// (a mode B session expiring) arms the restore again, because a reboot in
/// between is indistinguishable from here.
pub fn start_restore(main_db: DbPool, db: DbPool) {
    static STARTED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    if STARTED.set(()).is_err() {
        return;
    }
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        tracing::warn!("tentanas: no tokio runtime, block targets not restored");
        return;
    };
    handle.spawn(async move {
        // The same key file every other secret of the platform is read with;
        // a background loop has no request to borrow a cipher from.
        let cipher = match crate::crypto::load_or_create_master_key() {
            Ok(key) => SettingsCipher::new(&key),
            Err(e) => {
                tracing::error!("tentanas: block targets not restored, no master key: {e}");
                return;
            }
        };
        let mut restored = false;
        // When the next restore ATTEMPT may run, and how long the wait after
        // the next failure is. Both reset the moment an attempt succeeds or
        // the channel goes away, so a node that is merely waiting for an admin
        // to arm mode B is not penalised for it.
        let mut retry_at: Option<std::time::Instant> = None;
        let mut retry_after = RESTORE_RETRY_MIN;
        // One clock per reconcile half, and both apart from the restore's: a
        // node whose one-shot restore is waiting for a slow pool still
        // reconciles the rows that ARE ready, a reconcile that keeps failing
        // cannot hide behind the restore's clock, and a removal the kernel
        // keeps refusing cannot make a healthy apply wait five minutes.
        let mut remove_retry = RetryClock::new();
        let mut apply_retry = RetryClock::new();
        loop {
            if stopped().load(std::sync::atomic::Ordering::Relaxed) {
                return;
            }
            if super::instance_should_run(&main_db, &db) {
                // EVERY tick, whether or not the channel is armed and whether
                // or not the restore already ran: this is the periodic
                // judgement the owner's portal-drift decision needs. A DHCP
                // lease renewal moves an address at 03:00 and nobody is
                // editing a target that night — without this the row stays
                // green and no alert is ever raised. It costs an interface
                // enumeration and a handful of `stat` calls, needs no
                // privilege, and deliberately does NOT touch the kernel:
                // applying is still the business of a mutation and of the
                // one-shot restore below.
                let mut pending = Evaluation {
                    log: Vec::new(),
                    removals_pending: false,
                    applies_pending: false,
                };
                match evaluate(&db) {
                    Ok(seen) => {
                        for line in &seen.log {
                            tracing::info!("tentanas targets: {line}");
                        }
                        pending = seen;
                    }
                    Err(e) => tracing::warn!("tentanas: target state not evaluated: {e}"),
                }
                // A judgement is worth nothing until something carries it out,
                // and BOTH halves need carrying. `Remove` without an executor
                // meant a row saying "the target stays out of the kernel" over
                // a target that was very much in it; `Apply` without one meant
                // a row saying "active" over an empty kernel, forever, after a
                // single missed `stat`. Both gates are unprivileged `stat`s
                // per row, and neither can select a frozen target: `Freeze` is
                // a third variant and appears in neither list.
                //
                // The tick reaching this point has ALREADY made a privileged
                // call: `channel_available` runs `sudo -n -- tentanas-helper
                // --version` on every tick whatever these gates say. What the
                // gates save is the catalog invocation, not the sudo.
                let armed = super::broker::channel_available(&db).await;
                // THE `elevation` ALERT, which had a label in five locales, a
                // row in n02 and no producer anywhere.
                //
                // It is the fleet-visible half of the owner's decision of
                // 2026-09-04 (§3.4): a node without a provisioned channel does
                // not count as ready to serve, and its absence is a FAULT
                // rather than a neutral state. configfs is empty after every
                // reboot and §3.4 forbids `target.service`, so on such a node
                // the exports do not come back until a human arms the channel
                // — for SMB that is an inconvenience, for a raw disk handed to
                // a hypervisor it is a datastore with no disk.
                //
                // Raised only when there is something to serve: a node with no
                // block targets is not faulty for having no channel. Resolved
                // the moment the channel answers, so it cannot outlive what it
                // reports.
                // The decision is `channel_alert`, so it can be tested
                // without a tick, a database or a privileged channel — the
                // three things that kept the previous version untested.
                match channel_alert(armed, &pending) {
                    Some((severity, subject, summary, detail)) => {
                        let _ = store::raise_alert(
                            &db,
                            ELEVATION_ALERT_KEY,
                            severity,
                            "elevation",
                            subject,
                            summary,
                            detail,
                        );
                    }
                    None => {
                        let _ = store::resolve_alert(&db, ELEVATION_ALERT_KEY);
                    }
                }
                if armed {
                    // Two clocks, not one. A removal the kernel keeps
                    // refusing must not stretch the tick for a HEALTHY apply:
                    // a new target whose udev link has not appeared yet would
                    // otherwise wait five minutes because of somebody else's
                    // broken row.
                    //
                    // NOTHING PENDING IS SUCCESS. The commonest way a failing
                    // reconcile stops failing is the admin deleting the row it
                    // kept failing on — and then there is nothing to sweep, so
                    // a clock that only resets on a successful sweep would
                    // stay armed and its alert would stay open forever. It
                    // also keeps the failure count meaning CONSECUTIVE
                    // failures: two on Monday and one on Friday are not three
                    // in a row, and an alert that says they are is one an
                    // admin learns to ignore.
                    let now = std::time::Instant::now();
                    let mut trouble: Vec<String> = Vec::new();
                    if !pending.removals_pending {
                        remove_retry.succeeded();
                    } else if remove_retry.due(now) {
                        match sweep_removals(&db, None).await {
                            Ok(log) => {
                                remove_retry.succeeded();
                                for line in log {
                                    tracing::info!("tentanas targets: {line}");
                                }
                            }
                            Err(e) => {
                                remove_retry.failed(now);
                                trouble.push(format!("not taken out of the kernel: {e}"));
                            }
                        }
                    }
                    if !pending.applies_pending {
                        apply_retry.succeeded();
                    } else if apply_retry.due(now) {
                        match sweep_applies(&db, &cipher, None).await {
                            Ok(log) => {
                                apply_retry.succeeded();
                                for line in log {
                                    tracing::info!("tentanas targets: {line}");
                                }
                            }
                            Err(e) => {
                                apply_retry.failed(now);
                                trouble.push(format!("not applied to the kernel: {e}"));
                            }
                        }
                    }
                    if remove_retry.settled() && apply_retry.settled() {
                        // The node is doing what it decided. Closed on every
                        // such tick, whether or not this one had anything to
                        // sweep.
                        if let Err(e) = store::resolve_alert(&db, SWEEP_ALERT_KEY) {
                            tracing::warn!("tentanas targets: alert not closed: {e}");
                        }
                    } else if !trouble.is_empty() {
                        // The same backoff the restore has, for the same
                        // reason round 3 measured: a reconcile the kernel
                        // keeps refusing must not turn the tick into an
                        // unbounded stream of privileged calls, each one
                        // counted in the "how often did this app need root"
                        // number n16 shows as a trust measure.
                        let detail = trouble.join("; ");
                        tracing::warn!(
                            "tentanas: reconcile failed: {detail} (retrying in {}s)",
                            remove_retry.wait.max(apply_retry.wait).as_secs()
                        );
                        // And after enough CONSECUTIVE failures, an ALERT. A
                        // target the kernel will not let go of is exactly what
                        // an admin has to hear about — it is still serving a
                        // client a raw disk the node decided to stop serving.
                        // A `tracing::warn` in a log nobody opens is not
                        // hearing it.
                        if remove_retry.failures >= SWEEP_ALERT_AFTER
                            || apply_retry.failures >= SWEEP_ALERT_AFTER
                        {
                            if let Err(e) = store::raise_alert(
                                &db,
                                SWEEP_ALERT_KEY,
                                "warning",
                                "node",
                                "targets",
                                "Block targets: this node cannot reach the state it decided on",
                                &detail,
                            ) {
                                tracing::warn!("tentanas targets: alert not raised: {e}");
                            }
                        }
                    }
                    let due = retry_at.is_none_or(|at| std::time::Instant::now() >= at);
                    if !restored && due {
                        // Node-wide: this IS the restore, and after a reboot
                        // every row has to reach the kernel. It is also the
                        // one place the orphan sweep can run.
                        match apply(&db, &cipher, None, None).await {
                            Ok(log) => {
                                restored = true;
                                retry_at = None;
                                retry_after = RESTORE_RETRY_MIN;
                                for line in log {
                                    tracing::info!("tentanas targets: {line}");
                                }
                            }
                            Err(e) => {
                                // Backing off, and SAYING so: an admin reading
                                // the log has to be able to tell "retrying in
                                // five minutes" from "gave up". The rows that
                                // did apply stay applied — `apply` collects
                                // its errors rather than stopping at the first.
                                tracing::warn!(
                                    "tentanas: targets not restored: {e} (retrying in {}s)",
                                    retry_after.as_secs()
                                );
                                retry_at = Some(std::time::Instant::now() + retry_after);
                                retry_after = (retry_after * 2).min(RESTORE_RETRY_MAX);
                            }
                        }
                    }
                } else {
                    // A channel that went away and came back is
                    // indistinguishable from a reboot, so the restore is armed
                    // again — and so is its first retry, at full speed.
                    restored = false;
                    retry_at = None;
                    retry_after = RESTORE_RETRY_MIN;
                    remove_retry.succeeded();
                    apply_retry.succeeded();
                }
            }
            tokio::time::sleep(RESTORE_TICK).await;
        }
    });
}

// =============================================================================
// sessions
// =============================================================================

/// How long a session read may take. It is a directory walk behind a `sudo`,
/// on the path of a POLLED list — far shorter than an apply, and a node whose
/// channel is wedged must not hold the Sharing tab open waiting for it.
const SESSIONS_TIMEOUT: Duration = Duration::from_secs(15);

/// How long one NVMe-oF session read is reused.
///
/// WHY there is a cache at all: `targets_list` sits behind PERM_READ and the
/// Sharing tab POLLS it, so without this a user with nothing but the read
/// permission would drive one privileged invocation per poll — each one
/// counted in the node's "how often was the privilege channel used" audit that
/// n16 shows as a trust measure, and each one a line in `authpriv`. A read
/// that costs a sudo does not belong on a polling path at full rate. The
/// window is short enough that the delete dialog's blast radius is still a
/// recent measurement rather than a stale one.
const SESSIONS_CACHE: Duration = Duration::from_secs(10);

fn sessions_cache() -> &'static std::sync::Mutex<Option<(std::time::Instant, block::NvmetSessions)>>
{
    static CACHE: std::sync::OnceLock<
        std::sync::Mutex<Option<(std::time::Instant, block::NvmetSessions)>>,
    > = std::sync::OnceLock::new();
    CACHE.get_or_init(|| std::sync::Mutex::new(None))
}

/// The NVMe-oF controllers attached to this node, through the privilege
/// channel.
///
/// OWNER DECISION (2026-09-04): read them where the kernel publishes them and
/// say "this node cannot know" where it does not — never print a zero nobody
/// measured. nvmet keeps its live associations in debugfs
/// (`CONFIG_NVME_TARGET_DEBUGFS`, kernel 6.11+, mountpoint `0700 root:root`)
/// and not in configfs, which is why this needs the channel at all while
/// LIO's `dynamic_sessions` is an ordinary read.
///
/// Never an `Err`: a node without the channel, without debugfs, or without the
/// kernel option all mean the same thing to the caller — unknown — and each
/// one carries its own sentence for the UI.
pub async fn nvmet_sessions(db: &DbPool) -> block::NvmetSessions {
    if let Ok(cache) = sessions_cache().lock() {
        if let Some((at, found)) = cache.as_ref() {
            if at.elapsed() < SESSIONS_CACHE {
                return found.clone();
            }
        }
    }
    let found = read_nvmet_sessions(db).await;
    if let Ok(mut cache) = sessions_cache().lock() {
        *cache = Some((std::time::Instant::now(), found.clone()));
    }
    found
}

async fn read_nvmet_sessions(db: &DbPool) -> block::NvmetSessions {
    let outcome = super::broker::run_privileged(
        db,
        &HelperCommand::NvmetSessionsRead {},
        None,
        SESSIONS_TIMEOUT,
    )
    .await;
    let Ok((out, _)) = outcome else {
        return block::NvmetSessions::unavailable(
            "the privilege channel of this node is not armed, so its NVMe-oF controllers cannot be read",
        );
    };
    if !out.success() {
        return block::NvmetSessions::unavailable(
            out.stderr
                .trim()
                .lines()
                .next()
                .unwrap_or("the privileged read failed")
                .to_string(),
        );
    }
    serde_json::from_str(&out.stdout).unwrap_or_else(|e| {
        block::NvmetSessions::unavailable(format!("the node answered something unreadable: {e}"))
    })
}

/// The sessions of ONE target and whether this node could measure them at all.
///
/// The second half of the pair is the whole point: `0` and "unknown" are
/// different answers, and the delete dialog states its blast radius from this
/// number — an NVMe-oF target reporting a confident zero while a host is
/// writing to it would understate exactly the thing the retype is there for.
pub fn sessions_from(
    target: &TargetRow,
    nvmet: &block::NvmetSessions,
) -> (Vec<NasShareSession>, bool) {
    if target.protocol != "nvmet" {
        return sessions(target);
    }
    if !nvmet.available {
        return (Vec::new(), false);
    }
    let list = nvmet
        .controllers
        .get(&target.wwn)
        .map(|controllers| {
            controllers
                .iter()
                .map(|c| {
                    // The same shape every other protocol of this app uses:
                    // `client` is WHERE the session came from, `user` is WHO it
                    // says it is (smbd's machine/user, nfsd's address/name).
                    // nvmet publishes both — `host_traddr` is measured to be
                    // there — and they are not interchangeable: the address is
                    // the one thing about an NVMe-oF session the client cannot
                    // simply assert, while the NQN is exactly the string §5.5
                    // keeps saying is client-declared.
                    //
                    // A controller the kernel named neither way is still a
                    // session, so it is listed by its id rather than dropped.
                    let identity = if c.hostnqn.is_empty() {
                        format!("controller {}", c.cntlid)
                    } else {
                        c.hostnqn.clone()
                    };
                    NasShareSession {
                        client: if c.host_traddr.is_empty() {
                            identity.clone()
                        } else {
                            c.host_traddr.clone()
                        },
                        user: identity,
                        connected_at: None,
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    (list, true)
}

/// The initiators logged into an iSCSI target right now, from LIO's own
/// `dynamic_sessions` file. An unprivileged read of configfs.
///
/// nvmet has no counterpart HERE: the controllers of a subsystem are not in
/// configfs at all. They are read through the channel instead — see
/// `nvmet_sessions` — and this function answers for iSCSI only.
pub fn sessions(target: &TargetRow) -> (Vec<NasShareSession>, bool) {
    if target.protocol != "iscsi" {
        return (Vec::new(), true);
    }
    let tpg = format!("{}/iscsi/{}/tpgt_1", block::TARGET_CONFIGFS, target.wwn);
    // TRAP: `dynamic_sessions` lists ONLY generated ACLs —
    // `target_show_dynamic_sessions()` skips every `se_nacl` without
    // `dynamic_node_acl`. An allowlisted target has `generate_node_acls = 0`,
    // so all of its ACLs are static and that file is always empty: the
    // targets with the tighter configuration would report zero sessions while
    // clients were writing to them, and the delete dialog would understate its
    // own blast radius. The static half lives in each ACL's `info`.
    //
    // The second half of the pair is "did this node MEASURE the answer", the
    // same three-state the prune uses (`block::IscsiAclObserved::session`) and
    // for the same reason: an `info` this node could not read is not an ACL
    // with nobody on it. The number ends up in the delete dialog's blast
    // radius, where understating it costs a client its disk mid-write — so an
    // unreadable ACL makes the whole count unknown rather than lower.
    //
    // A target that is not in the kernel at all is a measured zero, not an
    // unknown: there is nothing to be logged into.
    if !Path::new(&tpg).is_dir() {
        return (Vec::new(), true);
    }
    let mut known = true;
    let mut out = match std::fs::read_to_string(format!("{tpg}/dynamic_sessions")) {
        Ok(text) => parse_dynamic_sessions(&text),
        // Absent is fine — LIO only publishes it for a TPG with generated
        // ACLs. Unreadable for any other reason is not.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(_) => {
            known = false;
            Vec::new()
        }
    };
    match std::fs::read_dir(format!("{tpg}/acls")) {
        Ok(acls) => {
            let mut names: Vec<String> = acls
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            for initiator in names {
                let info = match std::fs::read_to_string(format!("{tpg}/acls/{initiator}/info")) {
                    Ok(text) => text,
                    Err(_) => {
                        known = false;
                        continue;
                    }
                };
                if !acl_info_connected(&info) {
                    continue;
                }
                if out.iter().any(|s| s.client == initiator) {
                    continue;
                }
                out.push(NasShareSession {
                    client: initiator.clone(),
                    user: initiator,
                    connected_at: None,
                });
            }
        }
        Err(_) => known = false,
    }
    (out, known)
}

/// Whether an ACL's `info` describes a live session.
///
/// The sentinel lives in `tentanas_helper::block` next to the plan that uses
/// it to decide whether an ACL may be removed — two copies of "the kernel says
/// this when nothing is connected" would be two places to get it wrong.
pub use block::acl_info_connected;

pub fn parse_dynamic_sessions(text: &str) -> Vec<NasShareSession> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(|initiator| NasShareSession {
            client: initiator.to_string(),
            user: initiator.to_string(),
            connected_at: None,
        })
        .collect()
}

// =============================================================================
// Protocol view
// =============================================================================

/// The row as the dashboard sees it. The two secret fields are `None` and the
/// two `*_set` booleans carry the only fact the UI needs: a secret exists.
pub fn to_protocol(target: &TargetRow, sessions: u32, sessions_known: bool) -> NasTarget {
    NasTarget {
        target_id: target.target_id.clone(),
        name: target.name.clone(),
        protocol: target.protocol.clone(),
        wwn: target.wwn.clone(),
        enabled: target.enabled,
        luns: target.luns.clone(),
        portals: target.portals.clone(),
        auth: NasTargetAuth {
            method: target.auth_method.clone(),
            username: target.auth_username.clone(),
            secret: None,
            mutual_username: target.auth_mutual_username.clone(),
            mutual_secret: None,
            secret_set: !target.auth_secret.is_empty(),
            mutual_secret_set: !target.auth_mutual_secret.is_empty(),
            dhchap_hash: target.dhchap_hash.clone(),
            dhchap_dhgroup: target.dhchap_dhgroup.clone(),
        },
        initiators: target.initiators.clone(),
        port_groups: target.port_groups.clone(),
        sessions,
        sessions_known,
        state: target.state.clone(),
        state_detail: target.state_detail.clone(),
        created_at: target.created_at.clone(),
        updated_at: target.updated_at.clone(),
    }
}

/// The port group every new target starts with: one group, Active/Optimized.
/// A second path adds a second group later without touching the model (R8).
pub fn default_port_groups() -> Vec<NasTargetPortGroup> {
    vec![NasTargetPortGroup {
        group_id: 1,
        state: "optimized".to_string(),
        preferred: false,
    }]
}

/// The single LUN a target created by the wizard exports.
pub fn lun_for(protocol: &str, zvol: &str, size_bytes: u64, thin: bool, uuid: &str) -> NasTargetLun {
    NasTargetLun {
        // NVMe namespace ids start at 1, SCSI LUNs at 0.
        index: u32::from(protocol == "nvmet"),
        source: zvol.to_string(),
        device_path: device_path(zvol),
        size_bytes,
        thin,
        uuid: uuid.to_string(),
        group_id: 1,
        source_kind: "zvol".to_string(),
    }
}

/// What a `TargetUpdateRequest` is allowed to do to a row's portals.
///
/// THE rule behind the owner's drift decision (2026-09-04), extracted from the
/// handler so it can be tested: a portal's ADDRESS changes only when the
/// request says it means to.
///
///   * no portals asked for       → the row keeps exactly what it has. That is
///     every save that is not the wizard — pause/resume, the allowlist.
///   * `repick` → the address is re-derived from the node (`primary_address`),
///     which is the wizard's step 2 and the repair the drift alert asks for.
///     An interface with no bindable address is an error, not an empty string
///     that would become a portal on nothing.
///   * otherwise → each portal keeps the address the ROW holds for that
///     interface, so only the transport can change. An interface the row does
///     not already hold is a portal move without the intent, and it is refused
///     rather than silently dropped — a save that reports success for a change
///     it discarded is the failure this whole path was fixed for.
///
/// The request's own `address` field is never read in any branch: it comes
/// from a browser, and a portal pointed by a request is a raw disk pointed by
/// a request.
pub fn portals_for_update(
    protocol: &str,
    stored: &[NasTargetPortal],
    asked: &[NasTargetPortal],
    repick: bool,
    interfaces: &[NasBlockInterface],
) -> Result<Vec<NasTargetPortal>> {
    if asked.is_empty() {
        return Ok(stored.to_vec());
    }
    let mut out = Vec::with_capacity(asked.len());
    for p in asked {
        let address = if repick {
            if p.interface.is_empty() {
                String::new()
            } else {
                primary_address(interfaces, &p.interface).ok_or_else(|| {
                    anyhow!(
                        "'{}' is not an interface of this node with an address a portal can bind",
                        p.interface
                    )
                })?
            }
        } else {
            stored
                .iter()
                .find(|s| s.interface == p.interface)
                .map(|s| s.address.clone())
                .ok_or_else(|| {
                    anyhow!(
                        "the portal of a target moves only in the wizard, where the admin is shown \
                         which address it moves to — this request binds it to '{}' without \
                         asking for it",
                        if p.interface.is_empty() {
                            "every interface"
                        } else {
                            p.interface.as_str()
                        }
                    )
                })?
        };
        out.push(portal_for(protocol, &p.interface, &address, &p.transport));
    }
    Ok(out)
}

/// The portal a wizard choice becomes. An empty interface is the deliberate
/// "every interface" of §5.5(a), and it reaches the kernel as `0.0.0.0`.
pub fn portal_for(
    protocol: &str,
    interface: &str,
    address: &str,
    transport: &str,
) -> NasTargetPortal {
    NasTargetPortal {
        interface: interface.to_string(),
        address: if interface.is_empty() {
            "0.0.0.0".to_string()
        } else {
            address.to_string()
        },
        port: if protocol == "nvmet" {
            block::NVME_PORT
        } else {
            block::ISCSI_PORT
        },
        transport: transport.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tentaflow_protocol::tentanas::NasTargetPortGroup;

    /// A throwaway directory tree that cleans itself up on `Drop` — the same
    /// shape as `block.rs`'s, and here for the same reason: a test that
    /// removes its tree only after the last assertion leaves the tree behind
    /// whenever it fails, which is exactly when someone is looking.
    struct TempTree(std::path::PathBuf);

    impl TempTree {
        fn new(tag: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "tentanas-targets-{tag}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).expect("temp tree");
            Self(path)
        }

        fn dir(&self, rel: &str) -> std::path::PathBuf {
            let path = self.0.join(rel);
            std::fs::create_dir_all(&path).expect("dir");
            path
        }
    }

    impl Drop for TempTree {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// The node as the target's portal expects it: storage0 holding 10.10.0.5.
    fn here() -> BTreeMap<String, Vec<String>> {
        BTreeMap::from([("storage0".to_string(), vec!["10.10.0.5".to_string()])])
    }

    /// A node whose interfaces hold `addrs`, one interface per entry.
    fn node(entries: &[(&str, &[&str])]) -> BTreeMap<String, Vec<String>> {
        entries
            .iter()
            .map(|(name, addrs)| {
                (
                    name.to_string(),
                    addrs.iter().map(|a| a.to_string()).collect(),
                )
            })
            .collect()
    }

    fn caps() -> NasBlockCapabilities {
        NasBlockCapabilities {
            iscsi: true,
            nvmet: true,
            iser: true,
            nvme_rdma: true,
            dhchap: true,
            ..Default::default()
        }
    }

    fn target(protocol: &str) -> TargetRow {
        let nvme = protocol == "nvmet";
        TargetRow {
            target_id: "0191f2c0-0000-7000-8000-000000000001".into(),
            name: "vm-store".into(),
            protocol: protocol.into(),
            wwn: wwn_for(protocol, "helios", "vm-store"),
            enabled: true,
            luns: vec![lun_for(
                protocol,
                "tank/vm-store",
                2_199_023_255_552,
                true,
                "0191f2c0-0000-7000-8000-0000000000aa",
            )],
            portals: vec![portal_for(protocol, "storage0", "10.10.0.5", "tcp")],
            port_groups: default_port_groups(),
            initiators: Vec::new(),
            auth_method: "none".into(),
            auth_username: String::new(),
            auth_secret: String::new(),
            auth_mutual_username: String::new(),
            auth_mutual_secret: String::new(),
            dhchap_hash: if nvme { "hmac(sha256)".into() } else { String::new() },
            dhchap_dhgroup: if nvme { "ffdhe2048".into() } else { String::new() },
            state: "active".into(),
            state_detail: String::new(),
            created_at: "2026-09-03T12:00:00Z".into(),
            updated_at: "2026-09-03T12:00:00Z".into(),
        }
    }

    fn chap(mut row: TargetRow) -> TargetRow {
        row.auth_method = "mutual-chap".into();
        row.auth_username = "vmware01".into();
        row.auth_secret = "encb:one".into();
        row.auth_mutual_username = "helios".into();
        row.auth_mutual_secret = "encb:two".into();
        row
    }

    #[test]
    fn the_rendered_iscsi_configuration_says_whether_chap_is_on() {
        let open = preview(&target("iscsi")).expect("preview");
        assert!(open.contains("/tpgt_1/param/AuthMethod = None\n"), "{open}");
        assert!(open.contains("/tpgt_1/attrib/authentication = 0\n"), "{open}");
        // No allowlist -> LIO generates the ACL, which is what makes an
        // unauthenticated target open to whoever reaches the portal.
        assert!(open.contains("/tpgt_1/attrib/generate_node_acls = 1\n"), "{open}");

        let secured = preview(&chap(target("iscsi"))).expect("preview");
        assert!(secured.contains("/tpgt_1/param/AuthMethod = CHAP\n"), "{secured}");
        assert!(secured.contains("/tpgt_1/attrib/authentication = 1\n"), "{secured}");
        assert!(secured.contains("/tpgt_1/auth/userid = vmware01\n"), "{secured}");
        assert!(secured.contains("/tpgt_1/auth/userid_mutual = helios\n"), "{secured}");
        // Mutual CHAP is the mutual PAIR and nothing else: LIO's
        // `authenticate_target` is CONFIGFS_ATTR_RO — measured on a node, the
        // write fails with EACCES even as root, and the kernel raises the flag
        // itself once `userid_mutual`/`password_mutual` are in. A plan that
        // carried it could never be applied at all.
        assert!(!secured.contains("authenticate_target"), "{secured}");
        // The preview is rendered from placeholders and redacted on top: no
        // secret can reach a screen or a log through it.
        assert!(secured.contains("password = ***"), "{secured}");
        assert!(!secured.contains("xxxxxxxxxxxx"), "{secured}");
        assert!(!secured.contains("encb:"), "{secured}");
    }

    #[test]
    fn the_rendered_nvmet_configuration_says_whether_dhchap_is_on() {
        let mut open = target("nvmet");
        open.portals = vec![portal_for("nvmet", "storage0", "10.10.0.5", "tcp")];
        let text = preview(&open).expect("preview");
        assert!(text.contains("/attr_allow_any_host = 1\n"), "{text}");
        assert!(text.contains("/namespaces/1/device_path = /dev/zvol/tank/vm-store\n"), "{text}");
        assert!(text.contains("/addr_trtype = tcp\n"), "{text}");
        assert!(text.contains("/addr_traddr = 10.10.0.5\n"), "{text}");
        assert!(text.contains("/addr_trsvcid = 4420\n"), "{text}");

        let mut secured = target("nvmet");
        secured.auth_method = "dhchap-bidi".into();
        secured.auth_secret = "encb:one".into();
        secured.auth_mutual_secret = "encb:two".into();
        secured.initiators = vec!["nqn.2014-08.org.nvmexpress:uuid:1b4e28ba".into()];
        let text = preview(&secured).expect("preview");
        // Authentication forces the allowlist on: the keys live on the host
        // objects the allowlist is made of.
        assert!(text.contains("/attr_allow_any_host = 0\n"), "{text}");
        assert!(text.contains("mkdir /sys/kernel/config/nvmet/hosts/nqn.2014-08.org.nvmexpress:uuid:1b4e28ba\n"), "{text}");
        assert!(text.contains("/dhchap_key = ***\n"), "{text}");
        assert!(text.contains("/dhchap_ctrl_key = ***\n"), "{text}");
        assert!(text.contains("/dhchap_hash = hmac(sha256)\n"), "{text}");
    }

    #[test]
    fn a_portal_binds_a_named_interface_and_every_interface_needs_a_decision() {
        let bound = target("iscsi");
        assert_eq!(bound.portals[0].address, "10.10.0.5");
        assert_eq!(bound.portals[0].interface, "storage0");
        assert!(validate_options(&bound, &[], &caps(), false).is_ok());

        // §5.5(a): 0.0.0.0 is possible and never the default.
        let mut every = target("iscsi");
        every.portals = vec![portal_for("iscsi", "", "", "tcp")];
        assert_eq!(every.portals[0].address, "0.0.0.0");
        let refused = validate_options(&every, &[], &caps(), false).expect_err("refused");
        assert!(refused.to_string().contains("0.0.0.0"), "{refused}");
        assert!(validate_options(&every, &[], &caps(), true).is_ok(), "confirmed is allowed");
    }

    #[test]
    fn the_allowlist_is_applied_as_a_filter_and_is_never_the_authentication() {
        // An iSCSI target with an allowlist and no CHAP is legal — and the
        // rendered configfs shows exactly what that is worth: the ACL exists,
        // its credentials are CLEARED with LIO's own sentinel, AuthMethod is
        // None. A filter, not a login.
        let mut listed = target("iscsi");
        listed.initiators = vec!["iqn.1998-01.com.vmware:esx01".into()];
        assert!(validate_options(&listed, &[], &caps(), false).is_ok());
        let text = preview(&listed).expect("preview");
        assert!(text.contains("/tpgt_1/attrib/generate_node_acls = 0\n"), "{text}");
        assert!(text.contains("/acls/iqn.1998-01.com.vmware:esx01/auth/userid = NULL\n"), "{text}");
        assert!(text.contains("/tpgt_1/param/AuthMethod = None\n"), "{text}");
        // …and the target still reports why it is not safe, on every list.
        let (state, detail, _) = target_state(&listed, true, &|_| true, &here(), true);
        assert_eq!(state, "active");
        assert!(detail.contains("filter, not a login"), "{detail}");

        // With CHAP the same list carries credentials, and only then.
        let text = preview(&chap(listed)).expect("preview");
        assert!(text.contains("/acls/iqn.1998-01.com.vmware:esx01/auth/userid = vmware01\n"), "{text}");
    }

    #[test]
    fn nvmet_authentication_requires_the_allowlist_because_the_keys_live_on_it() {
        let mut row = target("nvmet");
        row.auth_method = "dhchap".into();
        let refused = validate_options(&row, &[], &caps(), false).expect_err("refused");
        assert!(refused.to_string().contains("host objects"), "{refused}");
        row.initiators = vec!["nqn.2014-08.org.nvmexpress:uuid:1b4e28ba".into()];
        assert!(validate_options(&row, &[], &caps(), false).is_ok());

        // A kernel without CONFIG_NVME_TARGET_AUTH does not get the option at
        // all, and says why.
        let mut no_auth = caps();
        no_auth.dhchap = false;
        no_auth.dhchap_detail = "this kernel was built without CONFIG_NVME_TARGET_AUTH".into();
        let refused = validate_options(&row, &[], &no_auth, false).expect_err("refused");
        assert!(refused.to_string().contains("CONFIG_NVME_TARGET_AUTH"), "{refused}");
    }

    #[test]
    fn a_transport_the_node_cannot_serve_is_refused_with_the_probe_s_own_reason() {
        let mut iser = target("iscsi");
        iser.portals = vec![portal_for("iscsi", "storage0", "10.10.0.5", "iser")];
        assert!(validate_options(&iser, &[], &caps(), false).is_ok());
        let mut no_rdma = caps();
        no_rdma.iser = false;
        no_rdma.rdma_detail = "no RDMA device under /sys/class/infiniband".into();
        let refused = validate_options(&iser, &[], &no_rdma, false).expect_err("refused");
        assert!(refused.to_string().contains("/sys/class/infiniband"), "{refused}");

        let mut rdma = target("nvmet");
        rdma.portals = vec![portal_for("nvmet", "storage0", "10.10.0.5", "rdma")];
        assert!(validate_options(&rdma, &[], &caps(), false).is_ok());
        no_rdma.nvme_rdma = false;
        assert!(validate_options(&rdma, &[], &no_rdma, false).is_err());
        // A transport of the other protocol is not silently accepted.
        let mut wrong = target("nvmet");
        wrong.portals = vec![portal_for("nvmet", "storage0", "10.10.0.5", "iser")];
        assert!(validate_options(&wrong, &[], &caps(), false).is_err());
    }

    #[test]
    fn the_port_group_state_reaches_both_protocols_and_preferred_only_reaches_iscsi() {
        let mut scsi = target("iscsi");
        scsi.port_groups = vec![NasTargetPortGroup {
            group_id: 2,
            state: "non-optimized".into(),
            preferred: true,
        }];
        scsi.luns[0].group_id = 2;
        assert!(validate_options(&scsi, &[], &caps(), false).is_ok());
        let text = preview(&scsi).expect("preview");
        assert!(text.contains("/alua/tentanas_gp2/alua_access_state = 1\n"), "{text}");
        assert!(text.contains("/alua/tentanas_gp2/preferred = 1\n"), "{text}");

        let mut nvme = target("nvmet");
        nvme.port_groups = vec![NasTargetPortGroup {
            group_id: 2,
            state: "non-optimized".into(),
            preferred: false,
        }];
        nvme.luns[0].group_id = 2;
        let text = preview(&nvme).expect("preview");
        assert!(text.contains("/ana_groups/2/ana_state = non-optimized\n"), "{text}");
        // ANA has no preferred bit, so asking for one is an error.
        nvme.port_groups[0].preferred = true;
        let refused = validate_options(&nvme, &[], &caps(), false).expect_err("refused");
        assert!(refused.to_string().contains("preferred"), "{refused}");
    }

    #[test]
    fn the_preview_wrapper_composes_the_two_configfs_roots_it_claims_to() {
        // `preview` is a two-line wrapper whose entire content is which roots
        // it hands `preview_in`. Every other preview test goes through the
        // wrapper against the build machine's real (empty) `/sys/kernel/config`
        // and would pass just as well with a typo in either constant — an
        // empty plan and no red anywhere. The one test with injected roots
        // calls `preview_in` and never sees them.
        assert_eq!(block::NVMET_CONFIGFS, "/sys/kernel/config/nvmet");
        assert_eq!(block::TARGET_CONFIGFS, "/sys/kernel/config/target");
        // …and the wrapper still renders through them: on this host nothing is
        // in configfs, so an nvmet target renders a first-install plan naming
        // those two roots and nothing else.
        let text = preview(&target("nvmet")).expect("preview");
        assert!(text.contains("mkdir /sys/kernel/config/nvmet/subsystems/"), "{text}");
        let text = preview(&target("iscsi")).expect("preview");
        assert!(text.contains("mkdir /sys/kernel/config/target/iscsi/"), "{text}");
    }

    #[test]
    fn the_preview_says_what_it_cannot_know_instead_of_guessing() {
        // `preview` is what the detail window prints under "podgląd konfiguracji",
        // and it is where an admin looks when a target is in `error`. It runs
        // UNPRIVILEGED with placeholder credentials, so it cannot read a
        // `dhchap_key` that `Protect` has chmodded to 0600.
        //
        // It used to force every shared host to "agrees", which made the plan
        // state "already holds exactly this key" as a fact — on the one screen
        // whose whole job is to explain a conflict, saying the opposite of it.
        //
        // Reachable at all only because the configfs roots are injected now;
        // hard-coded, every `preview` test ran against the build machine's
        // empty `/sys/kernel/config` and this branch had no test.
        let tree = TempTree::new("preview-shared");
        let mut row = target("nvmet");
        row.auth_method = "dhchap".into();
        row.auth_secret = "encb:one".into();
        row.initiators = vec!["nqn.2014-08.org.nvmexpress:uuid:esx01".into()];
        let host = &row.initiators[0];

        // Two subsystems link one host object — the ordinary §6.1 topology.
        let ours = tree.dir(&format!("subsystems/{}/allowed_hosts", row.wwn));
        let theirs = tree.dir("subsystems/nqn.2026-09.local.tentaflow:helios.vm-b/allowed_hosts");
        // The object as CONFIGFS presents one: all four attribute files
        // (obs. 48), the two non-key ones already at their defaults (obs. 53).
        // A host directory carrying `dhchap_key` alone is a shape the kernel
        // never produces, and building one here would have exercised a
        // comparison path that cannot occur on a real node.
        std::fs::create_dir_all(tree.0.join("hosts").join(host)).expect("host");
        for (name, value) in [
            ("dhchap_key", "DHHC-1:00:zzzz+/=:\n"),
            ("dhchap_ctrl_key", "\n"),
            ("dhchap_hash", "hmac(sha256)\n"),
            ("dhchap_dhgroup", "null\n"),
        ] {
            std::fs::write(tree.0.join("hosts").join(host).join(name), value).expect("attribute");
        }
        for dir in [&ours, &theirs] {
            std::os::unix::fs::symlink(tree.0.join("hosts").join(host), dir.join(host))
                .expect("link");
        }

        use std::os::unix::fs::PermissionsExt;
        let key = tree.0.join("hosts").join(host).join("dhchap_key");
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o000)).expect("chmod");
        let readable_anyway = std::fs::read_to_string(&key).is_ok();

        let rendered = preview_in(&row, &tree.0, &tree.0);
        if readable_anyway {
            // Running as root, which the core never is in production. The
            // comparison is then real: the placeholder key differs from the
            // stored one, so the honest answer is the same refusal the apply
            // would give, and it arrives as an error rather than as a plan.
            let refused = rendered.expect_err("a real difference is a refusal");
            assert!(refused.to_string().contains("DH-HMAC-CHAP settings"), "{refused}");
        } else {
            let text = rendered.expect("an unreadable host still renders");
            // The claim it must never make: it never read the key.
            assert!(!text.contains("already holds exactly"), "{text}");
            assert!(text.contains("readable only by root"), "{text}");
            assert!(text.contains("the node decides that when it applies"), "{text}");
            // And it does not pretend the object needs writing either.
            // `= ***`, not `= `: `protect …/dhchap_key = 0600` contains the
            // looser form, so the loose assertion passes for any plan at all.
            assert!(!text.contains(&format!("/hosts/{host}/dhchap_key = ***")), "{text}");
        }
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o644)).expect("chmod back");
    }

    #[test]
    fn two_nvmet_targets_may_not_disagree_about_a_host_they_share() {
        // nvmet's host object is NODE-WIDE and carries the DH-HMAC-CHAP
        // settings. `block::host_verdict` is THE authority on it and judges
        // against the kernel; this is the part of the same question the
        // DATABASE can answer, asked before the zvol is created instead of
        // after.
        //
        // Strict subset, and that is the property being asserted here — not
        // stated in a doc-comment and left unchecked, which is how it came to
        // be false in three different ways.
        let esx = "nqn.2014-08.org.nvmexpress:uuid:esx01";
        let mut authenticated = target("nvmet");
        authenticated.auth_method = "dhchap".into();
        authenticated.auth_secret = "encb:one".into();
        authenticated.initiators = vec![esx.to_string()];

        let mut open = target("nvmet");
        open.target_id = "0191f2c0-0000-7000-8000-0000000000c1".into();
        open.name = "scratch".into();
        open.wwn = wwn_for("nvmet", "helios", "scratch");
        open.auth_method = "none".into();
        open.initiators = vec![esx.to_string()];

        // The sibling is LIVE unless a case says otherwise: the whole rule
        // turns on that, and on a build machine nothing is ever in configfs.
        let live = |_: &TargetRow| true;
        let check = |row: &TargetRow, siblings: &[TargetRow], in_kernel: &dyn Fn(&TargetRow) -> bool| {
            validate_options_with(row, siblings, &caps(), false, in_kernel)
        };

        // Alone, each is a perfectly legal target: an allowlist without a key
        // is a filter, not a login (§5.5).
        assert!(check(&authenticated, &[], &live).is_ok());
        assert!(check(&open, &[], &live).is_ok());

        // Together they disagree about one kernel object, in both directions.
        let refused = check(&open, std::slice::from_ref(&authenticated), &live).expect_err("refused");
        assert!(refused.to_string().contains(esx), "{refused}");
        assert!(refused.to_string().contains("vm-store"), "{refused}");
        assert!(
            check(&authenticated, std::slice::from_ref(&open), &live).is_err(),
            "and the same pair the other way round"
        );

        // Case is not a way out: this check runs BEFORE the catalog's own
        // NQN-shape rule, so the message has to be THIS one.
        let mut shouting = open.clone();
        shouting.initiators = vec![esx.to_uppercase()];
        let refused = check(&shouting, std::slice::from_ref(&authenticated), &live).expect_err("refused");
        assert!(refused.to_string().contains("shared by the whole node"), "{refused}");

        // ---- the SUBSET property, which the doc-comment used to assert and
        // the code used to break in three separate ways ----

        // (1) A sibling whose object is NOT in the kernel. Disabled, frozen,
        // never applied — the node answers `Sole` and takes the save, so this
        // must too. It used to refuse.
        assert!(
            check(&open, std::slice::from_ref(&authenticated), &|_| false).is_ok(),
            "a sibling that holds no host object cannot disagree about one"
        );

        // (2) An imported `dhchap` row with no secret yet (§5.8). The catalog
        // refuses to render it, so it never creates a host object either — and
        // refusing the OTHER target because of it left the admin with a
        // message about an object that does not exist and no way out of it.
        let mut imported = authenticated.clone();
        imported.auth_secret = String::new();
        assert!(
            check(&open, std::slice::from_ref(&imported), &live).is_ok(),
            "a row that cannot be applied cannot own the object"
        );
        // …and once the admin retypes the key, the conflict is real again.
        assert!(check(&open, std::slice::from_ref(&authenticated), &live).is_err());

        // (3) Same method: the keys may still differ, and the core cannot
        // tell — ciphertext is bound to each target id, so equal plaintexts
        // are unequal strings. That case belongs to the node.
        let mut same = open.clone();
        same.auth_method = "dhchap".into();
        same.auth_secret = "encb:two".into();
        assert!(check(&same, std::slice::from_ref(&authenticated), &live).is_ok());

        // A different host NQN — no shared object at all:
        let mut elsewhere = open.clone();
        elsewhere.initiators = vec!["nqn.2014-08.org.nvmexpress:uuid:esx02".into()];
        assert!(check(&elsewhere, std::slice::from_ref(&authenticated), &live).is_ok());
        // An iSCSI neighbour: its allowlist is IQNs on a TPG, not a host
        // object, so it shares nothing.
        let mut iscsi_neighbour = authenticated.clone();
        iscsi_neighbour.protocol = "iscsi".into();
        iscsi_neighbour.auth_method = "chap".into();
        assert!(check(&open, std::slice::from_ref(&iscsi_neighbour), &live).is_ok());
        // …and the row itself is never its own neighbour, which is what makes
        // this survive an EDIT that changes nothing about the allowlist.
        assert!(check(&open, std::slice::from_ref(&open), &live).is_ok());
    }

    #[test]
    fn a_node_whose_modules_are_not_loaded_yet_can_still_serve_after_a_reboot() {
        // THE cold-boot loop, pinned as a pure decision so it holds on any
        // host. §3.4 forbids `target.service`, so after a reboot nothing has
        // loaded `target_core_mod` and `/sys/kernel/config/target` is absent.
        // The verdict used to ask exactly that question, judge every row
        // `Remove`, and skip the applying loop — which is the ONLY place
        // `BlockModulesLoad` is ever called. The modules loaded only once the
        // modules were loaded, so nothing ever came back and the restore loop
        // reported success over an empty apply.
        assert!(can_serve(false, 0), "a cold boot with the modules present can serve");
        assert!(can_serve(true, 2), "a loaded tree can serve whatever the module tree says");
        assert!(!can_serve(false, 1), "a kernel that has no such module cannot");

        // The verdict against BOTH answers, on any host. Deriving the expected
        // value from the same production call the code makes is a tautology —
        // it passes whatever `kernel_support` returns, including "no" for a
        // node that can serve perfectly well. So the predicate is injected with
        // each answer in turn and the verdict is asserted against a constant.
        for protocol in ["iscsi", "nvmet"] {
            let mut row = target("iscsi");
            row.protocol = protocol.to_string();
            let (state, detail, verdict) =
                target_state(&row, true, &|_| false, &here(), false);
            assert_eq!(verdict, Disposition::Remove, "a node that cannot serve it takes it out");
            assert_eq!(state, "error");
            assert!(detail.contains("not available on this node"), "{detail}");
            // Can serve, not in the kernel yet: appliable and honest about it.
            let (state, _, verdict) = target_state(&row, true, &|_| true, &here(), false);
            assert_eq!((state, verdict), ("pending", Disposition::Apply));
            // Can serve, and serving: the only green state there is.
            let (state, _, verdict) = target_state(&row, true, &|_| true, &here(), true);
            assert_eq!((state, verdict), ("active", Disposition::Apply));
        }

        // And the REAL predicate is what the callers pass. The injection is
        // why the cold-boot loop went unseen for a round, so the two have to
        // agree on every input this test can produce — including the one that
        // used to differ, a protocol whose configfs tree is absent.
        for protocol in ["iscsi", "nvmet", "not-a-protocol"] {
            assert_eq!(kernel_can_serve(protocol), kernel_support(protocol).0);
        }
        // A name that is not a block protocol is never "supported", whatever
        // this host's configfs happens to contain — the check that stops an
        // empty module list reading as "nothing is missing".
        assert!(!kernel_can_serve("not-a-protocol"));

        // The one host-dependent implication worth stating: a tree that is
        // there is a node that can serve. On a host without it this says
        // nothing, and that is fine — the decision itself is pinned above.
        for protocol in ["iscsi", "nvmet"] {
            if configfs_present(protocol) {
                let (support, detail) = kernel_support(protocol);
                assert!(support, "{protocol}: configfs is there but {detail}");
            }
        }
    }

    #[test]
    fn state_reports_the_reason_a_target_is_not_exported() {
        let row = target("iscsi");
        let here = here();
        let mut off = row.clone();
        off.enabled = false;
        // A disabled target and a node with no LIO both LEAVE the kernel: the
        // row no longer describes what would be served.
        assert_eq!(
            target_state(&off, true, &|_| true, &here, true),
            ("disabled", String::new(), Disposition::Remove)
        );
        assert_eq!(
            target_state(&row, true, &|_| false, &here, true),
            (
                "error",
                "the LIO kernel target is not available on this node".to_string(),
                Disposition::Remove
            )
        );
        let (state, detail, verdict) = target_state(&row, false, &|_| true, &here, true);
        assert_eq!((state, verdict), ("error", Disposition::Remove));
        assert!(detail.contains("backing volume"), "{detail}");
        // The kernel binds an ADDRESS, not a netdev, so an interface that
        // moved is REPORTED — and reported is all it is (owner decision
        // 2026-09-04): the target is neither re-bound onto whatever address
        // turned up nor torn out from under a client that is writing to it.
        let readdressed = node(&[("storage0", &["192.168.1.5"])]);
        let (state, detail, verdict) = target_state(&row, true, &|_| true, &readdressed, true);
        assert_eq!((state, verdict), ("error", Disposition::Freeze));
        assert!(detail.contains("192.168.1.5") && detail.contains("storage0"), "{detail}");
        assert!(detail.contains("10.10.0.5"), "the sentence names the portal that went missing: {detail}");
        assert!(detail.contains("no interface of this node has that address"), "{detail}");
        let (state, detail, verdict) = target_state(&row, true, &|_| true, &node(&[("lan0", &["192.168.1.5"])]), true);
        assert_eq!((state, verdict), ("error", Disposition::Freeze));
        assert!(detail.contains("gone from this node"), "{detail}");
    }

    #[test]
    fn a_save_moves_a_portal_only_when_the_request_asks_to_move_it() {
        // The rule the owner's drift decision rests on, tested where it lives
        // rather than only in the handler that calls it — the handler needs a
        // whole request context, which is why this branch had no test at all
        // and why the plan's description of it was unverifiable.
        let aliased = vec![
            NasBlockInterface {
                name: "storage0".into(),
                address: "10.10.0.5".into(),
                supported: true,
                ..Default::default()
            },
            NasBlockInterface {
                name: "storage0".into(),
                address: "10.10.0.9".into(),
                supported: true,
                ..Default::default()
            },
        ];
        // The row sits on the SECOND address — healthy, not drift.
        let stored = vec![portal_for("iscsi", "storage0", "10.10.0.9", "tcp")];

        // 1. Nothing asked for: the row keeps exactly what it has. This is
        //    pause/resume and the allowlist, which send no portals at all.
        let kept = portals_for_update("iscsi", &stored, &[], false, &aliased).expect("kept");
        assert_eq!(kept, stored);

        // 2. A transport change with no re-pick intent keeps the ADDRESS and
        //    takes the transport: iSER is not a different place to listen.
        let asked = vec![portal_for("iscsi", "storage0", "ignored-by-design", "iser")];
        let changed = portals_for_update("iscsi", &stored, &asked, false, &aliased).expect("ok");
        assert_eq!(changed[0].address, "10.10.0.9", "the alias is not collapsed");
        assert_eq!(changed[0].transport, "iser");

        // 3. WITH the intent the address is re-derived from the node — the
        //    wizard's re-pick, which is what the drift alert asks for.
        let moved = portals_for_update("iscsi", &stored, &asked, true, &aliased).expect("ok");
        assert_eq!(moved[0].address, "10.10.0.5");

        // 4. Binding another interface without the intent is REFUSED, not
        //    quietly ignored: a save that reports success for a change it threw
        //    away is the failure this path exists to prevent.
        let elsewhere = vec![portal_for("iscsi", "lan0", "", "tcp")];
        let refused = portals_for_update("iscsi", &stored, &elsewhere, false, &aliased)
            .expect_err("refused");
        assert!(refused.to_string().contains("only in the wizard"), "{refused}");

        // 5. And an interface with no address a portal can bind is an error
        //    even WITH the intent — never an empty address, which would become
        //    a portal on nothing.
        let ipv6 = vec![NasBlockInterface {
            name: "lan0".into(),
            address: "fe80::1".into(),
            supported: false,
            ..Default::default()
        }];
        let refused =
            portals_for_update("iscsi", &stored, &elsewhere, true, &ipv6).expect_err("refused");
        assert!(refused.to_string().contains("a portal can bind"), "{refused}");

        // 6. The deliberate every-interface portal survives a re-pick as
        //    `0.0.0.0` rather than as an empty address.
        let all = vec![portal_for("iscsi", "", "", "tcp")];
        let every = portals_for_update("iscsi", &stored, &all, true, &aliased).expect("ok");
        assert_eq!(every[0].address, "0.0.0.0");
    }

    #[test]
    fn one_interface_has_one_definition_of_its_address_and_an_alias_never_moves_a_live_portal() {
        // The bug two definitions made possible: `target_state` compared a
        // portal against ALL of an interface's addresses (so a target bound to
        // a secondary one was correctly NOT drifting), while the update
        // handler took the FIRST address of the first matching entry. An
        // interface holding both, saved for any reason at all — pause, resume,
        // the allowlist — had its portal rewritten onto the sibling address,
        // and the prune then `rmdir`-ed the live one underneath every
        // initiator logged in on it.
        let aliased = vec![
            NasBlockInterface {
                name: "storage0".into(),
                address: "10.10.0.5".into(),
                supported: true,
                ..Default::default()
            },
            NasBlockInterface {
                name: "storage0".into(),
                address: "10.10.0.9".into(),
                supported: true,
                ..Default::default()
            },
            // IPv6 is listed for the picker and is not bindable by this slice,
            // so it is not one of the interface's addresses as far as a portal
            // is concerned — an empty address would be a portal on nothing.
            NasBlockInterface {
                name: "lan0".into(),
                address: "fe80::1".into(),
                supported: false,
                ..Default::default()
            },
        ];
        assert_eq!(bindable_addresses(&aliased, "storage0"), ["10.10.0.5", "10.10.0.9"]);
        assert_eq!(primary_address(&aliased, "storage0").as_deref(), Some("10.10.0.5"));
        assert!(bindable_addresses(&aliased, "lan0").is_empty());
        assert_eq!(primary_address(&aliased, "lan0"), None);

        // The drift check and the picker now read the same list, so a target
        // on the SECONDARY address is healthy — and stays on it, because
        // nothing outside the wizard recomputes the address at all.
        let mut secondary = target("iscsi");
        secondary.portals[0].address = "10.10.0.9".into();
        let addresses = node(&[("storage0", &["10.10.0.5", "10.10.0.9"])]);
        let (state, _, verdict) = target_state(&secondary, true, &|_| true, &addresses, true);
        assert_eq!((state, verdict), ("active", Disposition::Apply));
    }

    #[test]
    fn a_portal_address_that_walked_to_another_nic_says_which_one() {
        // The dangerous shape of drift: the kernel binds an ADDRESS, so when
        // the address itself migrates the export keeps serving — on an
        // interface nobody picked for it, quite possibly the LAN. The app
        // still does not act (that is the owner's decision), so the sentence
        // is the whole safety net and it has to name the interface.
        let row = target("iscsi");
        let walked = node(&[("storage0", &["10.10.9.9"]), ("lan0", &["10.10.0.5", "192.168.1.5"])]);
        let (state, detail, verdict) = target_state(&row, true, &|_| true, &walked, true);
        assert_eq!((state, verdict), ("error", Disposition::Freeze));
        assert!(detail.contains("moved to lan0"), "{detail}");
        assert!(detail.contains("reachable there"), "{detail}");
        // And the interface it left is still named with what it holds now.
        assert!(detail.contains("storage0 now has 10.10.9.9"), "{detail}");

        // …but "the export is reachable there" is a claim about what this node
        // SERVES, and after a reboot configfs is empty and the freeze keeps it
        // that way. Telling an admin a raw disk is exposed on a NIC that is
        // serving nothing is the same class of lie as a measured zero.
        let (state, detail, verdict) = target_state(&row, true, &|_| true, &walked, false);
        assert_eq!((state, verdict), ("error", Disposition::Freeze));
        assert!(detail.contains("moved to lan0"), "{detail}");
        assert!(!detail.contains("reachable there"), "{detail}");
        assert!(detail.contains("not in the kernel"), "{detail}");
    }

    #[test]
    fn the_wizard_previews_the_wwn_with_this_module_s_own_naming_authority() {
        // The step-3 preview of a target that does not exist yet has to build
        // the IQN in the browser, so `target-wizard.js` carries a copy of this
        // constant. Nothing else checks the two against each other, and a
        // divergence would make the wizard show an IQN the node never creates.
        let js = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/www/js/modules/tentanas/target-wizard.js"
        ))
        .expect("the wizard source is part of this crate");
        assert!(
            js.contains(&format!("export const WWN_AUTHORITY = '{WWN_AUTHORITY}';")),
            "target-wizard.js must declare WWN_AUTHORITY = '{WWN_AUTHORITY}'"
        );
        assert!(
            js.contains("`${prefix}.${WWN_AUTHORITY}:${host}.${state.name}`"),
            "the preview must be built from that constant, not from a literal"
        );

        // The DERIVATION, not just the constant. The node name is sanitised on
        // both sides and the two do it differently — `to_ascii_lowercase` plus
        // an ASCII filter here, `.toLowerCase()` plus a regex there — so a
        // shared constant proves nothing about a name with an underscore, a
        // dot or a capital in it. This asserts the whole string against a
        // literal, and `the step-3 IQN preview derives the same WWN the node
        // would create` in target-wizard.test.js drives the real wizard to THE
        // SAME literal. Either side drifting fails one of the two.
        assert_eq!(
            wwn_for("iscsi", "Helios_02.lan", "vm-store"),
            "iqn.2026-09.local.tentaflow:helios02lan.vm-store"
        );
        assert_eq!(
            wwn_for("nvmet", "Helios_02.lan", "vm-store"),
            "nqn.2026-09.local.tentaflow:helios02lan.vm-store"
        );
    }

    #[test]
    fn the_view_answers_that_a_secret_exists_and_never_what_it_is() {
        let view = to_protocol(&chap(target("iscsi")), 3, true);
        assert!(view.auth.secret_set && view.auth.mutual_secret_set);
        assert!(view.auth.secret.is_none() && view.auth.mutual_secret.is_none());
        assert_eq!(view.auth.username, "vmware01");
        assert_eq!(view.sessions, 3);
        assert!(view.sessions_known);
        assert_eq!(view.port_groups, default_port_groups());
        let text = format!("{view:?}");
        assert!(!text.contains("encb:one"), "{text}");
    }

    #[test]
    fn the_wwn_is_derived_from_the_node_and_the_name_and_survives_the_name_rules() {
        assert_eq!(
            wwn_for("iscsi", "helios", "vm-store"),
            "iqn.2026-09.local.tentaflow:helios.vm-store"
        );
        assert_eq!(
            wwn_for("nvmet", "helios", "scratch"),
            "nqn.2026-09.local.tentaflow:helios.scratch"
        );
        // A node name with characters an IQN may not carry is filtered, not
        // rejected: the admin does not choose the hostname here.
        assert_eq!(
            wwn_for("iscsi", "Helios_02.lan", "x"),
            "iqn.2026-09.local.tentaflow:helios02lan.x"
        );
        assert!(name_valid("vm-store2"));
        assert!(name_valid("vm.store"));
        assert!(!name_valid("VM-Store"));
        assert!(!name_valid("2fast"));
        assert!(!name_valid("vm store"));
        assert!(!name_valid("vm/store"));
        assert!(!name_valid(""));
        assert!(!name_valid(&"a".repeat(NAME_MAX + 1)));
    }

    #[test]
    fn a_zvol_already_exported_is_named_with_the_target_that_holds_it() {
        let datasets = vec![
            NasDataset {
                name: "tank/vm-store".into(),
                kind: "volume".into(),
                volsize_bytes: Some(2_199_023_255_552),
                thin: true,
                ..Default::default()
            },
            NasDataset {
                name: "tank/wolny".into(),
                kind: "volume".into(),
                volsize_bytes: Some(1_073_741_824),
                thin: false,
                ..Default::default()
            },
            NasDataset {
                name: "tank/projekty".into(),
                kind: "filesystem".into(),
                ..Default::default()
            },
        ];
        let list = volumes(&datasets, &[target("iscsi")]);
        // A filesystem is not a block source.
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].name, "tank/vm-store");
        assert_eq!(list[0].exported_by, "vm-store");
        assert_eq!(list[0].device_path, "/dev/zvol/tank/vm-store");
        assert_eq!(list[0].pool, "tank");
        assert_eq!(list[1].exported_by, "");
    }

    #[test]
    fn the_default_route_interface_is_the_one_marked_shared() {
        // Two routes on one interface, one of them the default; a second
        // interface with a route that is not.
        let text = "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\n\
                    lan0\t00000000\t0101A8C0\t0003\t0\t0\t100\t00000000\n\
                    storage0\t000AA8C0\t00000000\t0001\t0\t0\t0\t00FFFFFF\n";
        assert_eq!(parse_default_route(text), Some("lan0".to_string()));
        // A node with no default route has no shared interface at all.
        let none = "Iface\tDestination\tGateway \tFlags\n\
                    storage0\t000AA8C0\t00000000\t0001\n";
        assert_eq!(parse_default_route(none), None);
        assert_eq!(parse_default_route(""), None);
    }

    #[test]
    fn the_lun_index_follows_the_protocol_and_the_backstore_name_is_derived_from_it() {
        assert_eq!(lun_for("iscsi", "tank/x", 1, true, "u").index, 0);
        assert_eq!(lun_for("nvmet", "tank/x", 1, true, "u").index, 1);
        // The backstore object name is a configfs directory, so the dots and
        // dashes of a target name become underscores.
        let mut row = target("iscsi");
        row.name = "vm-store.2".into();
        assert_eq!(lun_specs(&row)[0].name, "tentanas_vm_store_2_lun0");
    }

    #[test]
    fn sessions_come_from_lio_and_nvmet_reports_none_rather_than_a_number_it_cannot_know() {
        let text = "iqn.1998-01.com.vmware:esx01\niqn.1998-01.com.vmware:esx02\n\n";
        let parsed = parse_dynamic_sessions(text);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].client, "iqn.1998-01.com.vmware:esx01");
        // NVMe-oF sessions do not live in configfs at all, so this reader has
        // nothing to say about them — and says so as a MEASURED empty rather
        // than an unknown, because "not this protocol's business" is a fact
        // about the code and not about the node.
        assert_eq!(sessions(&target("nvmet")), (Vec::new(), true));

        // iSCSI, on a host with no configfs object for this target: a measured
        // zero. There is nothing to be logged into.
        let (found, known) = sessions(&target("iscsi"));
        assert!(found.is_empty());
        assert!(known, "a target that is not in the kernel has a knowable zero");
    }

    #[test]
    fn the_block_rows_are_decided_by_the_kernel_and_never_by_targetcli() {
        // The critical property (BLK-03): this app writes configfs itself and
        // its catalog has no entry that runs targetcli or nvmetcli, so the
        // Environment row must not be a verdict about those binaries. On THIS
        // host the answer comes from the modules and the configfs tree, and
        // whatever it is, it is the same one `capabilities` and `services` give.
        let mut row = FeatureState {
            id: "iscsi".into(),
            status: "missing".into(),
            detail: "missing: targetcli".into(),
            binaries: vec!["targetcli".into()],
            packages: vec!["targetcli-fb".into()],
            ..Default::default()
        };
        refine(&mut row);
        assert!(!row.detail.contains("targetcli"), "{}", row.detail);
        assert!(row.binaries.is_empty(), "no userspace tool decides this row");
        assert!(row.packages.is_empty(), "nothing to install for a kernel module");
        assert_eq!(row.kernel_module.as_deref(), Some("target_core_mod"));
        // §5.5a: the iSER module state is on the row.
        assert!(row.detail.contains("ib_isert"), "{}", row.detail);

        let (kernel_ok, kernel_detail) = kernel_support("iscsi");
        assert_eq!(row.status == "ok", kernel_ok);
        assert!(row.detail.starts_with(&kernel_detail), "{}", row.detail);

        // §5.5: the DH-HMAC-CHAP probe lives in the Environment tab. n16 gives
        // it its OWN row — an admin scanning the Status column for "can this
        // kernel authenticate NVMe-oF hosts" has to find an answer there, not
        // a fragment in the middle of another row's detail string.
        let mut nvmet = FeatureState {
            id: "nvmet".into(),
            ..Default::default()
        };
        refine(&mut nvmet);
        assert!(nvmet.detail.contains("nvmet-rdma"), "{}", nvmet.detail);

        let mut dhchap = FeatureState {
            id: DHCHAP_FEATURE_ID.into(),
            ..Default::default()
        };
        refine(&mut dhchap);
        let (dhchap_ok, dhchap_detail) = dhchap_support();
        assert_eq!(dhchap.detail, dhchap_detail);
        assert_eq!(dhchap.status, if dhchap_ok { "ok" } else { "missing_module" });
        // No module and no binary: the answer is a kernel BUILD option, and an
        // install button would promise something no package can deliver.
        assert_eq!(dhchap.kernel_module, None);
        assert!(dhchap.binaries.is_empty() && dhchap.packages.is_empty());

        // `services` and `capabilities` answer the same question the row does.
        let rows = services();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].protocol, "iscsi");
        assert_eq!(rows[0].config_path, "/sys/kernel/config/target");
        assert_eq!(rows[0].installed, kernel_ok);
        assert_eq!(rows[0].detail, kernel_detail);
        let caps = capabilities(&[], &[], &[]);
        assert_eq!(caps.iscsi, kernel_ok);
        assert_eq!(caps.iscsi_detail, kernel_detail);
    }

    #[test]
    fn a_kernel_config_verdict_is_read_from_the_symbol_and_nothing_else() {
        let yes = "CONFIG_NVME_TARGET=m\nCONFIG_NVME_TARGET_AUTH=y\n";
        assert_eq!(kconfig_verdict(yes, "CONFIG_NVME_TARGET_AUTH"), Some(true));
        let module = "CONFIG_NVME_TARGET_AUTH=m\n";
        assert_eq!(kconfig_verdict(module, "CONFIG_NVME_TARGET_AUTH"), Some(true));
        let no = "# CONFIG_NVME_TARGET_AUTH is not set\n";
        assert_eq!(kconfig_verdict(no, "CONFIG_NVME_TARGET_AUTH"), Some(false));
        // A kernel config that never mentions the symbol is "unknown", not
        // "off" — the two get different sentences.
        assert_eq!(kconfig_verdict("CONFIG_NVME_TARGET=m\n", "CONFIG_NVME_TARGET_AUTH"), None);
        // A symbol that merely CONTAINS the name is not the symbol.
        let other = "CONFIG_NVME_TARGET_AUTH_EXTRA=y\n";
        assert_eq!(kconfig_verdict(other, "CONFIG_NVME_TARGET_AUTH"), None);
    }

    #[test]
    fn an_interface_alias_is_not_drift_and_does_not_tear_the_target_out() {
        let row = target("iscsi");
        // The storage VLAN commonly lives on a SECONDARY address of the
        // interface. Reporting that as drift would push the target into
        // `error`, and `apply` takes every non-active target out of the kernel.
        let aliased = node(&[("storage0", &["192.168.50.1", "10.10.0.5"])]);
        let (state, _, verdict) = target_state(&row, true, &|_| true, &aliased, true);
        assert_eq!((state, verdict), ("active", Disposition::Apply));
        // A genuinely different set is still drift, and names every address it
        // did find — but drift FREEZES, so even here nothing is torn out.
        let moved = node(&[("storage0", &["192.168.50.1"])]);
        let (state, detail, verdict) = target_state(&row, true, &|_| true, &moved, true);
        assert_eq!((state, verdict), ("error", Disposition::Freeze));
        assert!(detail.contains("192.168.50.1") && detail.contains("10.10.0.5"), "{detail}");
    }

    #[test]
    fn a_drifted_portal_is_reported_and_a_missing_volume_is_torn_out() {
        // The two halves of the owner's decision, side by side, because the
        // difference is the whole point: BOTH are `error`, and only one of
        // them may reach `remove_one`. Keying the removal loop on the state
        // string — the shape this replaced — would take a live export away
        // from a client mid-write because a DHCP lease moved an address.
        let row = target("iscsi");
        let drifted = target_state(&row, true, &|_| true, &node(&[("storage0", &["10.10.9.9"])]), true);
        let volume_gone = target_state(&row, false, &|_| true, &here(), true);
        // Against the CONSTANT, not against each other: comparing the two
        // computed values to one another passes just as happily if both become
        // "warning" one day, which is the change that would take the drift row
        // off the error list entirely.
        assert_eq!(drifted.0, "error");
        assert_eq!(volume_gone.0, "error");
        assert_eq!(drifted.2, Disposition::Freeze);
        assert_eq!(volume_gone.2, Disposition::Remove);
        // The sentence is the one an admin can act on: the portal that went
        // missing, where it was, and that nothing happened automatically.
        assert!(drifted.1.contains("10.10.0.5"), "{}", drifted.1);
        assert!(drifted.1.contains("storage0"), "{}", drifted.1);
        assert!(drifted.1.contains("re-picks the interface"), "{}", drifted.1);
    }

    #[test]
    fn the_periodic_evaluation_raises_and_resolves_the_drift_alert_without_touching_the_kernel() {
        // OWNER DECISION #1 is "report the drift and wait for an admin". Until
        // this ran on a tick, the ONLY thing that recomputed a target's state
        // was a mutation — so a DHCP lease renewal at 03:00 moved the address,
        // the portal stopped answering, and the row stayed green until someone
        // happened to save an unrelated target. An alert that is raised only
        // while an admin is already working is not a report.
        //
        // What this pins: `evaluate_rows` persists the state, raises the alert
        // on the way in and resolves it on the way out, and does all of it
        // from local reads — no helper, no channel, no `apply_one`.
        let conn = rusqlite::Connection::open_in_memory().expect("db");
        super::super::db::migrate(&conn).expect("migrate");
        let db: DbPool = std::sync::Arc::new(crate::db::Db::from_connection(conn));
        let mut row = target("iscsi");
        // A device node that really exists, so the verdict is decided by the
        // portal and not by the missing zvol of a test host.
        row.luns[0].device_path = "/dev/null".to_string();
        row.portals[0].interface = "tentanas-nie-ma-takiego0".to_string();
        store::upsert_target(&db, &row).expect("insert");

        let mut rows = vec![row.clone()];
        let mut log = Vec::new();
        let verdicts = evaluate_rows(&db, &mut rows, &|_| true, &RetryMemory::new(), &mut log).expect("evaluate");
        assert_eq!(
            verdicts.get(&row.target_id).copied(),
            Some(Disposition::Freeze)
        );
        // Persisted, so n12 shows it to whoever opens the tab next.
        let stored = store::target_by_name(&db, &row.name).expect("read").expect("row");
        assert_eq!(stored.state, "error");
        assert!(stored.state_detail.contains("10.10.0.5"), "{}", stored.state_detail);
        // And raised on n02/n15, keyed on the target, named by the target — a
        // bare UUID in the alert row is the one thing an admin cannot place.
        let open = store::list_alerts(&db, true).expect("alerts");
        assert_eq!(open.len(), 1, "{open:?}");
        assert_eq!(open[0].subject_kind, "target");
        assert_eq!(open[0].subject_id, row.name);
        assert!(open[0].detail.contains("tentanas-nie-ma-takiego0"), "{:?}", open[0]);

        // The address comes back (here: the portal stops naming an interface,
        // which is the deliberate 0.0.0.0 case) — the same tick closes it.
        let mut fixed = vec![TargetRow {
            portals: vec![NasTargetPortal {
                interface: String::new(),
                ..row.portals[0].clone()
            }],
            ..row.clone()
        }];
        evaluate_rows(&db, &mut fixed, &|_| true, &RetryMemory::new(), &mut log).expect("evaluate");
        assert!(store::list_alerts(&db, true).expect("alerts").is_empty());
        // `pending`, not `active`: the drift is gone and the row is appliable
        // again, but this host has no configfs object for it, and "active" is
        // a claim about what the node is SERVING. A green chip over an empty
        // kernel is how a target lost to a transient sat "active" forever.
        assert_eq!(
            store::target_by_name(&db, &row.name).expect("read").expect("row").state,
            "pending"
        );
    }

    #[test]
    fn the_removal_sweep_fires_for_a_dead_volume_and_never_for_a_drifted_portal() {
        // Two properties, and the second one is the reason this test is
        // written out loud rather than left to the type system.
        //
        // (1) A `Remove` verdict needs an executor on the TICK. Before this,
        // the only things that took a target out of the kernel were a mutation
        // of that same row and the one-shot restore — so at 03:00 a zvol's
        // device node disappears, the row goes red saying "the target stays
        // out of the kernel", and the target keeps exporting a disk that is
        // gone until somebody happens to edit it.
        //
        // (2) A drifted portal must NEVER be swept. `Freeze` is the owner's
        // decision that a live export is not torn down over a moved address,
        // and the sweep must not become the automatic teardown that decision
        // rules out. Today that holds because `Freeze` is a different variant
        // from `Remove` — which is easy to break later by widening a filter to
        // "not Apply", so the property is asserted, not assumed.
        let conn = rusqlite::Connection::open_in_memory().expect("db");
        super::super::db::migrate(&conn).expect("migrate");
        let db: DbPool = std::sync::Arc::new(crate::db::Db::from_connection(conn));

        // A target whose backing device node is gone.
        let mut dead = target("iscsi");
        dead.luns[0].device_path = "/tentanas/no/such/device/node".to_string();
        // A target whose portal address moved — healthy volume, frozen row.
        let mut drifted = TargetRow {
            name: "vm-store-2".to_string(),
            target_id: "0191f2c0-0000-7000-8000-000000000002".to_string(),
            wwn: wwn_for("iscsi", "helios", "vm-store-2"),
            ..target("iscsi")
        };
        drifted.luns[0].device_path = "/dev/null".to_string();
        drifted.portals[0].interface = "tentanas-nie-ma-takiego0".to_string();
        // A healthy one, to prove the gate is about the verdict and not about
        // "some row is unhappy".
        let mut healthy = TargetRow {
            name: "vm-store-3".to_string(),
            target_id: "0191f2c0-0000-7000-8000-000000000003".to_string(),
            wwn: wwn_for("iscsi", "helios", "vm-store-3"),
            ..target("iscsi")
        };
        healthy.luns[0].device_path = "/dev/null".to_string();
        healthy.portals[0].interface = String::new();
        healthy.portals[0].address = "0.0.0.0".to_string();

        let mut rows = vec![dead.clone(), drifted.clone(), healthy.clone()];
        let mut log = Vec::new();
        let verdicts = evaluate_rows(&db, &mut rows, &|_| true, &RetryMemory::new(), &mut log).expect("evaluate");
        assert_eq!(verdicts.get(&dead.target_id).copied(), Some(Disposition::Remove));
        assert_eq!(verdicts.get(&drifted.target_id).copied(), Some(Disposition::Freeze));
        assert_eq!(verdicts.get(&healthy.target_id).copied(), Some(Disposition::Apply));

        // Everything below calls the REAL selection, with `in_kernel` injected
        // the way `installed` is. The previous version of this test
        // reimplemented the filter, which meant widening `rows_to_remove` to
        // "anything that is not Apply" — the one change that would sweep a
        // frozen target — would have passed it green. A guard that agrees with
        // the code instead of exercising it is the failure mode this whole
        // slice has been fighting since round 2.
        let names = |rows: Vec<&TargetRow>| -> Vec<String> {
            rows.into_iter().map(|t| t.name.clone()).collect()
        };
        let nothing_in_kernel = |_: &TargetRow| false;
        let all_in_kernel = |_: &TargetRow| true;
        let never_retried = |_: &TargetRow| false;
        // This test's OWN clock, not the process-wide one. Three tests used to
        // share the global map under the same target id — winding it backwards
        // and then clearing it wholesale — so these assertions could go either
        // way depending on which thread got there first. Including the way
        // where they pass over broken code.
        let clock = GraceClock::new();
        let waited = |t: &TargetRow| removal_is_due_with(t, &clock);
        let fresh = |t: &TargetRow| {
            clock.note(&t.target_id, false);
            removal_is_due_with(t, &clock)
        };

        // Nothing is in the kernel: nothing to remove (a row that was never
        // applied must not be swept forever), and the appliable-but-absent row
        // is what the APPLY half is for. The drifted one is in neither.
        assert!(names(rows_to_remove(&rows, &verdicts, &nothing_in_kernel, &fresh)).is_empty());
        assert_eq!(
            names(rows_to_apply(&rows, &verdicts, &nothing_in_kernel, &never_retried)),
            vec![healthy.name.clone()]
        );

        // Everything is in the kernel, and the dead-volume row has only just
        // been seen missing: NOTHING is removed yet. Cutting a client off from
        // a raw disk on the strength of one `stat` is what the grace period
        // exists to stop.
        assert!(
            names(rows_to_remove(&rows, &verdicts, &all_in_kernel, &fresh)).is_empty(),
            "a volume missing for one tick is not yet a reason to remove"
        );

        // Once the absence has lasted, the dead one goes — and the frozen one
        // still does not. That is the owner's drift decision, asserted through
        // the real selection rather than assumed from the enum.
        clock.rewind_for_test(&dead.target_id, VOLUME_GONE_GRACE + Duration::from_secs(1));
        assert_eq!(
            names(rows_to_remove(&rows, &verdicts, &all_in_kernel, &waited)),
            vec![dead.name.clone()],
            "a drifted portal is never torn out of the kernel"
        );
        assert!(names(rows_to_apply(&rows, &verdicts, &all_in_kernel, &never_retried)).is_empty());

        // And the apply gate's second input, which the directory alone cannot
        // see: a row whose last apply FAILED is retried even though its
        // configfs object is there. Without it a plan that died after `mkdir`
        // and before `enable` was green forever.
        let retry_dead = |t: &TargetRow| t.target_id == healthy.target_id;
        assert_eq!(
            names(rows_to_apply(&rows, &verdicts, &all_in_kernel, &retry_dead)),
            vec![healthy.name.clone()],
            "a half-applied target is retried, not left green"
        );
    }

    #[test]
    fn a_target_that_arrived_disabled_keeps_the_reason_and_one_that_was_stopped_does_not() {
        // The config import switches a target off for reasons the admin has to
        // read — no secret in an export, a portal that was "every interface"
        // on somebody else's node — and writes them into `state_detail`. The
        // same import then reconciles, and the judgement used to blank the
        // field: the sentence existed for one instant and only in the job log.
        let mut imported = target("iscsi");
        imported.enabled = false;
        imported.state = "disabled".to_string();
        imported.state_detail =
            "the authentication secret has to be entered again after an import".to_string();
        let (state, detail, verdict) = target_state(&imported, true, &|_| true, &here(), false);
        assert_eq!((state, verdict), ("disabled", Disposition::Remove));
        assert_eq!(detail, imported.state_detail, "the reason it arrived with survives");

        // A target the ADMIN stopped keeps nothing: whatever its detail said,
        // it described a target that was running.
        let mut stopped = target("iscsi");
        stopped.enabled = false;
        stopped.state = "active".to_string();
        stopped.state_detail = "no authentication — the IQN/NQN allowlist is a filter".to_string();
        let (state, detail, _) = target_state(&stopped, true, &|_| true, &here(), true);
        assert_eq!(state, "disabled");
        assert!(detail.is_empty(), "a stopped target does not keep a running target's sentence");
    }

    #[test]
    fn a_reconcile_clock_counts_consecutive_failures_and_settles_when_there_is_nothing_to_do() {
        // The alert this drives says "three failures in a row". Two loose
        // variables kept making that untrue in both directions: a counter that
        // never reset turned failures weeks apart into "three in a row", and a
        // clock that only reset on a successful SWEEP stayed armed forever
        // when the admin deleted the row it kept failing on — because then
        // there is nothing to sweep and no success to record.
        let mut clock = RetryClock::new();
        let t0 = std::time::Instant::now();
        assert!(clock.due(t0) && clock.settled());

        clock.failed(t0);
        assert!(!clock.settled(), "a failure is outstanding until something clears it");
        assert!(!clock.due(t0), "and the next attempt waits");
        assert!(clock.due(t0 + RESTORE_RETRY_MIN));
        assert_eq!(clock.failures, 1);

        // The wait doubles, and the count is CONSECUTIVE.
        clock.failed(t0);
        assert_eq!(clock.failures, 2);
        assert_eq!(clock.wait, RESTORE_RETRY_MIN * 4);
        clock.succeeded();
        assert!(clock.settled() && clock.due(t0), "success resets everything");
        assert_eq!(clock.failures, 0);
        assert_eq!(clock.wait, RESTORE_RETRY_MIN);

        // The ceiling holds however long it keeps failing.
        for _ in 0..20 {
            clock.failed(t0);
        }
        assert_eq!(clock.wait, RESTORE_RETRY_MAX);
        assert!(clock.failures >= SWEEP_ALERT_AFTER, "and the alert threshold is reachable");
    }

    #[test]
    fn the_tick_judges_every_row_and_says_what_it_would_have_to_do_about_them() {
        // `evaluate` had no test at all: the tick's only unprivileged half,
        // the thing that decides whether the node reaches for the privilege
        // channel, and nothing called it. This calls the REAL function against
        // a real database, with the real `object_in_kernel` and
        // `kernel_can_serve` — on any host, because what it asserts is the
        // RELATIONSHIP between the rows and the two flags, not a host fact.
        let conn = rusqlite::Connection::open_in_memory().expect("db");
        super::super::db::migrate(&conn).expect("migrate");
        let db: DbPool = std::sync::Arc::new(crate::db::Db::from_connection(conn));

        // An empty node has nothing to judge and nothing to do — this is the
        // ordinary tick on a node with no block targets, and it must not make
        // the loop reach for root.
        let quiet = evaluate(&db).expect("evaluate");
        assert!(quiet.log.is_empty());
        assert!(!quiet.removals_pending && !quiet.applies_pending);

        // A healthy row on a host with no configfs object for it: appliable,
        // not applied. That is the case the apply sweep exists for, and it is
        // exactly the state a target is in a second after the wizard saved it.
        let mut row = target("iscsi");
        row.luns[0].device_path = "/dev/null".to_string();
        row.portals[0].interface = String::new();
        row.portals[0].address = "0.0.0.0".to_string();
        store::upsert_target(&db, &row).expect("insert");
        let seen = evaluate(&db).expect("evaluate");
        assert!(seen.applies_pending, "a row the node is not exporting needs applying");
        assert!(!seen.removals_pending, "and there is nothing to take out");
        // …and the judgement is persisted, which is the other half of what
        // this function is for.
        let stored = store::target_by_name(&db, &row.name).expect("read").expect("row");
        assert_eq!(stored.state, "pending");

        // A DISABLED row that the node is not exporting either: judged
        // `Remove`, but there is nothing in the kernel to remove, so the tick
        // still has no reason to reach for the privilege channel. A row that
        // was never applied must not be swept forever.
        let mut off = row.clone();
        off.enabled = false;
        store::upsert_target(&db, &off).expect("update");
        let seen = evaluate(&db).expect("evaluate");
        assert!(!seen.removals_pending, "nothing in the kernel is nothing to remove");
        assert!(!seen.applies_pending, "and a disabled row is not applied either");
    }

    #[tokio::test]
    async fn the_sweeps_do_nothing_at_all_when_there_is_nothing_to_do() {
        // The two executors had no test that ran them. This runs both, end to
        // end, against a database with no rows: they must take the lock,
        // re-judge, find nothing, touch no kernel and no privilege channel,
        // and come back empty. A sweep that produced work here would be a
        // sweep acting without a verdict.
        let conn = rusqlite::Connection::open_in_memory().expect("db");
        super::super::db::migrate(&conn).expect("migrate");
        let db: DbPool = std::sync::Arc::new(crate::db::Db::from_connection(conn));
        let cipher = SettingsCipher::new(&[7u8; 32]);

        assert!(sweep_removals(&db, None).await.expect("removals").is_empty());
        assert!(sweep_applies(&db, &cipher, None).await.expect("applies").is_empty());

        // A row judged `Remove` whose object is NOT in the kernel is still
        // nothing to do — and this is the assertion that would catch a sweep
        // dropping the `in_kernel` half of its selection, which would send
        // every disabled target through the privilege channel on every tick.
        let mut off = target("iscsi");
        off.enabled = false;
        off.luns[0].device_path = "/dev/null".to_string();
        store::upsert_target(&db, &off).expect("insert");
        assert!(sweep_removals(&db, None).await.expect("removals").is_empty());

        // And both release the lock they take: a second call must not hang.
        assert!(sweep_removals(&db, None).await.expect("removals").is_empty());
        assert!(sweep_applies(&db, &cipher, None).await.expect("applies").is_empty());

        // NAMED HOLE, so nobody reads this test as more than it is: every
        // assertion above is negative. Replace either sweep's body with
        // `Ok(vec![])` and this still passes, so NO test in this crate proves
        // that a sweep ever does anything. The positive path needs the
        // privileged channel and a real configfs, which is why the selection
        // was split out into `rows_to_remove`/`rows_to_apply` and tested
        // directly — that is the half a unit test can own. The other half is
        // covered only by the kernel measurements.
    }

    #[test]
    fn a_vanished_volume_waits_out_its_grace_period_before_a_client_loses_its_disk() {
        // The removal half acts by cutting a client off from a raw disk
        // mid-write, and the evidence behind it is ONE `Path::exists` at one
        // instant. A udev link that has not appeared yet, a pool re-importing,
        // a `zpool export` about to be undone — all three read exactly like
        // "gone forever" to that `stat`.
        //
        // Every assertion here is against THIS test's own clock and this
        // test's own target id. Sharing the process-wide map under the fixture
        // id made three tests fight over one countdown; the two watching a
        // client's disk could then pass or fail on thread order alone.
        let clock = GraceClock::new();
        let mut vanished = target("iscsi");
        vanished.target_id = "0191f2c0-0000-7000-8000-0000000000aa".to_string();
        vanished.luns[0].device_path = "/tentanas/no/such/device/node".to_string();

        // First sighting: reported immediately, acted on not at all.
        clock.note(&vanished.target_id, false);
        assert!(
            !removal_is_due_with(&vanished, &clock),
            "a volume missing for one tick is not a reason to cut a client off"
        );

        // The clock is CONTINUOUS, not a counter — and this is the assertion
        // that shows it. Wind the mark back to one second short of the grace
        // period, then judge the row twice more the way the sweeps do
        // (re-judging under the lock). If `note` overwrote the mark instead of
        // keeping the first sighting, those two calls would restart the
        // countdown and the row would never become due; if it kept it, the row
        // is due one second later. The old version made these calls at t=0,
        // where `insert` and `or_insert_with` are indistinguishable.
        clock.rewind_for_test(&vanished.target_id, VOLUME_GONE_GRACE - Duration::from_secs(1));
        clock.note(&vanished.target_id, false);
        clock.note(&vanished.target_id, false);
        clock.rewind_for_test(&vanished.target_id, VOLUME_GONE_GRACE + Duration::from_secs(1));
        assert!(
            removal_is_due_with(&vanished, &clock),
            "re-judging a row must not restart its countdown"
        );

        // A volume that came BACK clears the mark outright — proven by the
        // rewind panicking if it did not, and by the row not being due after
        // the next sighting starts a fresh countdown.
        clock.note(&vanished.target_id, true);
        assert_eq!(clock.waited(&vanished.target_id), None, "a returned volume clears the mark");
        clock.note(&vanished.target_id, false);
        assert!(!removal_is_due_with(&vanished, &clock), "and the countdown starts again");

        // The admin's own "stop" needs no waiting — that click IS the
        // confirmation, and the grace period is about evidence, not about
        // slowing the admin down.
        let mut stopped = vanished.clone();
        stopped.enabled = false;
        assert!(removal_is_due_with(&stopped, &clock));

        // And a row whose volume is present is due whatever else made it
        // `Remove` — with no mark on the clock at all.
        let mut present = target("iscsi");
        present.target_id = "0191f2c0-0000-7000-8000-0000000000bb".to_string();
        present.luns[0].device_path = "/dev/null".to_string();
        assert!(removal_is_due_with(&present, &clock));
        assert_eq!(clock.waited(&present.target_id), None);

        // A row that left the database takes its countdown with it, so a new
        // target reusing nothing of it starts clean — and, in the other
        // direction, a row that is STILL there keeps its mark. Without the
        // second assertion `retain(|_, _| false)` passes this test, which
        // would throw away every countdown on every tick and make the grace
        // period unreachable.
        clock.forget_missing(std::slice::from_ref(&vanished));
        assert!(
            clock.waited(&vanished.target_id).is_some(),
            "a row still in the database keeps its countdown"
        );
        clock.forget_missing(&[]);
        assert_eq!(clock.waited(&vanished.target_id), None);
    }

    #[tokio::test]
    async fn the_session_cache_answers_unavailable_rather_than_zero_on_a_node_it_cannot_ask() {
        // `nvmet_sessions` — the CACHE wrapper — had no test call site at all;
        // only `read_nvmet_sessions`'s parsing was covered. The wrapper is
        // where the "unknown is not zero" rule survives or dies: a confident 0
        // here becomes the delete dialog's blast radius, which is the one
        // number that costs a client its disk mid-write.
        //
        // On this host there is no privilege channel, so the read fails and
        // the answer must be UNAVAILABLE — not an empty list.
        let conn = rusqlite::Connection::open_in_memory().expect("db");
        super::super::db::migrate(&conn).expect("migrate");
        let db: DbPool = std::sync::Arc::new(crate::db::Db::from_connection(conn));

        let first = nvmet_sessions(&db).await;
        assert!(!first.available, "a node that cannot be asked does not report zero");
        assert!(!first.reason.is_empty(), "and it says why: {first:?}");

        // …and the second call is served from the cache, which is the whole
        // reason this wrapper exists: the detail window must not spend a
        // privileged round trip per repaint.
        let second = nvmet_sessions(&db).await;
        assert_eq!(second.available, first.available);
        assert_eq!(second.reason, first.reason);
    }

    #[test]
    fn the_privilege_channel_alert_stands_only_while_both_halves_are_true() {
        // The `elevation` alert is a CONJUNCTION — no channel AND work
        // waiting — and either half clearing must clear it. The first version
        // resolved only when the channel came back, so an admin who instead
        // deleted the row the alert was about kept a red alert about work that
        // no longer exists, with no action available that could clear it.
        let nothing = Evaluation {
            log: Vec::new(),
            removals_pending: false,
            applies_pending: false,
        };
        let applies = Evaluation {
            applies_pending: true,
            ..Evaluation {
                log: Vec::new(),
                removals_pending: false,
                applies_pending: false,
            }
        };
        let removals = Evaluation {
            removals_pending: true,
            ..Evaluation {
                log: Vec::new(),
                removals_pending: false,
                applies_pending: false,
            }
        };

        // Raised only with the channel down AND something to do.
        let raised = channel_alert(false, &applies).expect("an unarmed node with work is a fault");
        assert_eq!(raised.0, "warning");
        assert_eq!(raised.1, "channel");
        assert!(raised.2.contains("not armed"), "{}", raised.2);
        // The detail has to say what an admin can DO about it — an alert whose
        // text names no action is one they learn to ignore.
        assert!(raised.3.contains("Environment"), "{}", raised.3);
        assert!(channel_alert(false, &removals).is_some(), "a pending removal counts too");

        // Both ways out.
        assert!(
            channel_alert(true, &applies).is_none(),
            "arming the channel clears it"
        );
        assert!(
            channel_alert(false, &nothing).is_none(),
            "and so does the work going away — the half that had no exit"
        );
        // A node with no block targets at all is not faulty for having no
        // channel, which is why this is not simply `!armed`.
        assert!(channel_alert(true, &nothing).is_none());
    }

    #[test]
    fn a_failed_apply_is_remembered_until_one_succeeds_or_the_row_goes_away() {
        // The retry memory is the WHOLE fix for "a partially applied target
        // shows green and is never tried again", and it had NO test: every
        // existing one injected its own `retry` closure into `rows_to_apply`,
        // exercising the seam and never the thing production puts in the seam.
        // The set could have been emptied, never filled, or never swept, and
        // the whole core suite stayed green.
        //
        // The real bodies — `RetryMemory::note` / `pending` / `forget_missing`,
        // which the two free functions are one-line delegations to — driven
        // against an OWNED instance, the way `GraceClock` is. Then the free
        // functions themselves, against the global, which is now safe because
        // `evaluate_rows` takes its memory as a parameter and no other test
        // writes to the process-wide one.
        let mut row = target("iscsi");
        row.target_id = "0191f2c0-0000-7000-8000-00000000fa01".to_string();
        row.luns[0].device_path = "/dev/null".to_string();
        let memory = RetryMemory::new();

        assert!(!memory.pending(&row), "a row nobody applied yet has nothing to retry");

        memory.note(&row.target_id, false);
        assert!(memory.pending(&row), "a failed apply is remembered");

        // And the memory is what puts the row back into the apply selection
        // even though the object IS in the kernel — the exact case the round-7
        // major described: the subsystem exists, so `!in_kernel` is false, and
        // without the retry the half-applied row would never be touched again.
        let targets = vec![row.clone()];
        let mut disposition = BTreeMap::new();
        disposition.insert(row.target_id.clone(), Disposition::Apply);
        let retry = |t: &TargetRow| memory.pending(t);
        let picked = rows_to_apply(&targets, &disposition, &|_| true, &retry);
        assert_eq!(picked.len(), 1, "the remembered failure re-selects a row already in the kernel");

        memory.note(&row.target_id, true);
        assert!(!memory.pending(&row), "a successful apply forgets it");
        assert!(
            rows_to_apply(&targets, &disposition, &|_| true, &retry).is_empty(),
            "and the row leaves the selection again"
        );

        // A row that left the database takes its remembered failure with it,
        // so the set cannot grow for the life of the process and a target
        // created later with a different id inherits nothing. Both directions:
        // `retain(|_| false)` passes the second assertion alone.
        memory.note(&row.target_id, false);
        memory.forget_missing(&targets);
        assert!(memory.pending(&row), "a row still in the database keeps its failure");
        memory.forget_missing(&[]);
        assert!(!memory.pending(&row), "a row that is gone does not");

        // …and the two PRODUCTION entry points, against the global set. This
        // was a named hole last round on the grounds that `evaluate_rows`
        // sweeps the global on every judgement, so another test could empty it
        // mid-assertion. That was true, and the fix was one parameter:
        // `evaluate_rows` now takes the memory the way it already took
        // `installed`, every test passes its own, and the global belongs to
        // this test alone.
        //
        // A unique id, so even a future test that does touch the global cannot
        // collide.
        assert!(!apply_retry_pending(&row), "the global set starts clean for this id");
        note_apply_outcome(&row.target_id, false);
        assert!(apply_retry_pending(&row), "the two free functions share one set");
        note_apply_outcome(&row.target_id, true);
        assert!(!apply_retry_pending(&row), "and a success clears it there too");

        // What remains uncovered, said plainly: `apply` and both sweeps call
        // `note_apply_outcome` on a real apply outcome, and reaching that
        // needs the privileged channel and a real configfs. The selection they
        // feed it into is tested above; the invocation is not.
    }

    #[test]
    fn a_save_on_a_frozen_target_reports_that_the_kernel_never_heard_it() {
        // The other half of the drift decision, and the half that had no test
        // at all until now: a frozen target is deliberately NOT applied, so
        // the most likely reaction to a drift alert — taking an initiator off
        // the allowlist of the target the alert is about — got a green
        // "saved" for a revocation that never reached the kernel.
        //
        // For two rounds this could not even be reached, because the update
        // handler quietly re-plumbed the portal onto the interface's current
        // address and lifted the very freeze this sentence reports. Now that
        // the portal only moves when somebody asks, it is reachable, and the
        // job ends with the reason instead of with a lie.
        let conn = rusqlite::Connection::open_in_memory().expect("db");
        super::super::db::migrate(&conn).expect("migrate");
        let db: DbPool = std::sync::Arc::new(crate::db::Db::from_connection(conn));
        let mut row = target("iscsi");
        row.luns[0].device_path = "/dev/null".to_string();
        row.portals[0].interface = "tentanas-nie-ma-takiego0".to_string();
        store::upsert_target(&db, &row).expect("insert");

        let reason = unapplied_reason(&db, &row.name)
            .expect("read")
            .expect("a frozen target has a reason");
        assert!(reason.contains("10.10.0.5"), "{reason}");
        assert!(reason.contains("re-picks the interface"), "{reason}");

        // A target with no drift is not automatically "applied" either. On
        // this host nothing is in configfs, so a row judged appliable is a row
        // the node is NOT yet exporting — and the job that claimed to have
        // created it must not report success for a target that does not exist.
        // That is the second of the three gaps: a zvol whose udev link has not
        // appeared yet produces exactly this shape.
        let healthy = TargetRow {
            portals: vec![NasTargetPortal {
                interface: String::new(),
                ..row.portals[0].clone()
            }],
            name: "vm-store-2".to_string(),
            target_id: "0191f2c0-0000-7000-8000-000000000002".to_string(),
            wwn: wwn_for("iscsi", "helios", "vm-store-2"),
            ..row.clone()
        };
        store::upsert_target(&db, &healthy).expect("insert");
        let pending = unapplied_reason(&db, &healthy.name)
            .expect("read")
            .expect("a row the node is not exporting is not a finished save");
        assert!(pending.contains("not exporting it yet"), "{pending}");
        // A name that is not a row at all is not an error either: the job
        // carries the subject it was spawned with, and a delete may have
        // overtaken it.
        assert_eq!(unapplied_reason(&db, "gone").expect("read"), None);
    }

    #[test]
    fn an_open_alert_follows_the_address_instead_of_freezing_on_the_first_reading() {
        // The detail is the whole value of a drift alert. With `INSERT OR
        // IGNORE` the first sentence stuck: the address moved on from `lan0`
        // to `mgmt0` and the admin kept reading about `lan0`.
        let conn = rusqlite::Connection::open_in_memory().expect("db");
        super::super::db::migrate(&conn).expect("migrate");
        let db: DbPool = std::sync::Arc::new(crate::db::Db::from_connection(conn));
        let key = drift_alert_key("t1");
        assert!(store::raise_alert(&db, &key, "warning", "target", "vm-store", "Target vm-store", "moved to lan0").expect("raise"));
        let first = store::list_alerts(&db, true).expect("alerts");
        assert_eq!(first.len(), 1);
        // A second raise is not a new event…
        assert!(!store::raise_alert(&db, &key, "warning", "target", "vm-store", "Target vm-store", "moved to mgmt0").expect("raise"));
        let open = store::list_alerts(&db, true).expect("alerts");
        assert_eq!(open.len(), 1);
        // …but it says what is true now, and it neither restarts the clock nor
        // becomes a different alert: the condition began when it began, and an
        // admin reading "for 4 hours" must keep reading that.
        assert_eq!(open[0].detail, "moved to mgmt0");
        assert_eq!(open[0].alert_id, first[0].alert_id);
        assert_eq!(open[0].raised_at, first[0].raised_at);
    }

    #[test]
    fn deleting_a_target_closes_the_drift_alert_it_left_behind() {
        // `apply` closes the alerts of the rows it iterates, and a deleted row
        // is not one of them — so without the delete path calling this, an
        // admin would keep a warning on n02/n15 pointing at a target that no
        // longer exists, with a drill-down to a row nobody can open.
        let conn = rusqlite::Connection::open_in_memory().expect("db");
        super::super::db::migrate(&conn).expect("migrate");
        let db: DbPool = std::sync::Arc::new(crate::db::Db::from_connection(conn));
        let id = "0191f2c0-0000-7000-8000-000000000001";
        let other = "0191f2c0-0000-7000-8000-000000000002";
        for target in [id, other] {
            store::raise_alert(
                &db,
                &drift_alert_key(target),
                "warning",
                "target",
                target,
                "Target vm-store: the portal address moved",
                "portal 10.10.0.5 no longer exists on storage0",
            )
            .expect("raise");
        }
        forget_alerts(&db, id).expect("forget");
        let mine = store::alerts_for_subject(&db, "target", id).expect("alerts");
        assert_eq!(mine.len(), 1, "the row stays, it is only resolved");
        assert!(mine[0].resolved_at.is_some());
        // Another target's alert is untouched: the key is per target.
        let theirs = store::alerts_for_subject(&db, "target", other).expect("alerts");
        assert!(theirs[0].resolved_at.is_none());
    }

    #[test]
    fn an_unmeasurable_nvme_target_reports_unknown_and_never_a_confident_zero() {
        // OWNER DECISION (2026-09-04): read the controllers from debugfs where
        // the node publishes them, and say "cannot know" where it does not.
        // The pair matters because `0` and "unknown" drive different UI: the
        // delete dialog states its blast radius from this number.
        let nvme = target("nvmet");
        let unknown = tentanas_helper::block::NvmetSessions::unavailable("no debugfs here");
        let (list, known) = sessions_from(&nvme, &unknown);
        assert!(list.is_empty() && !known);

        let mut seen = tentanas_helper::block::NvmetSessions {
            available: true,
            ..Default::default()
        };
        // A subsystem the kernel knows about with nobody attached is a MEASURED
        // zero — known, empty.
        seen.controllers.insert(nvme.wwn.clone(), Vec::new());
        let (list, known) = sessions_from(&nvme, &seen);
        assert!(list.is_empty() && known);

        seen.controllers.insert(
            nvme.wwn.clone(),
            vec![
                tentanas_helper::block::NvmetController {
                    cntlid: "1".into(),
                    hostnqn: "nqn.2014-08.org.nvmexpress:uuid:esx01".into(),
                    host_traddr: "192.168.10.24".into(),
                    port: "1".into(),
                    // Measured on a live association; carried verbatim and
                    // never branched on.
                    state: "ready".into(),
                },
                // A controller the kernel has not named yet is still a live
                // association, so it is listed by its id rather than dropped.
                tentanas_helper::block::NvmetController {
                    cntlid: "2".into(),
                    ..Default::default()
                },
            ],
        );
        let (list, known) = sessions_from(&nvme, &seen);
        assert!(known);
        // The same split as every other protocol here: `client` is WHERE the
        // session came from (nvmet's measured `host_traddr`), `user` is the
        // identity it declared. They are not interchangeable — the NQN is the
        // client-declared half.
        assert_eq!(
            list.iter().map(|s| s.client.as_str()).collect::<Vec<_>>(),
            vec!["192.168.10.24", "controller 2"]
        );
        assert_eq!(
            list.iter().map(|s| s.user.as_str()).collect::<Vec<_>>(),
            vec!["nqn.2014-08.org.nvmexpress:uuid:esx01", "controller 2"]
        );
        // iSCSI never depends on any of this: LIO publishes its sessions in
        // configfs, which any user can read — so a node that cannot read
        // debugfs still KNOWS an iSCSI target's session count. (Whether it
        // knows it in a given moment is `sessions`' own answer: an ACL whose
        // `info` will not read makes the count unknown there too, the same
        // three-state the prune uses.)
        assert!(sessions_from(&target("iscsi"), &unknown).1);
    }

    #[test]
    fn an_orphan_is_only_an_object_carrying_this_apps_own_authority() {
        // A delete whose job failed leaves the kernel object with no row.
        // `apply` then removes it — so the property that keeps this safe is
        // that ONLY names under this app's own WWN authority are candidates.
        // A target somebody made by hand, or another tool's, is never touched.
        //
        // This calls `orphans_in`, the real walk, against directories the test
        // builds. The previous version restated the filter over a list of
        // strings, which meant deleting the authority check from the function
        // would have left it green — and `orphans()` had no caller in any test
        // at all.
        // Cleaned up by `Drop` and tagged with the thread, so a panic in the
        // middle leaves nothing behind and two tests of this shape cannot
        // collide — the previous version keyed only on the pid and swept up
        // only if every assertion passed.
        let tree = TempTree::new("orphans");
        let iscsi = tree.dir("target/iscsi");
        let nvmet = tree.dir("nvmet/subsystems");

        let rows = vec![target("iscsi")];
        let mine = rows[0].wwn.clone();
        // Three iSCSI names in the kernel: ours WITH a row, ours WITHOUT one,
        // and one that is not ours at all.
        for name in [
            mine.as_str(),
            "iqn.2026-09.local.tentaflow:helios.zapomniany",
            "iqn.1998-01.com.vmware:recznie",
        ] {
            std::fs::create_dir_all(iscsi.join(name)).expect("object");
        }
        // …and the nvmet side, which uses the other prefix.
        for name in [
            "nqn.2026-09.local.tentaflow:helios.stary",
            "nqn.2014-08.org.nvmexpress:uuid:obcy",
        ] {
            std::fs::create_dir_all(nvmet.join(name)).expect("object");
        }

        let found = orphans_in(
            &iscsi.display().to_string(),
            &nvmet.display().to_string(),
            &rows,
        );
        assert_eq!(
            found,
            vec![
                ("iscsi".to_string(), "iqn.2026-09.local.tentaflow:helios.zapomniany".to_string()),
                ("nvmet".to_string(), "nqn.2026-09.local.tentaflow:helios.stary".to_string()),
            ],
            "only this app's own names, and only the ones with no row"
        );
        // The counter-examples, said out loud: the row's own object is not an
        // orphan, and a foreign name is not one however long it sits there.
        assert!(!found.iter().any(|(_, n)| *n == mine));
        assert!(!found.iter().any(|(_, n)| n.contains("vmware")));
        assert!(!found.iter().any(|(_, n)| n.contains("nvmexpress")));

        // A directory that does not exist is not an error — a node that never
        // served NVMe-oF has no `subsystems/` at all.
        let nowhere = iscsi.join("nie-ma-takiego-katalogu").display().to_string();
        let none = orphans_in(&nowhere, &nowhere, &rows);
        assert!(none.is_empty());

        // And the WRAPPER, which had no caller in any test: it composes the
        // two configfs paths, and a typo in either is invisible everywhere
        // else. Asserted as strings, then run — on a host with no configfs it
        // finds nothing, which is the only outcome this machine can produce
        // and is still an execution of the real function.
        let (iscsi_dir, nvmet_dir) = orphan_dirs();
        assert_eq!(iscsi_dir, "/sys/kernel/config/target/iscsi");
        assert_eq!(nvmet_dir, "/sys/kernel/config/nvmet/subsystems");
        assert!(orphans(&rows).iter().all(|(_, n)| n != &mine));
    }

    #[test]
    fn a_static_acl_session_counts_even_though_lio_leaves_it_out_of_dynamic_sessions() {
        // `dynamic_sessions` lists only GENERATED ACLs, and an allowlisted
        // target has none — so the safer targets would have reported zero
        // sessions while clients were writing, and the delete dialog would have
        // understated its own blast radius.
        assert!(acl_info_connected(
            "InitiatorName: iqn.1998-01.com.vmware:esx01\nInitiatorAlias: esx01\nLIO Session ID: 3"
        ));
        assert!(!acl_info_connected(
            "No active iSCSI Session for Initiator Endpoint: iqn.1998-01.com.vmware:esx01"
        ));
        assert!(!acl_info_connected(""));
        assert!(!acl_info_connected("   \n"));
    }

    #[test]
    fn an_ipv6_only_interface_is_listed_as_unusable_rather_than_dropped() {
        // IPv6 is a self-limitation of this slice, not a kernel one, so an
        // IPv6-only interface is SHOWN with that reason instead of leaving the
        // picker empty and unexplained.
        //
        // The consequences are asserted, not the predicate. Restating
        // `is_ipv4()` here agreed with every implementation that was any
        // function of the address family — including one that dropped the row
        // entirely — and on a host with only `lo` the loop ran zero times.
        let listed = vec![
            NasBlockInterface {
                name: "lan0".into(),
                address: "fe80::1".into(),
                supported: false,
                ..Default::default()
            },
            NasBlockInterface {
                name: "storage0".into(),
                address: "10.10.0.5".into(),
                supported: true,
                ..Default::default()
            },
        ];
        // Unsupported means "no address a portal can bind", which is what the
        // whole address rule is built on…
        assert!(bindable_addresses(&listed, "lan0").is_empty());
        assert_eq!(primary_address(&listed, "lan0"), None);
        // …and it means it in the handler too: an explicit re-pick onto such
        // an interface is an ERROR, never an empty address that would become a
        // portal on nothing.
        let stored = vec![portal_for("iscsi", "storage0", "10.10.0.5", "tcp")];
        let asked = vec![portal_for("iscsi", "lan0", "", "tcp")];
        let refused = portals_for_update("iscsi", &stored, &asked, true, &listed)
            .expect_err("an interface with no bindable address is refused");
        assert!(refused.to_string().contains("a portal can bind"), "{refused}");
        // But it is NOT dropped from the list the picker renders — that is the
        // difference between "you cannot pick this and here is why" and "your
        // node has no interfaces".
        assert_eq!(listed.iter().filter(|i| !i.supported).count(), 1);

        // On this host, whatever it is: the flag is about the address family
        // and nothing else, and no interface is silently missing an answer.
        for iface in interfaces() {
            assert!(!iface.address.is_empty(), "{iface:?}");
            if iface.supported {
                assert!(
                    iface.address.parse::<std::net::Ipv4Addr>().is_ok(),
                    "a bindable address must be one LIO and nvmet can take: {iface:?}"
                );
            }
        }
    }
}
