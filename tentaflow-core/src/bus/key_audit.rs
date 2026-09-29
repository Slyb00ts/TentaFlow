// =============================================================================
// File: bus/key_audit.rs — windowed audit rows for general API key requests
//       on the records REST (package K)
// =============================================================================
//
// A general API key publishing or consuming over `api/bus_rest.rs` is an
// external system, and it calls many times a second — one `audit_log` row per
// request would flood the shared, hash-chained audit writer, the reason
// `BusService`'s own `AuditWindows` exist. This is the same pattern for key
// requests, with two differences: an outcome counts requests AND records
// (published or delivered), and it is kept per key, not per organisation, so
// one key's traffic never takes another key's name in the audit trail.
//
// One bucket per (key, organisation, instance, topic, action, outcome), where
// the outcome is `ok` or the refusal's reason. The first request of a fresh
// bucket is written at once — an isolated refusal is never invisible — and
// the requests after it are only counted until either the window has passed
// (the next request writes them, itself included) or a flush runs: `flush`
// from a process-wide timer, and `flush_for` its database from every engine's
// `BusService::flush_audit_windows`, which its shutdown ends in. `flush` removes every bucket, so the map only
// holds what happened since the last flush.
//
// The buckets are process-wide, not an engine's: a request refused before its
// instance is resolved (a key without a grant) has no engine to belong to.
// =============================================================================

use std::sync::{LazyLock, Once};
use std::time::{Duration, Instant};

use dashmap::DashMap;

use crate::db::DbPool;

/// How long the requests after a bucket's first are only counted.
const WINDOW: Duration = Duration::from_secs(60);

/// How often the process-wide timer flushes every bucket.
const FLUSH_INTERVAL: Duration = Duration::from_secs(60);

/// One finished records REST request of a general API key.
#[derive(Debug, Clone)]
pub struct KeyRequest {
    pub key_uid: String,
    pub org_id: Option<String>,
    /// The instance as addressed (`""` on the instance-less legacy path).
    pub instance: String,
    /// The topic as addressed, or `<invalid>` for a malformed one.
    pub topic: String,
    /// `bus.rest.publish` or `bus.rest.consume`.
    pub action: &'static str,
    pub http_status: u16,
    /// `None` for a success, else why the request was refused or failed.
    pub reason: Option<String>,
    /// Records published or delivered.
    pub records: u64,
    pub peer_ip: Option<String>,
    pub user_agent: Option<String>,
}

type BucketKey = (String, String, String, String, &'static str, String);

struct Bucket {
    window_start: Instant,
    /// Requests and records counted since the last row was written.
    pending_requests: u64,
    pending_records: u64,
    /// The latest request of the bucket, whose facts a flushed row carries.
    last: KeyRequest,
    db: DbPool,
}

static BUCKETS: LazyLock<DashMap<BucketKey, Bucket>> = LazyLock::new(DashMap::new);
static TIMER: Once = Once::new();

fn bucket_key(request: &KeyRequest) -> BucketKey {
    (
        request.key_uid.clone(),
        request.org_id.clone().unwrap_or_default(),
        request.instance.clone(),
        request.topic.clone(),
        request.action,
        request.reason.clone().unwrap_or_else(|| "ok".to_string()),
    )
}

/// Records one request: writes a row at once when it opens a bucket or
/// closes a passed window (carrying the requests counted before it), else
/// only counts it.
pub fn record(db: &DbPool, request: KeyRequest) {
    TIMER.call_once(|| {
        std::thread::spawn(|| loop {
            std::thread::sleep(FLUSH_INTERVAL);
            flush();
        });
    });
    let now = Instant::now();
    let row = {
        let mut created = false;
        let mut bucket = BUCKETS.entry(bucket_key(&request)).or_insert_with(|| {
            created = true;
            Bucket {
                window_start: now,
                pending_requests: 0,
                pending_records: 0,
                last: request.clone(),
                db: db.clone(),
            }
        });
        if created {
            Some((1, request.records))
        } else if now.duration_since(bucket.window_start) >= WINDOW {
            let row = (
                bucket.pending_requests + 1,
                bucket.pending_records + request.records,
            );
            bucket.window_start = now;
            bucket.pending_requests = 0;
            bucket.pending_records = 0;
            bucket.last = request.clone();
            Some(row)
        } else {
            bucket.pending_requests += 1;
            bucket.pending_records += request.records;
            bucket.last = request.clone();
            None
        }
    };
    if let Some((requests, records)) = row {
        write(db, &request, requests, records);
    }
}

/// Writes the counted tail of every bucket and removes them all — the
/// process-wide timer's flush.
pub fn flush() {
    flush_matching(|_| true);
}

/// `flush` limited to the buckets whose rows go to `db` — what an engine's
/// `BusService::flush_audit_windows` (and so its shutdown) writes, leaving
/// other databases' buckets to their own engines and the timer.
pub fn flush_for(db: &DbPool) {
    flush_matching(|bucket_db| std::sync::Arc::ptr_eq(bucket_db, db));
}

fn flush_matching(matches: impl Fn(&DbPool) -> bool) {
    let keys: Vec<BucketKey> = BUCKETS
        .iter()
        .filter(|e| matches(&e.value().db))
        .map(|e| e.key().clone())
        .collect();
    for key in keys {
        if let Some((_, bucket)) = BUCKETS.remove(&key) {
            if bucket.pending_requests > 0 {
                write(
                    &bucket.db,
                    &bucket.last,
                    bucket.pending_requests,
                    bucket.pending_records,
                );
            }
        }
    }
}

/// One `audit_log` row covering `requests` requests and `records` records of
/// one bucket. It names the key by uid and by its current name; payloads never
/// reach it.
fn write(db: &DbPool, request: &KeyRequest, requests: u64, records: u64) {
    let (result, severity) = match request.http_status {
        200..=299 => ("ok", "info"),
        401 | 403 => ("denied", "warn"),
        400..=499 => ("rejected", "warn"),
        _ => ("error", "error"),
    };
    let key_name = crate::db::repository::get_api_key_by_uid(db, &request.key_uid)
        .ok()
        .flatten()
        .map(|k| k.name);
    let details = serde_json::json!({
        "surface": "v1.bus.records.rest",
        "instance_id": request.instance,
        "auth": "api_key",
        "api_key_uid": request.key_uid,
        "api_key_name": key_name,
        "http_status": request.http_status,
        "reason": request.reason,
        "requests": requests,
        "records": records,
        "user_agent": request.user_agent.as_deref().unwrap_or_default(),
    })
    .to_string();
    let actor = format!("{}{}", super::API_KEY_AUDIT_ACTOR_PREFIX, request.key_uid);
    if let Err(e) = crate::db::repository::log_audit_full(
        db,
        Some(&actor),
        None,
        request.action,
        Some("bus_topic"),
        Some(&request.topic),
        Some(&details),
        severity,
        if request.action == "bus.rest.publish" {
            "B"
        } else {
            "C"
        },
        Some(result),
        request.org_id.as_deref(),
        request.peer_ip.as_deref(),
        None,
    ) {
        tracing::warn!(error = %e, action = request.action, "bus records REST: audit write failed");
    }
}
