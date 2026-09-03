// =============================================================================
// File: tentanas/disks.rs — disk inventory, live I/O, SMART/NVMe health and
//       the sampler (plan-02 §5.4, tab "Dyski"). Sources, all JSON or /proc:
//
//       lsblk -J -b        identity, size, transport, partitions, mounts
//       /proc/diskstats    I/O counters every tick → rates, latency, util
//       smartctl --json=c  health, temperature, attributes, self-test log
//                          (privileged: goes through the broker)
//
//       The live picture lives in memory (one sampler per node); minute
//       samples and the raw SMART document go to tentanas.db so the detail
//       view can show 24 h / 7 d history and the attribute trend.
// =============================================================================

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use parking_lot::RwLock;
use serde_json::Value;
use tentaflow_protocol::tentanas::{
    NasDisk, NasDiskIo, NasSmartAttribute, NasSmartSelfTest, NasTelemetryState,
};
use tentanas_helper::HelperCommand;

use super::broker;
use super::db::{self as store, DiskIdentity, SampleInsert};
use crate::db::DbPool;

const TICK: Duration = Duration::from_secs(5);
const INVENTORY_EVERY: Duration = Duration::from_secs(30);
const SAMPLE_EVERY: Duration = Duration::from_secs(60);
const SMART_EVERY: Duration = Duration::from_secs(30 * 60);
const PRUNE_EVERY: Duration = Duration::from_secs(6 * 60 * 60);
const SUMMARY_EVERY: Duration = Duration::from_secs(60);
/// Points of the per-row sparkline (one per tick → five minutes).
const HISTORY_POINTS: usize = 60;
/// Ticks the IOPS baseline of the Overview tile averages over — one hour.
/// It is a live indicator, not history, so it stays in the sampler's memory:
/// the minute samples in `tentanas.db` carry throughput and latency, never
/// per-direction operation rates.
const IOPS_BASELINE_POINTS: usize = 3600 / TICK.as_secs() as usize;

// ----- lsblk -------------------------------------------------------------------

const LSBLK_COLUMNS: &str =
    "NAME,PATH,TYPE,MODEL,SERIAL,WWN,SIZE,TRAN,ROTA,RM,REV,VENDOR,MOUNTPOINTS,FSTYPE,LABEL";

/// Device name prefixes that are not physical disks (virtual, optical,
/// arrays and volumes built ON disks — those belong to the pool views).
const SKIP_PREFIXES: &[&str] = &["loop", "ram", "zram", "zd", "dm-", "sr", "fd", "md", "nbd", "drbd"];

/// Older util-linux emits every JSON value as a string; newer ones type them.
fn json_str(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => {
            let t = s.trim();
            (!t.is_empty()).then(|| t.to_string())
        }
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn json_u64(v: &Value) -> u64 {
    match v {
        Value::Number(n) => n.as_u64().unwrap_or(0),
        Value::String(s) => s.trim().parse().unwrap_or(0),
        _ => 0,
    }
}

fn json_bool(v: &Value) -> bool {
    match v {
        Value::Bool(b) => *b,
        Value::String(s) => matches!(s.trim(), "1" | "true"),
        Value::Number(n) => n.as_i64().unwrap_or(0) != 0,
        _ => false,
    }
}

fn sanitize_id(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':') { c } else { '_' })
        .collect()
}

/// Stable identity across reboots and name changes: WWN, then serial, then
/// (virtual disks without either) the kernel name.
fn disk_id(name: &str, serial: Option<&str>, wwn: Option<&str>) -> String {
    if let Some(w) = wwn {
        return format!("wwn-{}", sanitize_id(w.trim_start_matches("0x")));
    }
    if let Some(s) = serial {
        return format!("sn-{}", sanitize_id(s));
    }
    format!("dev-{}", sanitize_id(name))
}

#[derive(Debug, Default)]
struct Usage {
    mountpoints: Vec<String>,
    zfs_pool: Option<String>,
    raid_member: Option<String>,
    has_children_or_fs: bool,
}

fn collect_usage(node: &Value, usage: &mut Usage) {
    if let Some(mps) = node.get("mountpoints").and_then(Value::as_array) {
        usage
            .mountpoints
            .extend(mps.iter().filter_map(json_str));
    }
    let fstype = node.get("fstype").and_then(json_str);
    let label = node.get("label").and_then(json_str);
    match fstype.as_deref() {
        Some("zfs_member") => usage.zfs_pool = usage.zfs_pool.take().or(label),
        Some("linux_raid_member") => usage.raid_member = usage.raid_member.take().or(label),
        Some(_) => usage.has_children_or_fs = true,
        None => {}
    }
    if let Some(children) = node.get("children").and_then(Value::as_array) {
        usage.has_children_or_fs = true;
        for c in children {
            collect_usage(c, usage);
        }
    }
}

fn role_of(usage: &Usage) -> (&'static str, Option<String>) {
    if usage
        .mountpoints
        .iter()
        .any(|m| matches!(m.as_str(), "/" | "/boot" | "/boot/efi" | "[SWAP]"))
    {
        return ("system", None);
    }
    if let Some(pool) = &usage.zfs_pool {
        return ("pool_member", Some(pool.clone()));
    }
    if let Some(array) = &usage.raid_member {
        return ("array_member", Some(array.clone()));
    }
    if !usage.mountpoints.is_empty() {
        return ("mounted", None);
    }
    if usage.has_children_or_fs {
        return ("used", None);
    }
    ("free", None)
}

