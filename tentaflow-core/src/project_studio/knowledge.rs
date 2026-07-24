// ===== File: project_studio/knowledge.rs — shared knowledge-base access for flow nodes + agent tools =====
//
// One implementation of "search a project's knowledge base" reused by the
// `project_knowledge` flow node and the `core.project_search` /
// `core.project_list_sources` agent builtins (the dashboard's KbSearch handler
// keeps its own wire mapping in `dispatch/project_studio.rs`). Membership is
// always enforced here: a non-member gets the same error as a missing project,
// so a project's existence never leaks through a flow or an agent tool.

use anyhow::{anyhow, Result};

use super::models::ProjectRecord;
use super::{ingest, project_db, repository};
use crate::services::vector::error::VectorError;
use crate::services::vector::NamespaceManager;
use tentaflow_sdk_spec::{FieldValue, Filter};

/// Default number of hits when the caller does not ask for a specific `top_k`.
pub const DEFAULT_TOP_K: u64 = 8;
/// Hard cap on `top_k` — mirrors the dashboard KbSearch clamp.
pub const MAX_TOP_K: u64 = 50;
/// Snippet length used by envelope-bound callers (same as the dashboard KbHit).
pub const DEFAULT_SNIPPET_CHARS: usize = 400;

/// One knowledge-base hit with source metadata resolved from `project.db`.
/// `text` carries the full chunk text; callers snippet it to their own budget.
#[derive(Debug, Clone)]
pub struct KnowledgeHit {
    pub source_id: String,
    pub source_name: String,
    pub source_kind: String,
    pub file_id: String,
    pub file_path: String,
    pub chunk_index: u32,
    pub score: f32,
    pub text: String,
    pub location: String,
}

/// Uniform denial: the same message for "no such project" and "not a member",
/// so a caller probing project ids learns nothing about which ones exist.
fn access_denied() -> anyhow::Error {
    anyhow!("project not found or access denied")
}

/// Loads the project and requires the acting user to be a member (any role —
/// search is a read, matching the dashboard's Viewer gate). No admin override:
/// a flow node / agent tool always acts strictly as the user.
pub fn require_member(org_id: &str, project_id: &str, user_id: &str) -> Result<ProjectRecord> {
    project_db::validate_project_id(project_id).map_err(|_| access_denied())?;
    let record = repository::get_project(org_id, project_id)?.ok_or_else(access_denied)?;
    if repository::effective_role(project_id, user_id)?.is_none() {
        return Err(access_denied());
    }
    Ok(record)
}

/// Clamps a caller-supplied `top_k` into the supported range, defaulting when
/// absent. A hint outside the range is clamped (not rejected) — flows and
/// models should degrade, not fail, on an oversized ask.
pub fn clamp_top_k(raw: Option<u64>) -> usize {
    raw.unwrap_or(DEFAULT_TOP_K).clamp(1, MAX_TOP_K) as usize
}

