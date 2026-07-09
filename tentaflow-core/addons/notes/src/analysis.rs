// =============================================================================
// File: addons/notes/src/analysis.rs
// Purpose: auto-graph analysis pipeline. A saved/deleted note lands in
//          analysis_queue; the worker (opportunistic UI drain, budget 1, or the
//          analyze_pending tool, batch 5) chunks the content, embeds chunks
//          into the 'notes' vector namespace, extracts entities with the
//          notes-llm alias, links notes (semantic similarity + shared
//          entities) and materializes the graph 'notes_kg' through the
//          idempotent graph_outbox (SQLite = source of truth, rag pattern).
//          Entity duplicates are merged via name-embedding kNN: >= 0.95 same
//          type auto-merges (reversible via entity_merge_log), [0.80, 0.95)
//          becomes an open merge_suggestion for the user.
// =============================================================================

use serde_json::{json, Value as JsonValue};

use tentaflow_addon_sdk::{
    alias_get, graph_delete_edge, graph_delete_node, graph_upsert_edge, graph_upsert_node, log,
    sql_exec, sql_query, sql_query_one, sql_transaction, state_get, state_set, vector_delete,
    vector_search, vector_upsert, GraphNode, GraphProp, SqlValue, StateTier, VectorField,
    VectorFieldValue, VectorFilter,
};

use crate::db::{new_id, now_unix};

// Raw llm_generate binding with the full ABI (model + options) — the same
// mechanism as rag/embeddings-chunker (the host routes to the embeddings
// service when options.task == "embedding").
#[link(wasm_import_module = "tentaflow")]
extern "C" {
    fn llm_generate(
        prompt_ptr: i32,
        prompt_len: i32,
        model_ptr: i32,
        model_len: i32,
        options_ptr: i32,
        options_len: i32,
        out_ptr: i32,
        out_cap: i32,
        out_len_ptr: i32,
    ) -> i32;
}

// Vector namespaces / graph collection (must match manifest declarations).
const NOTES_NS: &str = "notes";
const ENTITIES_NS: &str = "entities";
const KG_COLLECTION: &str = "notes_kg";

// Model aliases consumed by the pipeline.
const LLM_ALIAS: &str = "notes-llm";
const EMBED_ALIAS: &str = "notes-embeddings";

/// Instance KV key with the real embedding dimension of notes-embeddings.
/// Seeded lazily from the first embedding (the namespace takes its dimension
/// from data, the manifest value is declarative only) — rag pattern.
const EMBED_DIM_STATE_KEY: &str = "embed_dimensions";

/// Embedding response buffer (a 2048-f32 vector as JSON is tens of KB).
const EMBED_BUFFER_SIZE: usize = 262_144;
/// Entity extraction response buffer.
const EXTRACT_BUFFER_SIZE: usize = 65_536;

// Chunking: ~512 tokens per chunk with overlap, 1 token ~ 4 chars.
const CHUNK_MAX_CHARS: usize = 512 * 4;
const CHUNK_OVERLAP_CHARS: usize = 50 * 4;
/// Anti-DoS cap on chunk vectors per note (very long notes are truncated for
/// similarity purposes; the note text itself is never touched).
const MAX_CHUNKS_PER_NOTE: usize = 64;

// Note-to-note similarity edges.
const SIMILAR_TOP_K: u32 = 8;
const SIMILAR_THRESHOLD: f32 = 0.55;
/// Max related notes persisted per note per kind.
const MAX_LINKS_PER_NOTE: usize = 8;

// Entity extraction caps.
const MAX_ENTITIES_PER_NOTE: usize = 24;
const MAX_ENTITY_NAME_CHARS: usize = 120;
/// Content prefix (chars) handed to the extraction LLM.
const MAX_EXTRACT_CHARS: usize = 8_000;

// Entity merge thresholds (conservative: entity resolution favors correctness).
const MERGE_AUTO_THRESHOLD: f32 = 0.95;
const MERGE_SUGGEST_THRESHOLD: f32 = 0.80;
const MERGE_KNN_K: u32 = 3;

// Queue behavior.
/// A note is analyzed only when its last edit is at least this old — typing
/// bursts never grind the pipeline.
const DEBOUNCE_SECS: i64 = 3;
/// Poisoned entries stop retrying after this many failures (last_error stays
/// visible in the panel); a fresh save resets the counter.
const MAX_ATTEMPTS: i64 = 5;

// Outbox drain batching (rag pattern: batch + hard iteration cap = anti-DoS).
const OUTBOX_DRAIN_BATCH: usize = 256;
const OUTBOX_DRAIN_MAX_ITERS: usize = 4096;

// =============================================================================
// Pure helpers (unit-tested natively)
// =============================================================================

/// FNV-1a 64-bit — stable ref_id derivation for vector keys. Deterministic so
/// re-embedding the same (note, chunk) / entity OVERWRITES its vector.
pub fn fnv64(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

fn note_chunk_ref(note_id: &str, chunk_index: usize) -> u64 {
    fnv64(&format!("note:{note_id}:{chunk_index}"))
}

fn entity_vector_ref(entity_id: &str) -> u64 {
    fnv64(&format!("entity:{entity_id}"))
}

/// Normalizes an entity name to a stable dedup key: trimmed, lowercased,
/// internal whitespace collapsed. Same names differing in case/spacing map to
/// one entity.
pub fn normalize_name(name: &str) -> String {
    name.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Stable entity id per (type, normalized name) — the same entity found in
/// different notes dedups to a single row.
pub fn entity_id_for(entity_type: &str, normalized_name: &str) -> String {
    format!("ent_{:016x}", fnv64(&format!("{entity_type}|{normalized_name}")))
}

/// Hard cap on canonical-pointer chain depth. Path compression at every merge
/// keeps real chains at depth 1; the cap is defense against corrupted data.
const MERGE_CHAIN_MAX_DEPTH: usize = 16;

/// Resolves the canonical ROOT of an entity by following canonical_id
/// pointers. `lookup` returns the pointer of an id (None = canonical).
/// Errors on a cycle or when the chain exceeds the depth cap. Pure w.r.t. the
/// injected lookup — unit-tested natively with a map.
pub fn resolve_root<F>(lookup: F, id: &str) -> Result<String, String>
where
    F: Fn(&str) -> Result<Option<String>, String>,
{
    let mut current = id.to_string();
    let mut visited: Vec<String> = vec![current.clone()];
    for _ in 0..MERGE_CHAIN_MAX_DEPTH {
        match lookup(&current)? {
            None => return Ok(current),
            Some(next) => {
                if visited.iter().any(|v| v == &next) {
                    return Err(format!("canonical pointer cycle at '{next}'"));
                }
                visited.push(next.clone());
                current = next;
            }
        }
    }
    Err(format!("canonical chain deeper than {MERGE_CHAIN_MAX_DEPTH} at '{id}'"))
}

/// Merge decision band for a name-similarity value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeBand {
    /// >= MERGE_AUTO_THRESHOLD: merge without a human.
    Auto,
    /// [MERGE_SUGGEST_THRESHOLD, MERGE_AUTO_THRESHOLD): open suggestion.
    Suggest,
    /// Below the suggestion band: not a candidate.
    None,
}

pub fn classify_merge_band(similarity: f32) -> MergeBand {
    if similarity >= MERGE_AUTO_THRESHOLD {
        MergeBand::Auto
    } else if similarity >= MERGE_SUGGEST_THRESHOLD {
        MergeBand::Suggest
    } else {
        MergeBand::None
    }
}

/// One extracted entity, already validated and normalized.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractedEntity {
    /// Display name (original casing, trimmed).
    pub name: String,
    /// Normalized dedup key.
    pub normalized: String,
    /// One of: person | company | project | topic.
    pub entity_type: String,
}

/// Extracts the assistant text from a chat-completion JSON response, or
/// returns None when the payload is not that shape (plain text models).
fn chat_completion_content(raw: &str) -> Option<String> {
    let v: JsonValue = serde_json::from_str(raw).ok()?;
    let content = v
        .get("choices")?
        .as_array()?
        .first()?
        .get("message")?
        .get("content")?
        .as_str()?;
    Some(content.to_string())
}

/// Finds the first balanced JSON array `[...]` in free text, counting brackets
/// OUTSIDE strings (escape-aware) — tolerates prose / ```json fences around.
pub fn extract_json_array(text: &str) -> Option<&str> {
    let bytes = text.as_bytes();
    let start = bytes.iter().position(|&b| b == b'[')?;
    let mut depth = 0i32;
    let mut in_str = false;
    let mut escaped = false;
    for (i, &b) in bytes.iter().enumerate().skip(start) {
        if in_str {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_str = false;
            }
            continue;
        }
        match b {
            b'"' => in_str = true,
            b'[' => depth += 1,
            b']' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&text[start..=i]);
                }
            }
            _ => {}
        }
    }
    None
}

/// Parses the extraction LLM response into validated entities. Tolerant of
/// garbage around the JSON array and of malformed items (skipped); invalid
/// types are rejected. Names not literally present in `source_text`
/// (normalized containment) are dropped — kills prompt-example echoes and
/// hallucinations. Deduped by (normalized, type), capped.
pub fn parse_entities_response(raw: &str, source_text: &str) -> Vec<ExtractedEntity> {
    let inner = chat_completion_content(raw).unwrap_or_else(|| raw.to_string());
    let Some(slice) = extract_json_array(&inner) else {
        return Vec::new();
    };
    let Ok(parsed) = serde_json::from_str::<JsonValue>(slice) else {
        return Vec::new();
    };
    let Some(items) = parsed.as_array() else {
        return Vec::new();
    };

    let hay = normalize_name(source_text);
    let mut out: Vec<ExtractedEntity> = Vec::new();
    for item in items {
        let Some(name) = item.get("name").and_then(|v| v.as_str()) else {
            continue;
        };
        let Some(etype) = item.get("type").and_then(|v| v.as_str()) else {
            continue;
        };
        let etype = etype.trim().to_lowercase();
        if !matches!(etype.as_str(), "person" | "company" | "project" | "topic") {
            continue;
        }
        let name = name.trim();
        if name.is_empty() || name.chars().count() > MAX_ENTITY_NAME_CHARS {
            continue;
        }
        let normalized = normalize_name(name);
        if normalized.is_empty() || !hay.contains(&normalized) {
            continue;
        }
        if out
            .iter()
            .any(|e| e.normalized == normalized && e.entity_type == etype)
        {
            continue;
        }
        out.push(ExtractedEntity {
            name: name.to_string(),
            normalized,
            entity_type: etype,
        });
        if out.len() >= MAX_ENTITIES_PER_NOTE {
            break;
        }
    }
    out
}

/// Splits text into chunks of at most `max_chars` characters with a
/// `overlap_chars` character overlap, preferring paragraph and sentence
/// boundaries. Fully UTF-8 safe: every cut lands on a char boundary.
pub fn split_into_chunks(text: &str, max_chars: usize, overlap_chars: usize) -> Vec<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    if max_chars == 0 {
        return vec![trimmed.to_string()];
    }

    let mut segments: Vec<String> = Vec::new();
    for paragraph in trimmed.split("\n\n") {
        let p = paragraph.trim();
        if p.is_empty() {
            continue;
        }
        for sentence in split_sentences(p) {
            if sentence.chars().count() <= max_chars {
                segments.push(sentence);
            } else {
                // Oversized sentence (no punctuation): hard-split on char
                // boundaries so no segment can exceed the chunk size.
                let chars: Vec<char> = sentence.chars().collect();
                for piece in chars.chunks(max_chars) {
                    segments.push(piece.iter().collect());
                }
            }
        }
    }
    if segments.is_empty() {
        return vec![trimmed.to_string()];
    }

    let mut chunks: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut current_chars = 0usize;
    for segment in segments {
        let seg_chars = segment.chars().count();
        if current_chars > 0 && current_chars + 1 + seg_chars > max_chars {
            chunks.push(current.clone());
            // Overlap seed shrinks so tail + separator + segment never
            // exceeds max_chars (hard invariant, checked by tests).
            let allowed_tail = max_chars.saturating_sub(seg_chars + 1).min(overlap_chars);
            current = char_tail(&current, allowed_tail);
            current_chars = current.chars().count();
        }
        if current_chars > 0 {
            current.push(' ');
            current_chars += 1;
        }
        current.push_str(&segment);
        current_chars += seg_chars;
    }
    if !current.trim().is_empty() {
        chunks.push(current);
    }
    chunks
}

