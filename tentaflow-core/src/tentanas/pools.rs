// =============================================================================
// File: tentanas/pools.rs — ZFS pools of one node (plan-02 §5.2, tabs "Pule"
//       and the pool detail). Three machine formats feed the whole view:
//
//         zpool list -Hp -o …    identity, raw capacity, fragmentation, health
//         zpool status -pP <p>   the vdev tree, per-leaf error counters, scan
//         zpool iostat -Hply 1 1 live throughput and latency
//
//       `zpool status` has no machine format at all, so its indentation IS
//       the contract: after the tab that starts every config line, column 0
//       is the pool (or a section keyword), column 2 a top-level vdev and
//       everything deeper a leaf. That is why the parser is fixture-tested
//       rather than trusted.
// =============================================================================

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tentaflow_protocol::tentanas::{
    NasDisk, NasImportablePool, NasJob, NasPool, NasPoolIo, NasPoolLayoutOption, NasPoolScan,
    NasVdev, NasVdevDisk,
};
use tentanas_helper::HelperCommand;

use super::broker::BrokerError;
use super::db as store;
use super::zfs;
use crate::db::DbPool;
use crate::profiling::collectors::elevation::ElevationToken;

/// `zpool list` columns, in the order the parser expects them.
pub const LIST_COLUMNS: &str =
    "name,guid,size,alloc,free,cap,frag,dedupratio,health,ashift,autotrim,readonly";

/// Capacity at which a pool stops being healthy. ZFS allocation slows down
/// sharply as a pool fills; 80 % is the community-standard warning line and
/// 90 % is where write performance collapses.
const CAPACITY_WARNING_PCT: u8 = 80;
const CAPACITY_CRITICAL_PCT: u8 = 90;

/// Beyond this spread the smallest disk wastes a visible share of the array.
const SIZE_SPREAD_WARNING: f64 = 0.10;

// ----- zpool list ---------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq)]
pub struct PoolListRow {
    pub name: String,
    pub guid: String,
    pub size_bytes: u64,
    pub alloc_bytes: u64,
    pub free_bytes: u64,
    pub capacity_pct: u8,
    pub fragmentation_pct: u8,
    pub dedup_ratio: f64,
    pub state: String,
    pub ashift: u32,
    pub autotrim: bool,
    pub read_only: bool,
}

pub fn parse_list(text: &str) -> Vec<PoolListRow> {
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|line| {
            let f: Vec<&str> = line.split('\t').collect();
            if f.len() < 12 {
                return None;
            }
            Some(PoolListRow {
                name: f[0].trim().to_string(),
                guid: zfs::field(f[1]).unwrap_or_default().to_string(),
                size_bytes: zfs::u64_field(f[2]),
                alloc_bytes: zfs::u64_field(f[3]),
                free_bytes: zfs::u64_field(f[4]),
                capacity_pct: zfs::u64_field(f[5]).min(100) as u8,
                fragmentation_pct: zfs::u64_field(f[6]).min(100) as u8,
                dedup_ratio: zfs::f64_field(f[7]),
                state: f[8].trim().to_ascii_lowercase(),
                ashift: zfs::u64_field(f[9]) as u32,
                autotrim: zfs::bool_field(f[10]),
                read_only: zfs::bool_field(f[11]),
            })
        })
        .collect()
}

// ----- zpool status -------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq)]
pub struct StatusReport {
    pub state: String,
    pub vdevs: Vec<NasVdev>,
    pub scan: NasPoolScan,
    /// The `errors:` line verbatim, for the card's health reason.
    pub errors_line: String,
    /// Permanent data errors the pool reports (0 for "No known data errors").
    pub data_errors: u64,
}

/// Section keywords that switch the role of the vdevs that follow.
fn section_role(word: &str) -> Option<&'static str> {
    match word {
        "logs" | "log" => Some("log"),
        "cache" => Some("cache"),
        "spares" | "spare" => Some("spare"),
        "special" => Some("special"),
        "dedup" => Some("dedup"),
        _ => None,
    }
}

/// Group names ZFS generates for a container rather than a physical leaf.
fn group_kind(name: &str) -> Option<&'static str> {
    let base = name.split('-').next().unwrap_or(name);
    match base {
        "mirror" => Some("mirror"),
        "raidz1" => Some("raidz1"),
        "raidz2" => Some("raidz2"),
        "raidz3" => Some("raidz3"),
        // `raidz` with no digit is the old spelling of raidz1.
        "raidz" => Some("raidz1"),
        "draid1" => Some("draid1"),
        "draid2" => Some("draid2"),
        "draid3" => Some("draid3"),
        "draid" => Some("draid"),
        "replacing" | "spare" | "indirect" => Some("mirror"),
        _ => None,
    }
}

fn normalize_state(raw: &str) -> String {
    match raw.to_ascii_uppercase().as_str() {
        // A spare is ONLINE and idle (AVAIL) or ONLINE and substituting (INUSE).
        "ONLINE" | "AVAIL" | "INUSE" => "online",
        "DEGRADED" => "degraded",
        "FAULTED" => "faulted",
        "OFFLINE" => "offline",
        "REMOVED" => "removed",
        "UNAVAIL" | "SUSPENDED" => "unavail",
        _ => "unavail",
    }
    .to_string()
}

pub fn fault_tolerance_of(kind: &str, leaves: usize) -> u8 {
    match kind {
        "mirror" => leaves.saturating_sub(1).min(255) as u8,
        "raidz1" | "draid1" => 1,
        "raidz2" | "draid2" => 2,
        "raidz3" | "draid3" => 3,
        _ => 0,
    }
}

/// One parsed config row: its depth, name, state and counters.
struct ConfigRow<'a> {
    depth: usize,
    name: &'a str,
    state: &'a str,
    read: u64,
    write: u64,
    cksum: u64,
    note: String,
}

fn parse_config_row(line: &str) -> Option<ConfigRow<'_>> {
    // Every config line starts with one tab; indentation after it is the tree.
    let body = line.strip_prefix('\t')?;
    let indent = body.len() - body.trim_start().len();
    let trimmed = body.trim();
    if trimmed.is_empty() || trimmed.starts_with("NAME ") {
        return None;
    }
    // A trailing free-text note is parenthesized ("(resilvering)", "(was …)").
    let (fields_part, note) = match trimmed.split_once('(') {
        Some((head, tail)) => (head.trim(), tail.trim_end_matches(')').trim().to_string()),
        None => (trimmed, String::new()),
    };
    let f: Vec<&str> = fields_part.split_whitespace().collect();
    let name = f.first()?;
    Some(ConfigRow {
        depth: indent / 2,
        name,
        state: f.get(1).copied().unwrap_or(""),
        read: f.get(2).and_then(|v| v.parse().ok()).unwrap_or(0),
        write: f.get(3).and_then(|v| v.parse().ok()).unwrap_or(0),
        cksum: f.get(4).and_then(|v| v.parse().ok()).unwrap_or(0),
        note,
    })
}