/// Searches the project's `passages` namespace with an already-embedded query
/// vector. A project whose knowledge base was never ingested has no namespace —
/// that is an empty result, not an error. Source name/kind are joined from the
/// per-project `project.db` (skipped entirely when there are no hits).
pub fn search(
    vectors: &NamespaceManager,
    org_id: &str,
    project_id: &str,
    query_vec: &[f32],
    source_ids: &[String],
    top_k: usize,
) -> Result<Vec<KnowledgeHit>> {
    let scope = ingest::vector_scope(project_id);
    let backend = match vectors.get(org_id, &scope, ingest::VECTOR_NAMESPACE) {
        Ok(b) => b,
        Err(VectorError::NamespaceNotFound { .. }) => return Ok(Vec::new()),
        Err(e) => return Err(anyhow!("vector namespace: {e}")),
    };
    let filter = if source_ids.is_empty() {
        None
    } else {
        Some(Filter::In(
            "source_id".to_string(),
            source_ids
                .iter()
                .map(|s| FieldValue::Str(s.clone()))
                .collect(),
        ))
    };
    let output_fields: Vec<String> = [
        "doc_id",
        "chunk_index",
        "text",
        "source_id",
        "path",
        "location",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let raw_hits = backend
        .search(query_vec, top_k, filter.as_ref(), &output_fields)
        .map_err(|e| anyhow!("vector search: {e}"))?;
    if raw_hits.is_empty() {
        return Ok(Vec::new());
    }

    let pool = project_db::open(project_id)?;
    let source_meta: std::collections::HashMap<String, (String, String)> =
        repository::list_sources(&pool)?
            .into_iter()
            .map(|s| (s.record.source_id.clone(), (s.record.name, s.record.kind)))
            .collect();

    Ok(raw_hits
        .into_iter()
        .map(|hit| {
            let mut fields: std::collections::HashMap<String, String> =
                std::collections::HashMap::new();
            let mut chunk_index: u32 = 0;
            for f in hit.fields {
                match f.value {
                    FieldValue::Str(s) => {
                        fields.insert(f.name, s);
                    }
                    FieldValue::Int(i) if f.name == "chunk_index" => {
                        chunk_index = i.max(0) as u32;
                    }
                    _ => {}
                }
            }
            let source_id = fields.remove("source_id").unwrap_or_default();
            let (source_name, source_kind) = source_meta
                .get(&source_id)
                .cloned()
                .unwrap_or_else(|| (source_id.clone(), String::new()));
            KnowledgeHit {
                source_id,
                source_name,
                source_kind,
                file_id: fields.remove("doc_id").unwrap_or_default(),
                file_path: fields.remove("path").unwrap_or_default(),
                chunk_index,
                score: hit.score,
                text: fields.remove("text").unwrap_or_default(),
                location: fields.remove("location").unwrap_or_default(),
            }
        })
        .collect())
}

/// Char-boundary-safe snippet of a chunk text.
fn snippet(text: &str, max_chars: usize) -> String {
    text.chars().take(max_chars).collect()
}

/// Serializes hits to the shared JSON contract of the `project_knowledge` node
/// and the `core.project_search` tool.
pub fn hits_to_json(hits: &[KnowledgeHit], snippet_chars: usize) -> serde_json::Value {
    serde_json::json!({
        "hits": hits
            .iter()
            .map(|h| serde_json::json!({
                "source_id": h.source_id,
                "source_name": h.source_name,
                "file_path": h.file_path,
                "chunk_index": h.chunk_index,
                "score": h.score,
                "snippet": snippet(&h.text, snippet_chars),
                "location": h.location,
            }))
            .collect::<Vec<_>>(),
    })
}

/// Like `hits_to_json` but bounded to `max_chars` of serialized JSON: snippets
/// are halved until the payload fits, then trailing hits are dropped. Keeps a
/// tool result valid JSON instead of relying on the middle-out truncation of
/// tool_exec (which would cut through the JSON).
pub fn hits_to_json_bounded(hits: &[KnowledgeHit], max_chars: usize) -> serde_json::Value {
    let mut kept = hits.len();
    let mut snippet_chars = DEFAULT_SNIPPET_CHARS;
    loop {
        let json = hits_to_json(&hits[..kept], snippet_chars);
        if json.to_string().chars().count() <= max_chars || kept == 0 {
            return json;
        }
        if snippet_chars > 100 {
            snippet_chars /= 2;
        } else {
            kept -= 1;
        }
    }
}

/// Citation entries for `envelope.meta["rag_citations"]` — same core keys the
/// `vector` node emits (`doc_id`/`chunk_index`/`text`/`score`), extended with
/// the project source metadata so ps-chat can render a richer reference.
pub fn citations_json(hits: &[KnowledgeHit], snippet_chars: usize) -> serde_json::Value {
    serde_json::Value::Array(
        hits.iter()
            .map(|h| {
                serde_json::json!({
                    "doc_id": h.file_id,
                    "chunk_index": h.chunk_index,
                    "text": snippet(&h.text, snippet_chars),
                    "score": h.score,
                    "source_id": h.source_id,
                    "source_name": h.source_name,
                    "file_path": h.file_path,
                    "location": h.location,
                })
            })
            .collect(),
    )
}

/// Lists the project's knowledge sources (id, name, kind, status, counters) as
/// the shared JSON contract of the node's `list_sources` operation and the
/// `core.project_list_sources` tool.
pub fn list_sources_json(project_id: &str) -> Result<serde_json::Value> {
    let pool = project_db::open(project_id)?;
    let items = repository::list_sources(&pool)?;
    Ok(serde_json::json!({
        "sources": items
            .iter()
            .map(|s| serde_json::json!({
                "source_id": s.record.source_id,
                "name": s.record.name,
                "kind": s.record.kind,
                "status": s.record.status,
                "file_count": s.file_count,
                "chunk_count": s.chunk_count,
            }))
            .collect::<Vec<_>>(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(i: u32, text_len: usize) -> KnowledgeHit {
        KnowledgeHit {
            source_id: format!("src-{i}"),
            source_name: format!("Source {i}"),
            source_kind: "files".into(),
            file_id: format!("file-{i}"),
            file_path: format!("docs/file-{i}.md"),
            chunk_index: i,
            score: i as f32,
            text: "x".repeat(text_len),
            location: String::new(),
        }
    }

    #[test]
    fn clamp_top_k_defaults_and_clamps() {
        assert_eq!(clamp_top_k(None), DEFAULT_TOP_K as usize);
        assert_eq!(clamp_top_k(Some(0)), 1);
        assert_eq!(clamp_top_k(Some(500)), MAX_TOP_K as usize);
        assert_eq!(clamp_top_k(Some(3)), 3);
    }

    #[test]
    fn bounded_json_fits_the_budget() {
        let hits: Vec<KnowledgeHit> = (0..50).map(|i| hit(i, 2_000)).collect();
        let budget = 15_000;
        let json = hits_to_json_bounded(&hits, budget);
        assert!(json.to_string().chars().count() <= budget);
        // The bounded result still returns SOME hits (it shrinks, not empties).
        assert!(!json["hits"].as_array().unwrap().is_empty());
    }

    #[test]
    fn snippet_is_char_boundary_safe() {
        let h = KnowledgeHit {
            text: "żółć".repeat(200),
            ..hit(0, 0)
        };
        let json = hits_to_json(&[h], 5);
        assert_eq!(json["hits"][0]["snippet"].as_str().unwrap(), "żółćż");
    }
}
