// =============================================================================
// File: addons/notes/src/blocks.rs
// Purpose: Flow Builder blocks (blocks.json) and LLM tools of the Notes addon.
//          Both share one set of handlers: hybrid search, note creation,
//          append and graph-related lookup — every path goes through the
//          acting user's UserCtx and the existing ACL predicates in db.rs /
//          search.rs. Blocks receive a FlowEnvelope-shaped JSON as params and
//          MUST return one: the input envelope is echoed with its payload
//          replaced by a Json FlowValue (context/artifacts travel through).
// =============================================================================

use serde_json::{json, Value as JsonValue};

use crate::db::{self, UserCtx};
use crate::search::{self, Method};
use crate::{analysis, ui};

/// Depth cap of get_related (1 = direct links, 2 = links of links).
const MAX_RELATED_DEPTH: u64 = 2;

// =============================================================================
// Pure envelope helpers (unit-tested natively)
// =============================================================================

/// Extracts the effective input object of a block: a Json payload passes its
/// data through; a Text payload becomes {"text": ...}; anything else (or a
/// plain tool call without an envelope) falls back to the params themselves.
pub fn block_input(params: &JsonValue) -> JsonValue {
    let Some(payload) = params.get("payload") else {
        return params.clone();
    };
    match payload.get("kind").and_then(|k| k.as_str()) {
        Some("json") => payload.get("data").cloned().unwrap_or(json!({})),
        Some("text") => json!({
            "text": payload.get("data").and_then(|d| d.as_str()).unwrap_or("")
        }),
        _ => json!({}),
    }
}

/// Builds the FlowEnvelope-shaped block response: the input envelope cloned
/// with `payload` set to a Json FlowValue carrying the handler result. A tool
/// call without an envelope returns the bare result.
pub fn block_response(params: &JsonValue, result: JsonValue) -> JsonValue {
    if params.get("payload").is_none() || params.get("schema_version").is_none() {
        return result;
    }
    let mut response = params.clone();
    if let Some(obj) = response.as_object_mut() {
        obj.insert("payload".to_string(), json!({"kind": "json", "data": result}));
    }
    response
}

fn method_wire(method: &Method) -> JsonValue {
    match method {
        Method::Vector => json!({"kind": "vector"}),
        Method::Graph { hops, entity, via } => {
            json!({"kind": "graph", "hops": hops, "entity": entity, "via": via})
        }
        Method::Text => json!({"kind": "text"}),
    }
}

fn snippet_text(snippet: &[(String, bool)]) -> String {
    snippet
        .iter()
        .map(|(w, _)| w.as_str())
        .collect::<Vec<_>>()
        .join(" ")
}

// =============================================================================
// Handlers (shared by blocks and tools)
// =============================================================================

