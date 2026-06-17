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
    /// Per-camera AI analysis frame rate (`0` = unlimited / native cadence).
    pub analysis_fps: i64,
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
    /// Per-camera AI analysis frame rate (`0` = unlimited / native cadence).
    pub analysis_fps: i64,
}

const CAMERA_COLS: &str =
    "id, name, location, rtsp_url, onvif_url, status, fps, detectors, analysis_fps, \
     created_at, updated_at";

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
        analysis_fps: g(8).as_i64(),
        created_at: g(9).as_i64(),
        updated_at: g(10).as_i64(),
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
    insert_camera_with_id(&id, c)?;
    Ok(id)
}

/// Inserts a camera under an EXPLICIT id — used when the core ingest supervisor
/// owns the authoritative `cam_<uuid>` id, so the addon row, the live
/// `camera:<id>` stream and the detection overlay all key on the same id.
pub fn insert_camera_with_id(id: &str, c: &NewCamera) -> Result<(), AbiError> {
    let now = now_secs();
    exec(
        "INSERT INTO cameras \
         (id, name, location, rtsp_url, onvif_url, status, fps, detectors, analysis_fps, \
          created_at, updated_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10)",
        &[
            SqlValue::Text(id.into()),
            SqlValue::Text(c.name.clone()),
            SqlValue::Text(c.location.clone()),
            SqlValue::Text(c.rtsp_url.clone()),
            SqlValue::Text(c.onvif_url.clone()),
            SqlValue::Text(c.status.clone()),
            SqlValue::I64(c.fps),
            SqlValue::Text(c.detectors.clone()),
            SqlValue::I64(c.analysis_fps),
            SqlValue::I64(now),
        ],
    )?;
    Ok(())
}

/// Updates an existing camera in place; bumps updated_at.
pub fn update_camera(c: &CameraRow) -> Result<u64, AbiError> {
    let now = now_secs();
    exec(
        "UPDATE cameras SET name = ?2, location = ?3, rtsp_url = ?4, onvif_url = ?5, \
         status = ?6, fps = ?7, detectors = ?8, analysis_fps = ?9, updated_at = ?10 WHERE id = ?1",
        &[
            SqlValue::Text(c.id.clone()),
            SqlValue::Text(c.name.clone()),
            SqlValue::Text(c.location.clone()),
            SqlValue::Text(c.rtsp_url.clone()),
            SqlValue::Text(c.onvif_url.clone()),
            SqlValue::Text(c.status.clone()),
            SqlValue::I64(c.fps),
            SqlValue::Text(c.detectors.clone()),
            SqlValue::I64(c.analysis_fps),
            SqlValue::I64(now),
        ],
    )
}

/// Updates only the liveness status of a camera (e.g. after a reachability
/// probe). Returns rows affected (0 if the camera does not exist).
pub fn set_camera_status(id: &str, status: &str) -> Result<u64, AbiError> {
    let now = now_secs();
    exec(
        "UPDATE cameras SET status = ?2, updated_at = ?3 WHERE id = ?1",
        &[
            SqlValue::Text(id.into()),
            SqlValue::Text(status.into()),
            SqlValue::I64(now),
        ],
    )
}

/// Updates only the liveness status of a camera (e.g. after a reachability
/// probe). Returns rows affected (0 if the camera does not exist).
pub fn set_camera_status(id: &str, status: &str) -> Result<u64, AbiError> {
    let now = now_secs();
    exec(
        "UPDATE cameras SET status = ?2, updated_at = ?3 WHERE id = ?1",
        &[
            SqlValue::Text(id.into()),
            SqlValue::Text(status.into()),
            SqlValue::I64(now),
        ],
    )
}

/// Deletes a camera by id. Returns rows affected (0 if it did not exist).
pub fn delete_camera(id: &str) -> Result<u64, AbiError> {
    exec("DELETE FROM cameras WHERE id = ?1", &[SqlValue::Text(id.into())])
}

// =============================================================================
// Profiles CRUD
// =============================================================================

