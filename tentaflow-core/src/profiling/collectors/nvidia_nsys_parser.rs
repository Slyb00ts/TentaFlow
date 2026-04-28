// =============================================================================
// File: collectors/nvidia_nsys_parser.rs — Parser that converts the raw nsys
// `.nsys-rep` artifact into normalized `TimelineEvent`s. We export the report
// to SQLite via `nsys export --type sqlite` and stream raw rows from the
// per-event tables (kernels, CUDA runtime, memcpy, NVTX, GPU metrics).
// =============================================================================

use std::path::Path;
use std::process::Command as StdCommand;

use rusqlite::{Connection, OptionalExtension};
use tentaflow_protocol::profiling::{
    EventCategory, EventPayload, GpuVendor, PowerDomain, TimelineEvent, TransferKind,
};

use crate::profiling::collectors::{
    CollectorError, CollectorParser, FrameInterner, NameInterner, RawCapture, SessionCtx,
};
use crate::profiling::nsys::nsys_binary;

/// Parser implementation paired with `NvidiaNsysCollector`.
pub struct NvidiaNsysParser;

impl CollectorParser for NvidiaNsysParser {
    fn parse(
        &self,
        raw: RawCapture,
        _ctx: &SessionCtx,
        names: &mut NameInterner,
        _frames: &mut FrameInterner,
    ) -> Result<Vec<TimelineEvent>, CollectorError> {
        // Locate the nsys report inside the raw capture.
        let rep_path = raw
            .artifacts
            .iter()
            .find(|p| {
                p.extension()
                    .and_then(|s| s.to_str())
                    .map(|s| s.eq_ignore_ascii_case("nsys-rep"))
                    .unwrap_or(false)
            })
            .cloned()
            .or_else(|| raw.artifacts.first().cloned());

        let Some(rep_path) = rep_path else {
            // No artifact -> nothing to parse, no error.
            return Ok(Vec::new());
        };

        // Export to SQLite next to the report (idempotent).
        let sqlite_path = rep_path.with_extension("sqlite");
        if !sqlite_path.exists() {
            export_to_sqlite(&rep_path, &sqlite_path)?;
        }

        let conn = Connection::open(&sqlite_path)
            .map_err(|e| CollectorError::Parse(format!("open sqlite: {e}")))?;

        let session_t0 = read_session_t0(&conn);

        let mut events: Vec<TimelineEvent> = Vec::new();
        parse_kernel_rows(&conn, names, session_t0, &mut events)?;
        parse_runtime_rows(&conn, names, session_t0, &mut events)?;
        parse_memcpy_rows(&conn, session_t0, &mut events)?;
        parse_nvtx_rows(&conn, names, session_t0, &mut events)?;
        parse_gpu_metric_rows(&conn, session_t0, &mut events)?;

        Ok(events)
    }
}

/// Run `nsys export --type sqlite` on `rep_path`. We invoke the binary
/// synchronously through `std::process::Command` because the parser runs on
/// the orchestrator's blocking pool.
fn export_to_sqlite(rep_path: &Path, sqlite_path: &Path) -> Result<(), CollectorError> {
    let nsys = nsys_binary().ok_or_else(|| {
        CollectorError::Parse("nsys binary required for SQLite export not found".into())
    })?;
    let output = StdCommand::new(nsys)
        .args(["export", "--type", "sqlite", "--output"])
        .arg(sqlite_path)
        .arg(rep_path)
        .output()
        .map_err(|e| CollectorError::Parse(format!("nsys export spawn: {e}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(CollectorError::Parse(format!(
            "nsys export failed: {stderr}"
        )));
    }
    Ok(())
}