pub fn parse_status(text: &str) -> StatusReport {
    let mut report = StatusReport::default();
    let lines: Vec<&str> = text.lines().collect();
    let mut role = "data".to_string();
    let mut in_config = false;
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("state:") {
            if !in_config {
                report.state = normalize_state(rest.trim());
            }
        } else if trimmed.starts_with("scan:") {
            let (scan, consumed) = parse_scan(&lines[i..]);
            report.scan = scan;
            i += consumed;
            continue;
        } else if let Some(rest) = trimmed.strip_prefix("errors:") {
            let rest = rest.trim();
            report.errors_line = rest.to_string();
            report.data_errors = rest
                .split_whitespace()
                .next()
                .and_then(|n| n.parse().ok())
                .unwrap_or(0);
            in_config = false;
        } else if trimmed.starts_with("config:") {
            in_config = true;
            role = "data".to_string();
        } else if in_config {
            if let Some(row) = parse_config_row(line) {
                apply_config_row(&mut report, &row, &mut role);
            }
        }
        i += 1;
    }
    report
}

fn apply_config_row(report: &mut StatusReport, row: &ConfigRow<'_>, role: &mut String) {
    if row.depth == 0 {
        if let Some(section) = section_role(row.name) {
            *role = section.to_string();
        }
        // Depth 0 is either a section keyword or the pool row itself; the pool
        // row's state already came from the `state:` header.
        return;
    }
    if row.depth == 1 {
        let kind = group_kind(row.name).unwrap_or("disk");
        if kind == "disk" {
            // A bare leaf at top level: a single-disk vdev of its own.
            report.vdevs.push(NasVdev {
                id: row.name.to_string(),
                role: role.clone(),
                kind: "disk".to_string(),
                state: normalize_state(row.state),
                fault_tolerance: 0,
                disks: vec![leaf(row)],
            });
        } else {
            report.vdevs.push(NasVdev {
                id: row.name.to_string(),
                role: role.clone(),
                kind: kind.to_string(),
                state: normalize_state(row.state),
                fault_tolerance: 0,
                disks: Vec::new(),
            });
        }
        return;
    }
    // Deeper rows are leaves of the current top-level vdev; a `replacing-N`
    // or `spare-N` container in between contributes no leaf of its own.
    if group_kind(row.name).is_some() && row.depth == 2 {
        return;
    }
    if let Some(vdev) = report.vdevs.last_mut() {
        vdev.disks.push(leaf(row));
        vdev.fault_tolerance = fault_tolerance_of(&vdev.kind, vdev.disks.len());
    }
}

fn leaf(row: &ConfigRow<'_>) -> NasVdevDisk {
    let path = if row.name.starts_with('/') {
        row.name.to_string()
    } else {
        String::new()
    };
    NasVdevDisk {
        disk_id: None,
        name: zfs::kernel_name_of(row.name),
        path,
        state: normalize_state(row.state),
        read_errors: row.read,
        write_errors: row.write,
        cksum_errors: row.cksum,
        size_bytes: 0,
        note: row.note.clone(),
    }
}

/// The `scan:` block: the header line plus the indented continuation lines a
/// running scrub or resilver adds. Returns how many lines it consumed.
pub fn parse_scan(lines: &[&str]) -> (NasPoolScan, usize) {
    let mut scan = NasPoolScan::default();
    let head = lines[0].trim().trim_start_matches("scan:").trim();
    let mut consumed = 1;
    while consumed < lines.len() {
        let next = lines[consumed];
        // Continuation lines of the scan block start with a tab and are not
        // one of the other `key:` headers.
        if !next.starts_with('\t') || next.trim().is_empty() {
            break;
        }
        consumed += 1;
    }
    let body: Vec<&str> = lines[1..consumed].iter().map(|l| l.trim()).collect();

    scan.kind = if head.contains("resilver") {
        "resilver"
    } else if head.contains("scrub") {
        "scrub"
    } else {
        "none"
    }
    .to_string();

    if head.starts_with("none requested") {
        scan.status = "none".to_string();
        return (scan, consumed);
    }
    if head.contains("in progress") || head.contains("paused") {
        scan.status = if head.contains("paused") { "paused" } else { "running" }.to_string();
        scan.started_at = head
            .split_once("since ")
            .and_then(|(_, t)| zfs::parse_status_time(t));
        for line in &body {
            if let Some(started) = line.strip_prefix("scrub started on ") {
                scan.started_at = zfs::parse_status_time(started).or(scan.started_at.take());
            }
            if let Some((scanned, _)) = line.split_once(" scanned") {
                let value = scanned.split('/').next().unwrap_or(scanned);
                scan.scanned_bytes = zfs::parse_bytes(value).unwrap_or(0);
            }
            if let Some((pct, _)) = line.split_once("% done") {
                if let Some(num) = pct.rsplit(", ").next() {
                    scan.progress_pct = num.trim().parse::<f64>().ok().map(|v| v.round() as u8);
                }
            }
            if let Some((eta, _)) = line.split_once(" to go") {
                if let Some(v) = eta.rsplit(", ").next() {
                    scan.eta_secs = zfs::parse_duration_secs(v);
                }
            }
        }
        return (scan, consumed);
    }
    if head.contains("canceled") {
        scan.status = "canceled".to_string();
        scan.finished_at = head
            .split_once(" on ")
            .and_then(|(_, t)| zfs::parse_status_time(t));
        return (scan, consumed);
    }
    // Finished: "scrub repaired 0B in 00:12:34 with 0 errors on <date>" or
    // "resilvered 1.20T in 02:11:00 with 0 errors on <date>".
    scan.status = "finished".to_string();
    scan.progress_pct = Some(100);
    if let Some((_, rest)) = head.split_once(" in ") {
        scan.duration_secs = rest.split(" with ").next().and_then(zfs::parse_duration_secs);
    }
    if let Some((_, rest)) = head.split_once("with ") {
        scan.errors = rest
            .split_whitespace()
            .next()
            .and_then(|n| n.parse().ok())
            .unwrap_or(0);
    }
    if let Some((_, rest)) = head.split_once(" on ") {
        scan.finished_at = zfs::parse_status_time(rest);
    }
    // A finished resilver reports how much data it rebuilt; a finished scrub
    // reports only what it repaired, which is not a scanned amount.
    if let Some(resilvered) = head.strip_prefix("resilvered ") {
        scan.scanned_bytes =
            zfs::parse_bytes(resilvered.split(' ').next().unwrap_or("")).unwrap_or(0);
    }
    (scan, consumed)
}

// ----- zpool iostat --------------------------------------------------------------