/// An analytic profile row as persisted in SQLite. `cameras` is a JSON array of
/// camera ids; `schedule` is a free-form schedule label/blob.
#[derive(Debug, Clone)]
pub struct ProfileRow {
    pub id: String,
    pub name: String,
    pub flow_id: String,
    pub risk_class: String,
    pub schedule: String,
    pub cameras: String,
    pub enabled: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

/// Fields needed to create a profile. id/timestamps are filled by
/// `insert_profile`.
#[derive(Debug, Clone)]
pub struct NewProfile {
    pub name: String,
    pub flow_id: String,
    pub risk_class: String,
    pub schedule: String,
    pub cameras: String,
    pub enabled: bool,
}

const PROFILE_COLS: &str =
    "id, name, flow_id, risk_class, schedule, cameras, enabled, created_at, updated_at";

fn row_to_profile(r: &Row) -> ProfileRow {
    let g = |i: usize| r.get(i).cloned().unwrap_or(SqlValue::Null);
    ProfileRow {
        id: g(0).as_str().into(),
        name: g(1).as_str().into(),
        flow_id: g(2).as_str().into(),
        risk_class: g(3).as_str().into(),
        schedule: g(4).as_str().into(),
        cameras: g(5).as_str().into(),
        enabled: g(6).as_i64() != 0,
        created_at: g(7).as_i64(),
        updated_at: g(8).as_i64(),
    }
}

/// Lists all analytic profiles ordered by name.
pub fn list_profiles() -> Result<Vec<ProfileRow>, AbiError> {
    let sql = alloc::format!("SELECT {PROFILE_COLS} FROM profiles ORDER BY name");
    let rows = query(&sql, &[])?;
    Ok(rows.iter().map(row_to_profile).collect())
}

/// Fetches a single profile by id, or None.
pub fn get_profile(id: &str) -> Result<Option<ProfileRow>, AbiError> {
    let sql = alloc::format!("SELECT {PROFILE_COLS} FROM profiles WHERE id = ?1");
    let rows = query(&sql, &[SqlValue::Text(id.into())])?;
    Ok(rows.first().map(row_to_profile))
}

/// Inserts a new profile, returning its generated id. created_at/updated_at are
/// stamped from the authoritative SQLite clock.
pub fn insert_profile(p: &NewProfile) -> Result<String, AbiError> {
    let id = generate_id("prof");
    let now = now_secs();
    exec(
        "INSERT INTO profiles \
         (id, name, flow_id, risk_class, schedule, cameras, enabled, created_at, updated_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)",
        &[
            SqlValue::Text(id.clone()),
            SqlValue::Text(p.name.clone()),
            SqlValue::Text(p.flow_id.clone()),
            SqlValue::Text(p.risk_class.clone()),
            SqlValue::Text(p.schedule.clone()),
            SqlValue::Text(p.cameras.clone()),
            SqlValue::I64(i64::from(p.enabled)),
            SqlValue::I64(now),
        ],
    )?;
    Ok(id)
}

/// Updates an existing profile in place; bumps updated_at.
pub fn update_profile(p: &ProfileRow) -> Result<u64, AbiError> {
    let now = now_secs();
    exec(
        "UPDATE profiles SET name = ?2, flow_id = ?3, risk_class = ?4, schedule = ?5, \
         cameras = ?6, enabled = ?7, updated_at = ?8 WHERE id = ?1",
        &[
            SqlValue::Text(p.id.clone()),
            SqlValue::Text(p.name.clone()),
            SqlValue::Text(p.flow_id.clone()),
            SqlValue::Text(p.risk_class.clone()),
            SqlValue::Text(p.schedule.clone()),
            SqlValue::Text(p.cameras.clone()),
            SqlValue::I64(i64::from(p.enabled)),
            SqlValue::I64(now),
        ],
    )
}

/// Flips a profile's enabled flag to `enabled`; bumps updated_at. Returns rows
/// affected (0 if the profile did not exist).
pub fn toggle_profile(id: &str, enabled: bool) -> Result<u64, AbiError> {
    let now = now_secs();
    exec(
        "UPDATE profiles SET enabled = ?2, updated_at = ?3 WHERE id = ?1",
        &[
            SqlValue::Text(id.into()),
            SqlValue::I64(i64::from(enabled)),
            SqlValue::I64(now),
        ],
    )
}

/// Deletes a profile by id. Returns rows affected (0 if it did not exist).
pub fn delete_profile(id: &str) -> Result<u64, AbiError> {
    exec("DELETE FROM profiles WHERE id = ?1", &[SqlValue::Text(id.into())])
}

// =============================================================================
// Models CRUD
// =============================================================================

/// An inference model row as persisted in SQLite. `status` is one of
/// active/loaded/loading/error/idle; `vram_mb` is the model's VRAM footprint in
/// megabytes (counted toward the budget only while the model is active/loaded).
#[derive(Debug, Clone)]
pub struct ModelRow {
    pub id: String,
    pub name: String,
    pub runtime: String,
    pub status: String,
    pub vram_mb: i64,
    pub version: String,
    pub created_at: i64,
}

/// Fields needed to create a model. id/created_at are filled by `insert_model`.
#[derive(Debug, Clone)]
pub struct NewModel {
    pub name: String,
    pub runtime: String,
    pub status: String,
    pub vram_mb: i64,
    pub version: String,
}

const MODEL_COLS: &str = "id, name, runtime, status, vram_mb, version, created_at";

fn row_to_model(r: &Row) -> ModelRow {
    let g = |i: usize| r.get(i).cloned().unwrap_or(SqlValue::Null);
    ModelRow {
        id: g(0).as_str().into(),
        name: g(1).as_str().into(),
        runtime: g(2).as_str().into(),
        status: g(3).as_str().into(),
        vram_mb: g(4).as_i64(),
        version: g(5).as_str().into(),
        created_at: g(6).as_i64(),
    }
}

/// Lists all models ordered by name.
pub fn list_models() -> Result<Vec<ModelRow>, AbiError> {
    let sql = alloc::format!("SELECT {MODEL_COLS} FROM models ORDER BY name");
    let rows = query(&sql, &[])?;
    Ok(rows.iter().map(row_to_model).collect())
}

/// Fetches a single model by id, or None.
pub fn get_model(id: &str) -> Result<Option<ModelRow>, AbiError> {
    let sql = alloc::format!("SELECT {MODEL_COLS} FROM models WHERE id = ?1");
    let rows = query(&sql, &[SqlValue::Text(id.into())])?;
    Ok(rows.first().map(row_to_model))
}

/// Inserts a new model, returning its generated id. created_at is stamped from
/// the authoritative SQLite clock.
pub fn insert_model(m: &NewModel) -> Result<String, AbiError> {
    let id = generate_id("mdl");
    let now = now_secs();
    exec(
        "INSERT INTO models (id, name, runtime, status, vram_mb, version, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        &[
            SqlValue::Text(id.clone()),
            SqlValue::Text(m.name.clone()),
            SqlValue::Text(m.runtime.clone()),
            SqlValue::Text(m.status.clone()),
            SqlValue::I64(m.vram_mb),
            SqlValue::Text(m.version.clone()),
            SqlValue::I64(now),
        ],
    )?;
    Ok(id)
}