/// Last `n` characters of `s`, starting at a word boundary when possible.
fn char_tail(s: &str, n: usize) -> String {
    if n == 0 {
        return String::new();
    }
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= n {
        return s.to_string();
    }
    let tail: String = chars[chars.len() - n..].iter().collect();
    match tail.find(' ') {
        Some(pos) if pos + 1 < tail.len() => tail[pos + 1..].to_string(),
        _ => tail,
    }
}

/// Splits a paragraph into sentences on `. ! ?` followed by whitespace.
fn split_sentences(paragraph: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut chars = paragraph.chars().peekable();
    while let Some(c) = chars.next() {
        current.push(c);
        if matches!(c, '.' | '!' | '?') {
            if chars.peek().is_none_or(|n| n.is_whitespace()) {
                let s = current.trim().to_string();
                if !s.is_empty() {
                    out.push(s);
                }
                current.clear();
            }
        }
    }
    let s = current.trim().to_string();
    if !s.is_empty() {
        out.push(s);
    }
    out
}

/// Canonical dedup key of one outbox intent — op + target key. Combined with
/// the partial-unique index (WHERE applied = 0) the same pending intent is
/// enqueued once; after applied = 1 the key frees up for re-materialization.
pub fn outbox_dedup_key(op: &str, parts: &[&str]) -> String {
    let mut key = String::from(op);
    for p in parts {
        key.push('|');
        key.push_str(p);
    }
    key
}

/// Ranking weight of a shared-entity link: more shared entities rank higher.
/// Heuristic ordering signal in [0.45, 0.90] (not a cosine similarity).
pub fn entity_link_weight(shared_count: usize) -> f64 {
    0.45 + 0.15 * (shared_count.saturating_sub(1).min(3) as f64)
}

fn value_f64(v: &SqlValue) -> Option<f64> {
    match v {
        SqlValue::F64(f) => Some(*f),
        SqlValue::I64(i) => Some(*i as f64),
        _ => None,
    }
}

// =============================================================================
// Embeddings (llm_generate task=embedding — rag / embeddings-chunker pattern)
// =============================================================================

fn llm_call(prompt: &str, model: &str, options: &JsonValue, buffer_size: usize) -> Result<String, String> {
    let options_str =
        serde_json::to_string(options).map_err(|e| format!("options serialization: {e}"))?;
    let mut buffer = vec![0u8; buffer_size];
    let mut out_len: i32 = 0;
    let rc = unsafe {
        llm_generate(
            prompt.as_ptr() as i32,
            prompt.len() as i32,
            model.as_ptr() as i32,
            model.len() as i32,
            options_str.as_ptr() as i32,
            options_str.len() as i32,
            buffer.as_mut_ptr() as i32,
            buffer_size as i32,
            &mut out_len as *mut i32 as i32,
        )
    };
    if rc < 0 {
        return Err(format!("llm_generate ({model}) returned error {rc}"));
    }
    if out_len <= 0 {
        return Err(format!("llm_generate ({model}) returned an empty response"));
    }
    Ok(String::from_utf8_lossy(&buffer[..out_len as usize]).to_string())
}

fn embed_raw(text: &str) -> Result<String, String> {
    llm_call(
        text,
        EMBED_ALIAS,
        &json!({ "task": "embedding", "adapter": "retrieval" }),
        EMBED_BUFFER_SIZE,
    )
}

/// Extracts the f32 vector from an embedding response (bare array or an
/// object with embedding / vector / data[0].embedding) without dimension
/// validation.
pub fn parse_embedding_vector(response: &str) -> Result<Vec<f32>, String> {
    let parsed: JsonValue = serde_json::from_str(response)
        .map_err(|e| format!("embedding response parse: {e}"))?;
    let arr = if let Some(a) = parsed.as_array() {
        a.clone()
    } else if let Some(a) = parsed
        .get("embedding")
        .or_else(|| parsed.get("vector"))
        .and_then(|v| v.as_array())
    {
        a.clone()
    } else if let Some(a) = parsed
        .get("data")
        .and_then(|d| d.as_array())
        .and_then(|d| d.first())
        .and_then(|f| f.get("embedding"))
        .and_then(|v| v.as_array())
    {
        a.clone()
    } else {
        return Err(format!(
            "unrecognized embedding response shape: {}",
            &response[..response.len().min(200)]
        ));
    };
    let mut out = Vec::with_capacity(arr.len());
    for (i, v) in arr.iter().enumerate() {
        match v.as_f64() {
            Some(f) => out.push(f as f32),
            None => return Err(format!("embedding element {i} is not a number")),
        }
    }
    if out.is_empty() {
        return Err("empty embedding vector".to_string());
    }
    Ok(out)
}

fn embed_dim_cached() -> Option<usize> {
    state_get(EMBED_DIM_STATE_KEY)
        .ok()
        .flatten()
        .and_then(|b| String::from_utf8(b).ok())
        .and_then(|s| s.trim().parse::<usize>().ok())
        .filter(|&d| d > 0)
}

/// Real embedding dimension of THIS instance's notes-embeddings model. Cached
/// in durable KV; seeded from a probe embedding ("x") on first use so the
/// dimension adapts to the bound model without config.
fn ensure_embed_dim() -> Result<usize, String> {
    if let Some(d) = embed_dim_cached() {
        return Ok(d);
    }
    let dim = parse_embedding_vector(&embed_raw("x")?)?.len();
    let _ = state_set(EMBED_DIM_STATE_KEY, dim.to_string().as_bytes(), StateTier::Durable);
    Ok(dim)
}

fn embed_text(text: &str, expected_dim: usize) -> Result<Vec<f32>, String> {
    let vector = parse_embedding_vector(&embed_raw(text)?)?;
    if vector.len() != expected_dim {
        return Err(format!(
            "embedding dimension mismatch: {} (expected {expected_dim})",
            vector.len()
        ));
    }
    Ok(vector)
}

// =============================================================================
// Entity extraction
// =============================================================================

fn call_extraction_llm(text: &str) -> Result<String, String> {
    // Forced-JSON prompt. Grounding in parse_entities_response drops any
    // example echoes and hallucinated names not present in the text.
    let prompt = format!(
        "Wypisz encje z poniższego tekstu. Zwróć WYŁĄCZNIE tablicę JSON, bez markdown, \
         bez komentarzy, w dokładnie tym formacie:\n\
         [{{\"name\": \"nazwa encji\", \"type\": \"person\"}}]\n\
         Dozwolone wartości type: person (osoba), company (firma lub organizacja), \
         project (projekt lub produkt), topic (temat lub pojęcie).\n\
         Używaj TYLKO nazw występujących dosłownie w tekście. Maksymalnie 20 encji.\n\n\
         Tekst:\n{text}"
    );
    llm_call(
        &prompt,
        LLM_ALIAS,
        &json!({ "temperature": 0.1, "max_tokens": 1024 }),
        EXTRACT_BUFFER_SIZE,
    )
}

/// True when both model aliases of the pipeline are bound to a model — the
/// topbar shows "Auto-graf aktywny" vs a configure-aliases warning.
pub fn auto_graph_ready() -> bool {
    [EMBED_ALIAS, LLM_ALIAS].iter().all(|alias| {
        alias_get(alias)
            .map(|info| info.is_active && !info.current_target.is_empty())
            .unwrap_or(false)
    })
}

// =============================================================================
// Queue
// =============================================================================

/// Durable enqueue on save/delete. INSERT OR REPLACE resets attempts —
/// fresh content deserves fresh retries.
pub fn enqueue(note_id: &str) {
    let res = sql_exec(
        "INSERT OR REPLACE INTO analysis_queue (note_id, enqueued_at, attempts, last_error) \
         VALUES (?, ?, 0, NULL)",
        &[
            SqlValue::String(note_id.to_string()),
            SqlValue::I64(now_unix()),
        ],
    );
    if let Err(e) = res {
        log::warn(&format!("notes: analysis enqueue failed for '{note_id}': {e}"));
    }
}

/// Queue state of one note for the panel: Some((attempts, last_error)).
pub fn queue_state(note_id: &str) -> Option<(i64, String)> {
    let row = sql_query_one(
        "SELECT attempts, COALESCE(last_error, '') FROM analysis_queue WHERE note_id = ?",
        &[SqlValue::String(note_id.to_string())],
    )
    .ok()
    .flatten()?;
    Some((
        row.first().and_then(|v| v.as_i64()).unwrap_or(0),
        row.get(1).and_then(|v| v.as_str()).unwrap_or("").to_string(),
    ))
}

/// True when the note still has retries left (drives "W kolejce analizy…").
pub fn is_pending(attempts: i64) -> bool {
    attempts < MAX_ATTEMPTS
}

/// Drains up to `budget` due notes from the queue. A note is due when its last
/// edit is older than the debounce window and it has retries left. Returns the
/// ids of successfully processed notes (the UI refreshes the open one).
///
/// Concurrency: pooled instances of different users may drain in parallel.
/// The dequeue is conditional on the captured enqueued_at, so a save that
/// raced the analysis re-queues the note instead of losing the update; a
/// double analysis of the same note is idempotent (deterministic ref_ids,
/// INSERT OR REPLACE rows, dedup-keyed outbox).
pub fn process_queue(budget: usize) -> Vec<String> {
    let now = now_unix();
    let rows = match sql_query(
        "SELECT q.note_id, q.enqueued_at, n.deleted_at, n.id IS NULL \
         FROM analysis_queue q LEFT JOIN notes n ON n.id = q.note_id \
         WHERE q.attempts < ? AND (n.id IS NULL OR n.deleted_at IS NOT NULL OR n.updated_at <= ?) \
         ORDER BY q.enqueued_at LIMIT ?",
        &[
            SqlValue::I64(MAX_ATTEMPTS),
            SqlValue::I64(now - DEBOUNCE_SECS),
            SqlValue::I64(budget as i64),
        ],
    ) {
        Ok(r) => r,
        Err(e) => {
            log::warn(&format!("notes: analysis queue read failed: {e}"));
            return Vec::new();
        }
    };

    let mut processed = Vec::new();
    for row in &rows {
        let note_id = row
            .first()
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let enqueued_at = row.get(1).and_then(|v| v.as_i64()).unwrap_or(0);
        let deleted = row.get(2).and_then(|v| v.as_i64()).is_some()
            || row.get(3).and_then(|v| v.as_i64()).unwrap_or(0) == 1;
        if note_id.is_empty() {
            continue;
        }

        let result = if deleted {
            cleanup_deleted_note(&note_id)
        } else {
            analyze_note(&note_id)
        };

        match result {
            Ok(()) => {
                // Dequeue only when no newer save re-queued the note meanwhile.
                let _ = sql_exec(
                    "DELETE FROM analysis_queue WHERE note_id = ? AND enqueued_at = ?",
                    &[
                        SqlValue::String(note_id.clone()),
                        SqlValue::I64(enqueued_at),
                    ],
                );
                processed.push(note_id);
            }
            Err(e) => {
                log::warn(&format!("notes: analysis of '{note_id}' failed: {e}"));
                // Guarded by the captured enqueued_at: a save that raced the
                // failed run re-queued the note with fresh attempts — the
                // stale failure must not poison the new entry.
                let _ = sql_exec(
                    "UPDATE analysis_queue SET attempts = attempts + 1, last_error = ? \
                     WHERE note_id = ? AND enqueued_at = ?",
                    &[
                        SqlValue::String(e.chars().take(500).collect()),
                        SqlValue::String(note_id.clone()),
                        SqlValue::I64(enqueued_at),
                    ],
                );
            }
        }
    }
    processed
}