/// Try a few well-known locations for the session start timestamp in nsys'
/// schema. Returns `0` if no source is available so timestamps remain in the
/// raw nsys clock. The orchestrator's drift report does not depend on this.
fn read_session_t0(conn: &Connection) -> i64 {
    // Newer nsys: TARGET_INFO_SESSION_START_TIME (single row, column `value`).
    if let Ok(Some(v)) = conn
        .query_row(
            "SELECT value FROM TARGET_INFO_SESSION_START_TIME LIMIT 1",
            [],
            |r| r.get::<_, i64>(0),
        )
        .optional()
    {
        return v;
    }
    // Older nsys: ANALYSIS_DETAILS.startTime.
    if let Ok(Some(v)) = conn
        .query_row("SELECT startTime FROM ANALYSIS_DETAILS LIMIT 1", [], |r| {
            r.get::<_, i64>(0)
        })
        .optional()
    {
        return v;
    }
    // Fallback: smallest timestamp across kernel/runtime/memcpy tables.
    let queries = [
        "SELECT MIN(start) FROM CUPTI_ACTIVITY_KIND_KERNEL",
        "SELECT MIN(start) FROM CUPTI_ACTIVITY_KIND_RUNTIME",
        "SELECT MIN(start) FROM CUPTI_ACTIVITY_KIND_MEMCPY",
        "SELECT MIN(timestamp) FROM GPU_METRICS",
    ];
    let mut min_seen: Option<i64> = None;
    for q in &queries {
        // The MIN(...) query returns a single row whose column may be NULL when
        // the table is empty. We only fold non-NULL values into `min_seen`.
        let row: Option<Option<i64>> = conn
            .query_row(q, [], |r| r.get::<_, Option<i64>>(0))
            .optional()
            .ok()
            .flatten();
        if let Some(Some(v)) = row {
            min_seen = Some(min_seen.map_or(v, |cur| cur.min(v)));
        }
    }
    min_seen.unwrap_or(0)
}

fn rel_ns(ts: i64, t0: i64) -> u64 {
    (ts.saturating_sub(t0)).max(0) as u64
}

fn table_exists(conn: &Connection, name: &str) -> bool {
    conn.query_row(
        "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1 LIMIT 1",
        [name],
        |_| Ok(()),
    )
    .optional()
    .ok()
    .flatten()
    .is_some()
}

fn parse_kernel_rows(
    conn: &Connection,
    names: &mut NameInterner,
    t0: i64,
    out: &mut Vec<TimelineEvent>,
) -> Result<(), CollectorError> {
    if !table_exists(conn, "CUPTI_ACTIVITY_KIND_KERNEL") {
        return Ok(());
    }
    let has_strings = table_exists(conn, "StringIds");
    // Schema across nsys versions: kernel name is either a `name` column with
    // a string id (joined against `StringIds`) or a literal `shortName` text.
    // We try the join first; fall back to inline text if needed.
    let sql = if has_strings {
        "SELECT k.start, k.end, COALESCE(s.value, '<unknown>') AS name, \
                COALESCE(k.deviceId, 0), COALESCE(k.gridX, 0), COALESCE(k.gridY, 0), \
                COALESCE(k.gridZ, 0), COALESCE(k.blockX, 0), COALESCE(k.blockY, 0), \
                COALESCE(k.blockZ, 0), COALESCE(k.staticSharedMemory, 0) \
         FROM CUPTI_ACTIVITY_KIND_KERNEL k \
         LEFT JOIN StringIds s ON s.id = k.shortName"
    } else {
        "SELECT start, end, COALESCE(shortName, '<unknown>'), \
                COALESCE(deviceId, 0), COALESCE(gridX, 0), COALESCE(gridY, 0), \
                COALESCE(gridZ, 0), COALESCE(blockX, 0), COALESCE(blockY, 0), \
                COALESCE(blockZ, 0), COALESCE(staticSharedMemory, 0) \
         FROM CUPTI_ACTIVITY_KIND_KERNEL"
    };
    let mut stmt = conn
        .prepare(sql)
        .map_err(|e| CollectorError::Parse(format!("prepare kernels: {e}")))?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)? as u32,
                r.get::<_, i64>(4)? as u32,
                r.get::<_, i64>(5)? as u32,
                r.get::<_, i64>(6)? as u32,
                r.get::<_, i64>(7)? as u32,
                r.get::<_, i64>(8)? as u32,
                r.get::<_, i64>(9)? as u32,
                r.get::<_, i64>(10)? as u64,
            ))
        })
        .map_err(|e| CollectorError::Parse(format!("query kernels: {e}")))?;
    for row in rows {
        let (start, end, name, device, gx, gy, gz, bx, by, bz, shared) =
            row.map_err(|e| CollectorError::Parse(format!("read kernel row: {e}")))?;
        let name_id = names.intern(&name);
        out.push(TimelineEvent {
            source_idx: 0,
            t_start_ns: rel_ns(start, t0),
            t_end_ns: rel_ns(end, t0),
            category: EventCategory::GpuKernel,
            lane_hint: device.min(u16::MAX as u32) as u16,
            payload: EventPayload::GpuKernel {
                device_id: device,
                name_id,
                grid: [gx, gy, gz],
                block: [bx, by, bz],
                shared_mem_bytes: shared,
            },
        });
    }
    Ok(())
}

