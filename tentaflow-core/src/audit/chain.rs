// =============================================================================
// File: audit/chain.rs — Merkle hash chain helpers for `audit_log` (F1b P4).
//
// Every audit row written through `audit_log_with_risk` stores two BLOBs:
//   - `prev_hash` (32 B) — copy of the previous row's `hash`, or all-zero for
//     the genesis row.
//   - `hash` (32 B) — `SHA256(canonical(row) || prev_hash)`.
//
// A verifier walks rows in id order, recomputes each row's `hash` from its
// stored content + the predecessor's `hash`, and reports any mismatch. The
// hash is unsalted on purpose — anyone with DB read can verify, no extra
// secret material to manage.
//
// Canonical serialization: the columns participating in the hash are
// concatenated with the NUL separator `\0` in a fixed order. `None` is
// encoded as the empty string. This keeps the hash deterministic across
// SQLite implementations and across endianness while staying trivial to
// re-implement in another language for offline audit.
// =============================================================================

use sha2::{Digest, Sha256};

/// 32-byte SHA-256 output, matching the column width.
pub type ChainHash = [u8; 32];

/// All-zero predecessor hash for the genesis row.
pub const GENESIS_PREV_HASH: ChainHash = [0u8; 32];

/// Canonical row fields participating in the hash. Order matters — changing
/// the layout invalidates every chained row already on disk, so a future
/// version bump should arrive with an explicit migration.
#[derive(Debug, Clone)]
pub struct AuditRowHashInput<'a> {
    pub user_id: Option<i64>,
    pub addon_id: Option<&'a str>,
    pub instance_id: Option<&'a str>,
    pub action: &'a str,
    pub resource: Option<&'a str>,
    pub resource_type: Option<&'a str>,
    pub resource_id: Option<&'a str>,
    pub result: Option<&'a str>,
    pub error_message: Option<&'a str>,
    pub details: Option<&'a str>,
    pub ip_address: Option<&'a str>,
    pub node_id: Option<&'a str>,
    pub severity: Option<&'a str>,
    pub risk_class: &'a str,
    pub related_claim_id: Option<&'a str>,
    pub request_id: Option<&'a str>,
    /// `timestamp` column value as stored in DB (TEXT, datetime('now') format).
    pub timestamp: &'a str,
}

/// Canonical byte representation of an audit row for hashing. Stable across
/// rustc versions and platforms — just `\0`-joined UTF-8.
pub fn canonical_row_bytes(input: &AuditRowHashInput<'_>) -> Vec<u8> {
    let user = input
        .user_id
        .map(|v| v.to_string())
        .unwrap_or_default();
    let parts: [&str; 17] = [
        &user,
        input.addon_id.unwrap_or(""),
        input.instance_id.unwrap_or(""),
        input.action,
        input.resource.unwrap_or(""),
        input.resource_type.unwrap_or(""),
        input.resource_id.unwrap_or(""),
        input.result.unwrap_or(""),
        input.error_message.unwrap_or(""),
        input.details.unwrap_or(""),
        input.ip_address.unwrap_or(""),
        input.node_id.unwrap_or(""),
        input.severity.unwrap_or(""),
        input.risk_class,
        input.related_claim_id.unwrap_or(""),
        input.request_id.unwrap_or(""),
        input.timestamp,
    ];

    let total: usize = parts.iter().map(|p| p.len()).sum::<usize>() + parts.len() - 1;
    let mut out = Vec::with_capacity(total);
    for (i, p) in parts.iter().enumerate() {
        if i > 0 {
            out.push(0u8);
        }
        out.extend_from_slice(p.as_bytes());
    }
    out
}

/// Compute `SHA256(row_bytes || prev_hash)`.
pub fn compute_hash(row_bytes: &[u8], prev_hash: &ChainHash) -> ChainHash {
    let mut hasher = Sha256::new();
    hasher.update(row_bytes);
    hasher.update(prev_hash);
    let out = hasher.finalize();
    let mut hash = [0u8; 32];
    hash.copy_from_slice(&out);
    hash
}