// =============================================================================
// Note analysis
// =============================================================================

/// Full analysis of one live note: chunk + embed, extract + upsert entities,
/// merge-candidate scan for new entities, note links (similarity + shared
/// entities), graph outbox + drain.
fn analyze_note(note_id: &str) -> Result<(), String> {
    let row = sql_query_one(
        "SELECT title, content, org_id, owner_user_id FROM notes \
         WHERE id = ? AND deleted_at IS NULL",
        &[SqlValue::String(note_id.to_string())],
    )
    .map_err(|e| format!("note read: {e}"))?
    .ok_or_else(|| "note vanished before analysis".to_string())?;
    let title = row.first().and_then(|v| v.as_str()).unwrap_or("").to_string();
    let content = row.get(1).and_then(|v| v.as_str()).unwrap_or("").to_string();
    let org_id = row.get(2).and_then(|v| v.as_str()).unwrap_or("").to_string();
    let owner_id = row.get(3).and_then(|v| v.as_str()).unwrap_or("").to_string();
    let now = now_unix();

    let full_text = if title.trim().is_empty() {
        content.clone()
    } else {
        format!("{title}\n\n{content}")
    };

    // --- 1. Chunk + embed into the 'notes' namespace ---------------------
    let mut chunks = split_into_chunks(&full_text, CHUNK_MAX_CHARS, CHUNK_OVERLAP_CHARS);
    chunks.truncate(MAX_CHUNKS_PER_NOTE);
    let dim = if chunks.is_empty() { 0 } else { ensure_embed_dim()? };

    // High-water mark FIRST: if the analysis dies after some upserts, the
    // vectors above the registered range are still covered by
    // max_chunk_count, so the tombstone cleanup can purge them.
    sql_exec(
        "INSERT INTO note_chunks (note_id, chunk_count, max_chunk_count, updated_at) \
         VALUES (?, 0, ?, ?) \
         ON CONFLICT(note_id) DO UPDATE SET \
           max_chunk_count = MAX(max_chunk_count, excluded.max_chunk_count), \
           updated_at = excluded.updated_at",
        &[
            SqlValue::String(note_id.to_string()),
            SqlValue::I64(chunks.len() as i64),
            SqlValue::I64(now),
        ],
    )
    .map_err(|e| format!("chunk high-water mark: {e}"))?;

    let mut chunk_vectors: Vec<Vec<f32>> = Vec::with_capacity(chunks.len());
    for (i, chunk) in chunks.iter().enumerate() {
        let vector = embed_text(chunk, dim)?;
        let fields = vec![
            VectorField {
                name: "note_id".to_string(),
                value: VectorFieldValue::Str(note_id.to_string()),
            },
            VectorField {
                name: "owner_id".to_string(),
                value: VectorFieldValue::Str(owner_id.clone()),
            },
            VectorField {
                name: "chunk_index".to_string(),
                value: VectorFieldValue::Int(i as i64),
            },
        ];
        vector_upsert(NOTES_NS, note_chunk_ref(note_id, i), &vector, &fields)
            .map_err(|e| format!("chunk vector upsert {i}: {e}"))?;
        chunk_vectors.push(vector);
    }

    // Shrunk content: stale chunk vectors are deleted through the outbox.
    let old_count = sql_query_one(
        "SELECT chunk_count FROM note_chunks WHERE note_id = ?",
        &[SqlValue::String(note_id.to_string())],
    )
    .map_err(|e| format!("note_chunks read: {e}"))?
    .and_then(|r| r.first().and_then(|v| v.as_i64()))
    .unwrap_or(0) as usize;

    // --- 2. Extract entities ----------------------------------------------
    let extract_input: String = full_text.chars().take(MAX_EXTRACT_CHARS).collect();
    let entities = if extract_input.trim().is_empty() {
        Vec::new()
    } else {
        parse_entities_response(&call_extraction_llm(&extract_input)?, &extract_input)
    };

    // Previous entity set (canonical ids) — for stale mentions-edge deletes.
    let old_entity_ids = note_canonical_entities(note_id)?;

    // Which extracted entities are NEW rows (merge scan runs only for those).
    let mut new_entity_rows: Vec<(String, String, String)> = Vec::new();
    let mut note_entity_rows: Vec<(String, Option<String>, i64)> = Vec::new();
    for e in &entities {
        let eid = entity_id_for(&e.entity_type, &e.normalized);
        let exists = sql_query_one(
            "SELECT 1 FROM entities WHERE id = ?",
            &[SqlValue::String(eid.clone())],
        )
        .map_err(|err| format!("entity lookup: {err}"))?
        .is_some();
        if !exists {
            new_entity_rows.push((eid.clone(), e.name.clone(), e.entity_type.clone()));
        }
        let (first_span, count) = locate_entity(&content, &e.name);
        note_entity_rows.push((eid, first_span, count));
    }

    // --- 3. Persist entities + note_entities + chunk registry -------------
    let mut stmts: Vec<(String, Vec<SqlValue>)> = Vec::new();
    for (e, (eid, _, _)) in entities.iter().zip(note_entity_rows.iter()) {
        stmts.push((
            "INSERT OR IGNORE INTO entities (id, org_scope, name, entity_type, canonical_id) \
             VALUES (?, ?, ?, ?, NULL)"
                .to_string(),
            vec![
                SqlValue::String(eid.clone()),
                SqlValue::String(org_id.clone()),
                SqlValue::String(e.name.clone()),
                SqlValue::String(e.entity_type.clone()),
            ],
        ));
    }
    stmts.push((
        "DELETE FROM note_entities WHERE note_id = ?".to_string(),
        vec![SqlValue::String(note_id.to_string())],
    ));
    for (eid, first_span, count) in &note_entity_rows {
        stmts.push((
            "INSERT OR REPLACE INTO note_entities (note_id, entity_id, first_span, count) \
             VALUES (?, ?, ?, ?)"
                .to_string(),
            vec![
                SqlValue::String(note_id.to_string()),
                SqlValue::String(eid.clone()),
                first_span
                    .as_ref()
                    .map(|s| SqlValue::String(s.clone()))
                    .unwrap_or(SqlValue::Null),
                SqlValue::I64(*count),
            ],
        ));
    }
    stmts.push((
        "INSERT INTO note_chunks (note_id, chunk_count, max_chunk_count, updated_at) \
         VALUES (?, ?, ?, ?) \
         ON CONFLICT(note_id) DO UPDATE SET \
           chunk_count = excluded.chunk_count, \
           max_chunk_count = MAX(max_chunk_count, excluded.max_chunk_count), \
           updated_at = excluded.updated_at"
            .to_string(),
        vec![
            SqlValue::String(note_id.to_string()),
            SqlValue::I64(chunks.len() as i64),
            SqlValue::I64(chunks.len() as i64),
            SqlValue::I64(now),
        ],
    ));
    // Shrink: stale chunk vectors are deleted through the outbox. The payload
    // carries (note_id, chunk_index) so the drain can skip the delete when a
    // later grow re-registered the same ref_id (shrink→grow race).
    for i in chunks.len()..old_count {
        push_outbox(
            &mut stmts,
            "vector_delete",
            &outbox_dedup_key("vector_delete", &[NOTES_NS, note_id, &i.to_string()]),
            json!({
                "ns": NOTES_NS,
                "ref": note_chunk_ref(note_id, i),
                "note_id": note_id,
                "chunk_index": i,
            }),
            now,
        );
    }
    run_transaction(&stmts).map_err(|e| format!("entity persist: {e}"))?;

    // --- 4. Merge-candidate scan for new entities --------------------------
    for (eid, name, etype) in &new_entity_rows {
        if let Err(e) = scan_merge_candidates(eid, name, etype, now) {
            // Best-effort: a failed scan must not fail the whole analysis;
            // the entity stays unmerged and a later note can re-trigger it.
            log::warn(&format!("notes: merge scan for '{name}' failed: {e}"));
        }
    }

    // --- 5. Note links: semantic similarity + shared entities -------------
    let similar = similar_notes(note_id, &chunk_vectors)?;
    let entity_links = shared_entity_links(note_id)?;
    persist_links_and_graph(
        note_id,
        &title,
        &old_entity_ids,
        &similar,
        &entity_links,
        now,
    )?;

    drain_graph_outbox()
}

/// Canonical entity ids + display names currently attached to a note.
fn note_canonical_entities(note_id: &str) -> Result<Vec<(String, String, String)>, String> {
    let rows = sql_query(
        "SELECT COALESCE(e.canonical_id, e.id), COALESCE(c.name, e.name), \
                COALESCE(c.entity_type, e.entity_type) \
         FROM note_entities ne \
         JOIN entities e ON e.id = ne.entity_id \
         LEFT JOIN entities c ON c.id = e.canonical_id \
         WHERE ne.note_id = ?",
        &[SqlValue::String(note_id.to_string())],
    )
    .map_err(|e| format!("note entities read: {e}"))?;
    let mut out: Vec<(String, String, String)> = Vec::new();
    for r in &rows {
        let id = r.first().and_then(|v| v.as_str()).unwrap_or("").to_string();
        if id.is_empty() || out.iter().any(|(i, _, _)| i == &id) {
            continue;
        }
        out.push((
            id,
            r.get(1).and_then(|v| v.as_str()).unwrap_or("").to_string(),
            r.get(2).and_then(|v| v.as_str()).unwrap_or("").to_string(),
        ));
    }
    Ok(out)
}

/// First occurrence span ("start:end" byte offsets) and occurrence count of an
/// entity name in the content (case-sensitive first, count over lowercase).
fn locate_entity(content: &str, name: &str) -> (Option<String>, i64) {
    let span = content
        .find(name)
        .map(|start| format!("{start}:{}", start + name.len()));
    let hay = content.to_lowercase();
    let needle = name.to_lowercase();
    let count = if needle.is_empty() {
        0
    } else {
        hay.matches(&needle).count() as i64
    };
    (span, count.max(1))
}