fn parse_runtime_rows(
    conn: &Connection,
    names: &mut NameInterner,
    t0: i64,
    out: &mut Vec<TimelineEvent>,
) -> Result<(), CollectorError> {
    if !table_exists(conn, "CUPTI_ACTIVITY_KIND_RUNTIME") {
        return Ok(());
    }
    let has_strings = table_exists(conn, "StringIds");
    let sql = if has_strings {
        "SELECT r.start, r.end, COALESCE(s.value, '<unknown>'), \
                COALESCE(r.returnValue, 0) \
         FROM CUPTI_ACTIVITY_KIND_RUNTIME r \
         LEFT JOIN StringIds s ON s.id = r.nameId"
    } else {
        "SELECT start, end, COALESCE(name, '<unknown>'), COALESCE(returnValue, 0) \
         FROM CUPTI_ACTIVITY_KIND_RUNTIME"
    };
    let mut stmt = conn
        .prepare(sql)
        .map_err(|e| CollectorError::Parse(format!("prepare runtime: {e}")))?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)? as i32,
            ))
        })
        .map_err(|e| CollectorError::Parse(format!("query runtime: {e}")))?;
    for row in rows {
        let (start, end, name, ret) =
            row.map_err(|e| CollectorError::Parse(format!("read runtime row: {e}")))?;
        let name_id = names.intern(&name);
        out.push(TimelineEvent {
            source_idx: 0,
            t_start_ns: rel_ns(start, t0),
            t_end_ns: rel_ns(end, t0),
            category: EventCategory::GpuApiCall,
            lane_hint: 0,
            payload: EventPayload::GpuApiCall {
                device_id: 0,
                name_id,
                return_code: ret,
            },
        });
    }
    Ok(())
}

fn parse_memcpy_rows(
    conn: &Connection,
    t0: i64,
    out: &mut Vec<TimelineEvent>,
) -> Result<(), CollectorError> {
    if !table_exists(conn, "CUPTI_ACTIVITY_KIND_MEMCPY") {
        return Ok(());
    }
    let mut stmt = conn
        .prepare(
            "SELECT start, end, COALESCE(deviceId, 0), COALESCE(bytes, 0), COALESCE(copyKind, 0) \
             FROM CUPTI_ACTIVITY_KIND_MEMCPY",
        )
        .map_err(|e| CollectorError::Parse(format!("prepare memcpy: {e}")))?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)? as u32,
                r.get::<_, i64>(3)? as u64,
                r.get::<_, i64>(4)? as u32,
            ))
        })
        .map_err(|e| CollectorError::Parse(format!("query memcpy: {e}")))?;
    for row in rows {
        let (start, end, device, bytes, copy_kind) =
            row.map_err(|e| CollectorError::Parse(format!("read memcpy row: {e}")))?;
        let kind = match copy_kind {
            // CUPTI copyKind constants (cupti_activity.h, CUpti_ActivityMemcpyKind):
            //   1 = HtoD, 2 = DtoH, 3 = HtoA, 4 = AtoH, 5 = AtoA, 6 = AtoD,
            //   7 = DtoA, 8 = DtoD, 9 = HtoH, 10 = PtoP (DtoD across devices),
            //   11 = UNIFIED.
            1 | 3 => TransferKind::H2D,
            2 | 4 => TransferKind::D2H,
            8 | 10 => TransferKind::D2D,
            11 => TransferKind::UnifiedAccess,
            // Anything else (including HtoH=9 or unknown future codes) is
            // surfaced under H2D since it is the common host->device direction.
            _ => TransferKind::H2D,
        };
        out.push(TimelineEvent {
            source_idx: 0,
            t_start_ns: rel_ns(start, t0),
            t_end_ns: rel_ns(end, t0),
            category: EventCategory::GpuMemTransfer,
            lane_hint: device.min(u16::MAX as u32) as u16,
            payload: EventPayload::GpuMemTransfer {
                device_id: device,
                kind,
                bytes,
            },
        });
    }
    Ok(())
}