/// Builds the identity part of a `NasDisk` from one lsblk block device.
pub fn disk_from_lsblk(node: &Value) -> Option<NasDisk> {
    let name = node.get("name").and_then(json_str)?;
    if node.get("type").and_then(json_str).as_deref() != Some("disk") {
        return None;
    }
    if SKIP_PREFIXES.iter().any(|p| name.starts_with(p)) {
        return None;
    }
    let serial = node.get("serial").and_then(json_str);
    let wwn = node.get("wwn").and_then(json_str);
    let rotational = json_bool(node.get("rota").unwrap_or(&Value::Null));
    let transport = node
        .get("tran")
        .and_then(json_str)
        .unwrap_or_else(|| if name.starts_with("nvme") { "nvme".into() } else { "unknown".into() });
    let kind = if name.starts_with("nvme") || transport == "nvme" {
        "nvme"
    } else if rotational {
        "hdd"
    } else {
        "ssd"
    };
    let mut usage = Usage::default();
    collect_usage(node, &mut usage);
    let (role, member_of) = role_of(&usage);
    let vendor = node.get("vendor").and_then(json_str);
    let model = node.get("model").and_then(json_str).unwrap_or_default();
    let model = match vendor {
        // lsblk reports ATA disks with vendor "ATA" — the model already
        // names the maker there.
        Some(v) if !model.starts_with(&v) && v != "ATA" => format!("{v} {model}"),
        _ => model,
    };
    Some(NasDisk {
        disk_id: disk_id(&name, serial.as_deref(), wwn.as_deref()),
        path: node
            .get("path")
            .and_then(json_str)
            .unwrap_or_else(|| format!("/dev/{name}")),
        name,
        kind: kind.to_string(),
        model,
        serial: serial.unwrap_or_default(),
        wwn,
        size_bytes: json_u64(node.get("size").unwrap_or(&Value::Null)),
        transport,
        rotational,
        removable: json_bool(node.get("rm").unwrap_or(&Value::Null)),
        firmware: node.get("rev").and_then(json_str),
        role: role.to_string(),
        member_of,
        health: "unknown".to_string(),
        health_reason: String::new(),
        temperature_c: None,
        power_on_hours: None,
        reallocated_sectors: None,
        pending_sectors: None,
        crc_errors: None,
        media_errors: None,
        wear_pct: None,
        smart_available: false,
        smart_passed: None,
        smart_read_at: None,
        io: NasDiskIo::default(),
        io_history_bps: Vec::new(),
        mountpoints: usage.mountpoints,
        // lsblk knows the pool from the member label, never the vdev inside
        // it; `refresh_inventory` fills these from `zpool status`.
        vdev_role: String::new(),
        vdev_kind: String::new(),
    })
}

/// Kernel name → (vdev role, vdev kind) for every leaf of every imported
/// pool. The Disks tab names the group a disk serves ("tank · RAIDZ2"), which
/// only `zpool status` knows; reading it here — once per inventory refresh —
/// keeps it off the tab's five-second poll, where it used to cost a full pool
/// listing (datasets and snapshots included) per tick.
async fn vdev_membership() -> HashMap<String, (String, String)> {
    if !super::zfs::available() {
        return HashMap::new();
    }
    let rows = match super::pools::list_rows().await {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!("tentanas: pool list for disk roles failed: {e}");
            return HashMap::new();
        }
    };
    let mut index = HashMap::new();
    for row in rows {
        let status = match super::pools::status(&row.name).await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("tentanas: pool status for disk roles failed ({}): {e}", row.name);
                continue;
            }
        };
        for vdev in status.vdevs {
            for leaf in vdev.disks {
                index.insert(leaf.name, (vdev.role.clone(), vdev.kind.clone()));
            }
        }
    }
    index
}

pub fn disks_from_lsblk_json(text: &str) -> Result<Vec<NasDisk>> {
    let doc: Value = serde_json::from_str(text)?;
    let devices = doc
        .get("blockdevices")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("lsblk: no blockdevices array"))?;
    Ok(devices.iter().filter_map(disk_from_lsblk).collect())
}

async fn inventory() -> Result<Vec<NasDisk>> {
    if !cfg!(target_os = "linux") {
        return Ok(Vec::new());
    }
    let out = broker::run_unprivileged(
        "lsblk",
        &["-J", "-b", "-o", LSBLK_COLUMNS],
        Duration::from_secs(15),
    )
    .await?;
    if !out.success() {
        return Err(anyhow!("lsblk exited with {}: {}", out.code, out.stderr.trim()));
    }
    disks_from_lsblk_json(&out.stdout)
}

// ----- /proc/diskstats ------------------------------------------------------------

#[derive(Debug, Clone, Copy, Default)]
pub struct DiskStats {
    reads: u64,
    sectors_read: u64,
    ms_reading: u64,
    writes: u64,
    sectors_written: u64,
    ms_writing: u64,
    ms_io: u64,
}

pub fn parse_diskstats(text: &str) -> HashMap<String, DiskStats> {
    let mut map = HashMap::new();
    for line in text.lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() < 14 {
            continue;
        }
        let n = |i: usize| f[i].parse::<u64>().unwrap_or(0);
        map.insert(
            f[2].to_string(),
            DiskStats {
                reads: n(3),
                sectors_read: n(5),
                ms_reading: n(6),
                writes: n(7),
                sectors_written: n(9),
                ms_writing: n(10),
                ms_io: n(12),
            },
        );
    }
    map
}

