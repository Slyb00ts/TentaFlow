// =============================================================================
// File: tentanas/ksmbd.rs — the SECOND SMB backend: SMB Direct on RDMA
//       interfaces only (plan-02 §5.4b, owner decision T9).
//
//       Samba stays the SMB server for everything a share can do — access
//       audit, Time Machine, "Previous versions", ZFS ACL mapping. It cannot
//       do SMB Direct: on Linux only `ksmbd` implements SMB3 over RDMA. So a
//       node with an RDMA card runs BOTH, split by interface: ksmbd binds the
//       storage interfaces (TCP 445 for the negotiation plus SMB Direct 5445),
//       Samba binds everything else. One `nas_shares` row feeds both configs;
//       only a share with the "SMB Direct (RDMA)" option gets a ksmbd section.
//
//       EXPOSURE GUARD. ksmbd is EXPERIMENTAL in the kernel's own
//       documentation and has a history of memory-safety bugs, so it may only
//       ever listen on a dedicated RDMA storage network. An interface that
//       also carries the node's default route is not that, and this module
//       refuses it — visibly, in the Environment row, in the wizard and in the
//       apply log — instead of starting a kernel SMB server on the way out.
//
//       Everything here is pure text over sysfs/procfs reads plus generation
//       over rows, so the fixtures below exercise the whole config surface on
//       a host that has neither ksmbd nor an RDMA card.
// =============================================================================

use tentaflow_protocol::tentanas::NasFeature;

use super::db::ShareRow;

/// The Environment row's feature id (`FEATURES` in environment.rs) and the id
/// the package install uses.
pub const FEATURE_ID: &str = "ksmbd";

/// The in-kernel SMB server. `ksmbd.mountd` does not load it — the unit
/// ksmbd-tools ships pulls `modprobe@ksmbd` in first — so the helper loads it
/// and this probe reports whether the kernel has it at all.
pub const MODULE: &str = "ksmbd";

/// The userspace tools the backend needs. `ksmbd.mountd` runs the daemon,
/// `ksmbd.control` reloads and stops it, `ksmbd.adduser` owns the second
/// password database.
pub const TOOLS: &[&str] = &["ksmbd.mountd", "ksmbd.control", "ksmbd.adduser"];

/// One RDMA interface as the listener would use it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Interface {
    pub netdev: String,
    pub addresses: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Probe {
    /// RDMA interfaces ksmbd may bind: an ACTIVE port, an address, and no
    /// default route of the node.
    pub interfaces: Vec<Interface>,
    /// RDMA interfaces that would be usable but carry the default gateway.
    /// Kept apart so the refusal can name them instead of reading as "no card".
    pub exposed: Vec<Interface>,
    /// Tools of `TOOLS` this node does not have.
    pub missing_tools: Vec<String>,
    pub module_loaded: bool,
    pub module_available: bool,
    /// The kernel release, shown in the n16 row next to the EXPERIMENTAL note.
    pub kernel: String,
}

impl Probe {
    /// Whether this node may serve SMB Direct right now. The ONE place the
    /// "is it offerable" question is answered, so the wizard, the config
    /// writer and the apply cannot drift apart — the same contract
    /// `rdma::available` has for the NFS transport.
    pub fn ready(&self) -> bool {
        !self.interfaces.is_empty()
            && self.missing_tools.is_empty()
            && (self.module_loaded || self.module_available)
    }

    /// The netdev names the listener binds.
    pub fn netdevs(&self) -> Vec<String> {
        self.interfaces.iter().map(|i| i.netdev.clone()).collect()
    }
}

/// Interfaces carrying a default route of this node, from the kernel's own
/// tables. Both families are read: a storage network may be IPv6-only, and an
/// interface that routes the world is not a storage network in either.
fn default_route_devices() -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(text) = std::fs::read_to_string("/proc/net/route") {
        out.extend(parse_ipv4_default_routes(&text));
    }
    if let Ok(text) = std::fs::read_to_string("/proc/net/ipv6_route") {
        out.extend(parse_ipv6_default_routes(&text));
    }
    out.sort();
    out.dedup();
    out
}