fn parse_nvtx_rows(
    conn: &Connection,
    names: &mut NameInterner,
    t0: i64,
    out: &mut Vec<TimelineEvent>,
) -> Result<(), CollectorError> {
    if !table_exists(conn, "NVTX_EVENTS") {
        return Ok(());
    }
    let mut stmt = conn
        .prepare(
            "SELECT start, COALESCE(end, start), COALESCE(text, '<unknown>'), \
                    COALESCE(color, 0) \
             FROM NVTX_EVENTS",
        )
        .map_err(|e| CollectorError::Parse(format!("prepare nvtx: {e}")))?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)? as u32,
            ))
        })
        .map_err(|e| CollectorError::Parse(format!("query nvtx: {e}")))?;
    for row in rows {
        let (start, end, text, color) =
            row.map_err(|e| CollectorError::Parse(format!("read nvtx row: {e}")))?;
        let name_id = names.intern(&text);
        out.push(TimelineEvent {
            source_idx: 0,
            t_start_ns: rel_ns(start, t0),
            t_end_ns: rel_ns(end, t0),
            category: EventCategory::NvtxRange,
            lane_hint: 0,
            payload: EventPayload::NvtxRange {
                device_id: 0,
                name_id,
                color,
            },
        });
    }
    Ok(())
}

fn parse_gpu_metric_rows(
    conn: &Connection,
    t0: i64,
    out: &mut Vec<TimelineEvent>,
) -> Result<(), CollectorError> {
    if !table_exists(conn, "GPU_METRICS") {
        return Ok(());
    }
    let mut stmt = conn
        .prepare(
            "SELECT timestamp, deviceId, smActive, dramActive, fbUsed, gpuPower \
             FROM GPU_METRICS ORDER BY timestamp",
        )
        .map_err(|e| CollectorError::Parse(format!("prepare gpu_metrics: {e}")))?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)? as u32,
                r.get::<_, f64>(2)? as f32,
                r.get::<_, f64>(3)? as f32,
                r.get::<_, i64>(4)? as u64,
                r.get::<_, f64>(5)? as f32,
            ))
        })
        .map_err(|e| CollectorError::Parse(format!("query gpu_metrics: {e}")))?;
    for row in rows {
        let (ts, device, sm, mem, fb, power) =
            row.map_err(|e| CollectorError::Parse(format!("read gpu_metric row: {e}")))?;
        let t_rel = rel_ns(ts, t0);
        // Util sample.
        out.push(TimelineEvent {
            source_idx: 0,
            t_start_ns: t_rel,
            t_end_ns: t_rel,
            category: EventCategory::GpuUtilSample,
            lane_hint: device.min(u16::MAX as u32) as u16,
            payload: EventPayload::GpuUtilSample {
                device_id: device,
                compute_pct: sm.clamp(0.0, 100.0),
                mem_pct: mem.clamp(0.0, 100.0),
                mem_used_bytes: fb,
                temp_c: 0.0,
            },
        });
        // Power sample (only when nsys recorded a non-zero reading).
        if power > 0.0 {
            out.push(TimelineEvent {
                source_idx: 0,
                t_start_ns: t_rel,
                t_end_ns: t_rel,
                category: EventCategory::PowerSample,
                lane_hint: device.min(u16::MAX as u32) as u16,
                payload: EventPayload::PowerSample {
                    domain: PowerDomain::Gpu(device),
                    watts: power,
                },
            });
        }
    }
    Ok(())
}