/// Updates an existing model in place (created_at is immutable).
pub fn update_model(m: &ModelRow) -> Result<u64, AbiError> {
    exec(
        "UPDATE models SET name = ?2, runtime = ?3, status = ?4, vram_mb = ?5, version = ?6 \
         WHERE id = ?1",
        &[
            SqlValue::Text(m.id.clone()),
            SqlValue::Text(m.name.clone()),
            SqlValue::Text(m.runtime.clone()),
            SqlValue::Text(m.status.clone()),
            SqlValue::I64(m.vram_mb),
            SqlValue::Text(m.version.clone()),
        ],
    )
}

/// Deletes a model by id. Returns rows affected (0 if it did not exist).
pub fn delete_model(id: &str) -> Result<u64, AbiError> {
    exec("DELETE FROM models WHERE id = ?1", &[SqlValue::Text(id.into())])
}

/// Sum of VRAM (MB) over models that count toward the live budget — those whose
/// status is active or loaded. idle/error/loading models do not occupy VRAM.
pub fn used_vram_mb() -> Result<i64, AbiError> {
    scalar_i64(
        "SELECT COALESCE(SUM(vram_mb), 0) FROM models WHERE status IN ('active', 'loaded')",
        &[],
    )
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

/// An alarm row joined with its camera's display name. Carries the full decision
/// lifecycle (status / decided_by / decided_at) so the Alarm Center can render
/// both the feed and the detail/workflow panel from a single fetch.
#[derive(Debug, Clone)]
pub struct AlarmRow {
    pub id: String,
    pub camera_id: String,
    pub camera_name: String,
    pub severity: String,
    pub kind: String,
    pub message: String,
    pub thumb_ref: String,
    pub ts: i64,
    pub status: String,
    pub decided_by: String,
    pub decided_at: i64,
}

/// Lists the most recent alarms (newest first), left-joining the camera name so
/// the card can show a friendly label instead of the raw camera id.
pub fn list_recent_alarms(limit: i64) -> Result<Vec<AlarmRow>, AbiError> {
    let sql = alloc::format!(
        "SELECT {ALARM_COLS} FROM alarms a LEFT JOIN cameras c ON c.id = a.camera_id \
         ORDER BY a.ts DESC LIMIT ?1"
    );
    let rows = query(&sql, &[SqlValue::I64(limit)])?;
    Ok(rows.iter().map(row_to_alarm).collect())
}

/// Column list (with the camera-name join) shared by every alarm SELECT so the
/// row decoder stays in lockstep with the query shape.
const ALARM_COLS: &str =
    "a.id, a.camera_id, COALESCE(c.name, ''), a.severity, a.type, a.message, \
     a.thumb_ref, a.ts, a.status, a.decided_by, a.decided_at";

fn row_to_alarm(r: &Row) -> AlarmRow {
    let g = |i: usize| r.get(i).cloned().unwrap_or(SqlValue::Null);
    AlarmRow {
        id: g(0).as_str().into(),
        camera_id: g(1).as_str().into(),
        camera_name: g(2).as_str().into(),
        severity: g(3).as_str().into(),
        kind: g(4).as_str().into(),
        message: g(5).as_str().into(),
        thumb_ref: g(6).as_str().into(),
        ts: g(7).as_i64(),
        status: g(8).as_str().into(),
        decided_by: g(9).as_str().into(),
        decided_at: g(10).as_i64(),
    }
}

/// Fetches a single alarm (with its camera name) by id, or None.
pub fn get_alarm(id: &str) -> Result<Option<AlarmRow>, AbiError> {
    let sql = alloc::format!(
        "SELECT {ALARM_COLS} FROM alarms a LEFT JOIN cameras c ON c.id = a.camera_id \
         WHERE a.id = ?1"
    );
    let rows = query(&sql, &[SqlValue::Text(id.into())])?;
    Ok(rows.first().map(row_to_alarm))
}

/// Lists alarms (newest first) with optional severity and status filters. An
/// empty filter string means "no constraint on that column"; `status_open`
/// collapses the two undecided states (`new`/`acknowledged`) into one feed view.
pub fn list_alarms(severity: &str, status: &str, status_open: bool) -> Result<Vec<AlarmRow>, AbiError> {
    let mut clauses: Vec<String> = Vec::new();
    let mut params: Vec<SqlValue> = Vec::new();
    if !severity.is_empty() {
        clauses.push(alloc::format!("a.severity = ?{}", params.len() + 1));
        params.push(SqlValue::Text(severity.into()));
    }
    if status_open {
        clauses.push("a.status IN ('new', 'acknowledged')".into());
    } else if !status.is_empty() {
        clauses.push(alloc::format!("a.status = ?{}", params.len() + 1));
        params.push(SqlValue::Text(status.into()));
    }
    let where_sql = if clauses.is_empty() {
        String::new()
    } else {
        alloc::format!("WHERE {}", clauses.join(" AND "))
    };
    let sql = alloc::format!(
        "SELECT {ALARM_COLS} FROM alarms a LEFT JOIN cameras c ON c.id = a.camera_id \
         {where_sql} ORDER BY a.ts DESC"
    );
    let rows = query(&sql, &params)?;
    Ok(rows.iter().map(row_to_alarm).collect())
}

/// Structured attribute search over alarms (newest first). Every argument is
/// optional: an empty `severity`/`camera_id`/`text` and a 0 `from`/`to` mean "no
/// constraint on that column". `text` matches case-insensitively against either
/// the alarm type or the message (LIKE %text%). This is the REAL backend for the
/// Search tab's attribute mode — no AI required.
pub fn search_alarms(
    severity: &str,
    text: &str,
    camera_id: &str,
    from: i64,
    to: i64,
) -> Result<Vec<AlarmRow>, AbiError> {
    let mut clauses: Vec<String> = Vec::new();
    let mut params: Vec<SqlValue> = Vec::new();
    if !severity.is_empty() {
        clauses.push(alloc::format!("a.severity = ?{}", params.len() + 1));
        params.push(SqlValue::Text(severity.into()));
    }
    if !camera_id.is_empty() {
        clauses.push(alloc::format!("a.camera_id = ?{}", params.len() + 1));
        params.push(SqlValue::Text(camera_id.into()));
    }
    if !text.trim().is_empty() {
        let like = alloc::format!("%{}%", text.trim().to_lowercase());
        clauses.push(alloc::format!(
            "(LOWER(a.type) LIKE ?{0} OR LOWER(a.message) LIKE ?{0})",
            params.len() + 1
        ));
        params.push(SqlValue::Text(like));
    }
    if from > 0 {
        clauses.push(alloc::format!("a.ts >= ?{}", params.len() + 1));
        params.push(SqlValue::I64(from));
    }
    if to > 0 {
        clauses.push(alloc::format!("a.ts <= ?{}", params.len() + 1));
        params.push(SqlValue::I64(to));
    }
    let where_sql = if clauses.is_empty() {
        String::new()
    } else {
        alloc::format!("WHERE {}", clauses.join(" AND "))
    };
    let sql = alloc::format!(
        "SELECT {ALARM_COLS} FROM alarms a LEFT JOIN cameras c ON c.id = a.camera_id \
         {where_sql} ORDER BY a.ts DESC LIMIT 100"
    );
    let rows = query(&sql, &params)?;
    Ok(rows.iter().map(row_to_alarm).collect())
}

/// Number of alarms matching the open-feed view (undecided) or a concrete status.
pub fn count_alarms(status_open: bool, status: &str) -> Result<i64, AbiError> {
    if status_open {
        scalar_i64("SELECT COUNT(*) FROM alarms WHERE status IN ('new','acknowledged')", &[])
    } else if status.is_empty() {
        scalar_i64("SELECT COUNT(*) FROM alarms", &[])
    } else {
        scalar_i64("SELECT COUNT(*) FROM alarms WHERE status = ?1", &[SqlValue::Text(status.into())])
    }
}

/// Records an operator decision: writes the new status + operator + decision time
/// onto the alarm row. Returns rows affected (0 if the alarm did not exist).
pub fn update_alarm_status(id: &str, status: &str, decided_by: &str) -> Result<u64, AbiError> {
    let now = now_secs();
    exec(
        "UPDATE alarms SET status = ?2, decided_by = ?3, decided_at = ?4 WHERE id = ?1",
        &[
            SqlValue::Text(id.into()),
            SqlValue::Text(status.into()),
            SqlValue::Text(decided_by.into()),
            SqlValue::I64(now),
        ],
    )
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

// =============================================================================
// Audit log (append-only, hash-chained)
// =============================================================================

/// Appends one tamper-evident audit entry. The hash chains over the previous
/// entry's hash plus this row's payload, so the Audit tab can later verify the
/// chain. `before`/`after` carry JSON snapshots of the changed state.
pub fn insert_audit(
    actor: &str,
    action: &str,
    target: &str,
    before: &str,
    after: &str,
) -> Result<String, AbiError> {
    let id = generate_id("aud");
    let ts = now_secs();
    let prev_hash = query("SELECT hash FROM audit_log ORDER BY ts DESC, id DESC LIMIT 1", &[])?
        .first()
        .and_then(|r| r.first())
        .map(SqlValue::as_str)
        .unwrap_or("")
        .to_string();
    // Cheap, deterministic FNV-1a chain hash over prev_hash + payload. Not a
    // cryptographic digest, but enough to make silent row edits detectable.
    let material = alloc::format!(
        "{prev_hash}|{ts}|{actor}|{action}|{target}|{before}|{after}"
    );
    let hash = fnv1a_hex(material.as_bytes());
    exec(
        "INSERT INTO audit_log (id, ts, actor, action, target, before, after, hash, prev_hash) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        &[
            SqlValue::Text(id.clone()),
            SqlValue::I64(ts),
            SqlValue::Text(actor.into()),
            SqlValue::Text(action.into()),
            SqlValue::Text(target.into()),
            SqlValue::Text(before.into()),
            SqlValue::Text(after.into()),
            SqlValue::Text(hash),
            SqlValue::Text(prev_hash),
        ],
    )?;
    Ok(id)
}

fn fnv1a_hex(bytes: &[u8]) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    alloc::format!("{:016x}", h)
}

