// =============================================================================
// File: addons/go2/src/db.rs
// Per-addon SQLite access for the go2 robot. Single-row `robot` state machine
// with compare-and-set transitions (so concurrent connect/tick/flow-block calls
// can't corrupt the connection state) + telemetry + durable e-stop gate.
// =============================================================================

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;
use core::sync::atomic::{AtomicI64, Ordering};

use serde_json::{self, json, Value as JsonValue};

use crate::AbiError;

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

#[derive(Debug, Clone)]
pub enum SqlValue {
    Null,
    I64(i64),
    Text(String),
}

impl SqlValue {
    #[cfg_attr(test, allow(dead_code))]
    fn to_json(&self) -> JsonValue {
        match self {
            SqlValue::Null => JsonValue::Null,
            SqlValue::I64(v) => json!(v),
            SqlValue::Text(s) => JsonValue::String(s.clone()),
        }
    }
    fn from_json(v: &JsonValue) -> SqlValue {
        match v {
            JsonValue::Number(n) => n.as_i64().map(SqlValue::I64).unwrap_or(SqlValue::Null),
            JsonValue::String(s) => SqlValue::Text(s.clone()),
            JsonValue::Bool(b) => SqlValue::I64(i64::from(*b)),
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
            _ => 0,
        }
    }
}

pub type Row = Vec<SqlValue>;
#[cfg_attr(test, allow(dead_code))]
const SQL_BUF: usize = 16384;

#[cfg_attr(test, allow(dead_code))]
fn params_to_json(params: &[SqlValue]) -> String {
    let arr: Vec<JsonValue> = params.iter().map(SqlValue::to_json).collect();
    serde_json::to_string(&arr).unwrap_or_else(|_| "[]".into())
}

// Single-threaded WASM: one reusable host-output buffer, never freed/zeroed
// between calls. The host writes `out_len` bytes; the uninitialized tail past
// out_len is never read (we only parse `buf[..out_len]`), so `set_len` is sound.
std::thread_local! {
    static SQL_OUT: RefCell<Vec<u8>> = RefCell::new(Vec::with_capacity(SQL_BUF));
}

#[cfg(test)]
fn call_sql(
    host_fn: unsafe extern "C" fn(i32, i32, i32, i32, i32, i32, i32) -> i32,
    query: &str,
    params: &[SqlValue],
) -> Result<JsonValue, AbiError> {
    let is_query = host_fn as *const () as usize == sql_query_v1 as *const () as usize;
    test_backend::dispatch(is_query, query, params)
}

#[cfg(not(test))]
fn call_sql(
    host_fn: unsafe extern "C" fn(i32, i32, i32, i32, i32, i32, i32) -> i32,
    query: &str,
    params: &[SqlValue],
) -> Result<JsonValue, AbiError> {
    let p = params_to_json(params);
    let q = query.as_bytes();
    let pb = p.as_bytes();
    SQL_OUT.with(|cell| {
        let mut buf = cell.borrow_mut();
        let mut cap = buf.capacity().max(SQL_BUF);
        loop {
            buf.clear();
            buf.reserve(cap);
            // SAFETY: capacity >= cap after reserve; host fills [..out_len] and
            // we only read that prefix, so the uninitialized tail is never observed.
            unsafe { buf.set_len(cap); }
            let mut out_len: i32 = 0;
            let ret = unsafe {
                host_fn(
                    q.as_ptr() as i32, q.len() as i32,
                    pb.as_ptr() as i32, pb.len() as i32,
                    buf.as_mut_ptr() as i32, cap as i32,
                    &mut out_len as *mut i32 as i32,
                )
            };
            if ret == 6 {
                let want = if out_len > 0 { out_len as usize } else { 0 };
                cap = want.max(cap.saturating_mul(2));
                continue;
            }
            if ret != 0 {
                return Err(AbiError::from_code(ret));
            }
            if out_len < 0 || out_len as usize > cap {
                return Err(AbiError::Operation);
            }
            let n = out_len as usize;
            return serde_json::from_slice(&buf[..n]).map_err(|_| AbiError::Operation);
        }
    })
}