/// `zpool iostat -Hply 1 1`: one interval-based sample per pool. `-y` drops
/// the boot-average first report, so what comes back is a real delta and not
/// the pool's lifetime average. Latencies are nanoseconds under `-p`.
pub fn parse_iostat(text: &str) -> HashMap<String, NasPoolIo> {
    let mut out = HashMap::new();
    for line in text.lines() {
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() < 9 || f[0].trim().is_empty() {
            continue;
        }
        let ns_to_ms = |raw: &str| zfs::opt_u64_field(raw).unwrap_or(0) as f64 / 1_000_000.0;
        out.insert(
            f[0].trim().to_string(),
            NasPoolIo {
                read_iops: zfs::f64_field(f[3]),
                write_iops: zfs::f64_field(f[4]),
                read_bps: zfs::u64_field(f[5]),
                write_bps: zfs::u64_field(f[6]),
                read_latency_ms: (ns_to_ms(f[7]) * 100.0).round() / 100.0,
                write_latency_ms: (ns_to_ms(f[8]) * 100.0).round() / 100.0,
            },
        );
    }
    out
}

// ----- zpool import (scan) -------------------------------------------------------

/// The human report of `zpool import` with no pool named. There is no machine
/// format for it, so the parser keys on the `  key: value` headers and the
/// config block that follows.
pub fn parse_import_scan(text: &str) -> Vec<NasImportablePool> {
    let mut pools: Vec<NasImportablePool> = Vec::new();
    let mut in_config = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(name) = trimmed.strip_prefix("pool:") {
            pools.push(NasImportablePool {
                name: name.trim().to_string(),
                exported_cleanly: true,
                ..Default::default()
            });
            in_config = false;
            continue;
        }
        let Some(current) = pools.last_mut() else {
            continue;
        };
        if let Some(id) = trimmed.strip_prefix("id:") {
            current.guid = id.trim().to_string();
        } else if let Some(state) = trimmed.strip_prefix("state:") {
            current.state = normalize_state(state.trim());
        } else if let Some(status) = trimmed.strip_prefix("status:") {
            // Any status line means the pool was not left cleanly exported —
            // importing it then needs `force`.
            current.exported_cleanly = false;
            current.message = status.trim().to_string();
        } else if trimmed.starts_with("config:") {
            in_config = true;
        } else if in_config {
            let Some(row) = parse_config_row(line) else {
                continue;
            };
            if row.depth == 0 {
                continue;
            }
            match group_kind(row.name) {
                Some(kind) if row.depth == 1 => {
                    if current.layout.is_empty() {
                        current.layout = kind.to_string();
                    }
                }
                Some(_) => {}
                None => {
                    if row.depth == 1 && current.layout.is_empty() {
                        current.layout = "stripe".to_string();
                    }
                    current.disks.push(zfs::kernel_name_of(row.name));
                }
            }
        }
    }
    pools
}

// ----- health, layout summary ----------------------------------------------------

/// The one status of the pool card. Capacity counts: a pool over 90 % full is
/// as much an operator problem as a degraded vdev, and it is the failure the
/// admin can still act on.
pub fn score_health(pool: &NasPool, errors_line: &str, data_errors: u64) -> (&'static str, String) {
    let mut critical = Vec::new();
    let mut warning = Vec::new();
    match pool.state.as_str() {
        "faulted" | "unavail" | "removed" => {
            critical.push(format!("pool is {}", pool.state));
        }
        "degraded" => warning.push("pool is degraded".to_string()),
        "offline" => warning.push("pool is offline".to_string()),
        _ => {}
    }
    if data_errors > 0 {
        critical.push(format!("{data_errors} permanent data errors"));
    } else if !errors_line.is_empty() && !errors_line.starts_with("No known data errors") {
        critical.push(errors_line.to_string());
    }
    let mut bad_leaves = 0;
    let mut soft_leaves = 0;
    for vdev in &pool.vdevs {
        for disk in &vdev.disks {
            match disk.state.as_str() {
                "faulted" | "unavail" | "removed" => bad_leaves += 1,
                "degraded" | "offline" => soft_leaves += 1,
                _ => {}
            }
        }
    }
    if bad_leaves > 0 {
        critical.push(format!("{bad_leaves} unusable disks"));
    }
    if soft_leaves > 0 {
        warning.push(format!("{soft_leaves} degraded disks"));
    }
    let error_disks = pool
        .vdevs
        .iter()
        .flat_map(|v| &v.disks)
        .filter(|d| d.read_errors + d.write_errors + d.cksum_errors > 0)
        .count();
    if error_disks > 0 {
        warning.push(format!("{error_disks} disks with I/O or checksum errors"));
    }
    if pool.scan.errors > 0 {
        warning.push(format!("last {} found {} errors", pool.scan.kind, pool.scan.errors));
    }
    if pool.capacity_pct >= CAPACITY_CRITICAL_PCT {
        critical.push(format!("{}% full", pool.capacity_pct));
    } else if pool.capacity_pct >= CAPACITY_WARNING_PCT {
        warning.push(format!("{}% full", pool.capacity_pct));
    }
    if !critical.is_empty() {
        ("critical", critical.join("; "))
    } else if !warning.is_empty() {
        ("warning", warning.join("; "))
    } else {
        ("ok", String::new())
    }
}

/// The redundancy word on the card: the kind shared by every data vdev, or
/// `mixed` when they disagree.
pub fn layout_summary(vdevs: &[NasVdev]) -> String {
    let mut kinds = vdevs
        .iter()
        .filter(|v| v.role == "data")
        .map(|v| if v.kind == "disk" { "stripe" } else { v.kind.as_str() });
    let Some(first) = kinds.next() else {
        return String::new();
    };
    if kinds.all(|k| k == first) {
        first.to_string()
    } else {
        "mixed".to_string()
    }
}

/// Leaves that carry data (parity included — they are all real disks) and the
/// simultaneous failures the pool survives, which is the weakest data vdev.
pub fn data_disks_and_tolerance(vdevs: &[NasVdev]) -> (u32, u8) {
    let data: Vec<&NasVdev> = vdevs.iter().filter(|v| v.role == "data").collect();
    let disks = data.iter().map(|v| v.disks.len() as u32).sum();
    let tolerance = data.iter().map(|v| v.fault_tolerance).min().unwrap_or(0);
    (disks, tolerance)
}

// ----- layout planning (wizard step "layout") -------------------------------------

/// Every layout the wizard offers, in the order it shows them.
const LAYOUTS: &[&str] = &["stripe", "mirror", "raidz1", "raidz2", "raidz3", "draid1", "draid2"];

fn min_disks(layout: &str) -> usize {
    tentanas_helper::VdevKind::parse(layout)
        .map(|k| k.min_disks())
        .unwrap_or(usize::MAX)
}

fn parity(layout: &str) -> u64 {
    tentanas_helper::VdevKind::parse(layout)
        .map(|k| u64::from(k.parity()))
        .unwrap_or(0)
}

/// Usable bytes of `n` disks of `smallest` bytes each. Mirrors are built as
/// pairs (an odd disk is left out of the plan), raidz and draid lose `parity`
/// disks worth of capacity per vdev.
pub fn usable_bytes(layout: &str, disks: usize, smallest: u64) -> u64 {
    let n = disks as u64;
    match layout {
        "stripe" => n * smallest,
        "mirror" => (n / 2) * smallest,
        _ => n.saturating_sub(parity(layout)) * smallest,
    }
}