/// Builds the exact chain-hash material `insert_audit` hashes, so verification
/// recomputes the identical digest. Any drift between these two would make the
/// chain falsely report as broken.
fn audit_hash_material(
    prev_hash: &str,
    ts: i64,
    actor: &str,
    action: &str,
    target: &str,
    before: &str,
    after: &str,
) -> String {
    alloc::format!("{prev_hash}|{ts}|{actor}|{action}|{target}|{before}|{after}")
}

/// One audit-log entry, decoded from the append-only hash-chained table.
#[derive(Debug, Clone)]
pub struct AuditRow {
    pub id: String,
    pub ts: i64,
    pub actor: String,
    pub action: String,
    pub target: String,
    pub before: String,
    pub after: String,
    pub hash: String,
    pub prev_hash: String,
}

const AUDIT_COLS: &str = "id, ts, actor, action, target, before, after, hash, prev_hash";

fn row_to_audit(r: &Row) -> AuditRow {
    let g = |i: usize| r.get(i).cloned().unwrap_or(SqlValue::Null);
    AuditRow {
        id: g(0).as_str().into(),
        ts: g(1).as_i64(),
        actor: g(2).as_str().into(),
        action: g(3).as_str().into(),
        target: g(4).as_str().into(),
        before: g(5).as_str().into(),
        after: g(6).as_str().into(),
        hash: g(7).as_str().into(),
        prev_hash: g(8).as_str().into(),
    }
}