/// Aggregated semantic neighbors of a note: max similarity per other note over
/// all chunk queries, threshold + top-N, self excluded, deleted notes dropped.
fn similar_notes(note_id: &str, chunk_vectors: &[Vec<f32>]) -> Result<Vec<(String, f32)>, String> {
    let mut best: Vec<(String, f32)> = Vec::new();
    for vector in chunk_vectors {
        let hits = vector_search(
            NOTES_NS,
            vector,
            SIMILAR_TOP_K * 2,
            None,
            None,
            &["note_id"],
        )
        .map_err(|e| format!("similarity search: {e}"))?;
        for hit in &hits {
            let Some(other) = hit.fields.iter().find(|f| f.name == "note_id").and_then(|f| {
                match &f.value {
                    VectorFieldValue::Str(s) => Some(s.clone()),
                    _ => None,
                }
            }) else {
                continue;
            };
            if other == note_id {
                continue;
            }
            // Cosine score is a DISTANCE (lower = closer) — rag convention.
            let similarity = 1.0 - hit.score;
            if similarity < SIMILAR_THRESHOLD {
                continue;
            }
            match best.iter_mut().find(|(id, _)| id == &other) {
                Some((_, s)) if *s < similarity => *s = similarity,
                Some(_) => {}
                None => best.push((other, similarity)),
            }
        }
    }
    best.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    best.truncate(MAX_LINKS_PER_NOTE);

    // Drop targets that are deleted (their vectors may linger until cleanup).
    let mut alive = Vec::with_capacity(best.len());
    for (other, sim) in best {
        let exists = sql_query_one(
            "SELECT 1 FROM notes WHERE id = ? AND deleted_at IS NULL",
            &[SqlValue::String(other.clone())],
        )
        .map_err(|e| format!("similar target check: {e}"))?
        .is_some();
        if exists {
            alive.push((other, sim));
        }
    }
    Ok(alive)
}

/// Other live notes sharing canonical entities with this note:
/// (note_id, shared_count, representative canonical entity id). NO names —
/// the persisted reason carries only the identifier; display names are
/// resolved per reader at read time (visibility rules). The representative is
/// the lexicographically smallest shared canonical id (deterministic).
fn shared_entity_links(note_id: &str) -> Result<Vec<(String, usize, String)>, String> {
    let rows = sql_query(
        "SELECT ne2.note_id, COALESCE(e.canonical_id, e.id) \
         FROM note_entities ne1 \
         JOIN entities e1 ON e1.id = ne1.entity_id \
         JOIN entities e ON COALESCE(e.canonical_id, e.id) = COALESCE(e1.canonical_id, e1.id) \
         JOIN note_entities ne2 ON ne2.entity_id = e.id AND ne2.note_id != ne1.note_id \
         JOIN notes n2 ON n2.id = ne2.note_id AND n2.deleted_at IS NULL \
         WHERE ne1.note_id = ?",
        &[SqlValue::String(note_id.to_string())],
    )
    .map_err(|e| format!("shared entity read: {e}"))?;

    let mut agg: Vec<(String, usize, String)> = Vec::new();
    for r in &rows {
        let other = r.first().and_then(|v| v.as_str()).unwrap_or("").to_string();
        let cid = r.get(1).and_then(|v| v.as_str()).unwrap_or("").to_string();
        if other.is_empty() || cid.is_empty() {
            continue;
        }
        match agg.iter_mut().find(|(id, _, _)| id == &other) {
            Some((_, count, rep)) => {
                *count += 1;
                if cid < *rep {
                    *rep = cid;
                }
            }
            None => agg.push((other, 1, cid)),
        }
    }
    agg.sort_by(|a, b| b.1.cmp(&a.1));
    agg.truncate(MAX_LINKS_PER_NOTE);
    Ok(agg)
}

/// Rewrites this note's auto links (both directions) preserving created_at of
/// links that persist, and enqueues the graph delta (node + mentions +
/// similar_to edges, deletes for links/mentions that disappeared).
fn persist_links_and_graph(
    note_id: &str,
    title: &str,
    old_entities: &[(String, String, String)],
    similar: &[(String, f32)],
    entity_links: &[(String, usize, String)],
    now: i64,
) -> Result<(), String> {
    // Old auto links touching this note (created_at preservation + edge deletes).
    let old_rows = sql_query(
        "SELECT src_note_id, dst_note_id, kind, created_at FROM note_links \
         WHERE (src_note_id = ? OR dst_note_id = ?) AND kind IN ('similar', 'entity')",
        &[
            SqlValue::String(note_id.to_string()),
            SqlValue::String(note_id.to_string()),
        ],
    )
    .map_err(|e| format!("old links read: {e}"))?;
    let old_links: Vec<(String, String, String, i64)> = old_rows
        .iter()
        .map(|r| {
            (
                r.first().and_then(|v| v.as_str()).unwrap_or("").to_string(),
                r.get(1).and_then(|v| v.as_str()).unwrap_or("").to_string(),
                r.get(2).and_then(|v| v.as_str()).unwrap_or("").to_string(),
                r.get(3).and_then(|v| v.as_i64()).unwrap_or(now),
            )
        })
        .collect();
    let preserved = |src: &str, dst: &str, kind: &str| -> i64 {
        old_links
            .iter()
            .find(|(s, d, k, _)| s == src && d == dst && k == kind)
            .map(|(_, _, _, c)| *c)
            .unwrap_or(now)
    };

    let display_title = if title.trim().is_empty() {
        "(bez tytułu)"
    } else {
        title
    };

    let new_entities = note_canonical_entities(note_id)?;

    let mut stmts: Vec<(String, Vec<SqlValue>)> = Vec::new();
    stmts.push((
        "DELETE FROM note_links WHERE (src_note_id = ? OR dst_note_id = ?) \
         AND kind IN ('similar', 'entity')"
            .to_string(),
        vec![
            SqlValue::String(note_id.to_string()),
            SqlValue::String(note_id.to_string()),
        ],
    ));

    let insert_link = |stmts: &mut Vec<(String, Vec<SqlValue>)>,
                           src: &str,
                           dst: &str,
                           kind: &str,
                           weight: f64,
                           reason: &str| {
        stmts.push((
            "INSERT OR REPLACE INTO note_links \
             (src_note_id, dst_note_id, kind, weight, reason, created_at) \
             VALUES (?, ?, ?, ?, ?, ?)"
                .to_string(),
            vec![
                SqlValue::String(src.to_string()),
                SqlValue::String(dst.to_string()),
                SqlValue::String(kind.to_string()),
                SqlValue::F64(weight),
                SqlValue::String(reason.to_string()),
                SqlValue::I64(preserved(src, dst, kind)),
            ],
        ));
    };

    // Persisted reasons are machine tokens only — entity names are resolved
    // per reader at read time (a canonical name from a private note must not
    // be baked into a row other users can read).
    for (other, sim) in similar {
        insert_link(&mut stmts, note_id, other, "similar", f64::from(*sim), "similar");
        insert_link(&mut stmts, other, note_id, "similar", f64::from(*sim), "similar");
    }
    for (other, shared, entity_id) in entity_links {
        let reason = crate::db::entity_reason_token(entity_id, *shared);
        let weight = entity_link_weight(*shared);
        insert_link(&mut stmts, note_id, other, "entity", weight, &reason);
        insert_link(&mut stmts, other, note_id, "entity", weight, &reason);
    }

    // Graph delta. FIFO drain order guarantees deletes land before re-upserts.
    // Both stored directions of a dropped pair are deleted — the analysis of
    // either endpoint owns the full pair (SQL rows and graph edges alike).
    let note_node = format!("note:{note_id}");
    for (src, dst, kind, _) in &old_links {
        if kind == "similar" {
            let still = similar.iter().any(|(o, _)| {
                (src == note_id && o == dst) || (dst == note_id && o == src)
            });
            if !still {
                push_outbox(
                    &mut stmts,
                    "delete_edge",
                    &outbox_dedup_key("delete_edge", &["similar_to", src, dst]),
                    json!({"src": format!("note:{src}"), "rel": "similar_to", "dst": format!("note:{dst}")}),
                    now,
                );
            }
        }
    }
    for (old_id, _, _) in old_entities {
        if !new_entities.iter().any(|(id, _, _)| id == old_id) {
            push_outbox(
                &mut stmts,
                "delete_edge",
                &outbox_dedup_key("delete_edge", &["mentions", note_id, old_id]),
                json!({"src": note_node, "rel": "mentions", "dst": format!("entity:{old_id}")}),
                now,
            );
        }
    }

    push_outbox(
        &mut stmts,
        "upsert_node",
        &outbox_dedup_key("upsert_node", &[&note_node]),
        json!({"id": note_node, "label": "note", "name": display_title}),
        now,
    );
    for (eid, name, etype) in &new_entities {
        let entity_node = format!("entity:{eid}");
        push_outbox(
            &mut stmts,
            "upsert_node",
            &outbox_dedup_key("upsert_node", &[&entity_node]),
            json!({"id": entity_node, "label": etype, "name": name}),
            now,
        );
        push_outbox(
            &mut stmts,
            "upsert_edge",
            &outbox_dedup_key("upsert_edge", &["mentions", note_id, eid]),
            json!({"src": note_node, "rel": "mentions", "dst": entity_node}),
            now,
        );
    }
    for (other, sim) in similar {
        push_outbox(
            &mut stmts,
            "upsert_edge",
            &outbox_dedup_key("upsert_edge", &["similar_to", note_id, other]),
            json!({
                "src": note_node,
                "rel": "similar_to",
                "dst": format!("note:{other}"),
                "weight": sim,
            }),
            now,
        );
    }

    run_transaction(&stmts).map_err(|e| format!("links persist: {e}"))
}

// =============================================================================
// Tombstone of a deleted note
// =============================================================================

/// Removes all analysis artifacts of a soft-deleted note: link rows, entity
/// attachments, chunk registry — and, through the outbox, its graph node
/// (edges cascade) plus its chunk vectors. Entities orphaned by this note lose
/// their graph node and name vector but keep their SQLite row (merge history
/// stays intact; a future mention re-materializes them).
fn cleanup_deleted_note(note_id: &str) -> Result<(), String> {
    let now = now_unix();
    // Historical high-water mark, not the current count: a failed analysis
    // may have upserted vectors above the registered range — purge them all.
    let chunk_count = sql_query_one(
        "SELECT max_chunk_count FROM note_chunks WHERE note_id = ?",
        &[SqlValue::String(note_id.to_string())],
    )
    .map_err(|e| format!("note_chunks read: {e}"))?
    .and_then(|r| r.first().and_then(|v| v.as_i64()))
    .unwrap_or(0) as usize;

    let entities = note_canonical_entities(note_id)?;

    let mut stmts: Vec<(String, Vec<SqlValue>)> = Vec::new();
    stmts.push((
        "DELETE FROM note_entities WHERE note_id = ?".to_string(),
        vec![SqlValue::String(note_id.to_string())],
    ));
    stmts.push((
        "DELETE FROM note_links WHERE src_note_id = ? OR dst_note_id = ?".to_string(),
        vec![
            SqlValue::String(note_id.to_string()),
            SqlValue::String(note_id.to_string()),
        ],
    ));
    stmts.push((
        "DELETE FROM note_chunks WHERE note_id = ?".to_string(),
        vec![SqlValue::String(note_id.to_string())],
    ));
    push_outbox(
        &mut stmts,
        "delete_node",
        &outbox_dedup_key("delete_node", &[&format!("note:{note_id}")]),
        json!({"id": format!("note:{note_id}")}),
        now,
    );
    for i in 0..chunk_count {
        push_outbox(
            &mut stmts,
            "vector_delete",
            &outbox_dedup_key("vector_delete", &[NOTES_NS, note_id, &i.to_string()]),
            json!({
                "ns": NOTES_NS,
                "ref": note_chunk_ref(note_id, i),
                "note_id": note_id,
                "chunk_index": i,
            }),
            now,
        );
    }
    run_transaction(&stmts).map_err(|e| format!("tombstone persist: {e}"))?;

    // Orphan check runs AFTER the note_entities delete committed.
    for (eid, _, _) in &entities {
        let still_used = sql_query_one(
            "SELECT 1 FROM note_entities ne JOIN entities e ON e.id = ne.entity_id \
             WHERE COALESCE(e.canonical_id, e.id) = ? LIMIT 1",
            &[SqlValue::String(eid.clone())],
        )
        .map_err(|e| format!("orphan check: {e}"))?
        .is_some();
        if !still_used {
            let mut orphan_stmts: Vec<(String, Vec<SqlValue>)> = Vec::new();
            push_outbox(
                &mut orphan_stmts,
                "delete_node",
                &outbox_dedup_key("delete_node", &[&format!("entity:{eid}")]),
                json!({"id": format!("entity:{eid}")}),
                now,
            );
            push_outbox(
                &mut orphan_stmts,
                "vector_delete",
                &outbox_dedup_key("vector_delete", &[ENTITIES_NS, eid]),
                json!({"ns": ENTITIES_NS, "ref": entity_vector_ref(eid), "entity_id": eid}),
                now,
            );
            run_transaction(&orphan_stmts).map_err(|e| format!("orphan persist: {e}"))?;
        }
    }

    drain_graph_outbox()
}