/// The layout the wizard preselects for a disk count: mirrors while the pool
/// is small enough that rebuild time and capacity both favour them, single
/// parity from four disks, double parity once a rebuild window gets long
/// enough that a second failure during it is a real risk.
pub fn recommended_layout(disks: usize) -> Option<&'static str> {
    match disks {
        0 => None,
        1 => Some("stripe"),
        2..=3 => Some("mirror"),
        4..=5 => Some("raidz1"),
        _ => Some("raidz2"),
    }
}

/// The wizard's layout step for a picked set of disks: what each layout would
/// give, plus the warnings that make a selection a bad idea rather than an
/// impossible one.
pub fn plan(disks: &[NasDisk]) -> (Vec<NasPoolLayoutOption>, Vec<String>, u64) {
    let n = disks.len();
    let smallest = disks.iter().map(|d| d.size_bytes).min().unwrap_or(0);
    let largest = disks.iter().map(|d| d.size_bytes).max().unwrap_or(0);
    let recommended = recommended_layout(n);

    let options = LAYOUTS
        .iter()
        .map(|layout| {
            let min = min_disks(layout);
            let available = n >= min;
            NasPoolLayoutOption {
                layout: (*layout).to_string(),
                available,
                reason: if available {
                    String::new()
                } else {
                    "too_few_disks".to_string()
                },
                usable_bytes: if available {
                    usable_bytes(layout, n, smallest)
                } else {
                    0
                },
                raw_bytes: (n as u64) * smallest,
                fault_tolerance: if *layout == "mirror" {
                    u8::from(n >= 2)
                } else {
                    parity(layout) as u8
                },
                recommended: available && recommended == Some(*layout),
            }
        })
        .collect();

    let mut warnings = Vec::new();
    if largest > 0 && (largest - smallest) as f64 / largest as f64 > SIZE_SPREAD_WARNING {
        warnings.push(format!(
            "mixed disk sizes ({} … {} bytes): every vdev is sized by the smallest disk, \
             so the extra capacity of the larger ones stays unused",
            smallest, largest
        ));
    }
    let rotating = disks.iter().filter(|d| d.rotational).count();
    if rotating > 0 && rotating < n {
        warnings.push(
            "mixed SSD and HDD: the vdev runs at the speed of its slowest member".to_string(),
        );
    }
    let unhealthy: Vec<&str> = disks
        .iter()
        .filter(|d| matches!(d.health.as_str(), "warning" | "critical"))
        .map(|d| d.name.as_str())
        .collect();
    if !unhealthy.is_empty() {
        warnings.push(format!(
            "SMART warnings on {}: building a pool on a disk that is already failing \
             starts it degraded",
            unhealthy.join(", ")
        ));
    }
    if n == 1 {
        warnings.push("a single disk has no redundancy: any failure loses the pool".to_string());
    }
    if n % 2 == 1 && n > 2 {
        warnings.push(
            "a mirror layout pairs disks two by two, so one of an odd selection stays unused"
                .to_string(),
        );
    }
    (options, warnings, smallest)
}

// ----- vdev spec building ---------------------------------------------------------

/// Turns a wizard layout plus a device list into the catalog's vdev groups.
/// A mirror becomes one group per pair — the shape the capacity plan assumes.
pub fn vdev_groups(
    role: tentanas_helper::VdevRole,
    layout: &str,
    devices: &[String],
) -> Result<Vec<tentanas_helper::VdevSpec>, BrokerError> {
    let kind = tentanas_helper::VdevKind::parse(layout)
        .ok_or_else(|| BrokerError::InvalidArgument(format!("unknown layout '{layout}'")))?;
    if devices.len() < kind.min_disks() {
        return Err(BrokerError::InvalidArgument(format!(
            "{layout} needs at least {} disks, got {}",
            kind.min_disks(),
            devices.len()
        )));
    }
    if kind == tentanas_helper::VdevKind::Mirror && role == tentanas_helper::VdevRole::Data {
        return Ok(devices
            .chunks_exact(2)
            .map(|pair| tentanas_helper::VdevSpec {
                role,
                kind,
                devices: pair.to_vec(),
            })
            .collect());
    }
    Ok(vec![tentanas_helper::VdevSpec {
        role,
        kind,
        devices: devices.to_vec(),
    }])
}

// ----- live reads -----------------------------------------------------------------

pub async fn list_rows() -> Result<Vec<PoolListRow>, BrokerError> {
    let text = zfs::zpool(&["list", "-Hp", "-o", LIST_COLUMNS]).await?;
    Ok(parse_list(&text))
}

pub async fn status(pool: &str) -> Result<StatusReport, BrokerError> {
    tentanas_helper::validate_pool_name(pool)
        .map_err(|e| BrokerError::InvalidArgument(e.to_string()))?;
    let text = zfs::zpool(&["status", "-pP", pool]).await?;
    Ok(parse_status(&text))
}

/// One interval sample of every pool. Blocks for the interval it measures.
pub async fn iostat() -> Result<HashMap<String, NasPoolIo>, BrokerError> {
    let text = zfs::run(
        zfs::ZPOOL,
        &["iostat", "-Hply", "1", "1"],
        Duration::from_secs(15),
    )
    .await?;
    Ok(parse_iostat(&text))
}

// ----- assembling the protocol view -------------------------------------------------

/// The last `zpool iostat` sample of every pool, refreshed by the node's
/// sampler. Requests read it instead of running `zpool iostat 1 1`, which
/// blocks for the second it measures.
fn live_io() -> &'static parking_lot::RwLock<HashMap<String, NasPoolIo>> {
    static IO: std::sync::OnceLock<parking_lot::RwLock<HashMap<String, NasPoolIo>>> =
        std::sync::OnceLock::new();
    IO.get_or_init(|| parking_lot::RwLock::new(HashMap::new()))
}