/// Lists audit entries newest-first, with optional case-insensitive substring
/// filters on actor/action and an inclusive `since`/`until` unix-second window.
/// Empty filter strings (and a 0 bound) mean "no constraint on that column".
/// `limit <= 0` lists all matching rows.
pub fn list_audit(
    limit: i64,
    actor: &str,
    action: &str,
    since: i64,
    until: i64,
) -> Result<Vec<AuditRow>, AbiError> {
    let mut clauses: Vec<String> = Vec::new();
    let mut params: Vec<SqlValue> = Vec::new();
    if !actor.trim().is_empty() {
        clauses.push(alloc::format!("LOWER(actor) LIKE ?{}", params.len() + 1));
        params.push(SqlValue::Text(alloc::format!("%{}%", actor.trim().to_lowercase())));
    }
    if !action.trim().is_empty() {
        clauses.push(alloc::format!("LOWER(action) LIKE ?{}", params.len() + 1));
        params.push(SqlValue::Text(alloc::format!("%{}%", action.trim().to_lowercase())));
    }
    if since > 0 {
        clauses.push(alloc::format!("ts >= ?{}", params.len() + 1));
        params.push(SqlValue::I64(since));
    }
    if until > 0 {
        clauses.push(alloc::format!("ts <= ?{}", params.len() + 1));
        params.push(SqlValue::I64(until));
    }
    let where_sql = if clauses.is_empty() {
        String::new()
    } else {
        alloc::format!("WHERE {}", clauses.join(" AND "))
    };
    let limit_sql = if limit > 0 {
        alloc::format!(" LIMIT ?{}", params.len() + 1)
    } else {
        String::new()
    };
    if limit > 0 {
        params.push(SqlValue::I64(limit));
    }
    let sql = alloc::format!(
        "SELECT {AUDIT_COLS} FROM audit_log {where_sql} ORDER BY ts DESC, id DESC{limit_sql}"
    );
    let rows = query(&sql, &params)?;
    Ok(rows.iter().map(row_to_audit).collect())
}

/// Total number of audit entries (unfiltered) — drives the header counter.
pub fn count_audit() -> Result<i64, AbiError> {
    scalar_i64("SELECT COUNT(*) FROM audit_log", &[])
}

/// Outcome of a full chain re-verification.
#[derive(Debug, Clone)]
pub struct ChainStatus {
    /// True when every row's stored hash matches the recomputed FNV-1a digest
    /// AND each row's prev_hash equals the previous row's stored hash.
    pub ok: bool,
    /// Number of rows checked (genesis-to-head).
    pub checked: i64,
    /// 0-based index (oldest = 0) of the first row that fails verification, or
    /// None when the chain is intact. Only meaningful when `ok == false`.
    pub first_broken_index: Option<i64>,
}