/// `/proc/net/route`: `Iface Destination Gateway Flags RefCnt Use Metric Mask …`,
/// every number little-endian hex. A default route is destination 0.0.0.0 with
/// mask 0.0.0.0.
fn parse_ipv4_default_routes(text: &str) -> Vec<String> {
    text.lines()
        .skip(1)
        .filter_map(|line| {
            let mut f = line.split_whitespace();
            let iface = f.next()?;
            let destination = f.next()?;
            let mask = f.nth(5)?;
            (u32::from_str_radix(destination, 16) == Ok(0) && u32::from_str_radix(mask, 16) == Ok(0))
                .then(|| iface.to_string())
        })
        .collect()
}

/// `/proc/net/ipv6_route`: `dest dest_plen src src_plen next_hop metric refcnt
/// use flags dev`. A default route is `::/0`.
fn parse_ipv6_default_routes(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|line| {
            let f: Vec<&str> = line.split_whitespace().collect();
            let (destination, plen, dev) = (f.first()?, f.get(1)?, f.get(9)?);
            (destination.bytes().all(|b| b == b'0') && u32::from_str_radix(plen, 16) == Ok(0))
                .then(|| dev.to_string())
        })
        .collect()
}

/// Reads what this node can serve SMB Direct on.
pub fn probe() -> Probe {
    let rdma = super::rdma::probe();
    let gateways = default_route_devices();
    let mut interfaces = Vec::new();
    let mut exposed = Vec::new();
    for device in rdma.devices.iter().filter(|d| d.active) {
        // A card with no netdev (a pure IB HCA without IPoIB) or no address
        // cannot carry the TCP 445 that negotiates SMB Direct in the first
        // place, so it is not a listener candidate at all.
        if device.netdev.is_empty() || device.addresses.is_empty() {
            continue;
        }
        let iface = Interface {
            netdev: device.netdev.clone(),
            addresses: device.addresses.clone(),
        };
        if gateways.contains(&device.netdev) {
            exposed.push(iface);
        } else {
            interfaces.push(iface);
        }
    }
    Probe {
        interfaces,
        exposed,
        missing_tools: TOOLS
            .iter()
            .filter(|t| super::environment::find_binary(t).is_none())
            .map(|t| (*t).to_string())
            .collect(),
        module_loaded: super::rdma::module_loaded(MODULE),
        module_available: super::rdma::module_in_tree(MODULE),
        kernel: super::rdma::kernel_release(),
    }
}

