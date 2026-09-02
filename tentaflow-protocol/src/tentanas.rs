// =============================================================================
// File: tentanas.rs
// Purpose: Binary CBOR protocol for TentaNas — the storage app: fleet overview,
//          per-node environment (OS, feature probes, privilege channel),
//          disks (inventory, SMART/NVMe health, live throughput, history,
//          alerts) and the jobs those operations spawn. Every request is
//          executed on the node that owns the hardware: the dashboard picks a
//          node and the platform forwards the body there (`Routing::Forward`),
//          so no payload carries a `node_id` — the envelope does.
// Example: MessageBody::TentaNasBody(TentaNasPayload::DisksListRequest {})
// =============================================================================

use serde::{Deserialize, Serialize};

/// A sudo password in transit. RAM-only on both ends of a forwarded request;
/// the executing node moves it into a `Zeroizing` buffer at once. `Debug` is
/// redacted so no tracing span of a request can ever print it.
#[derive(Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SudoSecret(pub String);

impl std::fmt::Debug for SudoSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SudoSecret(***)")
    }
}

// =============================================================================
// Fleet
// =============================================================================

/// One row of the fleet overview — the synced `nas_node_summary` registry
/// joined with the mesh roster. Aggregates only; details are fetched from the
/// node on demand through routing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct NasNodeInfo {
    pub node_id: String,
    pub node_name: String,
    pub is_local: bool,
    pub online: bool,
    /// Instance reconcile status on that node: 'ready' | 'unsupported' |
    /// 'init_error' | 'unknown' (no status row yet).
    pub instance_status: String,
    /// 'ok' | 'warning' | 'critical' | 'unknown'.
    pub health: String,
    pub os_name: String,
    pub zfs_version: Option<String>,
    /// 'unarmed' | 'helper' | 'interactive'.
    pub elevation_mode: String,
    pub disks_total: u32,
    pub disks_warning: u32,
    pub pools_total: u32,
    pub shares_total: u32,
    pub alerts_active: u32,
    pub capacity_bytes: u64,
    pub used_bytes: u64,
    pub updated_at: Option<String>,
}

// =============================================================================
// Environment
// =============================================================================

/// One feature probe of the environment screen. `status` is 'ok' |
/// 'missing_package' | 'missing_module' | 'version_too_low' |
/// 'unsupported_platform'. `packages` are what "Install" would pass to the
/// package manager — shown verbatim before anything runs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct NasFeature {
    pub id: String,
    pub status: String,
    pub version: Option<String>,
    pub required_version: Option<String>,
    pub binaries: Vec<String>,
    pub kernel_module: Option<String>,
    pub packages: Vec<String>,
    pub detail: String,
    pub optional: bool,
}

/// State of the privilege channel (§3.4) on the node. `mode`: 'unarmed' |
/// 'helper' (mode A, provisioned) | 'interactive' (mode B, password in RAM
/// until `armed_until`). `helper_state`: 'absent' | 'ok' | 'version_mismatch'
/// | 'sudoers_missing' — how the helper actually answered, not what the
/// database remembers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct NasElevation {
    pub mode: String,
    pub helper_state: String,
    pub helper_path: String,
    pub helper_version: Option<String>,
    pub sudoers_path: String,
    pub core_user: String,
    pub core_version: String,
    pub armed_until: Option<String>,
    pub ttl_secs: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct NasEnvironment {
    /// std::env::consts::OS: 'linux' | 'macos' | 'windows'.
    pub platform: String,
    /// True on Linux; other platforms run the limited mode (inventory only).
    pub full_support: bool,
    pub os_name: String,
    pub os_version: String,
    pub kernel: String,
    pub hostname: String,
    /// 'apt' | 'dnf' | 'pacman' | 'zypper' | '' (unknown — install disabled).
    pub package_manager: String,
    pub ram_bytes: u64,
    pub uptime_secs: u64,
    pub features: Vec<NasFeature>,
    pub elevation: NasElevation,
    pub probed_at: String,
}