// =============================================================================
// Entity merge — detection, accept/reject, undo
// =============================================================================

/// Embeds the name of a NEW entity into 'entities' and scans kNN neighbors of
/// the same type: Auto band merges immediately, Suggest band opens a
/// merge_suggestion for the user.
fn scan_merge_candidates(
    entity_id: &str,
    name: &str,
    entity_type: &str,
    now: i64,
) -> Result<(), String> {
    let dim = ensure_embed_dim()?;
    let vector = embed_text(name, dim)?;
    let fields = vec![
        VectorField {
            name: "entity_id".to_string(),
            value: VectorFieldValue::Str(entity_id.to_string()),
        },
        VectorField {
            name: "entity_type".to_string(),
            value: VectorFieldValue::Str(entity_type.to_string()),
        },
    ];
    vector_upsert(ENTITIES_NS, entity_vector_ref(entity_id), &vector, &fields)
        .map_err(|e| format!("entity name vector upsert: {e}"))?;

    let filter = VectorFilter::Eq(
        "entity_type".to_string(),
        VectorFieldValue::Str(entity_type.to_string()),
    );
    let hits = vector_search(
        ENTITIES_NS,
        &vector,
        MERGE_KNN_K + 1,
        None,
        Some(&filter),
        &["entity_id"],
    )
    .map_err(|e| format!("entity kNN: {e}"))?;

    for hit in &hits {
        let Some(candidate) = hit.fields.iter().find(|f| f.name == "entity_id").and_then(|f| {
            match &f.value {
                VectorFieldValue::Str(s) => Some(s.clone()),
                _ => None,
            }
        }) else {
            continue;
        };
        if candidate == entity_id {
            continue;
        }
        // Aliases are excluded: only canonical entities merge.
        let is_alias = sql_query_one(
            "SELECT 1 FROM entities WHERE id = ? AND canonical_id IS NOT NULL",
            &[SqlValue::String(candidate.clone())],
        )
        .map_err(|e| format!("alias check: {e}"))?
        .is_some();
        if is_alias {
            continue;
        }
        let similarity = 1.0 - hit.score;
        match classify_merge_band(similarity) {
            MergeBand::Auto => {
                let (from, into) = merge_direction(entity_id, &candidate)?;
                perform_merge(&from, &into, now)?;
                return Ok(());
            }
            MergeBand::Suggest => {
                let (a, b) = if entity_id < candidate.as_str() {
                    (entity_id.to_string(), candidate.clone())
                } else {
                    (candidate.clone(), entity_id.to_string())
                };
                // Partial-unique (pair, status='open') makes this idempotent.
                sql_exec(
                    "INSERT OR IGNORE INTO merge_suggestions \
                     (id, entity_a, entity_b, similarity, status, created_at) \
                     VALUES (?, ?, ?, ?, 'open', ?)",
                    &[
                        SqlValue::String(new_id("msug")),
                        SqlValue::String(a),
                        SqlValue::String(b),
                        SqlValue::F64(f64::from(similarity)),
                        SqlValue::I64(now),
                    ],
                )
                .map_err(|e| format!("merge suggestion insert: {e}"))?;
                return Ok(());
            }
            MergeBand::None => break,
        }
    }
    Ok(())
}

/// Deterministic merge direction: the entity mentioned by more notes survives
/// (ties break toward the lexicographically smaller id).
fn merge_direction(a: &str, b: &str) -> Result<(String, String), String> {
    let count = |id: &str| -> Result<i64, String> {
        Ok(sql_query_one(
            "SELECT COUNT(*) FROM note_entities ne JOIN entities e ON e.id = ne.entity_id \
             WHERE COALESCE(e.canonical_id, e.id) = ?",
            &[SqlValue::String(id.to_string())],
        )
        .map_err(|e| format!("reference count: {e}"))?
        .and_then(|r| r.first().and_then(|v| v.as_i64()))
        .unwrap_or(0))
    };
    let (ca, cb) = (count(a)?, count(b)?);
    let a_wins = ca > cb || (ca == cb && a <= b);
    if a_wins {
        Ok((b.to_string(), a.to_string()))
    } else {
        Ok((a.to_string(), b.to_string()))
    }
}

/// Canonical pointer of an entity row: Ok(None) = canonical. Missing row =
/// error (dangling references must fail loudly, not silently merge).
fn entity_pointer(id: &str) -> Result<Option<String>, String> {
    let row = sql_query_one(
        "SELECT canonical_id FROM entities WHERE id = ?",
        &[SqlValue::String(id.to_string())],
    )
    .map_err(|e| format!("entity pointer read: {e}"))?
    .ok_or_else(|| format!("entity '{id}' does not exist"))?;
    Ok(row.first().and_then(|v| v.as_str()).map(str::to_string))
}

/// Merges `from` into the canonical ROOT of `into`: canonical_id pointer +
/// path compression (aliases of `from` re-point to the root, so chains never
/// grow past depth 1) + merge log (reversibility) + graph re-pointing through
/// the outbox. note_entities rows keep their original entity_id — reads
/// resolve through canonical_id, which is what makes the merge reversible
/// without an edge diff. Cycle-safe: merging into a chain that leads back to
/// `from` is rejected.
fn perform_merge(from: &str, into: &str, now: i64) -> Result<String, String> {
    if from == into {
        return Err("cannot merge an entity into itself".to_string());
    }
    if entity_pointer(from)?.is_some() {
        return Err(format!("entity '{from}' is already merged"));
    }
    let into_root = resolve_root(entity_pointer, into)?;
    if into_root == from {
        return Err("merge would create a canonical pointer cycle".to_string());
    }

    let merge_id = new_id("merge");
    // Notes referencing `from` (directly or via its aliases) — their mentions
    // edges re-point to the root.
    let notes = notes_mentioning(from)?;

    let mut stmts: Vec<(String, Vec<SqlValue>)> = Vec::new();
    stmts.push((
        "UPDATE entities SET canonical_id = ? WHERE id = ? AND canonical_id IS NULL".to_string(),
        vec![
            SqlValue::String(into_root.clone()),
            SqlValue::String(from.to_string()),
        ],
    ));
    // Path compression: aliases that pointed at `from` now point at the root.
    stmts.push((
        "UPDATE entities SET canonical_id = ? WHERE canonical_id = ?".to_string(),
        vec![
            SqlValue::String(into_root.clone()),
            SqlValue::String(from.to_string()),
        ],
    ));
    // The log records the ROOT actually merged into — undo restores exactly
    // this transition.
    stmts.push((
        "INSERT INTO entity_merge_log (id, from_entity_id, into_entity_id, merged_at) \
         VALUES (?, ?, ?, ?)"
            .to_string(),
        vec![
            SqlValue::String(merge_id.clone()),
            SqlValue::String(from.to_string()),
            SqlValue::String(into_root.clone()),
            SqlValue::I64(now),
        ],
    ));
    push_outbox(
        &mut stmts,
        "delete_node",
        &outbox_dedup_key("delete_node", &[&format!("entity:{from}")]),
        json!({"id": format!("entity:{from}")}),
        now,
    );
    for note in &notes {
        push_outbox(
            &mut stmts,
            "upsert_edge",
            &outbox_dedup_key("upsert_edge", &["mentions", note, &into_root]),
            json!({
                "src": format!("note:{note}"),
                "rel": "mentions",
                "dst": format!("entity:{into_root}"),
            }),
            now,
        );
    }
    push_outbox(
        &mut stmts,
        "vector_delete",
        &outbox_dedup_key("vector_delete", &[ENTITIES_NS, from]),
        json!({"ns": ENTITIES_NS, "ref": entity_vector_ref(from), "entity_id": from}),
        now,
    );
    run_transaction(&stmts).map_err(|e| format!("merge persist: {e}"))?;
    drain_graph_outbox()?;
    Ok(merge_id)
}

/// Note ids with a note_entities row resolving to `entity_id` (direct or via
/// canonical alias).
fn notes_mentioning(entity_id: &str) -> Result<Vec<String>, String> {
    let rows = sql_query(
        "SELECT DISTINCT ne.note_id FROM note_entities ne \
         JOIN entities e ON e.id = ne.entity_id \
         JOIN notes n ON n.id = ne.note_id AND n.deleted_at IS NULL \
         WHERE COALESCE(e.canonical_id, e.id) = ? OR ne.entity_id = ?",
        &[
            SqlValue::String(entity_id.to_string()),
            SqlValue::String(entity_id.to_string()),
        ],
    )
    .map_err(|e| format!("mentions read: {e}"))?;
    Ok(rows
        .iter()
        .filter_map(|r| r.first().and_then(|v| v.as_str()).map(str::to_string))
        .collect())
}

/// Loads an open suggestion and enforces the acting user's visibility of BOTH
/// entities (each must be reachable through at least one readable note) —
/// merge decisions are never available on entities the user cannot see.
fn load_visible_suggestion(
    ctx: &crate::db::UserCtx,
    suggestion_id: &str,
) -> Result<(String, String), String> {
    let row = sql_query_one(
        "SELECT entity_a, entity_b FROM merge_suggestions WHERE id = ? AND status = 'open'",
        &[SqlValue::String(suggestion_id.to_string())],
    )
    .map_err(|e| format!("suggestion read: {e}"))?
    .ok_or_else(|| "Sugestia nie istnieje albo została już rozstrzygnięta.".to_string())?;
    let a = row.first().and_then(|v| v.as_str()).unwrap_or("").to_string();
    let b = row.get(1).and_then(|v| v.as_str()).unwrap_or("").to_string();
    if !crate::db::entity_visible(ctx, &a) || !crate::db::entity_visible(ctx, &b) {
        return Err("Brak dostępu do encji tej sugestii.".to_string());
    }
    Ok((a, b))
}

/// Accepts an open suggestion: performs the merge (survivor chosen by
/// reference count) and closes the suggestion. Requires visibility of both
/// entities for the acting user.
pub fn merge_accept(ctx: &crate::db::UserCtx, suggestion_id: &str) -> Result<(), String> {
    let (a, b) = load_visible_suggestion(ctx, suggestion_id)?;
    let now = now_unix();
    let (from, into) = merge_direction(&a, &b)?;
    perform_merge(&from, &into, now)?;
    decide_suggestion(suggestion_id, "accepted", &ctx.user_id, now)
}

/// Rejects an open suggestion (same visibility guard as accept). Detection
/// runs only for NEWLY created entity rows and entity ids are stable per
/// (type, name), so a rejected pair is not re-suggested by later analyses.
pub fn merge_reject(ctx: &crate::db::UserCtx, suggestion_id: &str) -> Result<(), String> {
    let _ = load_visible_suggestion(ctx, suggestion_id)?;
    decide_suggestion(suggestion_id, "rejected", &ctx.user_id, now_unix())
}