/// Rates between two readings `elapsed` apart. Sector size of diskstats is
/// always 512 bytes regardless of the device's block size.
pub fn io_rates(prev: &DiskStats, cur: &DiskStats, elapsed: Duration) -> NasDiskIo {
    let secs = elapsed.as_secs_f64().max(0.001);
    let d = |a: u64, b: u64| b.saturating_sub(a);
    let reads = d(prev.reads, cur.reads);
    let writes = d(prev.writes, cur.writes);
    let ops = reads + writes;
    let await_ms = if ops == 0 {
        0.0
    } else {
        (d(prev.ms_reading, cur.ms_reading) + d(prev.ms_writing, cur.ms_writing)) as f64 / ops as f64
    };
    NasDiskIo {
        read_bps: (d(prev.sectors_read, cur.sectors_read) as f64 * 512.0 / secs) as u64,
        write_bps: (d(prev.sectors_written, cur.sectors_written) as f64 * 512.0 / secs) as u64,
        read_iops: reads as f64 / secs,
        write_iops: writes as f64 / secs,
        await_ms: (await_ms * 100.0).round() / 100.0,
        util_pct: ((d(prev.ms_io, cur.ms_io) as f64 / (secs * 1000.0)) * 100.0).min(100.0),
    }
}

// ----- smartctl JSON ---------------------------------------------------------------

/// The fields of a `smartctl --json=c -x` document the app uses.
#[derive(Debug, Default, Clone)]
pub struct SmartSummary {
    pub passed: Option<bool>,
    pub temperature_c: Option<i32>,
    pub power_on_hours: Option<u64>,
    pub reallocated: Option<u64>,
    pub pending: Option<u64>,
    pub crc_errors: Option<u64>,
    pub media_errors: Option<u64>,
    pub wear_pct: Option<u8>,
    pub firmware: Option<String>,
    pub self_test_running_pct: Option<u8>,
    pub self_test_failed: bool,
}

fn attr_raw(doc: &Value, id: u64) -> Option<u64> {
    doc.get("ata_smart_attributes")?
        .get("table")?
        .as_array()?
        .iter()
        .find(|a| a.get("id").and_then(Value::as_u64) == Some(id))?
        .get("raw")?
        .get("value")?
        .as_u64()
}

fn attr_value(doc: &Value, id: u64) -> Option<u64> {
    doc.get("ata_smart_attributes")?
        .get("table")?
        .as_array()?
        .iter()
        .find(|a| a.get("id").and_then(Value::as_u64) == Some(id))?
        .get("value")?
        .as_u64()
}

pub fn summarize_smart(doc: &Value) -> SmartSummary {
    let nvme = doc.get("nvme_smart_health_information_log");
    let mut s = SmartSummary {
        passed: doc.pointer("/smart_status/passed").and_then(Value::as_bool),
        temperature_c: doc
            .pointer("/temperature/current")
            .and_then(Value::as_i64)
            .map(|t| t as i32),
        power_on_hours: doc.pointer("/power_on_time/hours").and_then(Value::as_u64),
        firmware: doc.get("firmware_version").and_then(Value::as_str).map(str::to_string),
        ..Default::default()
    };
    if let Some(n) = nvme {
        s.media_errors = n.get("media_errors").and_then(Value::as_u64);
        s.wear_pct = n
            .get("percentage_used")
            .and_then(Value::as_u64)
            .map(|p| p.min(100) as u8);
        if s.power_on_hours.is_none() {
            s.power_on_hours = n.get("power_on_hours").and_then(Value::as_u64);
        }
        if n.get("critical_warning").and_then(Value::as_u64).unwrap_or(0) != 0 {
            s.passed = Some(false);
        }
    } else {
        s.reallocated = attr_raw(doc, 5);
        s.pending = attr_raw(doc, 197);
        s.crc_errors = attr_raw(doc, 199);
        // 187 Reported_Uncorrect / 198 Offline_Uncorrectable: media errors of
        // an ATA disk; whichever the vendor exposes.
        s.media_errors = attr_raw(doc, 187).or_else(|| attr_raw(doc, 198));
        // SSD wear: the normalized value counts DOWN from 100 (177
        // Wear_Leveling_Count, 231 SSD_Life_Left, 233 Media_Wearout_Indicator).
        s.wear_pct = [177, 231, 233]
            .into_iter()
            .find_map(|id| attr_value(doc, id))
            .map(|v| (100 - v.min(100)) as u8);
        // Attribute 190/194 when the top-level temperature block is absent.
        if s.temperature_c.is_none() {
            s.temperature_c = attr_raw(doc, 194)
                .or_else(|| attr_raw(doc, 190))
                .map(|t| (t & 0xff) as i32);
        }
    }
    let st = doc.pointer("/ata_smart_data/self_test/status");
    if let Some(st) = st {
        if let Some(rem) = st.get("remaining_percent").and_then(Value::as_u64) {
            s.self_test_running_pct = Some((100 - rem.min(100)) as u8);
        }
        if st.get("passed").and_then(Value::as_bool) == Some(false)
            && st.get("remaining_percent").is_none()
        {
            s.self_test_failed = true;
        }
    }
    s
}