/// Recomputes the hash chain genesis-to-head and confirms it is intact. For each
/// row (oldest first) it (1) recomputes `fnv1a(prev_hash + payload)` and checks
/// it equals the stored `hash`, and (2) checks `prev_hash` links to the prior
/// row's stored hash (genesis links to the empty string). Returns the first
/// 0-based index that breaks, so a silent row edit is pinpointed, not just flagged.
pub fn verify_audit_chain() -> Result<ChainStatus, AbiError> {
    // Oldest-first so prev_hash linkage can be checked sequentially.
    let sql = alloc::format!("SELECT {AUDIT_COLS} FROM audit_log ORDER BY ts ASC, id ASC");
    let rows: Vec<AuditRow> = query(&sql, &[])?.iter().map(row_to_audit).collect();
    let mut expected_prev = String::new();
    for (i, row) in rows.iter().enumerate() {
        let recomputed = fnv1a_hex(
            audit_hash_material(
                &row.prev_hash, row.ts, &row.actor, &row.action, &row.target,
                &row.before, &row.after,
            )
            .as_bytes(),
        );
        if recomputed != row.hash || row.prev_hash != expected_prev {
            return Ok(ChainStatus { ok: false, checked: rows.len() as i64, first_broken_index: Some(i as i64) });
        }
        expected_prev = row.hash.clone();
    }
    Ok(ChainStatus { ok: true, checked: rows.len() as i64, first_broken_index: None })
}

// =============================================================================
// Zones CRUD (detection / exclusion / line zones drawn on a camera)
// =============================================================================

/// A detection zone row as persisted in SQLite. `kind` is one of
/// include/exclude/line; `polygon` is a JSON array of `[x, y]` points (in 0..100
/// percentage coordinates relative to the camera frame). The synthetic kinds
/// `schedule` and `rule` reuse this same table (one schedule row per camera, one
/// row per composite rule) with their config carried in `polygon` as JSON.
#[derive(Debug, Clone)]
pub struct ZoneRow {
    pub id: String,
    pub camera_id: String,
    pub name: String,
    pub kind: String,
    pub polygon: String,
    pub created_at: i64,
}

/// Fields needed to create a zone. id/created_at are filled by `insert_zone`.
#[derive(Debug, Clone)]
pub struct NewZone {
    pub camera_id: String,
    pub name: String,
    pub kind: String,
    pub polygon: String,
}

const ZONE_COLS: &str = "id, camera_id, name, kind, polygon, created_at";

fn row_to_zone(r: &Row) -> ZoneRow {
    let g = |i: usize| r.get(i).cloned().unwrap_or(SqlValue::Null);
    ZoneRow {
        id: g(0).as_str().into(),
        camera_id: g(1).as_str().into(),
        name: g(2).as_str().into(),
        kind: g(3).as_str().into(),
        polygon: g(4).as_str().into(),
        created_at: g(5).as_i64(),
    }
}

/// Lists the geometric zones (include/exclude/line) of a camera, oldest first.
/// The synthetic `schedule`/`rule` kinds are excluded so the zone list/canvas
/// only ever sees real drawable zones.
pub fn list_zones(camera_id: &str) -> Result<Vec<ZoneRow>, AbiError> {
    let sql = alloc::format!(
        "SELECT {ZONE_COLS} FROM zones \
         WHERE camera_id = ?1 AND kind IN ('include', 'exclude', 'line') \
         ORDER BY created_at, id"
    );
    let rows = query(&sql, &[SqlValue::Text(camera_id.into())])?;
    Ok(rows.iter().map(row_to_zone).collect())
}

/// Fetches a single zone by id, or None.
pub fn get_zone(id: &str) -> Result<Option<ZoneRow>, AbiError> {
    let sql = alloc::format!("SELECT {ZONE_COLS} FROM zones WHERE id = ?1");
    let rows = query(&sql, &[SqlValue::Text(id.into())])?;
    Ok(rows.first().map(row_to_zone))
}

/// Inserts a new zone, returning its generated id. created_at is stamped from
/// the authoritative SQLite clock.
pub fn insert_zone(z: &NewZone) -> Result<String, AbiError> {
    let id = generate_id("zone");
    let now = now_secs();
    exec(
        "INSERT INTO zones (id, camera_id, name, kind, polygon, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        &[
            SqlValue::Text(id.clone()),
            SqlValue::Text(z.camera_id.clone()),
            SqlValue::Text(z.name.clone()),
            SqlValue::Text(z.kind.clone()),
            SqlValue::Text(z.polygon.clone()),
            SqlValue::I64(now),
        ],
    )?;
    Ok(id)
}

/// Updates an existing zone in place (created_at is immutable).
pub fn update_zone(z: &ZoneRow) -> Result<u64, AbiError> {
    exec(
        "UPDATE zones SET camera_id = ?2, name = ?3, kind = ?4, polygon = ?5 WHERE id = ?1",
        &[
            SqlValue::Text(z.id.clone()),
            SqlValue::Text(z.camera_id.clone()),
            SqlValue::Text(z.name.clone()),
            SqlValue::Text(z.kind.clone()),
            SqlValue::Text(z.polygon.clone()),
        ],
    )
}

/// Deletes a zone by id. Returns rows affected (0 if it did not exist).
pub fn delete_zone(id: &str) -> Result<u64, AbiError> {
    exec("DELETE FROM zones WHERE id = ?1", &[SqlValue::Text(id.into())])
}

