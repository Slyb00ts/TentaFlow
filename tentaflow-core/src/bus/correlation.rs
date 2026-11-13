// ===== File: bus/correlation.rs — correlation id validation, minting and
// header lookup (TB-02). The id is an untrusted label: attribution comes from
// `tf.actor`/`tf.org`, and every search over it is scoped by org and ACL.

use bytes::Bytes;

use super::PublishRecord;
use tentaflow_protocol::bus::{CORRELATION_ID_HEADER, CORRELATION_ID_MAX_LEN};

/// Whether `id` is an acceptable correlation id: ASCII
/// `[A-Za-z0-9][A-Za-z0-9._:-]{0,127}`.
pub fn validate(id: &str) -> bool {
    let bytes = id.as_bytes();
    let Some((first, rest)) = bytes.split_first() else {
        return false;
    };
    bytes.len() <= CORRELATION_ID_MAX_LEN
        && first.is_ascii_alphanumeric()
        && rest
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-'))
}

/// A fresh UUID v7 (time-ordered). Called only at entry points, never in the
/// broker core.
pub fn mint() -> String {
    uuid::Uuid::now_v7().to_string()
}

/// The first valid value of the input `correlation-id` record header, if any.
/// An invalid value is not promoted (it stays a plain header). Only the exact
/// key `correlation-id` is read.
pub fn from_headers(headers: &[(String, Bytes)]) -> Option<&str> {
    let mut seen_invalid = false;
    for (_, value) in headers.iter().filter(|(k, _)| k == CORRELATION_ID_HEADER) {
        match std::str::from_utf8(value) {
            Ok(id) if validate(id) => return Some(id),
            _ => seen_invalid = true,
        }
    }
    if seen_invalid {
        tracing::debug!("bus: ignoring invalid `correlation-id` record header");
    }
    None
}

/// Settles the correlation id of every record of an entry-point batch and
/// returns them in order. A record with a valid `correlation-id` header keeps
/// it. Any other record takes `batch_id` (the caller sets it as
/// `BusCallContext::correlation_id`, so nothing is added to the record) or,
/// with no `batch_id`, gets its own freshly minted id as a `correlation-id`
/// header (minting is per record, never one id shared by unrelated records).
pub fn settle_records(records: &mut [PublishRecord], batch_id: Option<&str>) -> Vec<String> {
    let batch_id = batch_id.filter(|id| validate(id));
    records
        .iter_mut()
        .map(|r| {
            if let Some(id) = from_headers(&r.headers) {
                return id.to_string();
            }
            match batch_id {
                Some(id) => id.to_string(),
                None => {
                    let id = mint();
                    r.headers
                        .push((CORRELATION_ID_HEADER.to_string(), Bytes::from(id.clone())));
                    id
                }
            }
        })
        .collect()
}

/// Same lookup over a fetched record's raw `(key, value)` header pairs,
/// accepting the input header and the broker-stamped `tf.correlation_id`
/// (the latter wins, as the validated copy).
pub fn from_fetched_headers(headers: &[(Bytes, Bytes)]) -> Option<String> {
    let find = |name: &str| {
        headers
            .iter()
            .find(|(k, _)| k.as_ref() == name.as_bytes())
            .and_then(|(_, v)| std::str::from_utf8(v).ok())
            .filter(|id| validate(id))
    };
    find(tentaflow_protocol::bus::TF_CORRELATION_ID_HEADER)
        .or_else(|| find(CORRELATION_ID_HEADER))
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_accepts_the_documented_charset_and_bounds() {
        assert!(validate("a"));
        assert!(validate("A0._:-z"));
        assert!(validate(&"a".repeat(128)));
        assert!(validate(&mint()));
    }

    #[test]
    fn validate_rejects_empty_oversized_and_foreign_characters() {
        assert!(!validate(""));
        assert!(!validate(&"a".repeat(129)));
        assert!(!validate("-leading"));
        assert!(!validate(".leading"));
        assert!(!validate("has space"));
        assert!(!validate("slash/id"));
        assert!(!validate("line\nbreak"));
        assert!(!validate("zażółć"));
    }

    #[test]
    fn mint_returns_distinct_time_ordered_v7_uuids() {
        let a = mint();
        let b = mint();
        assert_ne!(a, b);
        let parsed = uuid::Uuid::parse_str(&a).unwrap();
        assert_eq!(parsed.get_version_num(), 7);
    }

    #[test]
    fn from_headers_reads_only_the_exact_valid_key() {
        let h = |k: &str, v: &str| (k.to_string(), Bytes::from(v.to_string()));
        assert_eq!(from_headers(&[h("correlation-id", "abc-1")]), Some("abc-1"));
        assert_eq!(from_headers(&[h("x-correlation-id", "abc-1")]), None);
        assert_eq!(from_headers(&[h("traceparent", "abc-1")]), None);
        assert_eq!(from_headers(&[h("correlation-id", "bad id")]), None);
        assert_eq!(
            from_headers(&[h("correlation-id", "bad id"), h("correlation-id", "ok-2")]),
            Some("ok-2")
        );
        assert_eq!(from_headers(&[]), None);
    }

    fn plain_record(headers: Vec<(String, Bytes)>) -> PublishRecord {
        PublishRecord {
            key: None,
            headers,
            payload: Bytes::from_static(b"{}"),
            timestamp_ms: 0,
            schema_id: 0,
        }
    }

    #[test]
    fn settle_records_mints_per_record_without_a_batch_id() {
        let own = ("correlation-id".to_string(), Bytes::from_static(b"own-1"));
        let bad = ("correlation-id".to_string(), Bytes::from_static(b"bad id"));
        let mut records = vec![
            plain_record(vec![own]),
            plain_record(vec![]),
            plain_record(vec![bad]),
        ];
        let ids = settle_records(&mut records, None);
        assert_eq!(ids[0], "own-1");
        assert!(validate(&ids[1]) && validate(&ids[2]));
        assert_ne!(ids[1], ids[2], "minting is per record");
        assert_eq!(from_headers(&records[1].headers), Some(ids[1].as_str()));
        assert_eq!(from_headers(&records[2].headers), Some(ids[2].as_str()));
        assert_eq!(records[0].headers.len(), 1, "a valid header is left alone");
    }

    #[test]
    fn settle_records_uses_the_batch_id_without_touching_records() {
        let own = ("correlation-id".to_string(), Bytes::from_static(b"own-1"));
        let mut records = vec![plain_record(vec![own]), plain_record(vec![])];
        let ids = settle_records(&mut records, Some("batch-1"));
        assert_eq!(ids, vec!["own-1".to_string(), "batch-1".to_string()]);
        assert!(records[1].headers.is_empty());
    }

    #[test]
    fn from_fetched_headers_prefers_the_stamped_copy() {
        let h = |k: &str, v: &str| (Bytes::from(k.to_string()), Bytes::from(v.to_string()));
        assert_eq!(
            from_fetched_headers(&[h("correlation-id", "in"), h("tf.correlation_id", "out")]),
            Some("out".to_string())
        );
        assert_eq!(
            from_fetched_headers(&[h("correlation-id", "in")]),
            Some("in".to_string())
        );
        assert_eq!(
            from_fetched_headers(&[h("tf.correlation_id", "bad id")]),
            None
        );
    }
}
