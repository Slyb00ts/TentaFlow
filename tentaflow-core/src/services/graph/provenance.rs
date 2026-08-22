// ===== File: services/graph/provenance.rs — multi-document provenance sets =====
//
// A graph row is written once per DOCUMENT that names it, but the row itself is
// SHARED: entity ids are normalized (`ETH Zurich` and `eth zurich` land on
// `eth_zurich`), so two documents mentioning the same entity write the same
// `nodes` key. Cozo's only upsert is `:put`, which is last-writer-wins on that
// key and offers no set-union — a single-valued `provenance.doc_id` therefore
// remembered ONLY the document that happened to write last, and deleting that
// document tombstoned a row the other document still relied on.
//
// The document reference is a SET instead: `doc_ids`. An upsert UNIONS the
// incoming set into the stored one (`merge`); a per-document delete drops ONE
// member (`without_doc`) and tombstones only once the set is `Emptied`. The
// read-modify-write that a `:put` cannot express is performed by `collection.rs`
// inside the per-collection write lock — the same critical section that already
// makes the quota check+mutate atomic — so two concurrent ingests of different
// documents cannot lose each other's membership.
//
// The rest of the provenance object (source_id, path, flow_node_id, or whatever
// an addon writes) stays last-writer-wins: it describes the WRITE, nothing reads
// it back, and only the document set decides what survives a delete.
//
// Rows written before this representation carry a scalar `doc_id`, and a Cozo
// collection has no migration path, so the readers accept that shape as the
// one-element set it always meant. A provenance that is absent, `null` or
// unparsable names NO document and is never swept — deleting on a parse failure
// would make a malformed row match EVERY document.

use serde_json::{Map, Value};

/// Canonical key of the document set inside a provenance object.
pub const DOC_IDS_KEY: &str = "doc_ids";

/// Pre-set spelling of the same reference: a single document id.
const LEGACY_DOC_ID_KEY: &str = "doc_id";

/// Outcome of removing one document from a stored provenance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DocRemoval {
    /// The row does not belong to that document — leave it alone.
    NotNamed,
    /// Other documents still name the row; store this provenance instead.
    Shrunk(String),
    /// That was the last document naming the row — tombstone it.
    Emptied,
}

/// Documents named by a stored provenance JSON, in stored order, deduplicated.
pub fn doc_ids_of(provenance_json: &str) -> Vec<String> {
    let Ok(value) = serde_json::from_str::<Value>(provenance_json) else {
        return Vec::new();
    };
    let Some(obj) = value.as_object() else {
        return Vec::new();
    };
    let mut ids: Vec<String> = Vec::new();
    let mut push = |candidate: &Value| {
        if let Some(id) = candidate.as_str().filter(|s| !s.is_empty()) {
            if !ids.iter().any(|kept| kept == id) {
                ids.push(id.to_string());
            }
        }
    };
    if let Some(list) = obj.get(DOC_IDS_KEY).and_then(|v| v.as_array()) {
        for entry in list {
            push(entry);
        }
    }
    if let Some(legacy) = obj.get(LEGACY_DOC_ID_KEY) {
        push(legacy);
    }
    ids
}

/// Provenance to store for an upsert: the incoming object carrying the UNION of
/// the stored and incoming document sets.
///
/// When the union is empty the incoming JSON is returned verbatim, so a writer
/// that names no document (an addon passing `null` or `{}`) keeps its exact
/// provenance. When it is not, the result must be an object able to hold the
/// set: an incoming non-object is replaced by one, because carrying the incoming
/// shape instead would drop the membership of documents that still name the row
/// — the very loss this representation exists to prevent.
pub fn merge(stored: Option<&str>, incoming: &str) -> String {
    let mut ids = stored.map(doc_ids_of).unwrap_or_default();
    for id in doc_ids_of(incoming) {
        if !ids.iter().any(|kept| kept == &id) {
            ids.push(id);
        }
    }
    if ids.is_empty() {
        return incoming.to_string();
    }
    let mut obj = serde_json::from_str::<Value>(incoming)
        .ok()
        .and_then(|v| match v {
            Value::Object(map) => Some(map),
            _ => None,
        })
        .unwrap_or_else(Map::new);
    write_doc_ids(&mut obj, ids);
    Value::Object(obj).to_string()
}

