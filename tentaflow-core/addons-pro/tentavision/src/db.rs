// =============================================================================
// File: addons-pro/tentavision/src/db.rs
// TentaVision SQLite access layer. Typed CRUD over the per-addon database via
// the sql_* host functions. Cameras are implemented now; later tabs (profiles,
// alarms, zones, models, audit, evidence, settings) add their CRUD alongside,
// reusing the SqlValue/row-decoding helpers below.
// =============================================================================

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

use serde_json::{self, json, Value as JsonValue};

use crate::AbiError;

// =============================================================================
// SQL host imports — JSON wire (params array in, {rows|row|rows_affected} out).
// Matches tentaflow-core/src/addon/host_functions/sql.rs and the addon-sdk
// SqlValue JSON mapping (TEXT/INTEGER/REAL/NULL; BLOB as {"$bytes": base64}).
// =============================================================================

#[link(wasm_import_module = "tentaflow")]
extern "C" {
    fn sql_exec_v1(
        query_ptr: i32, query_len: i32,
        params_json_ptr: i32, params_json_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;
    fn sql_query_v1(
        query_ptr: i32, query_len: i32,
        params_json_ptr: i32, params_json_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;
}

/// A single SQL value, mirroring the JSON wire mapping the host expects.
#[derive(Debug, Clone)]
pub enum SqlValue {
    Null,
    I64(i64),
    F64(f64),
    Text(String),
}

impl SqlValue {
    fn to_json(&self) -> JsonValue {
        match self {
            SqlValue::Null => JsonValue::Null,
            SqlValue::I64(v) => json!(v),
            SqlValue::F64(v) => json!(v),
            SqlValue::Text(s) => JsonValue::String(s.clone()),
        }
    }

    fn from_json(v: &JsonValue) -> SqlValue {
        match v {
            JsonValue::Null => SqlValue::Null,
            JsonValue::Bool(b) => SqlValue::I64(i64::from(*b)),
            JsonValue::Number(n) => n
                .as_i64()
                .map(SqlValue::I64)
                .or_else(|| n.as_f64().map(SqlValue::F64))
                .unwrap_or(SqlValue::Null),
            JsonValue::String(s) => SqlValue::Text(s.clone()),
            _ => SqlValue::Null,
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            SqlValue::Text(s) => s,
            _ => "",
        }
    }

    pub fn as_i64(&self) -> i64 {
        match self {
            SqlValue::I64(v) => *v,
            SqlValue::F64(v) => *v as i64,
            _ => 0,
        }
    }
}

/// A result row — values in column order of the SELECT.
pub type Row = Vec<SqlValue>;

const SQL_BUF: usize = 65536;

fn params_to_json(params: &[SqlValue]) -> String {
    let arr: Vec<JsonValue> = params.iter().map(SqlValue::to_json).collect();
    serde_json::to_string(&arr).unwrap_or_else(|_| "[]".into())
}

fn call_sql(
    host_fn: unsafe extern "C" fn(i32, i32, i32, i32, i32, i32, i32) -> i32,
    query: &str,
    params: &[SqlValue],
) -> Result<Vec<u8>, AbiError> {
    let params_json = params_to_json(params);
    let q = query.as_bytes();
    let p = params_json.as_bytes();
    let mut cap = SQL_BUF;
    loop {
        let mut out = alloc::vec![0u8; cap];
        let mut out_len: i32 = 0;
        let ret = unsafe {
            host_fn(
                q.as_ptr() as i32, q.len() as i32,
                p.as_ptr() as i32, p.len() as i32,
                out.as_mut_ptr() as i32, out.len() as i32,
                &mut out_len as *mut i32 as i32,
            )
        };
        // OutputBufferTooSmall == 6: host wrote the required size into out_len.
        if ret == 6 {
            cap = (out_len as usize).max(cap.saturating_mul(2));
            continue;
        }
        if ret != 0 {
            return Err(AbiError::from_code(ret));
        }
        out.truncate(out_len as usize);
        return Ok(out);
    }
}

/// Executes a DML statement (INSERT/UPDATE/DELETE). Returns rows affected.
pub fn exec(query: &str, params: &[SqlValue]) -> Result<u64, AbiError> {
    let bytes = call_sql(sql_exec_v1, query, params)?;
    let v: JsonValue = serde_json::from_slice(&bytes).map_err(|_| AbiError::Operation)?;
    Ok(v.get("rows_affected").and_then(JsonValue::as_u64).unwrap_or(0))
}

/// Runs a SELECT and decodes every row.
pub fn query(query_str: &str, params: &[SqlValue]) -> Result<Vec<Row>, AbiError> {
    let bytes = call_sql(sql_query_v1, query_str, params)?;
    let v: JsonValue = serde_json::from_slice(&bytes).map_err(|_| AbiError::Operation)?;
    let rows = v.get("rows").and_then(JsonValue::as_array).cloned().unwrap_or_default();
    Ok(rows
        .iter()
        .map(|row| {
            row.as_array()
                .map(|cols| cols.iter().map(SqlValue::from_json).collect())
                .unwrap_or_default()
        })
        .collect())
}

// =============================================================================
// Stable id generation
// =============================================================================

/// Generates a stable, collision-resistant id for a new row. SQLite has no
/// app-visible UUID, so we combine a per-process monotonic counter with the
/// current unix time pulled from the database (`unixepoch()`), which is the
/// authoritative clock the rows themselves are stamped with. Format:
/// `<prefix>-<unixsecs>-<counter>` (e.g. `cam-1718200000-3`).
pub fn generate_id(prefix: &str) -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let now = now_secs();
    alloc::format!("{}-{}-{}", prefix, now, n)
}

/// Current unix time in seconds, authoritative from SQLite. Falls back to 0 if
/// the database is unreachable (the row write will then carry a 0 timestamp
/// rather than failing the insert).
pub fn now_secs() -> i64 {
    match query("SELECT unixepoch()", &[]) {
        Ok(rows) => rows.first().and_then(|r| r.first()).map(SqlValue::as_i64).unwrap_or(0),
        Err(_) => 0,
    }
}

// =============================================================================
// Cameras CRUD
// =============================================================================

/// A camera row as persisted in SQLite.
#[derive(Debug, Clone)]
pub struct CameraRow {
    pub id: String,
    pub name: String,
    pub location: String,
    pub rtsp_url: String,
    pub onvif_url: String,
    pub status: String,
    pub fps: i64,
    pub detectors: String,
    pub created_at: i64,
    pub updated_at: i64,
}

/// Fields needed to create a camera. id/timestamps are filled by `insert_camera`.
#[derive(Debug, Clone)]
pub struct NewCamera {
    pub name: String,
    pub location: String,
    pub rtsp_url: String,
    pub onvif_url: String,
    pub status: String,
    pub fps: i64,
    pub detectors: String,
}

const CAMERA_COLS: &str =
    "id, name, location, rtsp_url, onvif_url, status, fps, detectors, created_at, updated_at";

fn row_to_camera(r: &Row) -> CameraRow {
    let g = |i: usize| r.get(i).cloned().unwrap_or(SqlValue::Null);
    CameraRow {
        id: g(0).as_str().into(),
        name: g(1).as_str().into(),
        location: g(2).as_str().into(),
        rtsp_url: g(3).as_str().into(),
        onvif_url: g(4).as_str().into(),
        status: g(5).as_str().into(),
        fps: g(6).as_i64(),
        detectors: g(7).as_str().into(),
        created_at: g(8).as_i64(),
        updated_at: g(9).as_i64(),
    }
}

/// Lists all cameras ordered by name.
pub fn list_cameras() -> Result<Vec<CameraRow>, AbiError> {
    let sql = alloc::format!("SELECT {CAMERA_COLS} FROM cameras ORDER BY name");
    let rows = query(&sql, &[])?;
    Ok(rows.iter().map(row_to_camera).collect())
}

/// Fetches a single camera by id, or None.
pub fn get_camera(id: &str) -> Result<Option<CameraRow>, AbiError> {
    let sql = alloc::format!("SELECT {CAMERA_COLS} FROM cameras WHERE id = ?1");
    let rows = query(&sql, &[SqlValue::Text(id.into())])?;
    Ok(rows.first().map(row_to_camera))
}

/// Inserts a new camera, returning its generated id. created_at/updated_at are
/// stamped from the authoritative SQLite clock.
pub fn insert_camera(c: &NewCamera) -> Result<String, AbiError> {
    let id = generate_id("cam");
    let now = now_secs();
    exec(
        "INSERT INTO cameras \
         (id, name, location, rtsp_url, onvif_url, status, fps, detectors, created_at, updated_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9)",
        &[
            SqlValue::Text(id.clone()),
            SqlValue::Text(c.name.clone()),
            SqlValue::Text(c.location.clone()),
            SqlValue::Text(c.rtsp_url.clone()),
            SqlValue::Text(c.onvif_url.clone()),
            SqlValue::Text(c.status.clone()),
            SqlValue::I64(c.fps),
            SqlValue::Text(c.detectors.clone()),
            SqlValue::I64(now),
        ],
    )?;
    Ok(id)
}

/// Updates an existing camera in place; bumps updated_at.
pub fn update_camera(c: &CameraRow) -> Result<u64, AbiError> {
    let now = now_secs();
    exec(
        "UPDATE cameras SET name = ?2, location = ?3, rtsp_url = ?4, onvif_url = ?5, \
         status = ?6, fps = ?7, detectors = ?8, updated_at = ?9 WHERE id = ?1",
        &[
            SqlValue::Text(c.id.clone()),
            SqlValue::Text(c.name.clone()),
            SqlValue::Text(c.location.clone()),
            SqlValue::Text(c.rtsp_url.clone()),
            SqlValue::Text(c.onvif_url.clone()),
            SqlValue::Text(c.status.clone()),
            SqlValue::I64(c.fps),
            SqlValue::Text(c.detectors.clone()),
            SqlValue::I64(now),
        ],
    )
}

/// Deletes a camera by id. Returns rows affected (0 if it did not exist).
pub fn delete_camera(id: &str) -> Result<u64, AbiError> {
    exec("DELETE FROM cameras WHERE id = ?1", &[SqlValue::Text(id.into())])
}

// =============================================================================
// Dashboard aggregates (read-only)
// =============================================================================

/// Returns the first column of the first row as an i64 (for COUNT() queries),
/// or 0 when the result set is empty.
fn scalar_i64(sql: &str, params: &[SqlValue]) -> Result<i64, AbiError> {
    let rows = query(sql, params)?;
    Ok(rows.first().and_then(|r| r.first()).map(SqlValue::as_i64).unwrap_or(0))
}

/// Total number of configured cameras.
pub fn count_cameras() -> Result<i64, AbiError> {
    scalar_i64("SELECT COUNT(*) FROM cameras", &[])
}

/// Number of cameras whose last known status is `online`.
pub fn count_online_cameras() -> Result<i64, AbiError> {
    scalar_i64("SELECT COUNT(*) FROM cameras WHERE status = 'online'", &[])
}

/// Number of alarms raised in the last 24 hours (ts >= now - 86400).
pub fn count_alarms_last_24h() -> Result<i64, AbiError> {
    scalar_i64(
        "SELECT COUNT(*) FROM alarms WHERE ts >= unixepoch() - 86400",
        &[],
    )
}

/// Number of critical alarms in the last 24 hours.
pub fn count_critical_alarms_last_24h() -> Result<i64, AbiError> {
    scalar_i64(
        "SELECT COUNT(*) FROM alarms WHERE severity = 'critical' AND ts >= unixepoch() - 86400",
        &[],
    )
}

/// Number of analytic profiles that are enabled (active detectors).
pub fn count_active_profiles() -> Result<i64, AbiError> {
    scalar_i64("SELECT COUNT(*) FROM profiles WHERE enabled = 1", &[])
}

/// An alarm row joined with its camera's display name for the dashboard list.
#[derive(Debug, Clone)]
pub struct AlarmRow {
    pub id: String,
    pub camera_id: String,
    pub camera_name: String,
    pub severity: String,
    pub kind: String,
    pub message: String,
    pub ts: i64,
}

/// Lists the most recent alarms (newest first), left-joining the camera name so
/// the card can show a friendly label instead of the raw camera id.
pub fn list_recent_alarms(limit: i64) -> Result<Vec<AlarmRow>, AbiError> {
    let sql = "SELECT a.id, a.camera_id, COALESCE(c.name, ''), a.severity, a.type, a.message, a.ts \
               FROM alarms a LEFT JOIN cameras c ON c.id = a.camera_id \
               ORDER BY a.ts DESC LIMIT ?1";
    let rows = query(sql, &[SqlValue::I64(limit)])?;
    Ok(rows
        .iter()
        .map(|r| {
            let g = |i: usize| r.get(i).cloned().unwrap_or(SqlValue::Null);
            AlarmRow {
                id: g(0).as_str().into(),
                camera_id: g(1).as_str().into(),
                camera_name: g(2).as_str().into(),
                severity: g(3).as_str().into(),
                kind: g(4).as_str().into(),
                message: g(5).as_str().into(),
                ts: g(6).as_i64(),
            }
        })
        .collect())
}

/// One aggregated heatmap bucket: alarm count for a camera in a given hour
/// offset (0 = oldest of the 24h window, 23 = current hour).
#[derive(Debug, Clone)]
pub struct AlarmHeatBucket {
    pub camera_id: String,
    pub hour_offset: i64,
    pub count: i64,
}

/// Aggregates alarm counts per camera per hour over the last 24h. `hour_offset`
/// is `(alarm_hour - window_start_hour)` clamped to 0..=23, so column 23 is the
/// current hour. Cameras with zero alarms simply do not appear here; the caller
/// fills the rest of the grid with zero cells.
pub fn alarm_heatmap_last_24h() -> Result<Vec<AlarmHeatBucket>, AbiError> {
    // unixepoch() - 86400 is the window start; integer-divide both the alarm ts
    // and the window start by 3600 to bucket into whole hours, then subtract.
    let sql = "SELECT camera_id, \
               CAST((ts / 3600) - ((unixepoch() - 86400) / 3600) AS INTEGER) AS hour_offset, \
               COUNT(*) AS cnt \
               FROM alarms \
               WHERE ts >= unixepoch() - 86400 \
               GROUP BY camera_id, hour_offset";
    let rows = query(sql, &[])?;
    Ok(rows
        .iter()
        .map(|r| {
            let g = |i: usize| r.get(i).cloned().unwrap_or(SqlValue::Null);
            AlarmHeatBucket {
                camera_id: g(0).as_str().into(),
                hour_offset: g(1).as_i64().clamp(0, 23),
                count: g(2).as_i64(),
            }
        })
        .collect())
}

/// Inserts an alarm. Used by the dashboard's own seeding flow / future analytics
/// integration. created with status='new'. Returns the generated id.
pub fn insert_alarm(
    camera_id: &str,
    severity: &str,
    kind: &str,
    message: &str,
    ts: i64,
) -> Result<String, AbiError> {
    let id = generate_id("alm");
    exec(
        "INSERT INTO alarms (id, camera_id, severity, type, message, thumb_ref, ts, status) \
         VALUES (?1, ?2, ?3, ?4, ?5, '', ?6, 'new')",
        &[
            SqlValue::Text(id.clone()),
            SqlValue::Text(camera_id.into()),
            SqlValue::Text(severity.into()),
            SqlValue::Text(kind.into()),
            SqlValue::Text(message.into()),
            SqlValue::I64(ts),
        ],
    )?;
    Ok(id)
}