/// Kernel name → (disk id, size) of every disk the inventory knows, so a vdev
/// leaf links to the Disks tab.
fn disk_index() -> HashMap<String, (String, u64)> {
    super::disks::snapshot()
        .0
        .into_iter()
        .map(|d| (d.name, (d.disk_id, d.size_bytes)))
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn assemble(
    db: &DbPool,
    row: PoolListRow,
    status: StatusReport,
    datasets: &[tentaflow_protocol::tentanas::NasDataset],
    snapshots: &[tentaflow_protocol::tentanas::NasSnapshot],
    disks: &HashMap<String, (String, u64)>,
) -> NasPool {
    let mut vdevs = status.vdevs;
    for vdev in vdevs.iter_mut() {
        for leaf in vdev.disks.iter_mut() {
            if let Some((disk_id, size)) = disks.get(&leaf.name) {
                leaf.disk_id = Some(disk_id.clone());
                leaf.size_bytes = *size;
            }
        }
    }
    let (read_errors, write_errors, cksum_errors) = vdevs
        .iter()
        .flat_map(|v| &v.disks)
        .fold((0, 0, 0), |(r, w, c), d| {
            (r + d.read_errors, w + d.write_errors, c + d.cksum_errors)
        });
    let root = datasets.iter().find(|d| d.name == row.name);
    let (data_disks, fault_tolerance) = data_disks_and_tolerance(&vdevs);
    let prefix = format!("{}/", row.name);
    let schedule = store::scrub_schedule(db, &row.name).ok().flatten();

    let mut pool = NasPool {
        name: row.name.clone(),
        guid: row.guid,
        kind: "zfs".to_string(),
        state: if status.state.is_empty() {
            row.state
        } else {
            status.state
        },
        health: String::new(),
        health_reason: String::new(),
        size_bytes: row.size_bytes,
        alloc_bytes: row.alloc_bytes,
        free_bytes: row.free_bytes,
        usable_bytes: root.map_or(0, |d| d.used_bytes + d.available_bytes),
        used_bytes: root.map_or(0, |d| d.used_bytes),
        available_bytes: root.map_or(0, |d| d.available_bytes),
        capacity_pct: row.capacity_pct,
        fragmentation_pct: row.fragmentation_pct,
        compress_ratio: root.map_or(0.0, |d| d.compress_ratio),
        dedup_ratio: row.dedup_ratio,
        ashift: row.ashift,
        autotrim: row.autotrim,
        read_only: row.read_only,
        layout: layout_summary(&vdevs),
        data_disks,
        fault_tolerance,
        // The root dataset is a dataset of the pool but not a row of the
        // dataset table, so it is not counted here either.
        dataset_count: datasets.iter().filter(|d| d.name.starts_with(&prefix)).count() as u32,
        snapshot_count: snapshots
            .iter()
            .filter(|s| s.dataset == row.name || s.dataset.starts_with(&prefix))
            .count() as u32,
        io: live_io().read().get(&row.name).cloned().unwrap_or_default(),
        compression: root.map_or_else(String::new, |d| d.compression.clone()),
        encryption: root.is_some_and(|d| d.encryption != "off"),
        scrub_schedule: schedule.as_ref().map(|s| s.schedule.clone()),
        last_scrub_at: status
            .scan
            .finished_at
            .clone()
            .filter(|_| status.scan.kind == "scrub"),
        next_scrub_at: schedule
            .as_ref()
            .filter(|s| s.enabled)
            .and_then(|s| s.next_run_at.clone()),
        scan: status.scan,
        read_errors,
        write_errors,
        cksum_errors,
        vdevs,
    };
    let (health, reason) = score_health(&pool, &status.errors_line, status.data_errors);
    pool.health = health.to_string();
    pool.health_reason = reason;
    pool
}

/// Every pool of this node, ready for `PoolsListRequest`.
pub async fn collect(db: &DbPool) -> Result<Vec<NasPool>, BrokerError> {
    let rows = list_rows().await?;
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    let datasets = super::datasets::list("").await.unwrap_or_default();
    let snapshots = super::snapshots::list("", "", false).await.unwrap_or_default();
    let disks = disk_index();
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let status = status(&row.name).await?;
        out.push(assemble(db, row, status, &datasets, &snapshots, &disks));
    }
    Ok(out)
}

/// One pool, or None when this node does not have it.
pub async fn one(db: &DbPool, name: &str) -> Result<Option<NasPool>, BrokerError> {
    tentanas_helper::validate_pool_name(name)
        .map_err(|e| BrokerError::InvalidArgument(e.to_string()))?;
    let text = match zfs::zpool(&["list", "-Hp", "-o", LIST_COLUMNS, name]).await {
        Ok(t) => t,
        // `zpool list <missing>` exits 1: not found, not a broken node.
        Err(BrokerError::Exit { .. }) => return Ok(None),
        Err(e) => return Err(e),
    };
    let Some(row) = parse_list(&text).into_iter().next() else {
        return Ok(None);
    };
    let status = status(name).await?;
    let datasets = super::datasets::list(name).await.unwrap_or_default();
    let snapshots = super::snapshots::list(name, "", false).await.unwrap_or_default();
    Ok(Some(assemble(
        db,
        row,
        status,
        &datasets,
        &snapshots,
        &disk_index(),
    )))
}

/// One `zpool iostat` sample into the live cache and the 24 h history. Called
/// from the disk sampler's minute tick, so pools and disks share a cadence.
pub async fn persist_sample(db: &DbPool) {
    if !zfs::available() {
        return;
    }
    let io = match iostat().await {
        Ok(io) => io,
        Err(BrokerError::ToolMissing(_)) => return,
        Err(e) => {
            tracing::warn!("tentanas: zpool iostat failed: {e}");
            return;
        }
    };
    let at = chrono::Utc::now().format("%Y-%m-%dT%H:%M:00Z").to_string();
    let rows: Vec<store::PoolSampleInsert<'_>> = io
        .iter()
        .map(|(pool, s)| store::PoolSampleInsert {
            pool,
            sampled_at: &at,
            read_bps: s.read_bps,
            write_bps: s.write_bps,
            read_iops: s.read_iops,
            write_iops: s.write_iops,
            read_latency_ms: s.read_latency_ms,
            write_latency_ms: s.write_latency_ms,
        })
        .collect();
    if let Err(e) = store::insert_pool_samples(db, &rows) {
        tracing::warn!("tentanas: pool sample persist failed: {e}");
    }
    *live_io().write() = io;
}

// ----- jobs ---------------------------------------------------------------------------

/// How long a single `zpool`/`zfs` mutation may take before the broker kills
/// it. Creation and import touch every disk, so they get the long end.
const MUTATION_TIMEOUT: Duration = Duration::from_secs(15 * 60);
/// How often a running scrub or resilver is polled for progress.
const SCAN_POLL: Duration = Duration::from_secs(15);

/// Runs one catalog command and reports the pool afterwards. Every one-shot
/// pool mutation (destroy, export, import, add, expand, device state, set)
/// uses it.
pub async fn command_job(
    h: super::jobs::JobHandle,
    command: HelperCommand,
    explicit: Option<Arc<ElevationToken>>,
) -> anyhow::Result<()> {
    super::jobs::run_step(&h, &command, explicit.as_deref(), MUTATION_TIMEOUT).await?;
    drop(explicit);
    h.progress(100);
    Ok(())
}