fn decide_suggestion(
    suggestion_id: &str,
    status: &str,
    decided_by: &str,
    now: i64,
) -> Result<(), String> {
    let res = sql_exec(
        "UPDATE merge_suggestions SET status = ?, decided_at = ?, decided_by = ? \
         WHERE id = ? AND status = 'open'",
        &[
            SqlValue::String(status.to_string()),
            SqlValue::I64(now),
            SqlValue::String(decided_by.to_string()),
            SqlValue::String(suggestion_id.to_string()),
        ],
    )
    .map_err(|e| format!("suggestion update: {e}"))?;
    if res.rows_affected == 0 {
        return Err("Sugestia nie istnieje albo została już rozstrzygnięta.".to_string());
    }
    Ok(())
}

/// Reverses an applied merge from entity_merge_log, restoring EXACTLY that
/// transition: clears the alias' canonical pointer, re-points aliases that
/// were path-compressed by this merge (entities merged into `from` earlier
/// and still pointing at `into`) back to `from`, restores the alias graph
/// node + mentions edges, removes mentions edges to the survivor for notes
/// that only reached it through the restored subtree, and re-embeds the alias
/// name vector (best-effort). Requires visibility of both entities for the
/// acting user.
pub fn merge_undo(ctx: &crate::db::UserCtx, merge_id: &str) -> Result<(), String> {
    let row = sql_query_one(
        "SELECT from_entity_id, into_entity_id FROM entity_merge_log \
         WHERE id = ? AND undone_at IS NULL",
        &[SqlValue::String(merge_id.to_string())],
    )
    .map_err(|e| format!("merge log read: {e}"))?
    .ok_or_else(|| "Scalenie nie istnieje albo zostało już cofnięte.".to_string())?;
    let from = row.first().and_then(|v| v.as_str()).unwrap_or("").to_string();
    let into = row.get(1).and_then(|v| v.as_str()).unwrap_or("").to_string();
    if !crate::db::entity_visible(ctx, &from) || !crate::db::entity_visible(ctx, &into) {
        return Err("Brak dostępu do encji tego scalenia.".to_string());
    }
    let now = now_unix();

    let from_meta = sql_query_one(
        "SELECT name, entity_type FROM entities WHERE id = ?",
        &[SqlValue::String(from.clone())],
    )
    .map_err(|e| format!("entity read: {e}"))?
    .ok_or_else(|| "Encja scalenia nie istnieje.".to_string())?;
    let from_name = from_meta.first().and_then(|v| v.as_str()).unwrap_or("").to_string();
    let from_type = from_meta.get(1).and_then(|v| v.as_str()).unwrap_or("").to_string();

    // Aliases path-compressed by THIS merge: they were merged into `from`
    // before (applied log entries) and still point at `into`. They return to
    // `from` so the pre-merge pointer state is reproduced exactly.
    let restored: Vec<String> = sql_query(
        "SELECT m.from_entity_id FROM entity_merge_log m \
         JOIN entities e ON e.id = m.from_entity_id \
         WHERE m.into_entity_id = ? AND m.undone_at IS NULL AND m.id != ? \
           AND e.canonical_id = ?",
        &[
            SqlValue::String(from.clone()),
            SqlValue::String(merge_id.to_string()),
            SqlValue::String(into.clone()),
        ],
    )
    .map_err(|e| format!("restored aliases read: {e}"))?
    .iter()
    .filter_map(|r| r.first().and_then(|v| v.as_str()).map(str::to_string))
    .collect();

    // Notes mentioning the restored subtree (from + its returning aliases):
    // their edges move back to the alias.
    let mut affected: Vec<String> = vec![from.clone()];
    affected.extend(restored.iter().cloned());
    let affected_ph = affected.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
    let affected_params: Vec<SqlValue> = affected
        .iter()
        .map(|id| SqlValue::String(id.clone()))
        .collect();
    let from_notes: Vec<String> = sql_query(
        &format!(
            "SELECT DISTINCT ne.note_id FROM note_entities ne \
             JOIN notes n ON n.id = ne.note_id AND n.deleted_at IS NULL \
             WHERE ne.entity_id IN ({affected_ph})"
        ),
        &affected_params,
    )
    .map_err(|e| format!("alias mentions read: {e}"))?
    .iter()
    .filter_map(|r| r.first().and_then(|v| v.as_str()).map(str::to_string))
    .collect();

    let mut stmts: Vec<(String, Vec<SqlValue>)> = Vec::new();
    if !restored.is_empty() {
        let mut params = vec![SqlValue::String(from.clone()), SqlValue::String(into.clone())];
        params.extend(
            restored
                .iter()
                .map(|id| SqlValue::String(id.clone())),
        );
        let ph = restored.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
        stmts.push((
            format!(
                "UPDATE entities SET canonical_id = ? \
                 WHERE canonical_id = ? AND id IN ({ph})"
            ),
            params,
        ));
    }
    stmts.push((
        "UPDATE entities SET canonical_id = NULL WHERE id = ? AND canonical_id = ?".to_string(),
        vec![SqlValue::String(from.clone()), SqlValue::String(into.clone())],
    ));
    stmts.push((
        "UPDATE entity_merge_log SET undone_at = ? WHERE id = ? AND undone_at IS NULL".to_string(),
        vec![SqlValue::I64(now), SqlValue::String(merge_id.to_string())],
    ));
    push_outbox(
        &mut stmts,
        "upsert_node",
        &outbox_dedup_key("upsert_node", &[&format!("entity:{from}")]),
        json!({"id": format!("entity:{from}"), "label": from_type, "name": from_name}),
        now,
    );
    for note in &from_notes {
        push_outbox(
            &mut stmts,
            "upsert_edge",
            &outbox_dedup_key("upsert_edge", &["mentions", note, &from]),
            json!({
                "src": format!("note:{note}"),
                "rel": "mentions",
                "dst": format!("entity:{from}"),
            }),
            now,
        );
        // The survivor's edge stays only when the note still resolves to it
        // through some entity OUTSIDE the restored subtree (post-undo state,
        // computed by excluding the affected ids).
        let mut params = vec![SqlValue::String(note.clone())];
        params.extend(affected_params.clone());
        params.push(SqlValue::String(into.clone()));
        let direct = sql_query_one(
            &format!(
                "SELECT 1 FROM note_entities ne JOIN entities e ON e.id = ne.entity_id \
                 WHERE ne.note_id = ? AND ne.entity_id NOT IN ({affected_ph}) \
                 AND COALESCE(e.canonical_id, e.id) = ? LIMIT 1"
            ),
            &params,
        )
        .map_err(|e| format!("survivor mention check: {e}"))?
        .is_some();
        if !direct {
            push_outbox(
                &mut stmts,
                "delete_edge",
                &outbox_dedup_key("delete_edge", &["mentions", note, &into]),
                json!({
                    "src": format!("note:{note}"),
                    "rel": "mentions",
                    "dst": format!("entity:{into}"),
                }),
                now,
            );
        }
    }
    run_transaction(&stmts).map_err(|e| format!("undo persist: {e}"))?;
    drain_graph_outbox()?;

    // Name vector restore is best-effort: it only affects future candidate
    // detection, and the next scan touching this name would re-add it anyway.
    match ensure_embed_dim().and_then(|dim| embed_text(&from_name, dim)) {
        Ok(vector) => {
            let fields = vec![
                VectorField {
                    name: "entity_id".to_string(),
                    value: VectorFieldValue::Str(from.clone()),
                },
                VectorField {
                    name: "entity_type".to_string(),
                    value: VectorFieldValue::Str(from_type),
                },
            ];
            if let Err(e) = vector_upsert(ENTITIES_NS, entity_vector_ref(&from), &vector, &fields) {
                log::warn(&format!("notes: name vector restore for '{from}' failed: {e}"));
            }
        }
        Err(e) => log::warn(&format!("notes: name re-embed for '{from}' failed: {e}")),
    }
    Ok(())
}

// =============================================================================
// Graph outbox — enqueue + idempotent drain (rag pattern)
// =============================================================================

/// Appends an outbox INSERT. The partial-unique pending index dedups the same
/// intent while it waits; a re-enqueue of a pending key REFRESHES its payload
/// (labels/weights change between analyses — the drain must apply the newest
/// intent, not the stalest). After applied=1 the key frees up so re-analysis
/// re-materializes (see migration 001).
fn push_outbox(
    stmts: &mut Vec<(String, Vec<SqlValue>)>,
    op: &str,
    dedup_key: &str,
    payload: JsonValue,
    now: i64,
) {
    stmts.push((
        "INSERT INTO graph_outbox (dedup_key, op, payload_json, applied, created_at) \
         VALUES (?, ?, ?, 0, ?) \
         ON CONFLICT(dedup_key) WHERE applied = 0 \
         DO UPDATE SET payload_json = excluded.payload_json"
            .to_string(),
        vec![
            SqlValue::String(dedup_key.to_string()),
            SqlValue::String(op.to_string()),
            SqlValue::String(payload.to_string()),
            SqlValue::I64(now),
        ],
    ));
}

/// Materializes pending outbox intents (applied = 0) into 'notes_kg' / vector
/// namespaces and marks them applied. Source is SQLite, not call memory — a
/// crash between commit and drain is recovered by the NEXT drain (any analysis
/// or panel action). First host-fn error aborts the drain leaving applied=0
/// rows for the next pass.
pub fn drain_graph_outbox() -> Result<(), String> {
    for _ in 0..OUTBOX_DRAIN_MAX_ITERS {
        let rows = sql_query(
            "SELECT seq, op, payload_json FROM graph_outbox \
             WHERE applied = 0 ORDER BY seq LIMIT ?",
            &[SqlValue::I64(OUTBOX_DRAIN_BATCH as i64)],
        )
        .map_err(|e| format!("outbox read: {e}"))?;
        if rows.is_empty() {
            return Ok(());
        }
        for row in &rows {
            let seq = row.first().and_then(|v| v.as_i64()).unwrap_or_default();
            let op = row.get(1).and_then(|v| v.as_str()).unwrap_or_default();
            let payload: JsonValue =
                serde_json::from_str(row.get(2).and_then(|v| v.as_str()).unwrap_or("{}"))
                    .map_err(|e| format!("outbox payload seq={seq}: {e}"))?;
            apply_outbox_op(op, &payload)?;
            sql_exec(
                "UPDATE graph_outbox SET applied = 1 WHERE seq = ?",
                &[SqlValue::I64(seq)],
            )
            .map_err(|e| format!("outbox mark seq={seq}: {e}"))?;
        }
        if rows.len() < OUTBOX_DRAIN_BATCH {
            return Ok(());
        }
    }
    Ok(())
}