// Silence dead-code lints for fields used only for documentation / future
// vendor dispatch: `GpuVendor` re-export is consumed by callers via
// `crate::profiling::collectors::nvidia_nsys`.
#[allow(dead_code)]
const _NVIDIA_VENDOR_TAG: GpuVendor = GpuVendor::Nvidia;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profiling::collectors::{FrameInterner, NameInterner, RawCapture, SessionCtx};
    use std::collections::HashMap;
    use std::path::PathBuf;
    use tentaflow_protocol::profiling::{
        ClockSamples, GpuTargets, ProfileScope, ProfileSourceFlags, ProfileTarget,
    };

    fn make_ctx() -> SessionCtx {
        SessionCtx {
            session_id: "deadbeefdeadbeef".into(),
            t0_monotonic_ns: 0,
            t0_wallclock_unix_ns: 0,
            output_dir: PathBuf::from("/tmp"),
            scope: ProfileScope {
                sources: ProfileSourceFlags(ProfileSourceFlags::GPU),
                gpu_targets: GpuTargets::All,
                cpu_sampling_hz: 99,
                target: ProfileTarget::OwnProcess,
                duration_seconds: 0,
                label: "t".into(),
            },
            target_pid: None,
            elevation: None,
            planned_duration_ns: 0,
        }
    }

    fn run_parser_against_db(db: Connection) -> Vec<TimelineEvent> {
        // Fake the SQLite export: we reach into the parse helpers directly so
        // we can use an in-memory DB without going through `nsys export`.
        let mut names = NameInterner::new();
        let mut events: Vec<TimelineEvent> = Vec::new();
        let t0 = read_session_t0(&db);
        parse_kernel_rows(&db, &mut names, t0, &mut events).unwrap();
        parse_runtime_rows(&db, &mut names, t0, &mut events).unwrap();
        parse_memcpy_rows(&db, t0, &mut events).unwrap();
        parse_nvtx_rows(&db, &mut names, t0, &mut events).unwrap();
        parse_gpu_metric_rows(&db, t0, &mut events).unwrap();
        events
    }

    fn run_full_parse(db: Connection) -> (Vec<TimelineEvent>, NameInterner) {
        let mut names = NameInterner::new();
        let mut events: Vec<TimelineEvent> = Vec::new();
        let t0 = read_session_t0(&db);
        parse_kernel_rows(&db, &mut names, t0, &mut events).unwrap();
        parse_runtime_rows(&db, &mut names, t0, &mut events).unwrap();
        parse_memcpy_rows(&db, t0, &mut events).unwrap();
        parse_nvtx_rows(&db, &mut names, t0, &mut events).unwrap();
        parse_gpu_metric_rows(&db, t0, &mut events).unwrap();
        (events, names)
    }

    #[test]
    fn parser_handles_empty_sqlite() {
        let conn = Connection::open_in_memory().unwrap();
        let events = run_parser_against_db(conn);
        assert!(events.is_empty());
    }

    #[test]
    fn parser_emits_kernel_events() {
        let conn = Connection::open_in_memory().unwrap();
        // Schema without StringIds so the parser falls back to inline name.
        conn.execute(
            "CREATE TABLE TARGET_INFO_SESSION_START_TIME (value INTEGER)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO TARGET_INFO_SESSION_START_TIME (value) VALUES (1000)",
            [],
        )
        .unwrap();
        conn.execute(
            "CREATE TABLE CUPTI_ACTIVITY_KIND_KERNEL (\
                start INTEGER, end INTEGER, shortName TEXT, deviceId INTEGER, \
                gridX INTEGER, gridY INTEGER, gridZ INTEGER, \
                blockX INTEGER, blockY INTEGER, blockZ INTEGER, \
                staticSharedMemory INTEGER)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO CUPTI_ACTIVITY_KIND_KERNEL VALUES \
             (1500, 2500, 'matmul', 0, 8, 1, 1, 32, 32, 1, 4096), \
             (3000, 3500, 'reduce', 1, 4, 1, 1, 64, 1, 1, 0)",
            [],
        )
        .unwrap();
        let (events, names) = run_full_parse(conn);
        assert_eq!(events.len(), 2);
        let names_vec = names.into_vec();
        assert!(names_vec.contains(&"matmul".to_string()));
        assert!(names_vec.contains(&"reduce".to_string()));
        // Timestamps are relative to t0 = 1000.
        assert_eq!(events[0].t_start_ns, 500);
        assert_eq!(events[0].t_end_ns, 1500);
        if let EventPayload::GpuKernel {
            device_id,
            grid,
            block,
            shared_mem_bytes,
            ..
        } = &events[0].payload
        {
            assert_eq!(*device_id, 0);
            assert_eq!(*grid, [8, 1, 1]);
            assert_eq!(*block, [32, 32, 1]);
            assert_eq!(*shared_mem_bytes, 4096);
        } else {
            panic!("expected GpuKernel payload");
        }
    }

    #[test]
    fn parser_emits_memcpy_events() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute(
            "CREATE TABLE CUPTI_ACTIVITY_KIND_MEMCPY (\
                start INTEGER, end INTEGER, deviceId INTEGER, bytes INTEGER, copyKind INTEGER)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO CUPTI_ACTIVITY_KIND_MEMCPY VALUES \
             (100, 200, 0, 1024, 1), \
             (300, 400, 0, 2048, 2), \
             (500, 600, 0, 4096, 8), \
             (700, 800, 0, 8192, 11)",
            [],
        )
        .unwrap();
        let events = run_parser_against_db(conn);
        assert_eq!(events.len(), 4);
        let kinds: Vec<TransferKind> = events
            .iter()
            .map(|e| match &e.payload {
                EventPayload::GpuMemTransfer { kind, .. } => *kind,
                _ => panic!("wrong payload"),
            })
            .collect();
        assert_eq!(
            kinds,
            vec![
                TransferKind::H2D,
                TransferKind::D2H,
                TransferKind::D2D,
                TransferKind::UnifiedAccess,
            ]
        );
    }

    #[test]
    fn parser_emits_nvtx_ranges() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute(
            "CREATE TABLE NVTX_EVENTS (start INTEGER, end INTEGER, text TEXT, color INTEGER)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO NVTX_EVENTS VALUES (10, 20, 'phase1', 255), (30, 40, 'phase2', 65280)",
            [],
        )
        .unwrap();
        let events = run_parser_against_db(conn);
        assert_eq!(events.len(), 2);
        match &events[0].payload {
            EventPayload::NvtxRange { color, .. } => assert_eq!(*color, 255),
            _ => panic!("expected NvtxRange"),
        }
    }

    #[test]
    fn parser_emits_api_call_events() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute(
            "CREATE TABLE CUPTI_ACTIVITY_KIND_RUNTIME (\
                start INTEGER, end INTEGER, name TEXT, returnValue INTEGER)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO CUPTI_ACTIVITY_KIND_RUNTIME VALUES \
             (10, 12, 'cudaMalloc', 0), (20, 22, 'cudaMemcpy', 1)",
            [],
        )
        .unwrap();
        let events = run_parser_against_db(conn);
        assert_eq!(events.len(), 2);
        match &events[0].payload {
            EventPayload::GpuApiCall { return_code, .. } => assert_eq!(*return_code, 0),
            _ => panic!("expected GpuApiCall"),
        }
        match &events[1].payload {
            EventPayload::GpuApiCall { return_code, .. } => assert_eq!(*return_code, 1),
            _ => panic!("expected GpuApiCall"),
        }
    }

    #[test]
    fn parser_intern_dedupes_kernel_names() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute(
            "CREATE TABLE CUPTI_ACTIVITY_KIND_KERNEL (\
                start INTEGER, end INTEGER, shortName TEXT, deviceId INTEGER, \
                gridX INTEGER, gridY INTEGER, gridZ INTEGER, \
                blockX INTEGER, blockY INTEGER, blockZ INTEGER, \
                staticSharedMemory INTEGER)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO CUPTI_ACTIVITY_KIND_KERNEL VALUES \
             (1, 2, 'kernel_x', 0, 1, 1, 1, 1, 1, 1, 0), \
             (3, 4, 'kernel_x', 0, 1, 1, 1, 1, 1, 1, 0)",
            [],
        )
        .unwrap();
        let (events, names) = run_full_parse(conn);
        assert_eq!(events.len(), 2);
        // Both events share the same name_id.
        let ids: Vec<u32> = events
            .iter()
            .filter_map(|e| match &e.payload {
                EventPayload::GpuKernel { name_id, .. } => Some(*name_id),
                _ => None,
            })
            .collect();
        assert_eq!(ids[0], ids[1]);
        // Only one unique entry in the interner.
        assert_eq!(names.into_vec().len(), 1);
    }

    #[test]
    fn parser_handles_missing_session_info() {
        // No TARGET_INFO_SESSION_START_TIME, no ANALYSIS_DETAILS — the
        // fallback uses MIN(start) of any present table; for an empty DB
        // it returns 0 and parsing succeeds.
        let conn = Connection::open_in_memory().unwrap();
        conn.execute(
            "CREATE TABLE CUPTI_ACTIVITY_KIND_KERNEL (\
                start INTEGER, end INTEGER, shortName TEXT, deviceId INTEGER, \
                gridX INTEGER, gridY INTEGER, gridZ INTEGER, \
                blockX INTEGER, blockY INTEGER, blockZ INTEGER, \
                staticSharedMemory INTEGER)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO CUPTI_ACTIVITY_KIND_KERNEL VALUES \
             (5000, 6000, 'k', 0, 1, 1, 1, 1, 1, 1, 0)",
            [],
        )
        .unwrap();
        let (events, _) = run_full_parse(conn);
        assert_eq!(events.len(), 1);
        // t0 falls back to MIN(start) = 5000, so first event is at t=0.
        assert_eq!(events[0].t_start_ns, 0);
        assert_eq!(events[0].t_end_ns, 1000);
    }

    #[test]
    fn parser_emits_gpu_util_and_power_samples() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute(
            "CREATE TABLE GPU_METRICS (\
                timestamp INTEGER, deviceId INTEGER, smActive REAL, dramActive REAL, \
                fbUsed INTEGER, gpuPower REAL)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO GPU_METRICS VALUES \
             (100, 0, 50.0, 30.0, 1048576, 250.0), \
             (200, 0, 80.0, 60.0, 2097152, 0.0)",
            [],
        )
        .unwrap();
        let events = run_parser_against_db(conn);
        // 2 util samples + 1 power sample (second has gpuPower=0 and is skipped).
        assert_eq!(events.len(), 3);
        let cats: Vec<EventCategory> = events.iter().map(|e| e.category).collect();
        assert_eq!(
            cats.iter()
                .filter(|c| **c == EventCategory::GpuUtilSample)
                .count(),
            2
        );
        assert_eq!(
            cats.iter()
                .filter(|c| **c == EventCategory::PowerSample)
                .count(),
            1
        );
    }

    #[test]
    fn parser_top_level_no_artifacts_returns_empty() {
        // The high-level entry point also tolerates missing artifacts.
        let p = NvidiaNsysParser;
        let raw = RawCapture {
            artifacts: Vec::new(),
            metadata: HashMap::new(),
            clock_samples: ClockSamples {
                collector_id: "x".into(),
                pairs: Vec::new(),
            },
            samples_observed: 0,
        };
        let mut names = NameInterner::new();
        let mut frames = FrameInterner::new();
        let ctx = make_ctx();
        let evs = p.parse(raw, &ctx, &mut names, &mut frames).unwrap();
        assert!(evs.is_empty());
    }
}
