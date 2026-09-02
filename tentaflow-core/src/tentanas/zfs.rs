// =============================================================================
// File: tentanas/zfs.rs — the ZFS layer's shared plumbing (plan-02 §5.2).
//
//       Reads run UNPRIVILEGED: `zpool list/status/iostat` and `zfs list/get`
//       answer any user, so the pools view works on a node whose privilege
//       channel is not armed — only mutations and `zpool import` (the scan
//       opens every disk) go through the broker's catalog.
//
//       Everything here is text parsing of the `-H -p` machine formats:
//       tab-separated, no headers, exact byte counts, `-` for "unset". The
//       parsers are pure functions over that text so the fixtures in
//       `pools.rs`/`datasets.rs`/`snapshots.rs` can exercise them on a host
//       with no ZFS at all.
// =============================================================================

use std::path::Path;
use std::time::Duration;

use super::broker::{self, BrokerError};

/// Where the distributions put the two binaries. A node without them answers
/// `ToolMissing`, which the dispatcher maps to `NotAvailable` — the UI then
/// points at the Environment tab instead of showing an empty pool list.
const TOOL_DIRS: &[&str] = &[
    "/usr/sbin",
    "/usr/bin",
    "/sbin",
    "/bin",
    "/usr/local/sbin",
    "/usr/local/bin",
];

pub const ZPOOL: &str = "zpool";
pub const ZFS: &str = "zfs";

const READ_TIMEOUT: Duration = Duration::from_secs(30);

fn tool_path(name: &'static str) -> Result<String, BrokerError> {
    TOOL_DIRS
        .iter()
        .map(|d| format!("{d}/{name}"))
        .find(|p| Path::new(p).is_file())
        .ok_or(BrokerError::ToolMissing(name))
}

/// Runs an unprivileged `zpool` read and returns stdout; a non-zero exit is
/// an error here because every read this app issues is expected to succeed.
pub async fn zpool(args: &[&str]) -> Result<String, BrokerError> {
    run(ZPOOL, args, READ_TIMEOUT).await
}

/// Same for `zfs`. `timeout` matters for `zpool iostat 1 1`, which blocks for
/// the interval it samples.
pub async fn zfs(args: &[&str]) -> Result<String, BrokerError> {
    run(ZFS, args, READ_TIMEOUT).await
}

pub async fn run(program: &'static str, args: &[&str], timeout: Duration) -> Result<String, BrokerError> {
    let path = tool_path(program)?;
    let out = broker::run_unprivileged(&path, args, timeout).await?;
    let out = broker::require_success(program, out)?;
    Ok(out.stdout)
}

/// Whether this node can answer pool questions at all.
pub fn available() -> bool {
    tool_path(ZPOOL).is_ok() && tool_path(ZFS).is_ok()
}

// ----- `-H -p` field helpers ---------------------------------------------------

/// `-` and the empty string both mean "no value" in the machine formats.
pub fn field(raw: &str) -> Option<&str> {
    let t = raw.trim();
    (!t.is_empty() && t != "-").then_some(t)
}

pub fn u64_field(raw: &str) -> u64 {
    field(raw)
        .map(|v| v.trim_end_matches(['%', 'x']))
        .and_then(|v| v.parse().ok())
        .unwrap_or(0)
}

pub fn opt_u64_field(raw: &str) -> Option<u64> {
    field(raw).and_then(|v| v.parse().ok())
}

pub fn f64_field(raw: &str) -> f64 {
    field(raw)
        .map(|v| v.trim_end_matches(['%', 'x']))
        .and_then(|v| v.parse().ok())
        .unwrap_or(0.0)
}

/// ZFS spells booleans `on`/`off` for properties and `yes`/`no` for a few
/// `zpool list` columns.
pub fn bool_field(raw: &str) -> bool {
    matches!(raw.trim(), "on" | "yes" | "1" | "true" | "enabled")
}

