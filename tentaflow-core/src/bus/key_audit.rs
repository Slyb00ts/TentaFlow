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
// A success is kept per (key, organisation, instance, topic, action); a
// refusal per (key, action, reason) alone, with the number of distinct topics
// it covered and the latest as an example — what a refused caller typed must
// not decide how many buckets (and rows) there are. Buckets are capped per key
// and in total; past a cap requests are counted into one overflow bucket. The first request of a fresh
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

use std::collections::BTreeSet;
use std::sync::{LazyLock, Once};
use std::time::{Duration, Instant};

use dashmap::DashMap;

use crate::db::DbPool;

/// How long the requests after a bucket's first are only counted.
const WINDOW: Duration = Duration::from_secs(60);

/// How often the process-wide timer flushes every bucket.
const FLUSH_INTERVAL: Duration = Duration::from_secs(60);

/// Buckets one key may hold at once, and all keys together. A request that
/// would open a bucket past either cap is counted into the one overflow
/// bucket instead, so no caller can grow this map — or the audit trail —
/// by varying what it asks for.
const MAX_BUCKETS_PER_KEY: usize = 16;
const MAX_BUCKETS: usize = 4096;

/// Distinct topics a refusal bucket remembers; past it the row says the
/// count is a lower bound.
const MAX_TRACKED_TOPICS: usize = 64;

/// Audit action of the overflow bucket's rows.
const OVERFLOW_ACTION: &str = "bus.rest.overflow";

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

/// `(key uid, org, instance, topic, action, outcome)`. A success is kept per
/// topic — only a granted topic can succeed, so there are as many as the
/// key's grants. A refusal is kept per key, action and reason alone: the
/// topic, organisation and instance of a refused request are whatever the
/// caller typed, and keying on them would let it open (and write) a new
/// bucket with every request.
type BucketKey = (String, String, String, String, &'static str, String);

struct Bucket {
    window_start: Instant,
    /// Requests and records counted since the last row was written.
    pending_requests: u64,
    pending_records: u64,
    /// Distinct topics among the requests the next row covers (refusals).
    topics: BTreeSet<String>,
    topics_capped: bool,
    /// The latest request of the bucket, whose facts a flushed row carries.
    last: KeyRequest,
    db: DbPool,
}

impl Bucket {
    fn note_topic(&mut self, topic: &str) {
        if self.topics.len() < MAX_TRACKED_TOPICS {
            self.topics.insert(topic.to_string());
        } else if !self.topics.contains(topic) {
            self.topics_capped = true;
        }
    }
}

/// What one row says beyond the request it is written for.
struct RowCounts {
    requests: u64,
    records: u64,
    distinct_topics: usize,
    topics_capped: bool,
}

static BUCKETS: LazyLock<DashMap<BucketKey, Bucket>> = LazyLock::new(DashMap::new);
static TIMER: Once = Once::new();

fn bucket_key(request: &KeyRequest) -> BucketKey {
    match &request.reason {
        None => (
            request.key_uid.clone(),
            request.org_id.clone().unwrap_or_default(),
            request.instance.clone(),
            request.topic.clone(),
            request.action,
            "ok".to_string(),
        ),
        Some(reason) => (
            request.key_uid.clone(),
            String::new(),
            String::new(),
            String::new(),
            request.action,
            reason.clone(),
        ),
    }
}

fn overflow_key() -> BucketKey {
    (
        String::new(),
        String::new(),
        String::new(),
        String::new(),
        OVERFLOW_ACTION,
        "overflow".to_string(),
    )
}

/// The bucket a request is counted in: its own, or the overflow bucket when
/// opening its own would pass a cap. Checked before the entry is taken — a
/// `DashMap` iteration while holding an entry would deadlock — so two
/// requests racing for the last free place may both get one; the caps are
/// bounds on growth, not exact counts.
fn place(request: &KeyRequest) -> BucketKey {
    let key = bucket_key(request);
    if BUCKETS.contains_key(&key) {
        return key;
    }
    let per_key = BUCKETS
        .iter()
        .filter(|e| e.key().0 == request.key_uid)
        .count();
    if BUCKETS.len() >= MAX_BUCKETS || per_key >= MAX_BUCKETS_PER_KEY {
        overflow_key()
    } else {
        key
    }
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
    let key = place(&request);
    let overflow = key.4 == OVERFLOW_ACTION;
    let now = Instant::now();
    let row = {
        let mut created = false;
        let mut bucket = BUCKETS.entry(key).or_insert_with(|| {
            created = true;
            Bucket {
                window_start: now,
                pending_requests: 0,
                pending_records: 0,
                topics: BTreeSet::new(),
                topics_capped: false,
                last: request.clone(),
                db: db.clone(),
            }
        });
        bucket.note_topic(&request.topic);
        bucket.last = request.clone();
        if created || now.duration_since(bucket.window_start) >= WINDOW {
            let row = RowCounts {
                requests: bucket.pending_requests + 1,
                records: bucket.pending_records + request.records,
                distinct_topics: bucket.topics.len(),
                topics_capped: bucket.topics_capped,
            };
            bucket.window_start = now;
            bucket.pending_requests = 0;
            bucket.pending_records = 0;
            bucket.topics.clear();
            bucket.topics_capped = false;
            Some(row)
        } else {
            bucket.pending_requests += 1;
            bucket.pending_records += request.records;
            None
        }
    };
    if let Some(counts) = row {
        write(db, &request, overflow, &counts);
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
                let counts = RowCounts {
                    requests: bucket.pending_requests,
                    records: bucket.pending_records,
                    distinct_topics: bucket.topics.len(),
                    topics_capped: bucket.topics_capped,
                };
                write(&bucket.db, &bucket.last, key.4 == OVERFLOW_ACTION, &counts);
            }
        }
    }
}