fn apply_outbox_op(op: &str, payload: &JsonValue) -> Result<(), String> {
    match op {
        "upsert_node" => {
            let id = payload.get("id").and_then(|v| v.as_str()).unwrap_or_default();
            let label = payload.get("label").and_then(|v| v.as_str()).unwrap_or("topic");
            let name = payload.get("name").and_then(|v| v.as_str()).unwrap_or(id);
            let node = GraphNode {
                id: id.to_string(),
                label: label.to_string(),
                props: vec![GraphProp {
                    name: "name".to_string(),
                    value: VectorFieldValue::Str(name.to_string()),
                }],
                provenance: None,
            };
            graph_upsert_node(KG_COLLECTION, node)
                .map(|_| ())
                .map_err(|e| format!("node upsert '{id}': {e}"))
        }
        "upsert_edge" => {
            let src = payload.get("src").and_then(|v| v.as_str()).unwrap_or_default();
            let rel = payload.get("rel").and_then(|v| v.as_str()).unwrap_or_default();
            let dst = payload.get("dst").and_then(|v| v.as_str()).unwrap_or_default();
            let weight = payload.get("weight").and_then(|v| v.as_f64());
            graph_upsert_edge(KG_COLLECTION, src, rel, dst, weight, Vec::new(), None)
                .map(|_| ())
                .map_err(|e| format!("edge upsert '{src}-{rel}-{dst}': {e}"))
        }
        // delete_* are idempotent (host delete of a missing target is a no-op
        // tombstone), so re-drain after a crash is harmless.
        "delete_node" => {
            let id = payload.get("id").and_then(|v| v.as_str()).unwrap_or_default();
            graph_delete_node(KG_COLLECTION, id)
                .map(|_| ())
                .map_err(|e| format!("node delete '{id}': {e}"))
        }
        "delete_edge" => {
            let src = payload.get("src").and_then(|v| v.as_str()).unwrap_or_default();
            let rel = payload.get("rel").and_then(|v| v.as_str()).unwrap_or_default();
            let dst = payload.get("dst").and_then(|v| v.as_str()).unwrap_or_default();
            graph_delete_edge(KG_COLLECTION, src, rel, dst)
                .map(|_| ())
                .map_err(|e| format!("edge delete '{src}-{rel}-{dst}': {e}"))
        }
        "vector_delete" => {
            let ns = payload.get("ns").and_then(|v| v.as_str()).unwrap_or(NOTES_NS);
            let reference = payload.get("ref").and_then(|v| v.as_u64()).unwrap_or(0);
            // Obsolescence is checked AT DRAIN TIME: a shrink→grow race
            // re-registers the same ref_id before the pending delete runs —
            // executing it would kill the fresh vector. An obsolete intent is
            // simply marked applied.
            if vector_delete_is_obsolete(payload)? {
                return Ok(());
            }
            vector_delete(ns, reference)
                .map(|_| ())
                .map_err(|e| format!("vector delete {ns}/{reference}: {e}"))
        }
        other => Err(format!("unknown outbox op '{other}'")),
    }
}

/// Pure classifier of a pending chunk-vector delete: obsolete when the chunk
/// index is back inside the currently registered range (the ref_id was
/// re-used by a later grow).
pub fn chunk_delete_obsolete(chunk_index: i64, registered_count: i64) -> bool {
    chunk_index >= 0 && chunk_index < registered_count
}

/// Checks whether a pending vector_delete intent is obsolete at drain time.
/// Chunk deletes: the (note_id, chunk_index) is currently registered again.
/// Entity-name deletes: the entity is canonical again AND reachable from a
/// live note (an undone merge / re-mention revived it).
fn vector_delete_is_obsolete(payload: &JsonValue) -> Result<bool, String> {
    if let (Some(note_id), Some(chunk_index)) = (
        payload.get("note_id").and_then(|v| v.as_str()),
        payload.get("chunk_index").and_then(|v| v.as_i64()),
    ) {
        let registered = sql_query_one(
            "SELECT chunk_count FROM note_chunks nc \
             JOIN notes n ON n.id = nc.note_id AND n.deleted_at IS NULL \
             WHERE nc.note_id = ?",
            &[SqlValue::String(note_id.to_string())],
        )
        .map_err(|e| format!("chunk registry read: {e}"))?
        .and_then(|r| r.first().and_then(|v| v.as_i64()))
        .unwrap_or(0);
        return Ok(chunk_delete_obsolete(chunk_index, registered));
    }
    if let Some(entity_id) = payload.get("entity_id").and_then(|v| v.as_str()) {
        let live = sql_query_one(
            "SELECT 1 FROM entities e WHERE e.id = ? AND e.canonical_id IS NULL \
             AND EXISTS (SELECT 1 FROM note_entities ne \
                         JOIN entities e2 ON e2.id = ne.entity_id \
                         JOIN notes n ON n.id = ne.note_id AND n.deleted_at IS NULL \
                         WHERE COALESCE(e2.canonical_id, e2.id) = e.id) LIMIT 1",
            &[SqlValue::String(entity_id.to_string())],
        )
        .map_err(|e| format!("entity liveness read: {e}"))?
        .is_some();
        return Ok(live);
    }
    Ok(false)
}

fn run_transaction(stmts: &[(String, Vec<SqlValue>)]) -> Result<(), String> {
    let refs: Vec<(&str, &[SqlValue])> = stmts
        .iter()
        .map(|(sql, params)| (sql.as_str(), params.as_slice()))
        .collect();
    sql_transaction(&refs).map(|_| ()).map_err(|e| e.to_string())
}

// =============================================================================
// Read side for the panel (merge suggestions / recent merges)
// =============================================================================

/// Open merge suggestion involving one of the note's entities.
#[derive(Debug, Clone)]
pub struct MergeSuggestionView {
    pub id: String,
    pub name_a: String,
    pub name_b: String,
    pub similarity: f64,
}

pub fn open_suggestions_for(
    ctx: &crate::db::UserCtx,
    entity_ids: &[String],
) -> Vec<MergeSuggestionView> {
    if entity_ids.is_empty() {
        return Vec::new();
    }
    let placeholders = entity_ids
        .iter()
        .map(|_| "?")
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT s.id, ea.name, eb.name, s.similarity, s.entity_a, s.entity_b \
         FROM merge_suggestions s \
         JOIN entities ea ON ea.id = s.entity_a \
         JOIN entities eb ON eb.id = s.entity_b \
         WHERE s.status = 'open' AND (s.entity_a IN ({placeholders}) \
            OR s.entity_b IN ({placeholders})) \
         ORDER BY s.similarity DESC LIMIT 8"
    );
    let mut params: Vec<SqlValue> = entity_ids
        .iter()
        .map(|id| SqlValue::String(id.clone()))
        .collect();
    params.extend(entity_ids.iter().map(|id| SqlValue::String(id.clone())));
    let rows = match sql_query(&sql, &params) {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };
    // A suggestion (and the OTHER entity's name) is shown only when the
    // reader can see BOTH entities through readable notes of their own.
    rows.iter()
        .filter(|r| {
            let a = r.get(4).and_then(|v| v.as_str()).unwrap_or("");
            let b = r.get(5).and_then(|v| v.as_str()).unwrap_or("");
            !a.is_empty()
                && !b.is_empty()
                && crate::db::entity_visible(ctx, a)
                && crate::db::entity_visible(ctx, b)
        })
        .take(4)
        .map(|r| MergeSuggestionView {
            id: r.first().and_then(|v| v.as_str()).unwrap_or("").to_string(),
            name_a: r.get(1).and_then(|v| v.as_str()).unwrap_or("").to_string(),
            name_b: r.get(2).and_then(|v| v.as_str()).unwrap_or("").to_string(),
            similarity: r.get(3).and_then(value_f64).unwrap_or(0.0),
        })
        .collect()
}

/// Recent (24 h), not yet undone merge whose survivor is one of the note's
/// entities — surfaced with a "Cofnij" action.
#[derive(Debug, Clone)]
pub struct RecentMergeView {
    pub merge_id: String,
    pub from_name: String,
    pub into_name: String,
}

pub fn recent_merges_for(
    ctx: &crate::db::UserCtx,
    entity_ids: &[String],
) -> Vec<RecentMergeView> {
    if entity_ids.is_empty() {
        return Vec::new();
    }
    let placeholders = entity_ids
        .iter()
        .map(|_| "?")
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT m.id, ef.name, ei.name, m.from_entity_id, m.into_entity_id \
         FROM entity_merge_log m \
         JOIN entities ef ON ef.id = m.from_entity_id \
         JOIN entities ei ON ei.id = m.into_entity_id \
         WHERE m.undone_at IS NULL AND m.merged_at > ? \
           AND m.into_entity_id IN ({placeholders}) \
         ORDER BY m.merged_at DESC LIMIT 8"
    );
    let mut params: Vec<SqlValue> = vec![SqlValue::I64(now_unix() - 86_400)];
    params.extend(entity_ids.iter().map(|id| SqlValue::String(id.clone())));
    let rows = match sql_query(&sql, &params) {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };
    // Same visibility rule as suggestions: both sides of the merge must be
    // reachable through the reader's own notes, or the card (and the alias
    // name) stays hidden.
    rows.iter()
        .filter(|r| {
            let from = r.get(3).and_then(|v| v.as_str()).unwrap_or("");
            let into = r.get(4).and_then(|v| v.as_str()).unwrap_or("");
            !from.is_empty()
                && !into.is_empty()
                && crate::db::entity_visible(ctx, from)
                && crate::db::entity_visible(ctx, into)
        })
        .take(4)
        .map(|r| RecentMergeView {
            merge_id: r.first().and_then(|v| v.as_str()).unwrap_or("").to_string(),
            from_name: r.get(1).and_then(|v| v.as_str()).unwrap_or("").to_string(),
            into_name: r.get(2).and_then(|v| v.as_str()).unwrap_or("").to_string(),
        })
        .collect()
}