// =============================================================================
// Weekly schedule (one row per camera, kind='schedule', polygon = JSON grid)
// =============================================================================

/// Reads the persisted weekly schedule JSON for a camera (the `polygon` column of
/// the camera's `kind='schedule'` row), or None when no schedule was ever saved.
/// The JSON is a 5×7 array of profile codes (`""`/`day`/`night`) — the row index
/// is the hour band, the column index is the weekday.
pub fn get_schedule(camera_id: &str) -> Result<Option<String>, AbiError> {
    let sql = "SELECT polygon FROM zones WHERE camera_id = ?1 AND kind = 'schedule' LIMIT 1";
    let rows = query(sql, &[SqlValue::Text(camera_id.into())])?;
    Ok(rows.first().and_then(|r| r.first()).map(|v| v.as_str().to_string()))
}

/// Upserts the weekly schedule JSON for a camera. There is at most one
/// `kind='schedule'` row per camera; this replaces its grid in place or inserts a
/// fresh row stamped from the SQLite clock.
pub fn set_schedule(camera_id: &str, grid_json: &str) -> Result<(), AbiError> {
    let existing = query(
        "SELECT id FROM zones WHERE camera_id = ?1 AND kind = 'schedule' LIMIT 1",
        &[SqlValue::Text(camera_id.into())],
    )?;
    if let Some(id) = existing.first().and_then(|r| r.first()).map(SqlValue::as_str) {
        exec(
            "UPDATE zones SET polygon = ?2 WHERE id = ?1",
            &[SqlValue::Text(id.into()), SqlValue::Text(grid_json.into())],
        )?;
    } else {
        let id = generate_id("sched");
        let now = now_secs();
        exec(
            "INSERT INTO zones (id, camera_id, name, kind, polygon, created_at) \
             VALUES (?1, ?2, 'schedule', 'schedule', ?3, ?4)",
            &[
                SqlValue::Text(id),
                SqlValue::Text(camera_id.into()),
                SqlValue::Text(grid_json.into()),
                SqlValue::I64(now),
            ],
        )?;
    }
    Ok(())
}

// =============================================================================
// Composite rules (kind='rule', polygon = JSON {name, expr, action, enabled})
// =============================================================================

/// Lists composite rule rows for a camera (kind='rule'), oldest first. Each
/// row's `polygon` carries the rule JSON; the caller decodes it.
pub fn list_rules(camera_id: &str) -> Result<Vec<ZoneRow>, AbiError> {
    let sql = alloc::format!(
        "SELECT {ZONE_COLS} FROM zones WHERE camera_id = ?1 AND kind = 'rule' \
         ORDER BY created_at, id"
    );
    let rows = query(&sql, &[SqlValue::Text(camera_id.into())])?;
    Ok(rows.iter().map(row_to_zone).collect())
}

/// Inserts a composite rule for a camera, returning its generated id. The rule
/// config (name/expr/action/enabled) is JSON-encoded by the caller into `cfg`.
pub fn insert_rule(camera_id: &str, name: &str, cfg: &str) -> Result<String, AbiError> {
    let id = generate_id("rule");
    let now = now_secs();
    exec(
        "INSERT INTO zones (id, camera_id, name, kind, polygon, created_at) \
         VALUES (?1, ?2, ?3, 'rule', ?4, ?5)",
        &[
            SqlValue::Text(id.clone()),
            SqlValue::Text(camera_id.into()),
            SqlValue::Text(name.into()),
            SqlValue::Text(cfg.into()),
            SqlValue::I64(now),
        ],
    )?;
    Ok(id)
}

// =============================================================================
// Evidence packages CRUD (HSM/TSA-signed export records for an alarm)
// =============================================================================

/// An evidence package row joined with its source alarm's message + camera name,
/// so the package list can show a human label for the underlying incident
/// without a second fetch. `signed_by` carries the recipient/organ the package
/// was issued to (empty = pending recipient assignment).
#[derive(Debug, Clone)]
pub struct EvidenceRow {
    pub id: String,
    pub alarm_id: String,
    pub package_ref: String,
    pub signed_by: String,
    pub created_at: i64,
    pub alarm_message: String,
    pub camera_name: String,
    pub alarm_severity: String,
}

/// Fields needed to create an evidence package. id/package_ref/created_at are
/// filled by `insert_evidence`.
#[derive(Debug, Clone)]
pub struct NewEvidence {
    pub alarm_id: String,
    pub signed_by: String,
}

/// Column list (with the alarm + camera join) shared by every evidence SELECT so
/// the row decoder stays in lockstep with the query shape.
const EVIDENCE_COLS: &str =
    "e.id, e.alarm_id, e.package_ref, e.signed_by, e.created_at, \
     COALESCE(a.message, ''), COALESCE(c.name, ''), COALESCE(a.severity, '')";

fn row_to_evidence(r: &Row) -> EvidenceRow {
    let g = |i: usize| r.get(i).cloned().unwrap_or(SqlValue::Null);
    EvidenceRow {
        id: g(0).as_str().into(),
        alarm_id: g(1).as_str().into(),
        package_ref: g(2).as_str().into(),
        signed_by: g(3).as_str().into(),
        created_at: g(4).as_i64(),
        alarm_message: g(5).as_str().into(),
        camera_name: g(6).as_str().into(),
        alarm_severity: g(7).as_str().into(),
    }
}