/// One `audit_log` row covering `counts` of one bucket, with `request` — its
/// latest — as the example. It names the key by uid and by its current name;
/// payloads never reach it. An overflow row is written under
/// `OVERFLOW_ACTION` and names only the latest key it counted.
fn write(db: &DbPool, request: &KeyRequest, overflow: bool, counts: &RowCounts) {
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
        "requests": counts.requests,
        "records": counts.records,
        "distinct_topics": counts.distinct_topics,
        "distinct_topics_capped": counts.topics_capped,
        "overflow": overflow,
        "user_agent": request.user_agent.as_deref().unwrap_or_default(),
    })
    .to_string();
    let actor = format!("{}{}", super::API_KEY_AUDIT_ACTOR_PREFIX, request.key_uid);
    let action = if overflow {
        OVERFLOW_ACTION
    } else {
        request.action
    };
    if let Err(e) = crate::db::repository::log_audit_full(
        db,
        Some(&actor),
        None,
        action,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn request(key_uid: &str, topic: &str, reason: Option<&str>) -> KeyRequest {
        KeyRequest {
            key_uid: key_uid.to_string(),
            org_id: Some("org-1".to_string()),
            instance: "tentabus-00000001".to_string(),
            topic: topic.to_string(),
            action: "bus.rest.publish",
            http_status: if reason.is_some() { 403 } else { 200 },
            reason: reason.map(str::to_string),
            records: 1,
            peer_ip: None,
            user_agent: None,
        }
    }

    fn rows(db: &DbPool, action: &str, key_uid: &str) -> Vec<serde_json::Value> {
        crate::db::repository::list_audit_logs(
            db,
            &crate::db::models::AuditLogFilters {
                action: Some(action.to_string()),
                ..Default::default()
            },
            0,
            1000,
        )
        .unwrap()
        .into_iter()
        .map(|r| serde_json::from_str(r.details.as_deref().unwrap()).unwrap())
        .filter(|d: &serde_json::Value| d["api_key_uid"] == key_uid)
        .collect()
    }

    /// Refusals are one bucket per key, action and reason, whatever topic
    /// each named: a caller cycling through new topics writes one row, and
    /// the flushed row says how many topics it tried.
    #[test]
    fn refusals_to_ever_new_topics_share_one_bucket() {
        let db = crate::dispatch::state::AppState::for_test().db.clone();
        let key = uuid::Uuid::new_v4().to_string();
        for i in 0..10 {
            record(
                &db,
                request(
                    &key,
                    &format!("probe.{i}"),
                    Some("api_key_topic_denied:write"),
                ),
            );
        }
        assert_eq!(rows(&db, "bus.rest.publish", &key).len(), 1);
        flush_for(&db);
        let written = rows(&db, "bus.rest.publish", &key);
        assert_eq!(written.len(), 2);
        let requests: u64 = written
            .iter()
            .map(|d| d["requests"].as_u64().unwrap())
            .sum();
        assert_eq!(requests, 10);
        assert!(written.iter().any(|d| d["distinct_topics"] == 9));
    }

    /// One key cannot hold more than `MAX_BUCKETS_PER_KEY` buckets: past the
    /// cap its requests are counted in the overflow bucket, whose rows go
    /// under `OVERFLOW_ACTION`.
    #[test]
    fn buckets_past_the_per_key_cap_overflow() {
        let db = crate::dispatch::state::AppState::for_test().db.clone();
        let key = uuid::Uuid::new_v4().to_string();
        for i in 0..MAX_BUCKETS_PER_KEY + 5 {
            record(&db, request(&key, &format!("granted.{i}"), None));
        }
        assert_eq!(
            rows(&db, "bus.rest.publish", &key).len(),
            MAX_BUCKETS_PER_KEY
        );
        let own = BUCKETS.iter().filter(|e| e.key().0 == key).count();
        assert_eq!(own, MAX_BUCKETS_PER_KEY);
        flush_for(&db);
        let overflow: u64 = rows(&db, OVERFLOW_ACTION, &key)
            .iter()
            .map(|d| d["requests"].as_u64().unwrap())
            .sum();
        assert_eq!(overflow, 5, "the overflow bucket counted the key's surplus");
        assert_eq!(BUCKETS.iter().filter(|e| e.key().0 == key).count(), 0);
    }
}