/// A byte count as `zpool status` prints it: a plain integer under `-p`, or a
/// suffixed value (`1.23T`, `512B`, `0B`) on the versions that keep the human
/// form in the scan line. Binary units — ZFS reports powers of 1024.
pub fn parse_bytes(text: &str) -> Option<u64> {
    let t = text.trim().trim_end_matches('B');
    if t.is_empty() {
        return None;
    }
    let (number, scale) = match t.chars().last() {
        Some(c) if c.is_ascii_alphabetic() => {
            let exp = match c.to_ascii_uppercase() {
                'K' => 1u32,
                'M' => 2,
                'G' => 3,
                'T' => 4,
                'P' => 5,
                'E' => 6,
                _ => return None,
            };
            (&t[..t.len() - c.len_utf8()], 1024f64.powi(exp as i32))
        }
        _ => (t, 1.0),
    };
    number.trim().parse::<f64>().ok().map(|v| (v * scale) as u64)
}

/// `HH:MM:SS` or `N days HH:MM:SS` — the two shapes of a scan duration/ETA.
pub fn parse_duration_secs(text: &str) -> Option<u64> {
    let text = text.trim();
    let (days, clock) = match text.split_once("days") {
        Some((d, rest)) => (d.trim().parse::<u64>().ok()?, rest.trim()),
        None => match text.split_once("day") {
            Some((d, rest)) => (d.trim().parse::<u64>().ok()?, rest.trim()),
            None => (0, text),
        },
    };
    let mut parts = clock.split(':').map(|p| p.trim().parse::<u64>());
    let (h, m, s) = (parts.next()?.ok()?, parts.next()?.ok()?, parts.next()?.ok()?);
    if parts.next().is_some() {
        return None;
    }
    Some(days * 86_400 + h * 3600 + m * 60 + s)
}

/// A `zpool status` timestamp (`Tue Sep  1 03:12:34 2026`) read as the node's
/// local time — that is what ZFS prints — and normalized to UTC RFC 3339.
pub fn parse_status_time(text: &str) -> Option<String> {
    use chrono::TimeZone;
    let naive = chrono::NaiveDateTime::parse_from_str(text.trim(), "%a %b %e %H:%M:%S %Y").ok()?;
    let local = chrono::Local.from_local_datetime(&naive).single()?;
    Some(
        local
            .with_timezone(&chrono::Utc)
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
    )
}