/// Lists all evidence packages newest-first, left-joining the source alarm and
/// its camera so the package card can render a friendly incident label.
pub fn list_evidence() -> Result<Vec<EvidenceRow>, AbiError> {
    let sql = alloc::format!(
        "SELECT {EVIDENCE_COLS} FROM evidence e \
         LEFT JOIN alarms a ON a.id = e.alarm_id \
         LEFT JOIN cameras c ON c.id = a.camera_id \
         ORDER BY e.created_at DESC, e.id DESC"
    );
    let rows = query(&sql, &[])?;
    Ok(rows.iter().map(row_to_evidence).collect())
}

/// Fetches a single evidence package (with its alarm/camera labels) by id, or None.
pub fn get_evidence(id: &str) -> Result<Option<EvidenceRow>, AbiError> {
    let sql = alloc::format!(
        "SELECT {EVIDENCE_COLS} FROM evidence e \
         LEFT JOIN alarms a ON a.id = e.alarm_id \
         LEFT JOIN cameras c ON c.id = a.camera_id \
         WHERE e.id = ?1"
    );
    let rows = query(&sql, &[SqlValue::Text(id.into())])?;
    Ok(rows.first().map(row_to_evidence))
}

/// Inserts a new evidence package, returning its generated id. The
/// `package_ref` is a generated, human-facing reference (`EV-<unixsecs>-<n>`);
/// created_at is stamped from the authoritative SQLite clock.
pub fn insert_evidence(e: &NewEvidence) -> Result<String, AbiError> {
    let id = generate_id("ev");
    let package_ref = generate_id("EV");
    let now = now_secs();
    exec(
        "INSERT INTO evidence (id, alarm_id, package_ref, signed_by, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
        &[
            SqlValue::Text(id.clone()),
            SqlValue::Text(e.alarm_id.clone()),
            SqlValue::Text(package_ref),
            SqlValue::Text(e.signed_by.clone()),
            SqlValue::I64(now),
        ],
    )?;
    Ok(id)
}

/// Deletes an evidence package by id. Returns rows affected (0 if it did not exist).
pub fn delete_evidence(id: &str) -> Result<u64, AbiError> {
    exec("DELETE FROM evidence WHERE id = ?1", &[SqlValue::Text(id.into())])
}

// =============================================================================
// Vector ref mapping (ref_id u64 ↔ alarm string id)
// =============================================================================

/// Records (or replaces) the mapping from a vector namespace `ref_id` to the
/// alarm string id its embedding was built from, so a search hit's numeric
/// ref_id can be resolved back to the real alarm row.
pub fn upsert_vector_ref(ref_id: u64, alarm_id: &str, ts: i64) -> Result<(), AbiError> {
    exec(
        "INSERT INTO vector_refs (ref_id, alarm_id, ts) VALUES (?1, ?2, ?3) \
         ON CONFLICT(ref_id) DO UPDATE SET alarm_id = ?2, ts = ?3",
        &[SqlValue::I64(ref_id as i64), SqlValue::Text(alarm_id.into()), SqlValue::I64(ts)],
    )?;
    Ok(())
}

/// Resolves a vector `ref_id` back to its alarm string id, or None when the
/// mapping is unknown (e.g. an alarm deleted after indexing).
pub fn alarm_id_for_ref(ref_id: u64) -> Result<Option<String>, AbiError> {
    let rows = query("SELECT alarm_id FROM vector_refs WHERE ref_id = ?1", &[SqlValue::I64(ref_id as i64)])?;
    Ok(rows.first().and_then(|r| r.first()).map(|v| v.as_str().to_string()))
}

/// Lists every alarm (newest first) for the reindex backfill. Reuses the alarm
/// SELECT shape so callers get fully decoded rows including the camera name.
pub fn list_all_alarms() -> Result<Vec<AlarmRow>, AbiError> {
    list_alarms("", "", false)
}

// =============================================================================
// Settings (key/value)
// =============================================================================

/// Reads a setting value by key, or None when it is absent.
pub fn get_setting(key: &str) -> Result<Option<String>, AbiError> {
    let rows = query("SELECT value FROM settings WHERE key = ?1", &[SqlValue::Text(key.into())])?;
    Ok(rows.first().and_then(|r| r.first()).map(|v| v.as_str().to_string()))
}

/// Reads a setting as i64, falling back to `default` when absent or unparsable.
pub fn get_setting_i64(key: &str, default: i64) -> i64 {
    match get_setting(key) {
        Ok(Some(s)) => s.trim().parse::<i64>().unwrap_or(default),
        _ => default,
    }
}

/// Upserts a setting key/value, stamping updated_at from the SQLite clock.
pub fn set_setting(key: &str, value: &str) -> Result<u64, AbiError> {
    let now = now_secs();
    exec(
        "INSERT INTO settings (key, value, updated_at) VALUES (?1, ?2, ?3) \
         ON CONFLICT(key) DO UPDATE SET value = ?2, updated_at = ?3",
        &[
            SqlValue::Text(key.into()),
            SqlValue::Text(value.into()),
            SqlValue::I64(now),
        ],
    )
}