fn handle_search(ctx: &UserCtx, input: &JsonValue) -> JsonValue {
    let query = input
        .get("query")
        .or_else(|| input.get("text"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if query.is_empty() {
        return json!({"ok": false, "error": "Brak zapytania (query)."});
    }
    let scope = input.get("scope").and_then(|v| v.as_str()).unwrap_or("all");
    let scope = if matches!(scope, "all" | "mine" | "group" | "org") {
        scope
    } else {
        "all"
    };
    let limit = input
        .get("limit")
        .and_then(|v| v.as_u64())
        .unwrap_or(8)
        .clamp(1, 12) as usize;

    match search::run_hybrid(ctx, &query, scope, None, false) {
        Ok(output) => {
            let results: Vec<JsonValue> = output
                .hits
                .iter()
                .take(limit)
                .map(|h| {
                    json!({
                        "note_id": h.note_id,
                        "title": h.title,
                        "snippet": snippet_text(&h.snippet),
                        "updated_at": h.updated_at,
                        "scope": h.scope,
                        "score": h.percent,
                        "method": method_wire(&h.method),
                    })
                })
                .collect();
            json!({
                "ok": true,
                "results": results,
                "text_fallback": output.text_fallback,
            })
        }
        Err(e) => json!({"ok": false, "error": e}),
    }
}

fn handle_create(ctx: &UserCtx, input: &JsonValue) -> JsonValue {
    let title = input.get("title").and_then(|v| v.as_str()).unwrap_or("");
    let content = input
        .get("content")
        .or_else(|| input.get("text"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if title.trim().is_empty() && content.trim().is_empty() {
        return json!({"ok": false, "error": "Podaj tytuł lub treść notatki."});
    }
    let note_id = match db::create_note(ctx) {
        Ok(id) => id,
        Err(e) => return json!({"ok": false, "error": e}),
    };
    if !title.is_empty() {
        if let Err(e) = db::update_note_field(ctx, &note_id, "title", title) {
            return json!({"ok": false, "error": e});
        }
    }
    if !content.is_empty() {
        if let Err(e) = db::update_note_field(ctx, &note_id, "content", content) {
            return json!({"ok": false, "error": e});
        }
    }
    if let Some(tags) = input.get("tags").and_then(|v| v.as_array()) {
        let tags: Vec<String> = tags
            .iter()
            .filter_map(|t| t.as_str().map(str::to_string))
            .take(32)
            .collect();
        if !tags.is_empty() {
            if let Err(e) = db::set_tags(ctx, &note_id, &tags) {
                return json!({"ok": false, "error": e});
            }
        }
    }
    analysis::enqueue(&note_id);
    json!({"ok": true, "note_id": note_id})
}

fn handle_append(ctx: &UserCtx, input: &JsonValue) -> JsonValue {
    let note_id = input.get("note_id").and_then(|v| v.as_str()).unwrap_or("");
    let content = input
        .get("content")
        .or_else(|| input.get("text"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if note_id.is_empty() {
        return json!({"ok": false, "error": "Brak note_id."});
    }
    if content.trim().is_empty() {
        return json!({"ok": false, "error": "Brak treści do dopisania."});
    }
    // get_note enforces the read ACL and exposes can_write; the UPDATE below
    // re-checks the write ACL in its own WHERE.
    let note = match db::get_note(ctx, note_id) {
        Ok(Some(n)) => n,
        Ok(None) => {
            return json!({"ok": false, "error": "Notatka nie istnieje lub brak dostępu."})
        }
        Err(e) => return json!({"ok": false, "error": e}),
    };
    if !note.can_write {
        return json!({"ok": false, "error": "Brak uprawnień do edycji tej notatki."});
    }
    let combined = if note.content.trim().is_empty() {
        content.to_string()
    } else {
        format!("{}\n\n{content}", note.content)
    };
    match db::update_note_field(ctx, note_id, "content", &combined) {
        Ok(()) => {
            analysis::enqueue(note_id);
            json!({"ok": true, "note_id": note_id, "length": combined.chars().count()})
        }
        Err(e) => json!({"ok": false, "error": e}),
    }
}

fn handle_get_related(ctx: &UserCtx, input: &JsonValue) -> JsonValue {
    let note_id = input.get("note_id").and_then(|v| v.as_str()).unwrap_or("");
    if note_id.is_empty() {
        return json!({"ok": false, "error": "Brak note_id."});
    }
    let depth = input
        .get("depth")
        .and_then(|v| v.as_u64())
        .unwrap_or(1)
        .clamp(1, MAX_RELATED_DEPTH);
    if db::get_note(ctx, note_id).ok().flatten().is_none() {
        return json!({"ok": false, "error": "Notatka nie istnieje lub brak dostępu."});
    }

    let direct = db::related_notes(ctx, note_id).unwrap_or_default();
    let mut items: Vec<JsonValue> = Vec::new();
    let mut seen: Vec<String> = vec![note_id.to_string()];
    for r in &direct {
        seen.push(r.id.clone());
        items.push(json!({
            "note_id": r.id,
            "title": r.title,
            "weight": r.weight,
            "reason": r.reason,
            "depth": 1,
        }));
    }
    if depth >= 2 {
        for r in &direct {
            for r2 in db::related_notes(ctx, &r.id).unwrap_or_default() {
                if seen.iter().any(|s| s == &r2.id) {
                    continue;
                }
                seen.push(r2.id.clone());
                items.push(json!({
                    "note_id": r2.id,
                    "title": r2.title,
                    "weight": r2.weight,
                    "reason": r2.reason,
                    "depth": 2,
                    "via_note_id": r.id,
                }));
            }
        }
    }
    let entities: Vec<JsonValue> = db::note_entities(ctx, note_id)
        .unwrap_or_default()
        .iter()
        .map(|e| json!({"id": e.id, "name": e.name, "type": e.entity_type}))
        .collect();
    json!({"ok": true, "related": items, "entities": entities})
}

// =============================================================================
// Dispatch (lib.rs entry points)
// =============================================================================

fn resolve_ctx(user_id: &str) -> Result<UserCtx, String> {
    db::resolve_user_ctx(user_id)
}

/// Flow block entry: `block_type` without the "block." prefix, params are the
/// FlowEnvelope JSON. The acting user is the flow execution's user.
pub fn handle_flow_block(block_type: &str, params: &JsonValue, user_id: &str) -> JsonValue {
    let ctx = match resolve_ctx(user_id) {
        Ok(c) => c,
        Err(e) => return json!({"ok": false, "error": e}),
    };
    // The dispatcher runs with a pooled session identity of the caller —
    // per-session UI state keys (search generation) key off it.
    ui::set_session_user(Some(user_id));
    let input = block_input(params);
    let result = match block_type {
        "notes.search" => handle_search(&ctx, &input),
        "notes.create" => handle_create(&ctx, &input),
        "notes.append" => handle_append(&ctx, &input),
        "notes.get_related" => handle_get_related(&ctx, &input),
        other => json!({"ok": false, "error": format!("Nieznany blok: {other}")}),
    };
    block_response(params, result)
}

/// LLM tool entry (same handlers, plain params).
pub fn handle_tool(tool_name: &str, params: &JsonValue, user_id: &str) -> Option<JsonValue> {
    let handler: fn(&UserCtx, &JsonValue) -> JsonValue = match tool_name {
        "search_notes" => handle_search,
        "create_note" => handle_create,
        "append_note" => handle_append,
        "get_related_notes" => handle_get_related,
        _ => return None,
    };
    let ctx = match resolve_ctx(user_id) {
        Ok(c) => c,
        Err(e) => return Some(json!({"ok": false, "error": e})),
    };
    ui::set_session_user(Some(user_id));
    Some(handler(&ctx, params))
}

// =============================================================================
// Tests — pure envelope shaping (no host fns on the native target)
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn envelope(payload: JsonValue) -> JsonValue {
        json!({
            "schema_version": 1,
            "payload": payload,
            "artifacts": {},
            "provenance": {},
            "context": {"messages": []},
            "meta": {"flow_id": "f1"},
            "trace": [],
        })
    }

    #[test]
    fn block_input_maps_text_and_json_payloads() {
        let text_env = envelope(json!({"kind": "text", "data": "co ustalono?"}));
        assert_eq!(block_input(&text_env), json!({"text": "co ustalono?"}));

        let json_env = envelope(json!({"kind": "json", "data": {"query": "q", "limit": 3}}));
        assert_eq!(block_input(&json_env), json!({"query": "q", "limit": 3}));

        // Plain tool params (no envelope) pass through untouched.
        let plain = json!({"query": "abc"});
        assert_eq!(block_input(&plain), plain);

        // Unsupported payload kinds yield an empty object, not a crash.
        let audio_env = envelope(json!({"kind": "audio", "data": {"blob_ref": "x"}}));
        assert_eq!(block_input(&audio_env), json!({}));
    }

    #[test]
    fn block_response_preserves_envelope_shape_and_swaps_payload() {
        let input = envelope(json!({"kind": "text", "data": "pytanie"}));
        let out = block_response(&input, json!({"ok": true, "results": []}));
        // Envelope invariants: schema_version + typed payload survive.
        assert_eq!(out["schema_version"], 1);
        assert_eq!(out["payload"]["kind"], "json");
        assert_eq!(out["payload"]["data"]["ok"], true);
        // Context / meta travel through unchanged.
        assert_eq!(out["meta"]["flow_id"], "f1");
        assert!(out.get("artifacts").is_some());
        assert!(out.get("trace").is_some());
    }

    #[test]
    fn block_response_without_envelope_returns_bare_result() {
        let out = block_response(&json!({"query": "x"}), json!({"ok": true}));
        assert_eq!(out, json!({"ok": true}));
    }

    #[test]
    fn method_wire_shapes() {
        assert_eq!(method_wire(&Method::Vector)["kind"], "vector");
        let g = method_wire(&Method::Graph {
            hops: 2,
            entity: "Firma".into(),
            via: Some("Spotkanie".into()),
        });
        assert_eq!(g["hops"], 2);
        assert_eq!(g["via"], "Spotkanie");
        assert_eq!(method_wire(&Method::Text)["kind"], "text");
    }

    #[test]
    fn snippet_text_joins_words() {
        let s = vec![("…".to_string(), false), ("pipeline".to_string(), true)];
        assert_eq!(snippet_text(&s), "… pipeline");
    }
}