/// A `creation` property under `-p`: seconds since the epoch.
pub fn epoch_to_rfc3339(secs: i64) -> String {
    chrono::DateTime::from_timestamp(secs, 0)
        .unwrap_or_default()
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

// ----- device links ------------------------------------------------------------

/// Prefix order of `/dev/disk/by-id` links, most stable first. A pool built
/// on `sdb` breaks the day the kernel enumerates that disk as `sdc`; a pool
/// built on a by-id link does not, so this is what `zpool create` receives.
const BY_ID_PREFERENCE: &[&str] = &["wwn-", "nvme-eui.", "ata-", "nvme-", "scsi-", "usb-"];

/// The stable `/dev/disk/by-id/…` path of a whole disk, or its `/dev/<name>`
/// node when the host publishes no link for it (virtio, some hypervisors).
pub fn stable_device_path(kernel_name: &str) -> String {
    let fallback = format!("/dev/{kernel_name}");
    let Ok(entries) = std::fs::read_dir("/dev/disk/by-id") else {
        return fallback;
    };
    let mut best: Option<(usize, String)> = None;
    for entry in entries.flatten() {
        let link = entry.file_name().to_string_lossy().into_owned();
        // `-partN` links point at partitions; pools take whole disks.
        if link.contains("-part") {
            continue;
        }
        let Ok(target) = std::fs::canonicalize(entry.path()) else {
            continue;
        };
        if target.file_name().is_none_or(|n| n != kernel_name) {
            continue;
        }
        let rank = BY_ID_PREFERENCE
            .iter()
            .position(|p| link.starts_with(p))
            .unwrap_or(BY_ID_PREFERENCE.len());
        let path = format!("/dev/disk/by-id/{link}");
        if best.as_ref().is_none_or(|(r, _)| rank < *r) {
            best = Some((rank, path));
        }
    }
    best.map(|(_, p)| p).unwrap_or(fallback)
}

/// The kernel name behind a vdev path or leaf name. `zpool status -P` prints
/// whole paths, which may be a by-id link, a partition of one, or a bare
/// `/dev` node; the disks inventory is keyed by kernel name.
pub fn kernel_name_of(vdev_path: &str) -> String {
    let resolved = if vdev_path.starts_with('/') {
        std::fs::canonicalize(vdev_path)
            .ok()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .unwrap_or_else(|| {
                vdev_path
                    .rsplit('/')
                    .next()
                    .unwrap_or(vdev_path)
                    .to_string()
            })
    } else {
        vdev_path.to_string()
    };
    strip_partition_suffix(&resolved)
}

/// `sda1` → `sda`, `nvme0n1p2` → `nvme0n1`, `mmcblk0p1` → `mmcblk0`. Only the
/// kernel's own naming schemes are folded: a by-id link name keeps its
/// trailing digits, which are part of a serial, not a partition number.
pub fn strip_partition_suffix(name: &str) -> String {
    for prefix in ["sd", "vd", "hd"] {
        let Some(rest) = name.strip_prefix(prefix) else {
            continue;
        };
        let letters = rest
            .bytes()
            .take_while(|b| b.is_ascii_lowercase())
            .count();
        let (letters_part, digits) = rest.split_at(letters);
        if letters_part.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return name.to_string();
        }
        return format!("{prefix}{letters_part}");
    }
    if name.starts_with("nvme") || name.starts_with("mmcblk") {
        if let Some((head, digits)) = name.rsplit_once('p') {
            if !digits.is_empty()
                && digits.bytes().all(|b| b.is_ascii_digit())
                && head.bytes().last().is_some_and(|b| b.is_ascii_digit())
            {
                return head.to_string();
            }
        }
    }
    name.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn machine_format_fields_treat_dash_as_unset() {
        assert_eq!(field("-"), None);
        assert_eq!(field("  "), None);
        assert_eq!(field(" tank "), Some("tank"));
        assert_eq!(u64_field("-"), 0);
        assert_eq!(u64_field("68%"), 68);
        assert_eq!(u64_field("12"), 12);
        assert_eq!(opt_u64_field("-"), None);
        assert_eq!(opt_u64_field("7"), Some(7));
        assert!((f64_field("1.31x") - 1.31).abs() < 1e-9);
        assert!(bool_field("on"));
        assert!(!bool_field("off"));
    }

    #[test]
    fn byte_counts_parse_in_both_status_forms() {
        assert_eq!(parse_bytes("0B"), Some(0));
        assert_eq!(parse_bytes("512"), Some(512));
        assert_eq!(parse_bytes("1K"), Some(1024));
        assert_eq!(parse_bytes("1.5G"), Some(1_610_612_736));
        assert_eq!(parse_bytes("4398046511104"), Some(4_398_046_511_104));
        assert_eq!(parse_bytes(""), None);
        assert_eq!(parse_bytes("nope"), None);
    }

    #[test]
    fn scan_durations_cover_both_shapes() {
        assert_eq!(parse_duration_secs("00:12:34"), Some(754));
        assert_eq!(parse_duration_secs("09:12:00"), Some(33_120));
        assert_eq!(parse_duration_secs("1 days 02:11:00"), Some(94_260));
        assert_eq!(parse_duration_secs("no estimate"), None);
    }

    #[test]
    fn partition_suffixes_fold_back_to_the_whole_disk() {
        assert_eq!(strip_partition_suffix("sda1"), "sda");
        assert_eq!(strip_partition_suffix("sda"), "sda");
        assert_eq!(strip_partition_suffix("nvme0n1p2"), "nvme0n1");
        assert_eq!(strip_partition_suffix("nvme0n1"), "nvme0n1");
        assert_eq!(strip_partition_suffix("mmcblk0p1"), "mmcblk0");
        assert_eq!(kernel_name_of("/dev/disk/by-id/ata-ST8000_ZR9"), "ata-ST8000_ZR9");
    }

    #[test]
    fn status_timestamps_become_utc() {
        let at = parse_status_time("Tue Sep  1 03:12:34 2026").expect("parsed");
        assert!(at.ends_with('Z'), "{at}");
        assert!(at.starts_with("2026-0"), "{at}");
        assert_eq!(parse_status_time("not a date"), None);
    }
}