/// `zpool create`, with the encryption key on stdin when the wizard asked for
/// an encrypted root. The key is stored only after the pool exists — a key
/// for a pool that was never created would be a lie the admin could not tell
/// from a real one.
pub async fn create_job(
    h: super::jobs::JobHandle,
    command: HelperCommand,
    key: Option<KeyForNewRoot>,
    explicit: Option<Arc<ElevationToken>>,
) -> anyhow::Result<()> {
    match key {
        Some(key) => {
            super::jobs::run_step_with_key(
                &h,
                &command,
                &key.material,
                explicit.as_deref(),
                MUTATION_TIMEOUT,
            )
            .await?;
            super::keystore::put(&key.cipher, &key.addon_id, &key.dataset, &key.material)?;
            h.log(format!("encryption key of {} stored in the node keystore", key.dataset));
        }
        None => {
            super::jobs::run_step(&h, &command, explicit.as_deref(), MUTATION_TIMEOUT).await?;
        }
    }
    drop(explicit);
    h.progress(100);
    Ok(())
}

/// Everything `create_job` needs to store the key of a new encryption root.
pub struct KeyForNewRoot {
    pub cipher: Arc<crate::crypto::SettingsCipher>,
    pub addon_id: String,
    pub dataset: String,
    pub material: zeroize::Zeroizing<Vec<u8>>,
}

/// Cancelling a scrub job must actually stop the scrub. `jobs::spawn` drops
/// the body future when the token fires, so the stop cannot be awaited in the
/// body — it is issued from this guard's `Drop` as a detached task.
struct StopScrubOnCancel {
    db: DbPool,
    pool: String,
    cancel: tokio_util::sync::CancellationToken,
    armed: bool,
}

impl Drop for StopScrubOnCancel {
    fn drop(&mut self) {
        if !self.armed || !self.cancel.is_cancelled() {
            return;
        }
        let (db, pool) = (self.db.clone(), self.pool.clone());
        tokio::spawn(async move {
            let stop = HelperCommand::ZpoolScrub {
                pool: pool.clone(),
                action: tentanas_helper::ScrubAction::Stop,
            };
            if let Err(e) =
                super::broker::run_privileged(&db, &stop, None, Duration::from_secs(60)).await
            {
                tracing::warn!("tentanas: scrub of '{pool}' not stopped after cancel: {e}");
            }
        });
    }
}

/// Starts a scrub and follows it to its end. The kernel owns the work, so the
/// job's role is progress and a place to cancel from.
pub async fn scrub_job(
    h: super::jobs::JobHandle,
    pool: String,
    explicit: Option<Arc<ElevationToken>>,
) -> anyhow::Result<()> {
    let start = HelperCommand::ZpoolScrub {
        pool: pool.clone(),
        action: tentanas_helper::ScrubAction::Start,
    };
    super::jobs::run_step(&h, &start, explicit.as_deref(), Duration::from_secs(60)).await?;
    drop(explicit);
    let mut guard = StopScrubOnCancel {
        db: h.db().clone(),
        pool: pool.clone(),
        cancel: h.cancel_token(),
        armed: true,
    };
    let outcome = follow_scan(&h, &pool, "scrub").await;
    guard.armed = false;
    outcome
}

/// Replaces a disk and follows the resilver the replacement starts.
pub async fn replace_job(
    h: super::jobs::JobHandle,
    pool: String,
    command: HelperCommand,
    explicit: Option<Arc<ElevationToken>>,
) -> anyhow::Result<()> {
    super::jobs::run_step(&h, &command, explicit.as_deref(), MUTATION_TIMEOUT).await?;
    drop(explicit);
    follow_scan(&h, &pool, "resilver").await
}

async fn follow_scan(h: &super::jobs::JobHandle, pool: &str, kind: &str) -> anyhow::Result<()> {
    loop {
        tokio::time::sleep(SCAN_POLL).await;
        if h.cancelled() {
            return Err(anyhow::anyhow!("cancelled"));
        }
        let report = match status(pool).await {
            Ok(r) => r,
            Err(e) => {
                h.log(format!("status poll failed: {e}"));
                continue;
            }
        };
        match report.scan.status.as_str() {
            "running" => {
                if let Some(pct) = report.scan.progress_pct {
                    h.progress(pct);
                }
            }
            "paused" => h.log(format!("{kind} paused")),
            "canceled" => return Err(anyhow::anyhow!("cancelled")),
            _ => {
                h.log(format!(
                    "{} finished with {} errors",
                    report.scan.kind, report.scan.errors
                ));
                h.progress(100);
                return if report.scan.errors > 0 {
                    Err(anyhow::anyhow!(
                        "{} found {} errors",
                        report.scan.kind,
                        report.scan.errors
                    ))
                } else {
                    Ok(())
                };
            }
        }
    }
}