pub fn smart_attributes(doc: &Value) -> Vec<NasSmartAttribute> {
    let Some(table) = doc.pointer("/ata_smart_attributes/table").and_then(Value::as_array) else {
        return nvme_pseudo_attributes(doc);
    };
    table
        .iter()
        .filter_map(|a| {
            let value = a.get("value").and_then(Value::as_i64)?;
            let threshold = a.get("thresh").and_then(Value::as_i64).unwrap_or(0);
            let failing_now = a
                .pointer("/when_failed")
                .and_then(Value::as_str)
                .is_some_and(|w| !w.is_empty());
            let status = if failing_now || (threshold > 0 && value <= threshold) {
                "failing"
            } else if threshold > 0 && value <= threshold + 10 {
                "warning"
            } else {
                "ok"
            };
            Some(NasSmartAttribute {
                id: a.get("id").and_then(Value::as_u64).unwrap_or(0) as u32,
                name: a.get("name").and_then(Value::as_str).unwrap_or("").to_string(),
                value,
                worst: a.get("worst").and_then(Value::as_i64).unwrap_or(value),
                threshold,
                raw: a.pointer("/raw/value").and_then(Value::as_i64).unwrap_or(0),
                raw_text: a
                    .pointer("/raw/string")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                status: status.to_string(),
                raw_week_ago: None,
            })
        })
        .collect()
}

/// NVMe has no attribute table; the health log fields are shown in the same
/// table so the detail view has one shape for both.
fn nvme_pseudo_attributes(doc: &Value) -> Vec<NasSmartAttribute> {
    let Some(log) = doc.get("nvme_smart_health_information_log") else {
        return Vec::new();
    };
    const FIELDS: &[(&str, &str)] = &[
        ("critical_warning", "Critical Warning"),
        ("temperature", "Temperature"),
        ("available_spare", "Available Spare"),
        ("available_spare_threshold", "Available Spare Threshold"),
        ("percentage_used", "Percentage Used"),
        ("data_units_read", "Data Units Read"),
        ("data_units_written", "Data Units Written"),
        ("power_cycles", "Power Cycles"),
        ("power_on_hours", "Power On Hours"),
        ("unsafe_shutdowns", "Unsafe Shutdowns"),
        ("media_errors", "Media and Data Integrity Errors"),
        ("num_err_log_entries", "Error Information Log Entries"),
    ];
    let spare_threshold = log
        .get("available_spare_threshold")
        .and_then(Value::as_i64)
        .unwrap_or(10);
    FIELDS
        .iter()
        .enumerate()
        .filter_map(|(i, (key, name))| {
            let raw = log.get(*key).and_then(Value::as_i64)?;
            let status = match *key {
                "critical_warning" | "media_errors" if raw > 0 => "failing",
                "available_spare" if raw <= spare_threshold => "failing",
                "percentage_used" if raw >= 90 => "warning",
                "unsafe_shutdowns" | "num_err_log_entries" if raw > 0 => "warning",
                _ => "ok",
            };
            Some(NasSmartAttribute {
                id: i as u32,
                name: name.to_string(),
                value: raw,
                worst: raw,
                threshold: 0,
                raw,
                raw_text: raw.to_string(),
                status: status.to_string(),
                raw_week_ago: None,
            })
        })
        .collect()
}

pub fn smart_self_tests(doc: &Value) -> Vec<NasSmartSelfTest> {
    let mut out = Vec::new();
    if let Some(rows) = doc
        .pointer("/ata_smart_self_test_log/standard/table")
        .and_then(Value::as_array)
    {
        for r in rows {
            let status_str = r.pointer("/status/string").and_then(Value::as_str).unwrap_or("");
            let passed = r.pointer("/status/passed").and_then(Value::as_bool);
            out.push(NasSmartSelfTest {
                kind: r.pointer("/type/string").and_then(Value::as_str).unwrap_or("").to_string(),
                status: match passed {
                    Some(true) => "passed",
                    Some(false) => "failed",
                    None if status_str.to_ascii_lowercase().contains("progress") => "running",
                    None => "unknown",
                }
                .to_string(),
                lifetime_hours: r.get("lifetime_hours").and_then(Value::as_u64).unwrap_or(0),
                started_at: None,
                detail: status_str.to_string(),
            });
        }
    }
    if let Some(rows) = doc.pointer("/nvme_self_test_log/table").and_then(Value::as_array) {
        for r in rows {
            let result = r.pointer("/self_test_result/value").and_then(Value::as_u64);
            out.push(NasSmartSelfTest {
                kind: r
                    .pointer("/self_test_code/string")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                status: match result {
                    Some(0) => "passed",
                    Some(1) => "aborted",
                    Some(_) => "failed",
                    None => "unknown",
                }
                .to_string(),
                lifetime_hours: r.get("power_on_hours").and_then(Value::as_u64).unwrap_or(0),
                started_at: None,
                detail: r
                    .pointer("/self_test_result/string")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
            });
        }
    }
    out
}

// ----- health ----------------------------------------------------------------------