/// Fetch the most recent non-NULL `hash` from `audit_log`. Returns `None`
/// when the chain has not started yet (every row is legacy / pre-P4 or the
/// table is empty), in which case the caller MUST use [`GENESIS_PREV_HASH`].
pub fn latest_chain_hash(
    conn: &rusqlite::Connection,
) -> rusqlite::Result<Option<ChainHash>> {
    let row: Option<Vec<u8>> = conn
        .query_row(
            "SELECT hash FROM audit_log WHERE hash IS NOT NULL ORDER BY id DESC LIMIT 1",
            [],
            |row| row.get::<_, Vec<u8>>(0),
        )
        .map(Some)
        .or_else(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(other),
        })?;

    match row {
        None => Ok(None),
        Some(bytes) if bytes.len() == 32 => {
            let mut h = [0u8; 32];
            h.copy_from_slice(&bytes);
            Ok(Some(h))
        }
        Some(bytes) => Err(rusqlite::Error::FromSqlConversionFailure(
            bytes.len(),
            rusqlite::types::Type::Blob,
            format!("audit_log.hash must be 32 bytes, got {}", bytes.len()).into(),
        )),
    }
}

/// Compute the (prev_hash, hash) pair for an audit row that is about to be
/// inserted. Returned as `Vec<u8>` (32 B each) for direct binding to
/// `rusqlite::params!`. The caller MUST hold the same DB lock for the
/// duration of "compute + INSERT" so the chain stays linearizable — the
/// shared `DbPool` Mutex already provides this for every writer reachable
/// from `AddonState.db` or `repository::acquire`.
pub fn compute_chain_for_insert(
    conn: &rusqlite::Connection,
    input: &AuditRowHashInput<'_>,
) -> rusqlite::Result<(Vec<u8>, Vec<u8>)> {
    let prev = latest_chain_hash(conn)?.unwrap_or(GENESIS_PREV_HASH);
    let row_bytes = canonical_row_bytes(input);
    let hash = compute_hash(&row_bytes, &prev);
    Ok((prev.to_vec(), hash.to_vec()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample<'a>(action: &'a str, ts: &'a str) -> AuditRowHashInput<'a> {
        AuditRowHashInput {
            user_id: Some(7),
            addon_id: Some("com.test"),
            instance_id: Some("inst-1"),
            action,
            resource: None,
            resource_type: Some("frame"),
            resource_id: Some("ref-x"),
            result: Some("ok"),
            error_message: None,
            details: None,
            ip_address: None,
            node_id: None,
            severity: None,
            risk_class: "A",
            related_claim_id: None,
            request_id: Some("req-1"),
            timestamp: ts,
        }
    }

    #[test]
    fn canonical_bytes_deterministic() {
        let a = canonical_row_bytes(&sample("act", "2026-05-16 10:00:00"));
        let b = canonical_row_bytes(&sample("act", "2026-05-16 10:00:00"));
        assert_eq!(a, b);
    }

    #[test]
    fn canonical_bytes_differ_on_action_change() {
        let a = canonical_row_bytes(&sample("act_a", "t1"));
        let b = canonical_row_bytes(&sample("act_b", "t1"));
        assert_ne!(a, b);
    }

    #[test]
    fn canonical_bytes_differ_on_timestamp_change() {
        let a = canonical_row_bytes(&sample("act", "t1"));
        let b = canonical_row_bytes(&sample("act", "t2"));
        assert_ne!(a, b);
    }

    #[test]
    fn hash_chains_via_prev() {
        let row = canonical_row_bytes(&sample("act", "t"));
        let h1 = compute_hash(&row, &GENESIS_PREV_HASH);
        let h2 = compute_hash(&row, &h1);
        assert_ne!(h1, h2, "different prev_hash must yield different hash");
    }

    #[test]
    fn nul_separator_collision_resistant() {
        // Ensure the `\0` separator prevents a `(addon_id="ab", action="cd")`
        // row from colliding with `(addon_id="a", action="bcd")` — without
        // the separator both would serialize to "abcd...".
        let row_a = AuditRowHashInput {
            user_id: None,
            addon_id: Some("ab"),
            instance_id: None,
            action: "cd",
            resource: None,
            resource_type: None,
            resource_id: None,
            result: None,
            error_message: None,
            details: None,
            ip_address: None,
            node_id: None,
            severity: None,
            risk_class: "A",
            related_claim_id: None,
            request_id: None,
            timestamp: "t",
        };
        let row_b = AuditRowHashInput {
            addon_id: Some("a"),
            action: "bcd",
            ..row_a.clone()
        };
        assert_ne!(
            canonical_row_bytes(&row_a),
            canonical_row_bytes(&row_b),
            "NUL separator must prevent prefix-collision attacks"
        );
    }
}