/// The scheduler's unattended scrub: the node's own channel, no password.
pub fn spawn_scheduled_scrub(db: &DbPool, pool: &str) -> anyhow::Result<NasJob> {
    let name = pool.to_string();
    super::jobs::spawn(db, "pool_scrub", pool, super::scheduler::STARTED_BY, move |h| {
        scrub_job(h, name, None)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEALTHY_MIRROR: &str = "  pool: backup\n\
 state: ONLINE\n\
  scan: scrub repaired 0B in 00:12:34 with 0 errors on Tue Sep  1 03:12:34 2026\n\
config:\n\
\n\
\tNAME                                    STATE     READ WRITE CKSUM\n\
\tbackup                                  ONLINE       0     0     0\n\
\t  mirror-0                              ONLINE       0     0     0\n\
\t    /dev/disk/by-id/ata-DISK0           ONLINE       0     0     0\n\
\t    /dev/disk/by-id/ata-DISK1           ONLINE       0     0     0\n\
\n\
errors: No known data errors\n";

    const DEGRADED_RAIDZ2: &str = "  pool: tank\n\
 state: DEGRADED\n\
status: One or more devices is currently being resilvered.\n\
action: Wait for the resilver to complete.\n\
  scan: resilver in progress since Tue Sep  1 09:00:00 2026\n\
\t1234567890 / 4398046511104 scanned at 1073741824/s, 987654321 / 4398046511104 issued at 786432000/s\n\
\t0B repaired, 22.50% done, 01:12:33 to go\n\
config:\n\
\n\
\tNAME                STATE     READ WRITE CKSUM\n\
\ttank                DEGRADED     0     0     0\n\
\t  raidz2-0          DEGRADED     0     0     0\n\
\t    /dev/sda        ONLINE       0     0     0\n\
\t    /dev/sdb        ONLINE       0     0     2\n\
\t    replacing-2     DEGRADED     0     0     0\n\
\t      /dev/sdc      OFFLINE      0     0     0\n\
\t      /dev/sdk      ONLINE       0     0     0  (resilvering)\n\
\t    /dev/sdd        ONLINE       0     0     0\n\
\t    /dev/sde        FAULTED      3    17     0  (too many errors)\n\
\t    /dev/sdf        ONLINE       0     0     0\n\
\n\
errors: 12 data errors, use '-v' for a list\n";

    const FULL_TOPOLOGY: &str = "  pool: media\n\
 state: ONLINE\n\
  scan: none requested\n\
config:\n\
\n\
\tNAME              STATE     READ WRITE CKSUM\n\
\tmedia             ONLINE       0     0     0\n\
\t  raidz1-0        ONLINE       0     0     0\n\
\t    /dev/sda      ONLINE       0     0     0\n\
\t    /dev/sdb      ONLINE       0     0     0\n\
\t    /dev/sdc      ONLINE       0     0     0\n\
\tspecial\n\
\t  mirror-1        ONLINE       0     0     0\n\
\t    /dev/nvme3n1  ONLINE       0     0     0\n\
\t    /dev/nvme4n1  ONLINE       0     0     0\n\
\tlogs\n\
\t  /dev/nvme1n1p2  ONLINE       0     0     0\n\
\tcache\n\
\t  /dev/nvme2n1    ONLINE       0     0     0\n\
\tspares\n\
\t  /dev/sdk        AVAIL\n\
\n\
errors: No known data errors\n";

    const CANCELED_SCRUB: &str = "  pool: fast\n\
 state: ONLINE\n\
  scan: scrub canceled on Mon Aug 31 22:15:00 2026\n\
config:\n\
\n\
\tNAME            STATE     READ WRITE CKSUM\n\
\tfast            ONLINE       0     0     0\n\
\t  /dev/nvme0n1  ONLINE       0     0     0\n\
\n\
errors: No known data errors\n";

    #[test]
    fn zpool_list_rows_parse_every_column() {
        let text = "tank\t12345678901234567890\t43980465111040\t23980465111040\t20000000000000\t54\t11\t1.00\tONLINE\t12\ton\toff\n\
                    fast\t99\t2000398934016\t900000000000\t1100398934016\t45\t3\t1.02\tDEGRADED\t13\toff\ton\n";
        let rows = parse_list(text);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].name, "tank");
        assert_eq!(rows[0].guid, "12345678901234567890");
        assert_eq!(rows[0].size_bytes, 43_980_465_111_040);
        assert_eq!(rows[0].capacity_pct, 54);
        assert_eq!(rows[0].fragmentation_pct, 11);
        assert_eq!(rows[0].state, "online");
        assert_eq!(rows[0].ashift, 12);
        assert!(rows[0].autotrim);
        assert!(!rows[0].read_only);
        assert_eq!(rows[1].state, "degraded");
        assert!(rows[1].read_only);
        assert!((rows[1].dedup_ratio - 1.02).abs() < 1e-9);
    }

    #[test]
    fn healthy_mirror_status_yields_one_vdev_and_a_finished_scrub() {
        let s = parse_status(HEALTHY_MIRROR);
        assert_eq!(s.state, "online");
        assert_eq!(s.vdevs.len(), 1);
        assert_eq!(s.vdevs[0].id, "mirror-0");
        assert_eq!(s.vdevs[0].kind, "mirror");
        assert_eq!(s.vdevs[0].role, "data");
        assert_eq!(s.vdevs[0].fault_tolerance, 1);
        assert_eq!(s.vdevs[0].disks.len(), 2);
        assert_eq!(s.vdevs[0].disks[0].name, "ata-DISK0");
        assert_eq!(s.vdevs[0].disks[0].path, "/dev/disk/by-id/ata-DISK0");
        assert_eq!(s.scan.kind, "scrub");
        assert_eq!(s.scan.status, "finished");
        assert_eq!(s.scan.duration_secs, Some(754));
        assert_eq!(s.scan.errors, 0);
        assert!(s.scan.finished_at.is_some());
        assert_eq!(s.data_errors, 0);
        assert_eq!(layout_summary(&s.vdevs), "mirror");
    }

    #[test]
    fn degraded_raidz2_reports_the_resilver_and_the_replacing_container() {
        let s = parse_status(DEGRADED_RAIDZ2);
        assert_eq!(s.state, "degraded");
        assert_eq!(s.vdevs.len(), 1);
        let vdev = &s.vdevs[0];
        assert_eq!(vdev.kind, "raidz2");
        assert_eq!(vdev.fault_tolerance, 2);
        // The `replacing-2` container is not a disk; both its children are.
        assert_eq!(vdev.disks.len(), 7);
        let names: Vec<&str> = vdev.disks.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, ["sda", "sdb", "sdc", "sdk", "sdd", "sde", "sdf"]);
        assert_eq!(vdev.disks[1].cksum_errors, 2);
        assert_eq!(vdev.disks[2].state, "offline");
        assert_eq!(vdev.disks[3].note, "resilvering");
        assert_eq!(vdev.disks[5].state, "faulted");
        assert_eq!(vdev.disks[5].read_errors, 3);
        assert_eq!(vdev.disks[5].write_errors, 17);
        assert_eq!(vdev.disks[5].note, "too many errors");
        assert_eq!(s.scan.kind, "resilver");
        assert_eq!(s.scan.status, "running");
        assert_eq!(s.scan.progress_pct, Some(23));
        assert_eq!(s.scan.eta_secs, Some(4353));
        assert_eq!(s.scan.scanned_bytes, 1_234_567_890);
        assert!(s.scan.started_at.is_some());
        assert_eq!(s.data_errors, 12);
    }

    #[test]
    fn special_log_cache_and_spare_sections_get_their_roles() {
        let s = parse_status(FULL_TOPOLOGY);
        let roles: Vec<(&str, &str)> = s
            .vdevs
            .iter()
            .map(|v| (v.id.as_str(), v.role.as_str()))
            .collect();
        assert_eq!(
            roles,
            [
                ("raidz1-0", "data"),
                ("mirror-1", "special"),
                ("/dev/nvme1n1p2", "log"),
                ("/dev/nvme2n1", "cache"),
                ("/dev/sdk", "spare"),
            ]
        );
        assert_eq!(s.vdevs[2].kind, "disk");
        assert_eq!(s.vdevs[2].disks[0].name, "nvme1n1");
        assert_eq!(s.vdevs[4].disks[0].state, "online");
        assert_eq!(s.scan.status, "none");
        assert_eq!(s.scan.kind, "none");
        let (data_disks, tolerance) = data_disks_and_tolerance(&s.vdevs);
        assert_eq!(data_disks, 3);
        assert_eq!(tolerance, 1);
        assert_eq!(layout_summary(&s.vdevs), "raidz1");
    }

    #[test]
    fn a_canceled_scrub_is_not_a_finished_one() {
        let s = parse_status(CANCELED_SCRUB);
        assert_eq!(s.scan.kind, "scrub");
        assert_eq!(s.scan.status, "canceled");
        assert_eq!(s.scan.progress_pct, None);
        assert!(s.scan.finished_at.is_some());
        assert_eq!(s.vdevs.len(), 1);
        assert_eq!(s.vdevs[0].kind, "disk");
        assert_eq!(layout_summary(&s.vdevs), "stripe");
    }

    #[test]
    fn iostat_latencies_convert_from_nanoseconds() {
        let text = "tank\t23980465111040\t20000000000000\t1420\t420\t335544320\t146800640\t2800000\t6100000\t-\t-\t-\t-\t-\t-\t-\t-\n";
        let io = parse_iostat(text);
        let tank = &io["tank"];
        assert_eq!(tank.read_bps, 335_544_320);
        assert_eq!(tank.write_bps, 146_800_640);
        assert!((tank.read_iops - 1420.0).abs() < 1e-9);
        assert!((tank.read_latency_ms - 2.8).abs() < 1e-9);
        assert!((tank.write_latency_ms - 6.1).abs() < 1e-9);
    }

    #[test]
    fn import_scan_reads_guid_state_and_cleanliness() {
        let text = "   pool: old-backup\n\
                        id: 12345678901234567890\n\
                     state: ONLINE\n\
                    action: The pool can be imported using its name or numeric identifier.\n\
                    config:\n\
                    \n\
                    \told-backup      ONLINE\n\
                    \t  mirror-0      ONLINE\n\
                    \t    sdo         ONLINE\n\
                    \t    sdp         ONLINE\n\
                    \n\
                       pool: foreign\n\
                         id: 42\n\
                      state: ONLINE\n\
                     status: The pool was last accessed by another system.\n\
                     config:\n\
                     \n\
                     \tforeign         ONLINE\n\
                     \t  sdq           ONLINE\n";
        let pools = parse_import_scan(text);
        assert_eq!(pools.len(), 2);
        assert_eq!(pools[0].name, "old-backup");
        assert_eq!(pools[0].guid, "12345678901234567890");
        assert_eq!(pools[0].state, "online");
        assert_eq!(pools[0].layout, "mirror");
        assert_eq!(pools[0].disks, ["sdo", "sdp"]);
        assert!(pools[0].exported_cleanly);
        assert_eq!(pools[1].name, "foreign");
        assert!(!pools[1].exported_cleanly);
        assert!(pools[1].message.contains("another system"));
        assert_eq!(pools[1].disks, ["sdq"]);
    }

    fn disk(name: &str, size: u64, rotational: bool, health: &str) -> NasDisk {
        NasDisk {
            disk_id: format!("sn-{name}"),
            name: name.to_string(),
            path: format!("/dev/{name}"),
            size_bytes: size,
            rotational,
            health: health.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn layout_plan_matches_the_wizard_rules() {
        const TB8: u64 = 8_001_563_222_016;
        let two = vec![disk("sdl", TB8, true, "ok"), disk("sdm", TB8, true, "ok")];
        let (options, warnings, smallest) = plan(&two);
        assert_eq!(smallest, TB8);
        assert!(warnings.is_empty(), "{warnings:?}");
        let by = |l: &str| options.iter().find(|o| o.layout == l).unwrap().clone();
        assert!(by("mirror").recommended);
        assert_eq!(by("mirror").usable_bytes, TB8);
        assert_eq!(by("mirror").fault_tolerance, 1);
        assert_eq!(by("stripe").usable_bytes, 2 * TB8);
        assert!(!by("raidz2").available);
        assert_eq!(by("raidz2").reason, "too_few_disks");
        assert_eq!(by("raidz2").usable_bytes, 0);

        let six: Vec<NasDisk> = (0..6)
            .map(|i| disk(&format!("sd{i}"), TB8, true, "ok"))
            .collect();
        let (options, _, _) = plan(&six);
        assert!(options.iter().find(|o| o.layout == "raidz2").unwrap().recommended);
        assert_eq!(
            options.iter().find(|o| o.layout == "raidz2").unwrap().usable_bytes,
            4 * TB8
        );
        assert_eq!(
            options.iter().find(|o| o.layout == "raidz1").unwrap().usable_bytes,
            5 * TB8
        );
        assert_eq!(
            options.iter().find(|o| o.layout == "mirror").unwrap().usable_bytes,
            3 * TB8
        );

        let four: Vec<NasDisk> = vec![
            disk("sda", TB8, true, "ok"),
            disk("sdb", TB8, true, "warning"),
            disk("sdc", 4_000_787_030_016, true, "ok"),
            disk("nvme0n1", TB8, false, "ok"),
        ];
        let (options, warnings, smallest) = plan(&four);
        assert_eq!(smallest, 4_000_787_030_016);
        assert!(options.iter().find(|o| o.layout == "raidz1").unwrap().recommended);
        assert!(warnings.iter().any(|w| w.contains("mixed disk sizes")));
        assert!(warnings.iter().any(|w| w.contains("mixed SSD and HDD")));
        assert!(warnings.iter().any(|w| w.contains("SMART warnings on sdb")));
    }

    #[test]
    fn mirrors_become_pairs_and_bad_layouts_are_refused() {
        let devices: Vec<String> = (0..4).map(|i| format!("/dev/disk/by-id/ata-D{i}")).collect();
        let groups =
            vdev_groups(tentanas_helper::VdevRole::Data, "mirror", &devices).expect("groups");
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].devices.len(), 2);
        assert_eq!(groups[1].devices[0], "/dev/disk/by-id/ata-D2");
        let raidz = vdev_groups(tentanas_helper::VdevRole::Data, "raidz2", &devices).expect("raidz");
        assert_eq!(raidz.len(), 1);
        assert_eq!(raidz[0].devices.len(), 4);
        assert!(vdev_groups(tentanas_helper::VdevRole::Data, "raidz3", &devices).is_err());
        assert!(vdev_groups(tentanas_helper::VdevRole::Data, "anyraid", &devices).is_err());
    }

    #[test]
    fn health_folds_state_errors_and_capacity_into_one_status() {
        let mut pool = NasPool {
            state: "online".to_string(),
            capacity_pct: 54,
            ..Default::default()
        };
        assert_eq!(score_health(&pool, "No known data errors", 0).0, "ok");

        pool.capacity_pct = 84;
        let (health, reason) = score_health(&pool, "No known data errors", 0);
        assert_eq!(health, "warning");
        assert!(reason.contains("84% full"));

        pool.capacity_pct = 92;
        assert_eq!(score_health(&pool, "No known data errors", 0).0, "critical");

        pool.capacity_pct = 10;
        pool.state = "degraded".to_string();
        pool.vdevs = vec![NasVdev {
            kind: "raidz2".to_string(),
            disks: vec![
                NasVdevDisk {
                    state: "online".to_string(),
                    cksum_errors: 4,
                    ..Default::default()
                },
                NasVdevDisk {
                    state: "faulted".to_string(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        }];
        let (health, reason) = score_health(&pool, "No known data errors", 0);
        assert_eq!(health, "critical");
        assert!(reason.contains("1 unusable disks"), "{reason}");

        pool.vdevs[0].disks[1].state = "online".to_string();
        let (health, reason) = score_health(&pool, "No known data errors", 0);
        assert_eq!(health, "warning");
        assert!(reason.contains("pool is degraded"));
        assert!(reason.contains("checksum errors"));

        pool.state = "online".to_string();
        pool.vdevs.clear();
        let (health, reason) = score_health(&pool, "12 data errors, use '-v' for a list", 12);
        assert_eq!(health, "critical");
        assert!(reason.contains("12 permanent data errors"));
    }
}