/// Exactly what mode-A provisioning writes, shown before it runs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct NasElevationPlan {
    pub helper_source: String,
    pub helper_source_present: bool,
    pub helper_path: String,
    pub sudoers_path: String,
    pub sudoers_line: String,
    pub core_user: String,
    pub core_version: String,
    /// The argv sequence provisioning executes as root, one entry per step.
    pub commands: Vec<Vec<String>>,
}

// =============================================================================
// Disks
// =============================================================================

/// Live throughput and latency of a block device, computed from two
/// `/proc/diskstats` samples by the node's sampler.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct NasDiskIo {
    pub read_bps: u64,
    pub write_bps: u64,
    pub read_iops: f64,
    pub write_iops: f64,
    pub await_ms: f64,
    pub util_pct: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct NasDisk {
    /// Stable id: WWN when known, else `serial`, else `dev:<name>`.
    pub disk_id: String,
    pub name: String,
    pub path: String,
    /// 'hdd' | 'ssd' | 'nvme' | 'unknown'.
    pub kind: String,
    pub model: String,
    pub serial: String,
    pub wwn: Option<String>,
    pub size_bytes: u64,
    pub transport: String,
    pub rotational: bool,
    pub removable: bool,
    pub firmware: Option<String>,
    /// 'free' | 'pool' | 'parity' | 'cache' | 'spare' | 'system' | 'partitioned'.
    pub role: String,
    /// Pool or array the disk belongs to, when `role` says so.
    pub member_of: Option<String>,
    /// 'ok' | 'warning' | 'critical' | 'unknown'.
    pub health: String,
    /// Human reason behind `health` ("3 new reallocated sectors in 7 days").
    pub health_reason: String,
    pub temperature_c: Option<i32>,
    pub power_on_hours: Option<u64>,
    pub reallocated_sectors: Option<u64>,
    pub pending_sectors: Option<u64>,
    pub crc_errors: Option<u64>,
    pub media_errors: Option<u64>,
    /// NVMe percentage-used / SSD wear-level, when the device reports it.
    pub wear_pct: Option<u8>,
    pub smart_available: bool,
    pub smart_passed: Option<bool>,
    pub smart_read_at: Option<String>,
    pub io: NasDiskIo,
    /// Last 60 samples of read+write throughput (bytes/s), oldest first — the
    /// row sparkline.
    pub io_history_bps: Vec<u64>,
    pub mountpoints: Vec<String>,
}

/// One SMART attribute (ATA) or NVMe log field, normalized.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct NasSmartAttribute {
    pub id: u32,
    pub name: String,
    pub value: i64,
    pub worst: i64,
    pub threshold: i64,
    pub raw: i64,
    pub raw_text: String,
    /// 'ok' | 'warning' | 'critical'.
    pub status: String,
    /// Raw value one week ago from the sample history, for the trend column.
    pub raw_week_ago: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct NasSmartSelfTest {
    pub kind: String,
    pub status: String,
    pub lifetime_hours: u64,
    pub started_at: Option<String>,
    pub detail: String,
}

/// Point of the per-disk health history (minute samples, downsampled).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct NasDiskSample {
    pub at: String,
    pub temperature_c: Option<i32>,
    pub reallocated_sectors: Option<u64>,
    pub pending_sectors: Option<u64>,
    pub read_bps: u64,
    pub write_bps: u64,
    pub await_ms: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct NasAlert {
    pub alert_id: String,
    /// 'info' | 'warning' | 'critical'.
    pub severity: String,
    /// 'disk' | 'pool' | 'elevation' | 'environment'.
    pub subject_kind: String,
    pub subject_id: String,
    pub title: String,
    pub detail: String,
    pub raised_at: String,
    pub acked_at: Option<String>,
    pub resolved_at: Option<String>,
}

/// Whether the disk data of the answer is fresh. Mode B between armed
/// sessions cannot run smartctl, so the UI shows the age of what it sees.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct NasTelemetryState {
    pub sampled_at: Option<String>,
    pub smart_read_at: Option<String>,
    /// 'live' | 'stale_unarmed' | 'unavailable'.
    pub smart_state: String,
    pub detail: String,
}

// =============================================================================
// Jobs
// =============================================================================