/// Thresholds (§5.4): `critical` means "replace / act now", `warning` means
/// "watch and plan"; a growing reallocated count is the classic early sign
/// so the trend against the week-old sample counts, not only the level.
pub fn score_health(s: &SmartSummary, kind: &str, reallocated_week_ago: Option<i64>) -> (&'static str, String) {
    let mut critical = Vec::new();
    let mut warning = Vec::new();
    if s.passed == Some(false) {
        critical.push("SMART overall status FAILED".to_string());
    }
    if let Some(p) = s.pending.filter(|v| *v > 0) {
        critical.push(format!("{p} pending sectors"));
    }
    if let Some(m) = s.media_errors.filter(|v| *v > 0) {
        critical.push(format!("{m} media errors"));
    }
    if s.self_test_failed {
        critical.push("last self-test failed".to_string());
    }
    let temp_warn = if kind == "hdd" { 50 } else { 65 };
    let temp_crit = if kind == "hdd" { 60 } else { 75 };
    if let Some(t) = s.temperature_c {
        if t >= temp_crit {
            critical.push(format!("{t}°C"));
        } else if t >= temp_warn {
            warning.push(format!("{t}°C"));
        }
    }
    if let Some(r) = s.reallocated.filter(|v| *v > 0) {
        match reallocated_week_ago {
            Some(old) if (r as i64) > old => critical.push(format!(
                "reallocated sectors growing ({old} → {r} in 7 days)"
            )),
            _ => warning.push(format!("{r} reallocated sectors")),
        }
    }
    if let Some(c) = s.crc_errors.filter(|v| *v > 0) {
        warning.push(format!("{c} UDMA CRC errors (cable/backplane)"));
    }
    if let Some(w) = s.wear_pct {
        if w >= 95 {
            critical.push(format!("{w}% worn"));
        } else if w >= 85 {
            warning.push(format!("{w}% worn"));
        }
    }
    if !critical.is_empty() {
        ("critical", critical.join("; "))
    } else if !warning.is_empty() {
        ("warning", warning.join("; "))
    } else if s.passed.is_some() {
        ("ok", String::new())
    } else {
        ("unknown", "no SMART data".to_string())
    }
}

// ----- live state --------------------------------------------------------------------

struct Live {
    disk: NasDisk,
    history: VecDeque<u64>,
    /// Latest SMART counters, kept for the minute sample.
    smart: SmartSummary,
}

struct State {
    disks: BTreeMap<String, Live>,
    /// Total IOPS of the node per tick, newest last — the last hour of it.
    iops_hour: VecDeque<f64>,
    prev_stats: HashMap<String, DiskStats>,
    prev_stats_at: Option<Instant>,
    last_inventory: Option<Instant>,
    last_sample: Option<Instant>,
    last_smart: Option<Instant>,
    last_prune: Option<Instant>,
    last_summary: Option<Instant>,
    telemetry: NasTelemetryState,
    inventory_error: Option<String>,
}

fn state() -> &'static RwLock<State> {
    static STATE: OnceLock<RwLock<State>> = OnceLock::new();
    STATE.get_or_init(|| {
        RwLock::new(State {
            disks: BTreeMap::new(),
            iops_hour: VecDeque::with_capacity(IOPS_BASELINE_POINTS),
            prev_stats: HashMap::new(),
            prev_stats_at: None,
            last_inventory: None,
            last_sample: None,
            last_smart: None,
            last_prune: None,
            last_summary: None,
            telemetry: NasTelemetryState {
                sampled_at: None,
                smart_read_at: None,
                smart_state: "pending".to_string(),
                detail: String::new(),
            },
            inventory_error: None,
        })
    })
}

/// Current disks with live I/O, the sparkline and the telemetry state.
pub fn snapshot() -> (Vec<NasDisk>, NasTelemetryState) {
    let st = state().read();
    let disks = st
        .disks
        .values()
        .map(|l| {
            let mut d = l.disk.clone();
            d.io_history_bps = l.history.iter().copied().collect();
            d
        })
        .collect();
    (disks, st.telemetry.clone())
}

/// Mean total IOPS of the node over the sampled hour, 0 before the sampler
/// has produced a single rate. The Overview tile shows the current value
/// against it ("+12% vs śr. godzinowa", n02).
pub fn iops_hour_avg() -> f64 {
    let st = state().read();
    if st.iops_hour.is_empty() {
        return 0.0;
    }
    st.iops_hour.iter().sum::<f64>() / st.iops_hour.len() as f64
}

pub fn disk(disk_id: &str) -> Option<NasDisk> {
    let st = state().read();
    st.disks.get(disk_id).map(|l| {
        let mut d = l.disk.clone();
        d.io_history_bps = l.history.iter().copied().collect();
        d
    })
}

/// Device path of a known disk, for privileged commands (the catalog then
/// validates it again — the id → path mapping is this node's own inventory,
/// never a caller-supplied path).
pub fn device_path(disk_id: &str) -> Option<String> {
    state().read().disks.get(disk_id).map(|l| l.disk.path.clone())
}

fn stale(last: Option<Instant>, every: Duration) -> bool {
    last.is_none_or(|t| t.elapsed() >= every)
}