/// Removes one document from a stored provenance.
pub fn without_doc(provenance_json: &str, doc_id: &str) -> DocRemoval {
    let ids = doc_ids_of(provenance_json);
    if !ids.iter().any(|id| id == doc_id) {
        return DocRemoval::NotNamed;
    }
    let remaining: Vec<String> = ids.into_iter().filter(|id| id != doc_id).collect();
    if remaining.is_empty() {
        return DocRemoval::Emptied;
    }
    // The row survives, so the shrunken set has to be written back: a later
    // delete of one of the remaining documents reads this value, and leaving the
    // removed id in place would keep the row alive after its last document went.
    let mut obj = match serde_json::from_str::<Value>(provenance_json) {
        Ok(Value::Object(map)) => map,
        _ => Map::new(),
    };
    write_doc_ids(&mut obj, remaining);
    DocRemoval::Shrunk(Value::Object(obj).to_string())
}

/// Stores `ids` as the canonical set and retires the pre-set spelling, so a row
/// never carries two sources of truth for the same membership.
fn write_doc_ids(obj: &mut Map<String, Value>, ids: Vec<String>) {
    obj.remove(LEGACY_DOC_ID_KEY);
    obj.insert(
        DOC_IDS_KEY.to_string(),
        Value::Array(ids.into_iter().map(Value::String).collect()),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_unions_document_sets() {
        let first = merge(None, r#"{"doc_ids":["file-1"],"path":"a.txt"}"#);
        let both = merge(Some(&first), r#"{"doc_ids":["file-2"],"path":"b.txt"}"#);
        assert_eq!(doc_ids_of(&both), vec!["file-1", "file-2"]);
        // Descriptive fields stay last-writer-wins; only the set accumulates.
        let value: Value = serde_json::from_str(&both).unwrap();
        assert_eq!(value.get("path").and_then(|v| v.as_str()), Some("b.txt"));
    }

    #[test]
    fn merge_is_idempotent_for_the_same_document() {
        let once = merge(None, r#"{"doc_ids":["file-1"]}"#);
        let twice = merge(Some(&once), r#"{"doc_ids":["file-1"]}"#);
        assert_eq!(doc_ids_of(&twice), vec!["file-1"]);
    }

    #[test]
    fn merge_keeps_a_provenance_that_names_no_document() {
        assert_eq!(merge(Some("null"), "null"), "null");
        assert_eq!(merge(None, "{}"), "{}");
    }

    #[test]
    fn merge_carries_the_set_over_a_writer_that_names_no_document() {
        let stored = merge(None, r#"{"doc_ids":["file-1"]}"#);
        assert_eq!(doc_ids_of(&merge(Some(&stored), "null")), vec!["file-1"]);
    }

    #[test]
    fn a_pre_set_scalar_reads_as_a_one_element_set() {
        assert_eq!(doc_ids_of(r#"{"doc_id":"file-1"}"#), vec!["file-1"]);
        assert_eq!(
            without_doc(r#"{"doc_id":"file-1"}"#, "file-1"),
            DocRemoval::Emptied
        );
        let merged = merge(Some(r#"{"doc_id":"file-1"}"#), r#"{"doc_ids":["file-2"]}"#);
        assert_eq!(doc_ids_of(&merged), vec!["file-1", "file-2"]);
        assert!(
            serde_json::from_str::<Value>(&merged)
                .unwrap()
                .get(LEGACY_DOC_ID_KEY)
                .is_none(),
            "the retired spelling must not survive as a second source of truth"
        );
    }

    #[test]
    fn unparsable_or_empty_provenance_names_no_document() {
        assert!(doc_ids_of("not json").is_empty());
        assert!(doc_ids_of("null").is_empty());
        assert_eq!(without_doc("not json", "file-1"), DocRemoval::NotNamed);
        assert_eq!(without_doc("null", "file-1"), DocRemoval::NotNamed);
    }

    #[test]
    fn without_doc_shrinks_when_another_document_remains() {
        let stored = merge(
            Some(r#"{"doc_ids":["file-1"]}"#),
            r#"{"doc_ids":["file-2"]}"#,
        );
        let DocRemoval::Shrunk(next) = without_doc(&stored, "file-2") else {
            panic!("file-1 still names the row");
        };
        assert_eq!(doc_ids_of(&next), vec!["file-1"]);
        assert_eq!(without_doc(&next, "file-1"), DocRemoval::Emptied);
    }
}