/// A long-running system operation (package install, SMART self-test, later
/// scrub/resilver/mover). Progress lines are stored with the job so a
/// dashboard that reconnects sees the whole log, not only what streamed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct NasJob {
    pub job_id: String,
    /// 'packages_install' | 'smart_test' | 'elevation_provision'.
    pub kind: String,
    pub subject: String,
    /// 'queued' | 'running' | 'done' | 'failed' | 'blocked' | 'cancelled'.
    pub status: String,
    pub progress_pct: Option<u8>,
    pub started_by: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub error: Option<String>,
    pub log: Vec<String>,
}

// =============================================================================
// Payload
// =============================================================================

/// Every TentaNas request/response. Ciborium tags variants by NAME, but the
/// order is still the contract — append-only, never insert or reorder — and
/// no variant or field may be renamed without updating the frontend and the
/// golden test (`tentanas_wire_golden`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum TentaNasPayload {
    // ----- fleet (answered by the node showing the dashboard) -----
    NodesListRequest {},
    NodesListResponse {
        local_node_id: String,
        nodes: Vec<NasNodeInfo>,
    },

    // ----- environment (executed on the target node) -----
    /// `refresh` re-runs every probe instead of answering from the last probe.
    EnvironmentRequest {
        #[serde(default)]
        refresh: bool,
    },
    EnvironmentResponse {
        environment: NasEnvironment,
    },
    ElevationPlanRequest {},
    ElevationPlanResponse {
        plan: NasElevationPlan,
    },
    /// Mode A: install the helper + sudoers line. Always needs the password
    /// (both modes, §3.1 table). Answers with `JobResponse`.
    ElevationProvisionRequest {
        sudo_password: SudoSecret,
    },
    /// Mode B: keep the password in RAM for `ttl_secs` (0 = node default).
    ElevationArmRequest {
        sudo_password: SudoSecret,
        #[serde(default)]
        ttl_secs: u32,
    },
    /// Mode B: forget the password now.
    ElevationDisarmRequest {},
    /// Mode A: remove the helper and the sudoers line (needs a fresh password —
    /// the helper's catalog deliberately cannot do this itself).
    ElevationRemoveRequest {
        sudo_password: SudoSecret,
    },
    ElevationResponse {
        elevation: NasElevation,
    },
    /// Install the packages of one feature through the node's package
    /// manager. Mode B needs the password; mode A runs through the helper.
    PackagesInstallRequest {
        feature_id: String,
        #[serde(default)]
        sudo_password: Option<SudoSecret>,
    },

    // ----- jobs -----
    JobsListRequest {
        #[serde(default)]
        limit: u32,
    },
    JobsListResponse {
        jobs: Vec<NasJob>,
    },
    JobGetRequest {
        job_id: String,
    },
    JobCancelRequest {
        job_id: String,
    },
    JobResponse {
        job: NasJob,
    },

    // ----- disks -----
    DisksListRequest {},
    DisksListResponse {
        disks: Vec<NasDisk>,
        telemetry: NasTelemetryState,
    },
    DiskGetRequest {
        disk_id: String,
    },
    DiskGetResponse {
        disk: NasDisk,
        attributes: Vec<NasSmartAttribute>,
        self_tests: Vec<NasSmartSelfTest>,
        history: Vec<NasDiskSample>,
        alerts: Vec<NasAlert>,
        telemetry: NasTelemetryState,
    },
    /// 'short' | 'long'. Answers with `JobResponse`.
    DiskSmartTestRequest {
        disk_id: String,
        kind: String,
        #[serde(default)]
        sudo_password: Option<SudoSecret>,
    },
    DiskLocateRequest {
        disk_id: String,
        enable: bool,
    },
    DiskLocateResponse {
        /// 'ledctl' | 'none' — with 'none' the UI shows serial/WWN/slot large.
        method: String,
        active: bool,
        detail: String,
    },

    // ----- alerts -----
    AlertsListRequest {
        #[serde(default)]
        include_acked: bool,
    },
    AlertsListResponse {
        alerts: Vec<NasAlert>,
    },
    AlertAckRequest {
        alert_id: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message_body::MessageBody;

    fn hex_bytes(hex: &str) -> Vec<u8> {
        (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).expect("hex"))
            .collect()
    }

    /// Pins the exact bytes of the family tag and of a request with a
    /// `#[serde(default)]` field, so a renamed variant, field or body tag
    /// fails here before it fails in a browser.
    #[test]
    fn tentanas_wire_golden() {
        let req = TentaNasPayload::DiskGetRequest {
            disk_id: "d1".to_string(),
        };
        let bytes = crate::cbor::encode(&req).expect("encode");
        assert_eq!(
            bytes,
            hex_bytes("a16e4469736b47657452657175657374a1676469736b5f6964626431"),
            "DiskGetRequest wire drift"
        );

        let body = MessageBody::TentaNasBody(req);
        let bytes = crate::cbor::encode(&body).expect("encode");
        assert_eq!(
            bytes,
            hex_bytes(
                "a16c54656e74614e6173426f6479a16e4469736b47657452657175657374a1676469736b5f6964626431"
            ),
            "MessageBody::TentaNasBody wire drift"
        );

        // A request with a defaulted field decodes when the field is absent —
        // that is how the JSON-across-wasm encoder relies on `#[serde(default)]`.
        let json = serde_json::json!({ "EnvironmentRequest": {} });
        let decoded: TentaNasPayload = serde_json::from_value(json).expect("decode");
        assert_eq!(decoded, TentaNasPayload::EnvironmentRequest { refresh: false });
    }

    #[test]
    fn sudo_secret_never_prints_in_debug() {
        let req = TentaNasPayload::ElevationArmRequest {
            sudo_password: SudoSecret("hunter2".to_string()),
            ttl_secs: 0,
        };
        let text = format!("{req:?}");
        assert!(!text.contains("hunter2"), "{text}");
        // It IS still on the wire, as a plain string — the mesh channel is
        // the protection, not the encoding.
        let bytes = crate::cbor::encode(&req).expect("encode");
        let back: TentaNasPayload = crate::cbor::decode(&bytes).expect("decode");
        assert_eq!(back, req);
    }

    #[test]
    fn a_disk_round_trips_without_losing_a_field() {
        let disk = NasDisk {
            disk_id: "wwn-0x5000c500a1b2c3d4".to_string(),
            name: "sdd".to_string(),
            path: "/dev/sdd".to_string(),
            kind: "hdd".to_string(),
            model: "ST8000NM000A".to_string(),
            serial: "ZR9AB12K".to_string(),
            wwn: Some("0x5000c500a1b2c3d4".to_string()),
            size_bytes: 8_001_563_222_016,
            transport: "sata".to_string(),
            rotational: true,
            removable: false,
            firmware: Some("SN02".to_string()),
            role: "pool".to_string(),
            member_of: Some("tank".to_string()),
            health: "warning".to_string(),
            health_reason: "3 new reallocated sectors in 7 days".to_string(),
            temperature_c: Some(47),
            power_on_hours: Some(18_211),
            reallocated_sectors: Some(11),
            pending_sectors: Some(2),
            crc_errors: Some(0),
            media_errors: None,
            wear_pct: None,
            smart_available: true,
            smart_passed: Some(true),
            smart_read_at: Some("2026-09-01T14:06:00Z".to_string()),
            io: NasDiskIo {
                read_bps: 1_000,
                write_bps: 2_000,
                read_iops: 3.5,
                write_iops: 4.5,
                await_ms: 6.25,
                util_pct: 12.0,
            },
            io_history_bps: vec![1, 2, 3],
            mountpoints: vec![],
        };
        let body = MessageBody::TentaNasBody(TentaNasPayload::DisksListResponse {
            disks: vec![disk],
            telemetry: NasTelemetryState {
                sampled_at: Some("2026-09-01T14:06:00Z".to_string()),
                smart_read_at: None,
                smart_state: "live".to_string(),
                detail: String::new(),
            },
        });
        let bytes = crate::cbor::encode(&body).expect("encode");
        let back: MessageBody = crate::cbor::decode(&bytes).expect("decode");
        assert_eq!(back, body);
    }
}