/// Re-reads lsblk and merges into the live map, keeping I/O history and SMART
/// of disks that are still there. A disk that disappeared is dropped from
/// the live view (its DB row stays for history).
pub async fn refresh_inventory(db: &DbPool) -> Result<()> {
    let found = match inventory().await {
        Ok(d) => d,
        Err(e) => {
            let mut st = state().write();
            st.inventory_error = Some(e.to_string());
            st.last_inventory = Some(Instant::now());
            return Err(e);
        }
    };
    let vdevs = vdev_membership().await;
    for d in &found {
        store::upsert_disk_seen(
            db,
            &DiskIdentity {
                disk_id: &d.disk_id,
                name: &d.name,
                model: &d.model,
                serial: &d.serial,
                wwn: d.wwn.as_deref(),
                size_bytes: d.size_bytes,
                kind: &d.kind,
            },
        )?;
    }
    // Health persisted from the previous process lifetime, so a restart does
    // not show every disk as "unknown" until the first SMART pass.
    let mut persisted = HashMap::new();
    for d in &found {
        if let Some(row) = store::disk_row(db, &d.disk_id)? {
            persisted.insert(d.disk_id.clone(), row);
        }
    }
    let mut st = state().write();
    let mut next = BTreeMap::new();
    for mut d in found {
        if let Some((role, kind)) = vdevs.get(&d.name) {
            d.vdev_role.clone_from(role);
            d.vdev_kind.clone_from(kind);
        }
        let live = match st.disks.remove(&d.disk_id) {
            Some(mut old) => {
                // Identity/role/mounts from the fresh scan, everything SMART
                // and I/O from the live record.
                d.health = old.disk.health.clone();
                d.health_reason = old.disk.health_reason.clone();
                d.temperature_c = old.disk.temperature_c;
                d.power_on_hours = old.disk.power_on_hours;
                d.reallocated_sectors = old.disk.reallocated_sectors;
                d.pending_sectors = old.disk.pending_sectors;
                d.crc_errors = old.disk.crc_errors;
                d.media_errors = old.disk.media_errors;
                d.wear_pct = old.disk.wear_pct;
                d.smart_available = old.disk.smart_available;
                d.smart_passed = old.disk.smart_passed;
                d.smart_read_at = old.disk.smart_read_at.clone();
                d.io = old.disk.io.clone();
                if d.firmware.is_none() {
                    d.firmware = old.disk.firmware.take();
                }
                old.disk = d;
                old
            }
            None => {
                let mut smart = SmartSummary::default();
                if let Some(row) = persisted.get(&d.disk_id) {
                    d.health = row.health.clone();
                    d.health_reason = row.health_reason.clone();
                    d.smart_read_at = row.smart_read_at.clone();
                    if let Some(doc) = row
                        .smart_json
                        .as_deref()
                        .and_then(|j| serde_json::from_str::<Value>(j).ok())
                    {
                        smart = summarize_smart(&doc);
                        apply_summary(&mut d, &smart);
                    }
                }
                Live {
                    disk: d,
                    history: VecDeque::with_capacity(HISTORY_POINTS),
                    smart,
                }
            }
        };
        next.insert(live.disk.disk_id.clone(), live);
    }
    st.disks = next;
    st.inventory_error = None;
    st.last_inventory = Some(Instant::now());
    Ok(())
}

fn apply_summary(d: &mut NasDisk, s: &SmartSummary) {
    d.smart_available = true;
    d.smart_passed = s.passed;
    d.temperature_c = s.temperature_c;
    d.power_on_hours = s.power_on_hours;
    d.reallocated_sectors = s.reallocated;
    d.pending_sectors = s.pending;
    d.crc_errors = s.crc_errors;
    d.media_errors = s.media_errors;
    d.wear_pct = s.wear_pct;
    if let Some(fw) = &s.firmware {
        d.firmware = Some(fw.clone());
    }
}

fn tick_io() {
    let Ok(text) = std::fs::read_to_string("/proc/diskstats") else {
        return;
    };
    let cur = parse_diskstats(&text);
    let now = Instant::now();
    let mut st = state().write();
    if let Some(prev_at) = st.prev_stats_at {
        let elapsed = now.duration_since(prev_at);
        let prev = std::mem::take(&mut st.prev_stats);
        let mut node_iops = 0.0;
        for live in st.disks.values_mut() {
            let (Some(p), Some(c)) = (prev.get(&live.disk.name), cur.get(&live.disk.name)) else {
                continue;
            };
            let io = io_rates(p, c, elapsed);
            if live.history.len() == HISTORY_POINTS {
                live.history.pop_front();
            }
            live.history.push_back(io.read_bps + io.write_bps);
            node_iops += io.read_iops + io.write_iops;
            live.disk.io = io;
        }
        if st.iops_hour.len() == IOPS_BASELINE_POINTS {
            st.iops_hour.pop_front();
        }
        st.iops_hour.push_back(node_iops);
    }
    st.prev_stats = cur;
    st.prev_stats_at = Some(now);
    st.telemetry.sampled_at = Some(store::now());
}

/// Reads SMART of every disk through the broker and rescores health; raises
/// or resolves the per-disk alert on transitions. Runs only when a
/// privilege channel exists — otherwise `telemetry.smart_state` says why.
pub async fn refresh_smart(db: &DbPool) -> Result<()> {
    if !broker::channel_available(db).await {
        let mut st = state().write();
        st.telemetry.smart_state = "unarmed".to_string();
        st.telemetry.detail = "no privilege channel: SMART needs root".to_string();
        return Ok(());
    }
    let targets: Vec<(String, String, String)> = state()
        .read()
        .disks
        .values()
        .map(|l| (l.disk.disk_id.clone(), l.disk.path.clone(), l.disk.kind.clone()))
        .collect();
    let mut failures = Vec::new();
    for (id, path, kind) in targets {
        match read_smart_document(db, &path, None).await {
            Ok(doc) => {
                let summary = summarize_smart(&doc);
                let week_ago = store::attribute_week_ago(db, &id, "reallocated").unwrap_or(None);
                let (health, reason) = score_health(&summary, &kind, week_ago);
                store::store_smart(db, &id, &doc.to_string(), health, &reason)?;
                let previous = {
                    let mut st = state().write();
                    let Some(live) = st.disks.get_mut(&id) else { continue };
                    let previous = live.disk.health.clone();
                    apply_summary(&mut live.disk, &summary);
                    live.disk.health = health.to_string();
                    live.disk.health_reason = reason.clone();
                    live.disk.smart_read_at = Some(store::now());
                    live.smart = summary;
                    previous
                };
                sync_health_alert(db, &id, &previous, health, &reason)?;
            }
            Err(e) => failures.push(format!("{path}: {e}")),
        }
    }
    let mut st = state().write();
    st.last_smart = Some(Instant::now());
    st.telemetry.smart_read_at = Some(store::now());
    if failures.is_empty() {
        st.telemetry.smart_state = "ok".to_string();
        st.telemetry.detail = String::new();
    } else {
        st.telemetry.smart_state = "partial".to_string();
        st.telemetry.detail = failures.join("\n");
    }
    Ok(())
}