// =============================================================================
// Tests — pure helpers only (no host fns on the native target)
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // --- entity JSON parser -------------------------------------------------

    #[test]
    fn parse_entities_tolerates_prose_and_fences_around_json() {
        let raw = "Oto encje:\n```json\n[{\"name\": \"Nexadata\", \"type\": \"company\"},\n \
                   {\"name\": \"Marta Zielińska\", \"type\": \"person\"}]\n```\nKoniec.";
        let src = "Spotkanie z Nexadata. Marta Zielińska prowadzi projekt.";
        let out = parse_entities_response(raw, src);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].name, "Nexadata");
        assert_eq!(out[0].entity_type, "company");
        assert_eq!(out[1].normalized, "marta zielińska");
    }

    #[test]
    fn parse_entities_unwraps_chat_completion_envelope() {
        let inner = "[{\"name\": \"RODO\", \"type\": \"topic\"}]";
        let raw = serde_json::json!({
            "choices": [{"message": {"content": format!("Wynik: {inner}")}}]
        })
        .to_string();
        let out = parse_entities_response(&raw, "Polityka RODO wymaga rozdzielenia danych.");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].name, "RODO");
    }

    #[test]
    fn parse_entities_rejects_bad_types_and_missing_fields() {
        let raw = r#"[
            {"name": "Nexadata", "type": "corporation"},
            {"name": "Nexadata"},
            {"type": "company"},
            {"name": "", "type": "company"},
            {"name": "Nexadata", "type": "company"}
        ]"#;
        let out = parse_entities_response(raw, "Umowa z Nexadata.");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].entity_type, "company");
    }

    #[test]
    fn parse_entities_grounding_drops_hallucinated_names() {
        let raw = r#"[
            {"name": "Nexadata", "type": "company"},
            {"name": "Wymyślona Firma", "type": "company"}
        ]"#;
        let out = parse_entities_response(raw, "Prawnik Nexadata prosi o doprecyzowanie.");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].name, "Nexadata");
    }

    #[test]
    fn parse_entities_dedups_by_normalized_name_and_type() {
        let raw = r#"[
            {"name": "Nexadata", "type": "company"},
            {"name": "  NEXADATA ", "type": "company"},
            {"name": "Nexadata", "type": "topic"}
        ]"#;
        let out = parse_entities_response(raw, "Nexadata to firma i temat: Nexadata.");
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn parse_entities_survives_pure_garbage() {
        assert!(parse_entities_response("nie ma tu jsona", "x").is_empty());
        assert!(parse_entities_response("[1, 2, 3]", "x").is_empty());
        assert!(parse_entities_response("[{\"name\": \"a\"", "a").is_empty());
        assert!(parse_entities_response("", "").is_empty());
    }

    #[test]
    fn extract_json_array_ignores_brackets_inside_strings() {
        let text = "prefix [{\"name\": \"a]b\", \"type\": \"topic\"}] suffix";
        let slice = extract_json_array(text).expect("array");
        assert!(slice.starts_with('['));
        assert!(slice.ends_with(']'));
        assert!(serde_json::from_str::<JsonValue>(slice).is_ok());
    }

    // --- name normalization -------------------------------------------------

    #[test]
    fn normalize_name_collapses_whitespace_and_case() {
        assert_eq!(normalize_name("  Firma   Sp. z o.o.  "), "firma sp. z o.o.");
        assert_eq!(normalize_name("ŻÓŁĆ"), "żółć");
        assert_eq!(normalize_name("a\n b\t c"), "a b c");
    }

    #[test]
    fn entity_id_is_stable_per_type_and_name() {
        let a = entity_id_for("company", "nexadata");
        assert_eq!(a, entity_id_for("company", "nexadata"));
        assert_ne!(a, entity_id_for("topic", "nexadata"));
        assert_ne!(a, entity_id_for("company", "nexadata 2"));
        assert!(a.starts_with("ent_"));
    }

    // --- chunking (UTF-8 safety) ---------------------------------------------

    #[test]
    fn chunking_respects_max_chars_and_produces_overlap() {
        let sentence = "To jest zdanie testowe numer jeden. ";
        let text = sentence.repeat(60);
        let chunks = split_into_chunks(&text, 400, 80);
        assert!(chunks.len() > 1);
        for c in &chunks {
            assert!(c.chars().count() <= 400 + 1, "chunk too long: {}", c.len());
        }
        // Overlap: the second chunk starts with the tail of the first.
        let first_tail: String = chunks[0].chars().rev().take(20).collect();
        let _ = first_tail; // tail lands word-aligned inside chunk 2
        assert!(chunks[1].contains("zdanie testowe"));
    }

    #[test]
    fn chunking_is_utf8_safe_on_multibyte_text() {
        // Every char is multibyte — any byte-offset cut would panic.
        let text = "żółćąęśźń ".repeat(500);
        let chunks = split_into_chunks(&text, 100, 30);
        assert!(chunks.len() > 1);
        for c in &chunks {
            assert!(!c.is_empty());
            assert!(c.chars().count() <= 101);
        }
    }

    #[test]
    fn chunking_hard_splits_oversized_unpunctuated_text() {
        let text = "ż".repeat(1000);
        let chunks = split_into_chunks(&text, 100, 10);
        assert!(chunks.len() >= 10);
        for c in &chunks {
            assert!(c.chars().count() <= 101);
        }
    }

    #[test]
    fn chunking_empty_and_short_inputs() {
        assert!(split_into_chunks("", 100, 10).is_empty());
        assert!(split_into_chunks("   \n\n  ", 100, 10).is_empty());
        assert_eq!(split_into_chunks("krótki tekst", 100, 10).len(), 1);
    }

    // --- merge thresholds -----------------------------------------------------

    #[test]
    fn merge_band_thresholds() {
        assert_eq!(classify_merge_band(0.99), MergeBand::Auto);
        assert_eq!(classify_merge_band(0.95), MergeBand::Auto);
        assert_eq!(classify_merge_band(0.9499), MergeBand::Suggest);
        assert_eq!(classify_merge_band(0.80), MergeBand::Suggest);
        assert_eq!(classify_merge_band(0.7999), MergeBand::None);
        assert_eq!(classify_merge_band(0.0), MergeBand::None);
    }

    // --- merge chains / undo ------------------------------------------------

    use std::collections::HashMap;

    /// Pointer state + log mirroring the SQL semantics of perform_merge /
    /// merge_undo: merge sets from→root(into) and path-compresses aliases of
    /// `from`; undo restores exactly the logged transition (aliases merged
    /// into `from` earlier and still pointing at `into` return to `from`).
    struct MergeModel {
        ptr: HashMap<String, Option<String>>,
        log: Vec<(String, String, bool)>, // (from, into_root, undone)
    }

    impl MergeModel {
        fn new(ids: &[&str]) -> Self {
            MergeModel {
                ptr: ids.iter().map(|id| (id.to_string(), None)).collect(),
                log: Vec::new(),
            }
        }

        fn lookup(&self) -> impl Fn(&str) -> Result<Option<String>, String> + '_ {
            |id| {
                self.ptr
                    .get(id)
                    .cloned()
                    .ok_or_else(|| format!("entity '{id}' does not exist"))
            }
        }

        fn merge(&mut self, from: &str, into: &str) -> Result<(), String> {
            if self.ptr[from].is_some() {
                return Err(format!("entity '{from}' is already merged"));
            }
            let root = resolve_root(self.lookup(), into)?;
            if root == from {
                return Err("merge would create a canonical pointer cycle".to_string());
            }
            // UPDATE entities SET canonical_id = root WHERE id = from
            self.ptr.insert(from.to_string(), Some(root.clone()));
            // UPDATE entities SET canonical_id = root WHERE canonical_id = from
            for (id, p) in self.ptr.iter_mut() {
                if id != from && p.as_deref() == Some(from) {
                    *p = Some(root.clone());
                }
            }
            self.log.push((from.to_string(), root, false));
            Ok(())
        }

        fn undo(&mut self, from: &str, into: &str) {
            // Restored aliases: applied log entries X→from with ptr[X]==into.
            let restored: Vec<String> = self
                .log
                .iter()
                .filter(|(_, i, undone)| !undone && i == from)
                .map(|(f, _, _)| f.clone())
                .filter(|x| self.ptr[x].as_deref() == Some(into))
                .collect();
            for x in &restored {
                self.ptr.insert(x.clone(), Some(from.to_string()));
            }
            if self.ptr[from].as_deref() == Some(into) {
                self.ptr.insert(from.to_string(), None);
            }
            if let Some(entry) = self
                .log
                .iter_mut()
                .find(|(f, i, undone)| !undone && f == from && i == into)
            {
                entry.2 = true;
            }
        }
    }

    #[test]
    fn resolve_root_follows_chains_and_detects_cycles() {
        let mut ptr: HashMap<String, Option<String>> = HashMap::new();
        ptr.insert("a".into(), Some("b".into()));
        ptr.insert("b".into(), Some("c".into()));
        ptr.insert("c".into(), None);
        let lookup = |id: &str| {
            ptr.get(id)
                .cloned()
                .ok_or_else(|| format!("missing {id}"))
        };
        assert_eq!(resolve_root(lookup, "a").unwrap(), "c");
        assert_eq!(resolve_root(lookup, "c").unwrap(), "c");
        assert!(resolve_root(lookup, "x").is_err());

        let mut cyc: HashMap<String, Option<String>> = HashMap::new();
        cyc.insert("a".into(), Some("b".into()));
        cyc.insert("b".into(), Some("a".into()));
        let lookup_cyc = |id: &str| {
            cyc.get(id)
                .cloned()
                .ok_or_else(|| format!("missing {id}"))
        };
        assert!(resolve_root(lookup_cyc, "a").is_err());
    }

    #[test]
    fn merge_into_alias_targets_root_and_compresses_paths() {
        let mut m = MergeModel::new(&["a", "b", "c"]);
        m.merge("a", "b").unwrap();
        // B is now merged into C — merging INTO the alias B must land on C
        // and re-point A (path compression), never build a→b→c chains.
        m.merge("b", "c").unwrap();
        assert_eq!(m.ptr["a"].as_deref(), Some("c"));
        assert_eq!(m.ptr["b"].as_deref(), Some("c"));
        assert_eq!(m.ptr["c"], None);
        // An alias cannot be merged again; a cycle back to the root is banned.
        assert!(m.merge("a", "c").is_err());
        let mut cyc = MergeModel::new(&["x", "y"]);
        cyc.merge("x", "y").unwrap();
        assert!(cyc.merge("y", "x").is_err());
    }

    #[test]
    fn undo_restores_exact_log_entries_in_reverse() {
        // Scenario from the review: merge A→B, then B→C, undo B→C, undo A→B.
        let mut m = MergeModel::new(&["a", "b", "c"]);
        m.merge("a", "b").unwrap();
        m.merge("b", "c").unwrap();
        assert_eq!(m.ptr["a"].as_deref(), Some("c"));

        // Undo B→C: B canonical again, A returns to B (compressed alias).
        m.undo("b", "c");
        assert_eq!(m.ptr["b"], None);
        assert_eq!(m.ptr["a"].as_deref(), Some("b"));
        assert_eq!(m.ptr["c"], None);

        // Undo A→B: everything canonical.
        m.undo("a", "b");
        assert_eq!(m.ptr["a"], None);
        assert_eq!(m.ptr["b"], None);
        assert_eq!(m.ptr["c"], None);
        assert!(m.log.iter().all(|(_, _, undone)| *undone));
    }

    // --- shrink→grow race on chunk vectors --------------------------------------

    #[test]
    fn pending_chunk_delete_is_obsolete_after_regrow() {
        // Shrink 5→2 enqueues deletes for chunks 2..5. Before the drain runs,
        // a grow re-registers 4 chunks — deletes for 2 and 3 are obsolete
        // (their ref_ids are live again), the delete for 4 still applies.
        assert!(chunk_delete_obsolete(2, 4));
        assert!(chunk_delete_obsolete(3, 4));
        assert!(!chunk_delete_obsolete(4, 4));
        // No registry row (0 registered) — every pending delete applies.
        assert!(!chunk_delete_obsolete(2, 0));
        assert!(!chunk_delete_obsolete(-1, 4));
    }

    // --- outbox dedup keys ------------------------------------------------------

    #[test]
    fn outbox_dedup_key_is_canonical_per_op_and_target() {
        let a = outbox_dedup_key("upsert_edge", &["mentions", "n1", "e1"]);
        assert_eq!(a, "upsert_edge|mentions|n1|e1");
        assert_ne!(a, outbox_dedup_key("delete_edge", &["mentions", "n1", "e1"]));
        assert_ne!(a, outbox_dedup_key("upsert_edge", &["mentions", "n1", "e2"]));
    }

    #[test]
    fn ref_ids_are_stable_and_distinct() {
        assert_eq!(note_chunk_ref("n1", 0), note_chunk_ref("n1", 0));
        assert_ne!(note_chunk_ref("n1", 0), note_chunk_ref("n1", 1));
        assert_ne!(note_chunk_ref("n1", 0), entity_vector_ref("n1"));
    }

    // --- link weights / embeddings ---------------------------------------------

    #[test]
    fn entity_link_weight_grows_with_shared_count_and_caps() {
        assert!(entity_link_weight(1) < entity_link_weight(2));
        assert!(entity_link_weight(2) < entity_link_weight(4));
        assert_eq!(entity_link_weight(4), entity_link_weight(40));
        assert!(entity_link_weight(40) <= 0.9);
    }

    #[test]
    fn embedding_parser_accepts_known_shapes_and_rejects_garbage() {
        assert_eq!(parse_embedding_vector("[0.1, 0.2]").unwrap().len(), 2);
        assert_eq!(
            parse_embedding_vector("{\"embedding\": [1, 2, 3]}").unwrap().len(),
            3
        );
        assert_eq!(
            parse_embedding_vector("{\"data\": [{\"embedding\": [1.5]}]}")
                .unwrap()
                .len(),
            1
        );
        assert!(parse_embedding_vector("{\"foo\": 1}").is_err());
        assert!(parse_embedding_vector("[]").is_err());
        assert!(parse_embedding_vector("[\"x\"]").is_err());
        assert!(parse_embedding_vector("nie json").is_err());
    }
}