fn describe_interfaces(list: &[Interface]) -> String {
    list.iter()
        .map(|i| match i.addresses.first() {
            Some(ip) => format!("{} {ip}", i.netdev),
            None => i.netdev.clone(),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Turns a probe into the Environment row (n16). Split from `probe` so the
/// wording is testable against a fixture instead of against this host.
///
/// The EXPERIMENTAL note is not decoration: it is how the kernel's own
/// documentation describes ksmbd, and an admin turning on a second SMB server
/// deserves to read it in the same row that says the feature is available.
pub fn describe(probe: &Probe) -> (&'static str, String) {
    let module = if probe.module_loaded {
        format!("{MODULE} loaded")
    } else if probe.module_available {
        format!("{MODULE} available, loaded when the listener opens")
    } else {
        format!("{MODULE} is not in this kernel's module tree")
    };
    let experimental = "EXPERIMENTAL (kernel docs)";
    if probe.interfaces.is_empty() && probe.exposed.is_empty() {
        return (
            "no_device",
            format!("no RDMA interface with an address · {experimental} · {module}"),
        );
    }
    if probe.interfaces.is_empty() {
        return (
            "exposed",
            format!(
                "{} also carries the default gateway — SMB Direct needs a dedicated storage network · {experimental}",
                describe_interfaces(&probe.exposed)
            ),
        );
    }
    let listener = describe_interfaces(&probe.interfaces);
    if !probe.missing_tools.is_empty() {
        return (
            "missing",
            format!(
                "missing: {} · listener would be {listener} · {experimental}",
                probe.missing_tools.join(", ")
            ),
        );
    }
    if !(probe.module_loaded || probe.module_available) {
        return ("missing_module", format!("{module} · {experimental}"));
    }
    (
        "ok",
        format!("{listener} · {experimental} · {module}"),
    )
}

/// Replaces the generic feature probe's answer for the ksmbd row, the way the
/// RDMA row is refined: "is this binary there" is only one of four questions
/// here, and a node with the tools installed but the wrong network must read
/// as unavailable rather than as an installed feature.
pub fn refine(feature: &mut NasFeature) {
    let probe = probe();
    let (status, detail) = describe(&probe);
    feature.status = status.to_string();
    feature.detail = detail;
    // The kernel release IS the version of this backend: ksmbd is in-tree, so
    // ksmbd-tools' own version says nothing about the server that serves.
    feature.version = Some(probe.kernel).filter(|k| !k.is_empty());
    feature.kernel_module = Some(MODULE.to_string());
}

/// Whether the ksmbd row of an environment says this node may serve SMB
/// Direct — what the wizard offers the option on and what `validate_options`
/// re-checks before a share can store it.
pub fn available(features: &[NasFeature]) -> bool {
    features
        .iter()
        .any(|f| f.id == FEATURE_ID && f.status == "ok")
}

/// Whether this node has a ksmbd password database to keep in step with
/// Samba's at all.
///
/// Deliberately NOT `available()`: an account mirror must survive a link that
/// went down or a routing change. Those close the listener; they do not make
/// the second database stop existing, and an account missing from it would
/// only surface much later, as a share the user cannot open over RDMA.
pub fn has_user_database() -> bool {
    super::environment::find_binary("ksmbd.adduser").is_some()
}

/// The refusal the apply reports and pins on every share that asked for SMB
/// Direct, when the node cannot serve it. Never silent: §5.4b says a warning,
/// never a quiet start and never a quiet skip.
pub fn refusal(probe: &Probe) -> String {
    let (_, detail) = describe(probe);
    format!("SMB Direct is not served on this node: {detail}")
}

// =============================================================================
// ksmbd.conf generation
// =============================================================================

const HEADER: &str = "# Generated by TentaNas. The WHOLE file belongs to the app: ksmbd runs on\n\
                      # this node only to serve SMB Direct on its RDMA interfaces, and every\n\
                      # section below is rebuilt from the app's database on every share change.\n";

fn line(out: &mut String, key: &str, value: &str) {
    out.push('\t');
    out.push_str(key);
    out.push_str(" = ");
    out.push_str(value);
    out.push('\n');
}

/// Whether a share asked for SMB Direct.
pub fn smb_direct(share: &ShareRow) -> bool {
    share.protocol == "smb" && share.smb.as_ref().is_some_and(|s| s.smb_direct)
}

/// Whether any exportable share asked for it. One is enough: the listener
/// belongs to the node, not to a share.
pub fn wants_smb_direct(shares: &[ShareRow]) -> bool {
    shares.iter().any(smb_direct)
}

/// The `[global]` section: the listener split plus the two facts SMB Direct
/// actually depends on.
fn global_section(interfaces: &[String], guests: bool) -> String {
    let mut out = String::from("[global]\n");
    line(&mut out, "interfaces", &interfaces.join(" "));
    line(&mut out, "bind interfaces only", "yes");
    line(&mut out, "tcp port", &tentanas_helper::KSMBD_TCP_PORT.to_string());
    // A Windows client only asks for an RDMA channel after the server has told
    // it there are several channels; without multichannel the SMB Direct
    // listener is open and nobody ever uses it.
    line(&mut out, "server multi channel support", "yes");
    // SMB Direct exists from SMB3 on, and this listener serves nothing else.
    line(&mut out, "server min protocol", "SMB3_00");
    line(&mut out, "server string", "TentaNas SMB Direct");
    if guests {
        // ksmbd defaults to `map to guest = never`, so a share with guests
        // enabled would silently refuse them — the one difference between the
        // backends that would be invisible to the admin.
        line(&mut out, "map to guest", "bad user");
    }
    out
}

/// The ksmbd section of one share. Deliberately the SAME access model as the
/// Samba section — path, grants, masks, share group — and nothing else:
/// shadow_copy2, recycle, fruit and the audit module do not exist in ksmbd,
/// which is what the "SMB Direct: bez audytu" chip tells the admin.
pub fn ksmbd_section(share: &ShareRow) -> String {
    let smb = share.smb.clone().unwrap_or_default();
    let mut out = format!("[{}]\n", share.name);
    line(&mut out, "path", &share.source_path);
    line(&mut out, "browseable", "yes");
    line(&mut out, "read only", "no");
    line(&mut out, "guest ok", if smb.guests { "yes" } else { "no" });
    if smb.guests {
        line(&mut out, "force group", tentanas_helper::SHARE_GROUP);
    }
    let names: Vec<&str> = smb.users.iter().map(|u| u.user.as_str()).collect();
    if !names.is_empty() {
        line(&mut out, "valid users", &names.join(", "));
        let write: Vec<&str> = smb
            .users
            .iter()
            .filter(|u| u.mode == "rw")
            .map(|u| u.user.as_str())
            .collect();
        let read: Vec<&str> = smb
            .users
            .iter()
            .filter(|u| u.mode != "rw")
            .map(|u| u.user.as_str())
            .collect();
        if !write.is_empty() {
            line(&mut out, "write list", &write.join(", "));
        }
        if !read.is_empty() {
            line(&mut out, "read list", &read.join(", "));
        }
    }
    line(&mut out, "create mask", "0660");
    line(&mut out, "directory mask", "2770");
    out
}

/// The whole app-owned `/etc/ksmbd/ksmbd.conf` for the shares that asked for
/// SMB Direct, bound to `interfaces`.
pub fn ksmbd_document(shares: &[ShareRow], interfaces: &[String]) -> String {
    let direct: Vec<&ShareRow> = shares.iter().filter(|s| smb_direct(s)).collect();
    let guests = direct
        .iter()
        .any(|s| s.smb.as_ref().is_some_and(|o| o.guests));
    let mut out = HEADER.to_string();
    out.push('\n');
    out.push_str(&global_section(interfaces, guests));
    for share in direct {
        out.push('\n');
        out.push_str(&ksmbd_section(share));
    }
    out
}

// =============================================================================
// the Samba side of the split
// =============================================================================

/// Every network interface of this node, from sysfs. Samba's `interfaces` has
/// no exclusion syntax, so the only way to keep smbd off the RDMA interfaces
/// is to list everything else.
fn all_netdevs() -> Vec<String> {
    let Ok(entries) = std::fs::read_dir("/sys/class/net") else {
        return Vec::new();
    };
    let mut out: Vec<String> = entries
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    out.sort();
    out
}

/// The `interfaces` list the app-owned Samba include pins when ksmbd takes the
/// RDMA ones. `lo` is always in it: with `bind interfaces only = yes` and no
/// loopback, `smbpasswd`, `smbstatus` and every local tool of the app stop
/// reaching smbd.
pub fn samba_interfaces(all: &[String], excluded: &[String]) -> Vec<String> {
    let mut out = vec!["lo".to_string()];
    for netdev in all {
        if netdev != "lo" && !excluded.contains(netdev) {
            out.push(netdev.clone());
        }
    }
    out
}

/// The list for THIS node.
pub fn samba_interfaces_here(excluded: &[String]) -> Vec<String> {
    samba_interfaces(&all_netdevs(), excluded)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tentaflow_protocol::tentanas::{NasShareAccess, NasSmbOptions};

    fn iface(netdev: &str, ip: &str) -> Interface {
        Interface {
            netdev: netdev.to_string(),
            addresses: vec![ip.to_string()],
        }
    }

    fn ready_probe() -> Probe {
        Probe {
            interfaces: vec![iface("enp1s0f0np0", "10.10.0.5")],
            exposed: Vec::new(),
            missing_tools: Vec::new(),
            module_loaded: true,
            module_available: true,
            kernel: "6.12.4-arch1-1".to_string(),
        }
    }

    fn share(name: &str, smb: NasSmbOptions) -> ShareRow {
        ShareRow {
            share_id: format!("s-{name}"),
            name: name.to_string(),
            protocol: "smb".into(),
            source_path: format!("/mnt/tank/{name}"),
            dataset: Some(format!("tank/{name}")),
            enabled: true,
            fleet_mount: false,
            smb: Some(smb),
            nfs: None,
            state: "active".into(),
            state_detail: String::new(),
            created_at: "2026-09-03T10:00:00Z".into(),
            updated_at: "2026-09-03T10:00:00Z".into(),
        }
    }

    #[test]
    fn the_default_route_is_read_from_both_kernel_tables() {
        // The exact shape /proc/net/route has, header included.
        let ipv4 = "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT\n\
                    enp3s0\t00000000\t0101A8C0\t0003\t0\t0\t100\t00000000\t0\t0\t0\n\
                    enp3s0\t0001A8C0\t00000000\t0001\t0\t0\t100\t00FFFFFF\t0\t0\t0\n\
                    enp1s0f0np0\t000A0A0A\t00000000\t0001\t0\t0\t100\t00FFFFFF\t0\t0\t0\n";
        assert_eq!(parse_ipv4_default_routes(ipv4), vec!["enp3s0".to_string()]);

        // A storage VLAN may be IPv6-only, so an interface that routes the
        // world there must be caught too.
        let ipv6 = "00000000000000000000000000000000 00 00000000000000000000000000000000 00 \
                    fe800000000000000000000000000001 00000400 00000001 00000000 00000003 enp3s0\n\
                    fd000000000000000000000000000000 40 00000000000000000000000000000000 00 \
                    00000000000000000000000000000000 00000100 00000000 00000000 00000001 enp1s0f0np0\n";
        assert_eq!(parse_ipv6_default_routes(ipv6), vec!["enp3s0".to_string()]);
        assert!(parse_ipv4_default_routes("").is_empty());
        assert!(parse_ipv6_default_routes("").is_empty());
    }

    #[test]
    fn a_dedicated_rdma_interface_with_the_tools_is_the_only_ok_state() {
        let ready = ready_probe();
        let (status, detail) = describe(&ready);
        assert_eq!(status, "ok");
        assert_eq!(
            detail,
            "enp1s0f0np0 10.10.0.5 · EXPERIMENTAL (kernel docs) · ksmbd loaded"
        );
        assert!(ready.ready());
        assert_eq!(ready.netdevs(), vec!["enp1s0f0np0".to_string()]);
    }

    #[test]
    fn an_rdma_interface_carrying_the_default_gateway_is_refused_by_name() {
        // The exposure guard of §5.4b: this is not "no device", it is a
        // network the admin has to fix, and the row says which one.
        let exposed = Probe {
            interfaces: Vec::new(),
            exposed: vec![iface("enp3s0", "192.168.1.20")],
            ..ready_probe()
        };
        let (status, detail) = describe(&exposed);
        assert_eq!(status, "exposed");
        assert!(detail.contains("enp3s0 192.168.1.20"), "{detail}");
        assert!(detail.contains("default gateway"), "{detail}");
        assert!(!exposed.ready());
        assert!(refusal(&exposed).contains("default gateway"));

        // One safe interface next to an exposed one is enough to serve, and
        // only the safe one is ever bound.
        let mixed = Probe {
            exposed: vec![iface("enp3s0", "192.168.1.20")],
            ..ready_probe()
        };
        assert_eq!(describe(&mixed).0, "ok");
        assert_eq!(mixed.netdevs(), vec!["enp1s0f0np0".to_string()]);
    }

    #[test]
    fn a_node_without_the_tools_or_the_module_says_which_one_is_missing() {
        let no_tools = Probe {
            missing_tools: vec!["ksmbd.mountd".to_string(), "ksmbd.adduser".to_string()],
            ..ready_probe()
        };
        let (status, detail) = describe(&no_tools);
        assert_eq!(status, "missing");
        assert!(detail.contains("missing: ksmbd.mountd, ksmbd.adduser"), "{detail}");
        assert!(!no_tools.ready());

        let no_module = Probe {
            module_loaded: false,
            module_available: false,
            ..ready_probe()
        };
        let (status, detail) = describe(&no_module);
        assert_eq!(status, "missing_module");
        assert!(detail.contains("not in this kernel's module tree"), "{detail}");
        assert!(!no_module.ready());

        // In the tree but not loaded is a usable node: the helper loads it
        // before it starts the daemon.
        let on_demand = Probe {
            module_loaded: false,
            ..ready_probe()
        };
        assert_eq!(describe(&on_demand).0, "ok");
        assert!(on_demand.ready());

        let nothing = Probe {
            interfaces: Vec::new(),
            exposed: Vec::new(),
            ..ready_probe()
        };
        assert_eq!(describe(&nothing).0, "no_device");
        assert!(!nothing.ready());
    }

    #[test]
    fn the_environment_row_is_the_one_gate_the_rest_of_the_app_reads() {
        let row = |status: &str| NasFeature {
            id: FEATURE_ID.to_string(),
            status: status.to_string(),
            ..Default::default()
        };
        assert!(available(&[row("ok")]));
        assert!(!available(&[row("exposed")]));
        assert!(!available(&[row("no_device")]));
        assert!(!available(&[row("missing")]));
        assert!(!available(&[]));
        assert!(!available(&[NasFeature {
            id: "rdma".to_string(),
            status: "ok".to_string(),
            ..Default::default()
        }]));
    }

    #[test]
    fn only_a_share_with_the_option_reaches_the_config_and_it_binds_the_interfaces() {
        let plain = share(
            "dokumenty",
            NasSmbOptions {
                previous_versions: true,
                recycle_bin: true,
                ..Default::default()
            },
        );
        let direct = share(
            "modele",
            NasSmbOptions {
                smb_direct: true,
                users: vec![
                    NasShareAccess {
                        user: "anna".into(),
                        mode: "rw".into(),
                    },
                    NasShareAccess {
                        user: "jan".into(),
                        mode: "ro".into(),
                    },
                ],
                // The Samba-only options are set here on purpose: they must
                // NOT reach the ksmbd section, which has no module for them.
                previous_versions: true,
                recycle_bin: true,
                time_machine: true,
                ..Default::default()
            },
        );
        assert!(!wants_smb_direct(&[plain.clone()]));
        assert!(wants_smb_direct(&[plain.clone(), direct.clone()]));

        let document = ksmbd_document(&[plain, direct], &["enp1s0f0np0".to_string()]);
        assert_eq!(
            document
                .lines()
                .filter(|l| !l.starts_with('#'))
                .collect::<Vec<_>>()
                .join("\n"),
            "\n[global]\n\
             \tinterfaces = enp1s0f0np0\n\
             \tbind interfaces only = yes\n\
             \ttcp port = 445\n\
             \tserver multi channel support = yes\n\
             \tserver min protocol = SMB3_00\n\
             \tserver string = TentaNas SMB Direct\n\
             \n[modele]\n\
             \tpath = /mnt/tank/modele\n\
             \tbrowseable = yes\n\
             \tread only = no\n\
             \tguest ok = no\n\
             \tvalid users = anna, jan\n\
             \twrite list = anna\n\
             \tread list = jan\n\
             \tcreate mask = 0660\n\
             \tdirectory mask = 2770"
        );
        assert!(!document.contains("[dokumenty]"));
        assert!(!document.contains("shadow"), "no previous versions on this path");
        assert!(!document.contains("recycle"), "no recycle bin on this path");
        assert!(!document.contains("fruit"), "no Time Machine on this path");
        assert!(tentanas_helper::validate_ksmbd_config(&document).is_ok());
    }

    #[test]
    fn a_guest_share_gets_the_mapping_ksmbd_needs_and_the_share_group() {
        let document = ksmbd_document(
            &[share(
                "publiczne",
                NasSmbOptions {
                    smb_direct: true,
                    guests: true,
                    ..Default::default()
                },
            )],
            &["enp1s0f0np0".to_string(), "enp1s0f1np1".to_string()],
        );
        // Without the global mapping ksmbd refuses every guest session, which
        // would be a silent difference between the two backends.
        assert!(document.contains("\tmap to guest = bad user\n"), "{document}");
        assert!(document.contains("\tguest ok = yes\n"));
        assert!(document.contains("\tforce group = tentanas-share\n"));
        assert!(document.contains("\tinterfaces = enp1s0f0np0 enp1s0f1np1\n"));
        assert!(tentanas_helper::validate_ksmbd_config(&document).is_ok());

        // A config without guests does not carry the mapping at all.
        let strict = ksmbd_document(
            &[share(
                "modele",
                NasSmbOptions {
                    smb_direct: true,
                    ..Default::default()
                },
            )],
            &["enp1s0f0np0".to_string()],
        );
        assert!(!strict.contains("map to guest"));
    }

    #[test]
    fn a_config_that_would_bind_the_whole_node_is_refused_by_the_catalog() {
        // The generator cannot produce these, which is exactly why the root
        // side parses rather than trusts: an unbound listener is the one
        // shape §5.4b forbids.
        let unbound = "[global]\n\tserver multi channel support = yes\n[modele]\n\tpath = /mnt/tank/modele\n";
        assert!(tentanas_helper::validate_ksmbd_config(unbound).is_err());
        let empty_list = "[global]\n\tinterfaces = \n\tbind interfaces only = yes\n";
        assert!(tentanas_helper::validate_ksmbd_config(empty_list).is_err());
        let not_only = "[global]\n\tinterfaces = enp1s0f0np0\n\tbind interfaces only = no\n";
        assert!(tentanas_helper::validate_ksmbd_config(not_only).is_err());
        // An interface name that is really a second value.
        let injected = "[global]\n\tinterfaces = enp1s0f0np0/../x\n\tbind interfaces only = yes\n";
        assert!(tentanas_helper::validate_ksmbd_config(injected).is_err());
        // A path outside the pools, and a parameter that is not ksmbd's.
        let escape = "[global]\n\tinterfaces = eth0\n\tbind interfaces only = yes\n[x]\n\tpath = /etc\n";
        assert!(tentanas_helper::validate_ksmbd_config(escape).is_err());
        let samba_only = "[global]\n\tinterfaces = eth0\n\tbind interfaces only = yes\n[x]\n\tvfs objects = shadow_copy2\n";
        assert!(tentanas_helper::validate_ksmbd_config(samba_only).is_err());
    }

    #[test]
    fn samba_keeps_every_interface_the_listener_did_not_take_plus_loopback() {
        let all = vec![
            "enp1s0f0np0".to_string(),
            "enp3s0".to_string(),
            "lo".to_string(),
            "tf-mesh0".to_string(),
        ];
        assert_eq!(
            samba_interfaces(&all, &["enp1s0f0np0".to_string()]),
            vec![
                "lo".to_string(),
                "enp3s0".to_string(),
                "tf-mesh0".to_string()
            ]
        );
        // Loopback is listed exactly once and always: `bind interfaces only`
        // without it cuts smbpasswd and smbstatus off from smbd.
        assert_eq!(
            samba_interfaces(&all, &[]),
            vec![
                "lo".to_string(),
                "enp1s0f0np0".to_string(),
                "enp3s0".to_string(),
                "tf-mesh0".to_string()
            ]
        );
        assert_eq!(samba_interfaces(&[], &[]), vec!["lo".to_string()]);
    }
}