/// One `smartctl --json=c -x` run. smartctl's exit code is a bitmask; bits
/// 0–2 mean the command itself failed, the rest are disk findings that
/// still come with a full document.
pub async fn read_smart_document(
    db: &DbPool,
    device: &str,
    explicit: Option<&crate::profiling::collectors::elevation::ElevationToken>,
) -> Result<Value> {
    let (out, _) = broker::run_privileged(
        db,
        &HelperCommand::SmartctlInfo {
            device: device.to_string(),
        },
        explicit,
        Duration::from_secs(60),
    )
    .await?;
    if out.code & 0b111 != 0 || out.stdout.trim().is_empty() {
        return Err(anyhow!(
            "smartctl failed ({}): {}",
            out.code,
            out.stderr.trim().lines().next().unwrap_or("no output")
        ));
    }
    Ok(serde_json::from_str(&out.stdout)?)
}

fn sync_health_alert(db: &DbPool, disk_id: &str, previous: &str, health: &str, reason: &str) -> Result<()> {
    let key = format!("disk:{disk_id}:health");
    match health {
        "critical" | "warning" => {
            if previous != health {
                // Severity changed: close the old row so the new severity
                // gets its own timestamp.
                store::resolve_alert(db, &key)?;
            }
            let name = disk(disk_id).map(|d| d.name).unwrap_or_else(|| disk_id.to_string());
            store::raise_alert(
                db,
                &key,
                health,
                "disk",
                disk_id,
                &format!("Disk {name}: {health}"),
                reason,
            )?;
        }
        _ => store::resolve_alert(db, &key)?,
    }
    Ok(())
}

fn persist_minute_sample(db: &DbPool) -> Result<()> {
    let at = chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:00Z")
        .to_string();
    let st = state().read();
    let rows: Vec<(String, SmartSummary, NasDiskIo)> = st
        .disks
        .values()
        .map(|l| (l.disk.disk_id.clone(), l.smart.clone(), l.disk.io.clone()))
        .collect();
    drop(st);
    let samples: Vec<SampleInsert<'_>> = rows
        .iter()
        .map(|(id, s, io)| SampleInsert {
            disk_id: id,
            at: &at,
            temperature_c: s.temperature_c,
            reallocated: s.reallocated,
            pending: s.pending,
            crc_errors: s.crc_errors,
            media_errors: s.media_errors,
            read_bps: io.read_bps,
            write_bps: io.write_bps,
            await_ms: io.await_ms,
        })
        .collect();
    store::insert_samples(db, &samples)
}

async fn tick(db: &DbPool) {
    let (inv, sample, smart, prune) = {
        let st = state().read();
        (
            stale(st.last_inventory, INVENTORY_EVERY),
            stale(st.last_sample, SAMPLE_EVERY),
            stale(st.last_smart, SMART_EVERY),
            stale(st.last_prune, PRUNE_EVERY),
        )
    };
    if inv {
        if let Err(e) = refresh_inventory(db).await {
            tracing::warn!("tentanas: disk inventory failed: {e}");
        }
    }
    tick_io();
    if smart {
        if let Err(e) = refresh_smart(db).await {
            tracing::warn!("tentanas: SMART refresh failed: {e}");
        }
    }
    if sample {
        if let Err(e) = persist_minute_sample(db) {
            tracing::warn!("tentanas: sample persist failed: {e}");
        }
        // Pools ride the same cadence: one `zpool iostat` sample per minute
        // feeds both the live pool cards and their 24 h chart.
        super::pools::persist_sample(db).await;
        state().write().last_sample = Some(Instant::now());
    }
    if prune {
        match store::prune_samples(db) {
            Ok(n) if n > 0 => tracing::info!("tentanas: pruned {n} disk samples"),
            Ok(_) => {}
            Err(e) => tracing::warn!("tentanas: sample prune failed: {e}"),
        }
        if let Err(e) = store::prune_pool_samples(db) {
            tracing::warn!("tentanas: pool sample prune failed: {e}");
        }
        state().write().last_prune = Some(Instant::now());
    }
}

/// Starts the per-node sampler once per process. Called from the native
/// init hook; a second call (reconcile re-runs init) is a no-op.
pub fn start_sampler(main_db: DbPool, addon_id: String, db: DbPool) {
    static STARTED: OnceLock<()> = OnceLock::new();
    if STARTED.set(()).is_err() {
        return;
    }
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        tracing::warn!("tentanas: no tokio runtime, disk sampler not started");
        return;
    };
    handle.spawn(async move {
        loop {
            tick(&db).await;
            if stale(state().read().last_summary, SUMMARY_EVERY) {
                super::fleet::publish_local_summary(&main_db, &addon_id, &db).await;
                state().write().last_summary = Some(Instant::now());
            }
            tokio::time::sleep(TICK).await;
        }
    });
}

/// Forces a SMART pass on the next tick (after arming, after provisioning).
pub fn request_smart_refresh() {
    state().write().last_smart = None;
}

#[cfg(test)]
mod tests {
    use super::*;