pub fn exec(query: &str, params: &[SqlValue]) -> Result<u64, AbiError> {
    let v = call_sql(sql_exec_v1, query, params)?;
    Ok(v.get("rows_affected").and_then(JsonValue::as_u64).unwrap_or(0))
}

pub fn query(query_str: &str, params: &[SqlValue]) -> Result<Vec<Row>, AbiError> {
    let v = call_sql(sql_query_v1, query_str, params)?;
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

// Cached wall-clock seconds, seeded from the host tick timestamp so the hot
// path never issues a SQL roundtrip just to read the clock.
static NOW_SECS: AtomicI64 = AtomicI64::new(0);

/// Seed the cached clock from the `on_tick` timestamp (top of every tick).
pub fn set_now_secs(secs: i64) {
    if secs > 0 {
        NOW_SECS.store(secs, Ordering::Relaxed);
    }
}

/// Current wall-clock seconds. The service instance seeds a cached tick time at
/// the top of every `on_tick` (zero-cost atomic read on the hot path). Any
/// instance that has NOT been tick-seeded — notably a pooled tool-call worker,
/// which never runs `on_tick` — reads the real wall clock directly on every
/// call. A one-shot cached read there would freeze at the worker's first call
/// and drift arbitrarily from the service instance, making time-based watchdogs
/// (connect / validation timeout) fire against a `last_update` stamped with a
/// stale clock and tear down a healthy connection.
pub fn now_secs() -> i64 {
    let cached = NOW_SECS.load(Ordering::Relaxed);
    if cached != 0 {
        return cached;
    }
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

// =============================================================================
// Robot state (single row, id = ROBOT_ID)
// =============================================================================

pub const ROBOT_ID: &str = "go2";

#[derive(Debug, Clone, Default)]
pub struct Robot {
    pub ip: String,
    pub status: String,
    pub channel_id: String,
    pub camera_id: String,
    pub battery_pct: i64,
    pub rtt_ms: i64,
    pub estop_active: bool,
    pub tick_count: i64,
    pub last_update: i64,
    pub last_telemetry: i64,
}

// Full single-row SELECT as one const literal — no per-call format! alloc.
const ROBOT_SELECT: &str =
    "SELECT ip, status, COALESCE(channel_id,''), COALESCE(camera_id,''), \
     COALESCE(battery_pct,-1), COALESCE(rtt_ms,-1), estop_active, tick_count, \
     COALESCE(last_update,0), COALESCE(last_telemetry,0) FROM robot WHERE id = ?1";

pub fn get_robot() -> Result<Robot, AbiError> {
    let rows = query(ROBOT_SELECT, &[SqlValue::Text(ROBOT_ID.into())])?;
    Ok(rows
        .first()
        .map(|r| {
            let g = |i: usize| r.get(i).cloned().unwrap_or(SqlValue::Null);
            Robot {
                ip: g(0).as_str().into(),
                status: g(1).as_str().into(),
                channel_id: g(2).as_str().into(),
                camera_id: g(3).as_str().into(),
                battery_pct: g(4).as_i64(),
                rtt_ms: g(5).as_i64(),
                estop_active: g(6).as_i64() != 0,
                tick_count: g(7).as_i64(),
                last_update: g(8).as_i64(),
                last_telemetry: g(9).as_i64(),
            }
        })
        .unwrap_or_default())
}

/// Ensure the singleton row exists, optionally setting the IP.
pub fn ensure_robot(ip: &str) -> Result<(), AbiError> {
    exec(
        "INSERT INTO robot (id, ip, status, last_update) VALUES (?1, ?2, 'offline', ?3) \
         ON CONFLICT(id) DO UPDATE SET ip = CASE WHEN ?2 <> '' THEN ?2 ELSE robot.ip END",
        &[SqlValue::Text(ROBOT_ID.into()), SqlValue::Text(ip.into()), SqlValue::I64(now_secs())],
    )?;
    Ok(())
}

/// CAS: move offline/error -> connecting. Returns true if THIS call won it
/// (rows_affected == 1), false if a connect is already in flight.
pub fn try_begin_connect() -> Result<bool, AbiError> {
    let n = exec(
        "UPDATE robot SET status='connecting', status_msg='', last_update=?2 \
         WHERE id=?1 AND status IN ('offline','error')",
        &[SqlValue::Text(ROBOT_ID.into()), SqlValue::I64(now_secs())],
    )?;
    Ok(n == 1)
}

/// CAS: connecting -> validating. False if the connect was cancelled meanwhile
/// (caller must close the fresh channel it just opened).
pub fn set_channel(channel_id: &str) -> Result<bool, AbiError> {
    let n = exec(
        "UPDATE robot SET channel_id=?2, status='validating', last_update=?3 \
         WHERE id=?1 AND status='connecting'",
        &[SqlValue::Text(ROBOT_ID.into()), SqlValue::Text(channel_id.into()), SqlValue::I64(now_secs())],
    )?;
    Ok(n == 1)
}

/// CAS: validating -> online, only if the live channel is still the one we
/// validated. False if a disconnect/reconnect raced (caller closes the channel).
pub fn set_online(channel_id: &str, camera_id: &str) -> Result<bool, AbiError> {
    // Seed last_telemetry = now so the online watchdog grants a grace window
    // before the first lowstate arrives.
    let now = now_secs();
    let n = exec(
        "UPDATE robot SET status='online', camera_id=?3, last_update=?4, last_telemetry=?4 \
         WHERE id=?1 AND status='validating' AND channel_id=?2",
        &[
            SqlValue::Text(ROBOT_ID.into()),
            SqlValue::Text(channel_id.into()),
            SqlValue::Text(camera_id.into()),
            SqlValue::I64(now),
        ],
    )?;
    Ok(n == 1)
}

/// Record a REAL lowstate receipt: persists battery and advances last_telemetry
/// (drives the online liveness watchdog). Called every tick a lowstate arrives.
pub fn record_lowstate(battery_pct: i64) -> Result<(), AbiError> {
    let now = now_secs();
    exec(
        "UPDATE robot SET battery_pct=?2, last_telemetry=?3, last_update=?3 WHERE id=?1",
        &[SqlValue::Text(ROBOT_ID.into()), SqlValue::I64(battery_pct), SqlValue::I64(now)],
    )?;
    Ok(())
}

/// Persist the keepalive RTT snapshot (does NOT touch last_telemetry — RTT comes
/// from the transport, not from the robot's telemetry stream).
pub fn set_rtt(rtt_ms: i64) -> Result<(), AbiError> {
    exec(
        "UPDATE robot SET rtt_ms=?2, last_update=?3 WHERE id=?1",
        &[SqlValue::Text(ROBOT_ID.into()), SqlValue::I64(rtt_ms), SqlValue::I64(now_secs())],
    )?;
    Ok(())
}

pub fn set_estop(active: bool) -> Result<(), AbiError> {
    exec(
        "UPDATE robot SET estop_active=?2, last_update=?3 WHERE id=?1",
        &[SqlValue::Text(ROBOT_ID.into()), SqlValue::I64(i64::from(active)), SqlValue::I64(now_secs())],
    )?;
    Ok(())
}

/// Mark disconnected/offline and clear the live handles.
pub fn set_offline(status: &str, msg: &str) -> Result<(), AbiError> {
    exec(
        "UPDATE robot SET status=?2, status_msg=?3, channel_id='', camera_id='', \
         battery_pct=NULL, rtt_ms=NULL, last_update=?4 WHERE id=?1",
        &[
            SqlValue::Text(ROBOT_ID.into()),
            SqlValue::Text(status.into()),
            SqlValue::Text(msg.into()),
            SqlValue::I64(now_secs()),
        ],
    )?;
    Ok(())
}

/// One-time upgrade bridge: read the legacy operator LiDAR enable INTENT out of
/// the vestigial `robot_live` table. Returns `Ok(Some(enabled))` when the table
/// still exists with the singleton row, `Ok(None)` when the table is gone (a
/// later migration dropped it), has no row, or the statement otherwise fails to
/// run (a "no such table" after the later drop migration is reported by the host
/// as an Operation error). The bridge is best-effort and idempotent — on the
/// absent-table / failed-read case it simply leaves the durable store key as-is,
/// to be reconsidered on the next start; only a permission failure propagates so
/// a sandbox misconfiguration is surfaced loudly.
pub fn legacy_lidar_enabled() -> Result<Option<bool>, AbiError> {
    match query(
        "SELECT lidar_enabled FROM robot_live WHERE id = ?1",
        &[SqlValue::Text(ROBOT_ID.into())],
    ) {
        Ok(rows) => Ok(rows
            .first()
            .and_then(|r| r.first())
            .map(|v| v.as_i64() != 0)),
        Err(AbiError::Permission) => Err(AbiError::Permission),
        // Table gone after the drop migration, or any other run failure: nothing
        // to bridge this start (idempotent — re-evaluated next start).
        Err(_) => Ok(None),
    }
}

/// Increment the tick counter (UPDATE only — the caller already holds the prior
/// count from get_robot(), so no read-back SELECT).
pub fn bump_tick() {
    let _ = exec(
        "UPDATE robot SET tick_count = tick_count + 1 WHERE id=?1",
        &[SqlValue::Text(ROBOT_ID.into())],
    );
}

/// Test-only: seed the legacy `robot_live.lidar_enabled` the on_start bridge reads.
/// `None` simulates the table already dropped (a later migration).
#[cfg(test)]
pub fn test_set_legacy_lidar(v: Option<bool>) {
    test_backend::set_legacy_lidar(v);
}

#[cfg(test)]
mod test_backend {
    use super::{SqlValue, AbiError};
    use core::cell::Cell;
    use serde_json::{json, Value as JsonValue};

    std::thread_local! {
        // Simulates the legacy `robot_live.lidar_enabled` column for the on_start
        // upgrade-bridge tests. `None` = the table is gone (later drop migration)
        // → the SELECT errors (Operation), matching the live host's "no such table".
        static LEGACY_LIDAR: Cell<Option<bool>> = const { Cell::new(None) };
    }

    /// Test hook: seed the legacy `robot_live.lidar_enabled` value the bridge reads.
    pub fn set_legacy_lidar(v: Option<bool>) {
        LEGACY_LIDAR.with(|c| c.set(v));
    }

    /// The `robot` connection-state table uses an SQL CAS state machine that
    /// native tests do not exercise (the live-state tests target the shared store
    /// in `state.rs`). Every query is therefore an inert no-op: `unixepoch()`
    /// returns 0 (the tests seed the clock via `set_now_secs`), a SELECT returns
    /// no rows and an exec reports zero rows affected. The one exception is the
    /// legacy `robot_live` SELECT, served from the LEGACY_LIDAR test hook so the
    /// on_start bridge can be exercised offline.
    pub fn dispatch(is_query: bool, query: &str, _params: &[SqlValue]) -> Result<JsonValue, AbiError> {
        if query.contains("unixepoch()") {
            return Ok(json!({ "rows": [[0]] }));
        }
        if query.contains("FROM robot_live") {
            return match LEGACY_LIDAR.with(Cell::get) {
                Some(v) => Ok(json!({ "rows": [[i64::from(v)]] })),
                // No legacy table → the host reports a run error (Operation).
                None => Err(AbiError::Operation),
            };
        }
        if is_query {
            return Ok(json!({ "rows": [] }));
        }
        Ok(json!({ "rows_affected": 0 }))
    }
}