    const LSBLK: &str = r#"{"blockdevices":[
      {"name":"sda","path":"/dev/sda","type":"disk","model":"WDC WD80EFZZ","serial":"WD-1","wwn":"0x5000cca","size":8001563222016,"tran":"sata","rota":true,"rm":false,"rev":"81.00","vendor":"ATA     ","mountpoints":[null],"fstype":null,"label":null,
       "children":[{"name":"sda1","path":"/dev/sda1","type":"part","mountpoints":[null],"fstype":"zfs_member","label":"tank"}]},
      {"name":"nvme0n1","path":"/dev/nvme0n1","type":"disk","model":"Samsung 980","serial":"S1","wwn":null,"size":1000204886016,"tran":"nvme","rota":false,"rm":false,"rev":null,"vendor":null,"mountpoints":[null],"fstype":null,"label":null,
       "children":[{"name":"nvme0n1p2","path":"/dev/nvme0n1p2","type":"part","mountpoints":["/"],"fstype":"ext4","label":null}]},
      {"name":"sdb","path":"/dev/sdb","type":"disk","model":"ST4000","serial":"Z4","wwn":null,"size":"4000787030016","tran":"sata","rota":"1","rm":"0","rev":null,"vendor":"ATA","mountpoints":[null],"fstype":null,"label":null},
      {"name":"loop0","path":"/dev/loop0","type":"loop","size":1,"mountpoints":["/snap/x"]},
      {"name":"zd0","path":"/dev/zd0","type":"disk","size":1,"mountpoints":[null]}
    ]}"#;

    #[test]
    fn lsblk_inventory_classifies_disks() {
        let disks = disks_from_lsblk_json(LSBLK).unwrap();
        assert_eq!(disks.len(), 3);
        let sda = &disks[0];
        assert_eq!(sda.disk_id, "wwn-5000cca");
        assert_eq!(sda.kind, "hdd");
        assert_eq!(sda.role, "pool_member");
        assert_eq!(sda.member_of.as_deref(), Some("tank"));
        assert_eq!(sda.model, "WDC WD80EFZZ");
        let nvme = &disks[1];
        assert_eq!(nvme.kind, "nvme");
        assert_eq!(nvme.role, "system");
        assert_eq!(nvme.disk_id, "sn-S1");
        assert_eq!(nvme.mountpoints, vec!["/"]);
        let sdb = &disks[2];
        assert_eq!(sdb.size_bytes, 4000787030016);
        assert!(sdb.rotational);
        assert_eq!(sdb.role, "free");
    }

    #[test]
    fn diskstats_rates() {
        let a = parse_diskstats("   8       0 sda 100 0 2048 50 10 0 1024 20 0 500 70\n");
        let b = parse_diskstats("   8       0 sda 200 0 4096 150 20 0 2048 40 0 1500 190\n");
        let io = io_rates(&a["sda"], &b["sda"], Duration::from_secs(1));
        assert_eq!(io.read_bps, 2048 * 512);
        assert_eq!(io.write_bps, 1024 * 512);
        assert_eq!(io.read_iops, 100.0);
        assert!((io.await_ms - 120.0 / 110.0).abs() < 0.01);
        assert!((io.util_pct - 100.0).abs() < 0.01);
    }

    #[test]
    fn smart_summary_and_health_for_ata() {
        let doc: Value = serde_json::json!({
            "smart_status": {"passed": true},
            "temperature": {"current": 41},
            "power_on_time": {"hours": 12345},
            "firmware_version": "81.00A81",
            "ata_smart_attributes": {"table": [
                {"id": 5, "name": "Reallocated_Sector_Ct", "value": 100, "worst": 100, "thresh": 10, "when_failed": "", "raw": {"value": 8, "string": "8"}},
                {"id": 197, "name": "Current_Pending_Sector", "value": 100, "worst": 100, "thresh": 0, "when_failed": "", "raw": {"value": 0, "string": "0"}},
                {"id": 199, "name": "UDMA_CRC_Error_Count", "value": 200, "worst": 200, "thresh": 0, "when_failed": "", "raw": {"value": 3, "string": "3"}}
            ]},
            "ata_smart_self_test_log": {"standard": {"table": [
                {"type": {"string": "Short offline"}, "status": {"string": "Completed without error", "passed": true}, "lifetime_hours": 12000}
            ]}}
        });
        let s = summarize_smart(&doc);
        assert_eq!(s.reallocated, Some(8));
        assert_eq!(s.crc_errors, Some(3));
        assert_eq!(s.temperature_c, Some(41));
        let (h, reason) = score_health(&s, "hdd", Some(8));
        assert_eq!(h, "warning");
        assert!(reason.contains("8 reallocated"));
        let (h, reason) = score_health(&s, "hdd", Some(2));
        assert_eq!(h, "critical");
        assert!(reason.contains("growing"));
        assert_eq!(smart_attributes(&doc).len(), 3);
        let tests = smart_self_tests(&doc);
        assert_eq!(tests[0].status, "passed");
    }

    #[test]
    fn smart_summary_for_nvme() {
        let doc: Value = serde_json::json!({
            "smart_status": {"passed": true},
            "temperature": {"current": 38},
            "nvme_smart_health_information_log": {
                "critical_warning": 0, "temperature": 38, "available_spare": 100,
                "available_spare_threshold": 10, "percentage_used": 3, "media_errors": 0,
                "power_on_hours": 900, "unsafe_shutdowns": 2, "num_err_log_entries": 0
            }
        });
        let s = summarize_smart(&doc);
        assert_eq!(s.wear_pct, Some(3));
        assert_eq!(s.power_on_hours, Some(900));
        assert_eq!(score_health(&s, "nvme", None).0, "ok");
        let attrs = smart_attributes(&doc);
        assert!(attrs.iter().any(|a| a.name == "Unsafe Shutdowns" && a.status == "warning"));
    }
}
