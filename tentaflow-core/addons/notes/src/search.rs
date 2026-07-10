// =============================================================================
// File: addons/notes/src/search.rs
// Purpose: hybrid search engine of the Notes addon (mockup n05). Two engines
//          run side by side — vector kNN over note-chunk embeddings and a
//          graph walk over visible entities + note links (≤ 2 hops) — fused
//          with Reciprocal Rank Fusion. EVERY candidate passes the reader's
//          acl_read_clause BEFORE ranking or disclosure (over-fetched vector
//          hits are intersected with the accessible set first). When the
//          model aliases are not bound, a pure LIKE text search takes over
//          ("tekstowo" badge, no answer card). Pure helpers (tokenizing, RRF,
//          snippet extraction, answer prompt) are unit-tested natively.
// =============================================================================

use std::collections::HashMap;

use crate::analysis;
use crate::db::{self, UserCtx};

/// Over-fetch factor of the vector engine: ACL filtering happens AFTER the
/// kNN, so the query asks for more hits than the page shows.
const VECTOR_OVERFETCH_K: u32 = 40;
/// Final result page size.
const MAX_RESULTS: usize = 12;
/// RRF smoothing constant (standard k=60).
const RRF_K: f64 = 60.0;
/// Snippet window (words) around the first term hit.
const SNIPPET_WORDS: usize = 32;
/// Entity chips in the right rail.
const MAX_RAIL_ENTITIES: usize = 8;
/// "Zawęź przez graf" suggestions.
pub const MAX_NARROW_SUGGESTIONS: usize = 3;
/// Notes fed into the streamed LLM answer.
pub const ANSWER_SOURCES: usize = 4;
/// Content prefix (chars) of one source in the answer prompt.
const ANSWER_SOURCE_CHARS: usize = 2_500;
/// "Ostatnie 90 dni" window in seconds (mockup n05 recency filter chip).
const RECENT_WINDOW_SECS: i64 = 90 * 86_400;

/// The `created_at` cutoff for the recency filter, or `None` when off.
fn recent_cutoff(recent: bool) -> Option<i64> {
    if recent {
        Some(db::now_unix() - RECENT_WINDOW_SECS)
    } else {
        None
    }
}

// =============================================================================
// Result model
// =============================================================================

/// How a result reached the page — drives the method badge and breadcrumb.
#[derive(Debug, Clone, PartialEq)]
pub enum Method {
    /// Semantic chunk similarity.
    Vector,
    /// Graph walk: query entity → (via note →) result note.
    Graph {
        hops: u8,
        entity: String,
        /// Title of the intermediate note (2-hop path only).
        via: Option<String>,
    },
    /// LIKE fallback (aliases not bound / embedding failed).
    Text,
}

#[derive(Debug, Clone)]
pub struct SearchHit {
    pub note_id: String,
    pub title: String,
    /// Snippet words with a bold flag per word (structural highlight — the
    /// note content is never interpreted as markup).
    pub snippet: Vec<(String, bool)>,
    pub updated_at: i64,
    pub owner_user_id: String,
    /// Widest-share scope of the note ("private" | "user" | "group" | "org").
    pub scope: String,
    /// True when the reader owns the note (scope badge "Moje").
    pub is_owner: bool,
    /// Group display name when group-scoped and resolvable (scope badge).
    pub group_name: Option<String>,
    /// Display score 0..100 (similarity for vector, link heuristic for graph,
    /// occurrence weight for text).
    pub percent: i64,
    pub method: Method,
    /// Content prefix for the answer prompt (already ACL-passed).
    pub content: String,
}

#[derive(Debug, Clone)]
pub struct RailEntity {
    pub id: String,
    pub name: String,
    pub entity_type: String,
    pub count: usize,
}

#[derive(Debug, Clone, Default)]
pub struct SearchOutput {
    pub hits: Vec<SearchHit>,
    pub entities: Vec<RailEntity>,
    /// True when the LIKE fallback ran — the UI shows "tekstowo" badges and
    /// skips the answer card.
    pub text_fallback: bool,
}

// =============================================================================
// Pure helpers (unit-tested natively)
// =============================================================================

/// Splits a query into lowercase alphanumeric tokens of >= 3 chars (short
/// stop-words drop out naturally), capped at 8 tokens.
pub fn tokenize_query(query: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for raw in query.split(|c: char| !c.is_alphanumeric()) {
        let token = raw.to_lowercase();
        if token.chars().count() < 3 || out.iter().any(|t| t == &token) {
            continue;
        }
        out.push(token);
        if out.len() >= 8 {
            break;
        }
    }
    out
}

/// Reciprocal Rank Fusion over ranked id lists: score(id) = Σ 1/(k + rank).
/// Input lists are already ACL-filtered — fusion introduces no new ids.
pub fn rrf_fuse(rankings: &[Vec<String>], k: f64) -> Vec<(String, f64)> {
    let mut scores: Vec<(String, f64)> = Vec::new();
    for list in rankings {
        for (rank, id) in list.iter().enumerate() {
            let add = 1.0 / (k + rank as f64 + 1.0);
            match scores.iter_mut().find(|(i, _)| i == id) {
                Some((_, s)) => *s += add,
                None => scores.push((id.clone(), add)),
            }
        }
    }
    scores.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    scores
}

/// Cuts a word-level snippet around the first occurrence of any term and marks
/// matching words. Char-based (never splits UTF-8), whitespace collapsed.
pub fn extract_snippet(content: &str, terms: &[String], max_words: usize) -> Vec<(String, bool)> {
    let words: Vec<&str> = content.split_whitespace().collect();
    if words.is_empty() {
        return Vec::new();
    }
    let matches_term = |word: &str| -> bool {
        let lower = word.to_lowercase();
        terms.iter().any(|t| lower.contains(t.as_str()))
    };
    let first_hit = words.iter().position(|w| matches_term(w)).unwrap_or(0);
    // Center the window on the hit, clamped to the text bounds.
    let start = first_hit.saturating_sub(max_words / 3);
    let end = (start + max_words).min(words.len());
    let start = end.saturating_sub(max_words);
    let mut out: Vec<(String, bool)> = Vec::with_capacity(end - start + 2);
    if start > 0 {
        out.push(("…".to_string(), false));
    }
    for w in &words[start..end] {
        out.push(((*w).to_string(), matches_term(w)));
    }
    if end < words.len() {
        out.push(("…".to_string(), false));
    }
    out
}

/// Occurrence-based score of the text fallback: title hits weigh 3x content
/// hits, normalized later against the best hit of the page.
pub fn text_match_score(title: &str, content: &str, terms: &[String]) -> f64 {
    let count = |hay: &str| -> usize {
        let lower = hay.to_lowercase();
        terms.iter().map(|t| lower.matches(t.as_str()).count()).sum()
    };
    (3 * count(title) + count(content)) as f64
}

/// Display weight of a graph hit (0..1): direct entity mention scores by the
/// shared-entity heuristic; a 2-hop result decays through the link weight.
pub fn graph_hit_score(hops: u8, mention_count: i64, link_weight: f64) -> f64 {
    match hops {
        1 => analysis::entity_link_weight(mention_count.max(1) as usize),
        _ => (0.35 + 0.35 * link_weight.clamp(0.0, 1.0)).min(0.75),
    }
}

/// Builds the Polish synthesis prompt: numbered sources with titles, an
/// instruction to answer ONLY from them and cite with [n]. Sources are the
/// top accessible hits — the accessible-set invariant is the caller's.
pub fn build_answer_prompt(query: &str, sources: &[(String, String)]) -> String {
    let mut prompt = String::with_capacity(1024);
    prompt.push_str(
        "Jesteś asystentem przeszukującym prywatne notatki użytkownika. \
         Odpowiedz po polsku na pytanie, korzystając WYŁĄCZNIE z poniższych \
         notatek. Po każdym fakcie dodaj cytowanie w formie [n] wskazujące \
         numer notatki źródłowej. Jeśli notatki nie zawierają odpowiedzi, \
         napisz to wprost. Odpowiadaj zwięźle, bez preambuły.\n\n",
    );
    for (i, (title, content)) in sources.iter().enumerate() {
        let body: String = content.chars().take(ANSWER_SOURCE_CHARS).collect();
        prompt.push_str(&format!("[{}] {title}\n{body}\n\n", i + 1));
    }
    prompt.push_str(&format!("Pytanie: {query}\nOdpowiedź:"));
    prompt
}

/// Removes complete `[n]` citation markers whose number falls outside
/// `1..=source_count` — the prompt numbers at most [`ANSWER_SOURCES`] sources,
/// but nothing stops the model from emitting `[7]` or `[99]`, which would
/// render as citations pointing at nothing. Brackets that are not a pure
/// digit run (e.g. `[abc]`, a lone `[`) pass through verbatim.
pub fn strip_out_of_range_citations(text: &str, source_count: usize) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < bytes.len() {
        let next = match text[i..].find('[') {
            Some(p) => i + p,
            None => {
                out.push_str(&text[i..]);
                break;
            }
        };
        out.push_str(&text[i..next]);
        let mut j = next + 1;
        while j < bytes.len() && bytes[j].is_ascii_digit() {
            j += 1;
        }
        if j > next + 1 && j < bytes.len() && bytes[j] == b']' {
            // Complete [digits] marker — keep only when it cites a real
            // source (an unparsable digit run overflows to "invalid").
            let keep = text[next + 1..j]
                .parse::<usize>()
                .map(|n| n >= 1 && n <= source_count)
                .unwrap_or(false);
            if keep {
                out.push_str(&text[next..=j]);
            }
            i = j + 1;
        } else {
            out.push('[');
            i = next + 1;
        }
    }
    out
}

/// Streaming wrapper over [`strip_out_of_range_citations`]: a marker can be
/// split across batch boundaries (`"… ["` + `"7] …"`), so a trailing partial
/// marker is withheld from the output and re-joined with the next chunk. The
/// tail is bounded at 4 bytes (`"[999"`) — a longer bracket run can never
/// complete into a marker this filter would remove, so it flows through.
pub struct CitationFilter {
    source_count: usize,
    tail: String,
}

impl CitationFilter {
    pub fn new(source_count: usize) -> Self {
        Self {
            source_count,
            tail: String::new(),
        }
    }

    /// Filters one streamed chunk; returns the text safe to display now.
    pub fn push(&mut self, chunk: &str) -> String {
        let mut joined = std::mem::take(&mut self.tail);
        joined.push_str(chunk);
        let keep = partial_marker_suffix_len(&joined);
        // '[' and digits are ASCII, so the cut always lands on a char boundary.
        let cut = joined.len() - keep;
        self.tail = joined[cut..].to_string();
        strip_out_of_range_citations(&joined[..cut], self.source_count)
    }

    /// Flushes the withheld tail at end of stream. An unterminated `"[2"` is
    /// plain prose at this point, not a marker — it is emitted verbatim.
    pub fn finish(&mut self) -> String {
        std::mem::take(&mut self.tail)
    }
}

/// Byte length of the trailing partial citation marker (`'['` followed only
/// by digits, max 4 bytes), 0 when the text ends outside a marker.
fn partial_marker_suffix_len(s: &str) -> usize {
    let b = s.as_bytes();
    let start = b.len().saturating_sub(4);
    (start..b.len())
        .find(|&i| b[i] == b'[' && b[i + 1..].iter().all(|c| c.is_ascii_digit()))
        .map(|i| b.len() - i)
        .unwrap_or(0)
}

/// Which engines a query runs, decided by the two alias tiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchTier {
    /// No llm alias — pure LIKE, no answer card ("tekstowo").
    TextOnly,
    /// llm bound, embeddings not — graph + text fusion, answer card, no
    /// "wektorowo" badge.
    GraphText,
    /// Both bound — vector + graph fusion, answer card.
    VectorGraph,
}

/// Pure tier selector (unit-tested); mirrors the branch order of `run_hybrid`.
pub fn search_tier(llm_ready: bool, embeddings_ready: bool) -> SearchTier {
    if !llm_ready {
        SearchTier::TextOnly
    } else if !embeddings_ready {
        SearchTier::GraphText
    } else {
        SearchTier::VectorGraph
    }
}

// =============================================================================
// Engine
// =============================================================================

/// One graph-walk candidate before fusion.
struct GraphCandidate {
    note_id: String,
    hops: u8,
    entity_name: String,
    via_note_id: Option<String>,
    score: f64,
}

/// Runs the hybrid search for a reader. `scope` matches the list filter chips
/// ("all" | "mine" | "group" | "org"); `narrow` restricts results to notes
/// connected to that entity (right-rail "Zawęź przez graf").
pub fn run_hybrid(
    ctx: &UserCtx,
    query: &str,
    scope: &str,
    narrow: Option<&str>,
    recent: bool,
) -> Result<SearchOutput, String> {
    let terms = tokenize_query(query);
    if query.trim().is_empty() {
        return Ok(SearchOutput::default());
    }
    let cutoff = recent_cutoff(recent);

    match search_tier(analysis::llm_ready(), analysis::embeddings_ready()) {
        // Tier 1 missing → pure LIKE fallback, no answer card.
        SearchTier::TextOnly => return text_search(ctx, query, scope, &terms, cutoff),
        // Tier 2 missing → graph + text hybrid (still an answer card, just no
        // vector similarity / "wektorowo" badge).
        SearchTier::GraphText => return graph_text_search(ctx, query, scope, &terms, narrow, cutoff),
        SearchTier::VectorGraph => {}
    }

    // --- Vector engine (over-fetch, then ACL) -----------------------------
    let vector_ranked_raw = match analysis::embed_query(query) {
        Ok(vector) => analysis::query_note_vectors(&vector, VECTOR_OVERFETCH_K)?,
        // A bound but unreachable embeddings model degrades to graph + text
        // instead of an error page.
        Err(_) => return graph_text_search(ctx, query, scope, &terms, narrow, cutoff),
    };

    // --- Graph engine (visible entities only) ------------------------------
    let graph_candidates = graph_walk(ctx, &terms)?;

    // --- ACL BEFORE ranking -------------------------------------------------
    let mut candidate_ids: Vec<String> = vector_ranked_raw.iter().map(|(id, _)| id.clone()).collect();
    for c in &graph_candidates {
        if !candidate_ids.iter().any(|id| id == &c.note_id) {
            candidate_ids.push(c.note_id.clone());
        }
    }
    let mut meta = db::search_notes_meta(ctx, &candidate_ids, scope, cutoff)?;

    // Narrow filter: keep only notes connected to the chosen entity.
    if let Some(entity_id) = narrow {
        let connected = db::notes_connected_to_entity(ctx, entity_id)?;
        meta.retain(|id, _| connected.iter().any(|c| c == id));
    }

    let vector_ranked: Vec<String> = vector_ranked_raw
        .iter()
        .filter(|(id, _)| meta.contains_key(id))
        .map(|(id, _)| id.clone())
        .collect();
    let graph_ranked: Vec<String> = graph_candidates
        .iter()
        .filter(|c| meta.contains_key(&c.note_id))
        .map(|c| c.note_id.clone())
        .collect();

    // --- RRF fusion ---------------------------------------------------------
    let fused = rrf_fuse(&[vector_ranked.clone(), graph_ranked.clone()], RRF_K);

    let vector_sim: HashMap<&str, f64> = vector_ranked_raw
        .iter()
        .map(|(id, sim)| (id.as_str(), *sim))
        .collect();

    let mut hits: Vec<SearchHit> = Vec::new();
    for (note_id, _) in fused.into_iter().take(MAX_RESULTS) {
        let Some(m) = meta.get(&note_id) else { continue };
        let vec_rank = vector_ranked.iter().position(|id| id == &note_id);
        let graph_hit = graph_candidates.iter().find(|c| c.note_id == note_id);
        // The badge shows the engine that ranked the note better; a note found
        // by both defaults to the vector engine (mockup shows one method).
        let (method, score) = match (vec_rank, graph_hit) {
            (Some(_), _) => (
                Method::Vector,
                vector_sim.get(note_id.as_str()).copied().unwrap_or(0.0),
            ),
            (None, Some(g)) => (
                Method::Graph {
                    hops: g.hops,
                    entity: g.entity_name.clone(),
                    via: g
                        .via_note_id
                        .as_ref()
                        .and_then(|via| meta.get(via).map(|vm| vm.title.clone())),
                },
                g.score,
            ),
            (None, None) => continue,
        };
        hits.push(SearchHit {
            note_id: note_id.clone(),
            title: m.title.clone(),
            snippet: extract_snippet(&m.content, &terms, SNIPPET_WORDS),
            updated_at: m.updated_at,
            is_owner: m.owner_user_id == ctx.user_id,
            group_name: m.group_name.clone(),
            owner_user_id: m.owner_user_id.clone(),
            scope: m.scope.clone(),
            percent: (score.clamp(0.0, 1.0) * 100.0).round() as i64,
            method,
            content: m.content.clone(),
        });
    }

    let entities = rail_entities(ctx, &hits);
    Ok(SearchOutput {
        hits,
        entities,
        text_fallback: false,
    })
}

/// LIKE fallback ranked by term occurrences (title weighted). Same ACL path
/// as the list view.
fn text_search(
    ctx: &UserCtx,
    query: &str,
    scope: &str,
    terms: &[String],
    cutoff: Option<i64>,
) -> Result<SearchOutput, String> {
    let rows = db::text_search_notes(ctx, scope, query, cutoff)?;
    let mut scored: Vec<(db::SearchNoteMeta, f64)> = rows
        .into_iter()
        .map(|m| {
            let s = text_match_score(&m.title, &m.content, terms).max(1.0);
            (m, s)
        })
        .collect();
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let max = scored.first().map(|(_, s)| *s).unwrap_or(1.0);
    let hits: Vec<SearchHit> = scored
        .into_iter()
        .take(MAX_RESULTS)
        .map(|(m, s)| SearchHit {
            snippet: extract_snippet(&m.content, terms, SNIPPET_WORDS),
            percent: ((s / max).clamp(0.0, 1.0) * 100.0).round() as i64,
            method: Method::Text,
            note_id: m.id,
            title: m.title,
            updated_at: m.updated_at,
            is_owner: m.owner_user_id == ctx.user_id,
            group_name: m.group_name,
            owner_user_id: m.owner_user_id,
            scope: m.scope,
            content: m.content,
        })
        .collect();
    let entities = rail_entities(ctx, &hits);
    Ok(SearchOutput {
        hits,
        entities,
        text_fallback: true,
    })
}

/// Graph + text hybrid used when the llm alias is bound but the embeddings
/// alias is not: the graph engine (entity → note walk) is fused with the LIKE
/// text engine through RRF. No vector similarity, so no "wektorowo" badge — a
/// hit is either `Graph` (walked) or `Text` (LIKE). The answer card still
/// streams (llm is available), so `text_fallback` stays false.
fn graph_text_search(
    ctx: &UserCtx,
    query: &str,
    scope: &str,
    terms: &[String],
    narrow: Option<&str>,
    cutoff: Option<i64>,
) -> Result<SearchOutput, String> {
    let graph_candidates = graph_walk(ctx, terms)?;
    let text_rows = db::text_search_notes(ctx, scope, query, cutoff)?;

    let mut candidate_ids: Vec<String> = text_rows.iter().map(|m| m.id.clone()).collect();
    for c in &graph_candidates {
        if !candidate_ids.iter().any(|id| id == &c.note_id) {
            candidate_ids.push(c.note_id.clone());
        }
    }
    let mut meta = db::search_notes_meta(ctx, &candidate_ids, scope, cutoff)?;
    if let Some(entity_id) = narrow {
        let connected = db::notes_connected_to_entity(ctx, entity_id)?;
        meta.retain(|id, _| connected.iter().any(|c| c == id));
    }

    let mut text_scored: Vec<(String, f64)> = text_rows
        .iter()
        .filter(|m| meta.contains_key(&m.id))
        .map(|m| (m.id.clone(), text_match_score(&m.title, &m.content, terms).max(1.0)))
        .collect();
    text_scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let text_max = text_scored.first().map(|(_, s)| *s).unwrap_or(1.0);

    let graph_ranked: Vec<String> = graph_candidates
        .iter()
        .filter(|c| meta.contains_key(&c.note_id))
        .map(|c| c.note_id.clone())
        .collect();
    let text_ranked: Vec<String> = text_scored.iter().map(|(id, _)| id.clone()).collect();

    let fused = rrf_fuse(&[graph_ranked, text_ranked], RRF_K);

    let mut hits: Vec<SearchHit> = Vec::new();
    for (note_id, _) in fused.into_iter().take(MAX_RESULTS) {
        let Some(m) = meta.get(&note_id) else { continue };
        let graph_hit = graph_candidates.iter().find(|c| c.note_id == note_id);
        let (method, score) = match graph_hit {
            Some(g) => (
                Method::Graph {
                    hops: g.hops,
                    entity: g.entity_name.clone(),
                    via: g
                        .via_note_id
                        .as_ref()
                        .and_then(|via| meta.get(via).map(|vm| vm.title.clone())),
                },
                g.score,
            ),
            None => {
                let raw = text_scored
                    .iter()
                    .find(|(id, _)| id == &note_id)
                    .map(|(_, s)| *s)
                    .unwrap_or(1.0);
                (Method::Text, raw / text_max)
            }
        };
        hits.push(SearchHit {
            note_id: note_id.clone(),
            title: m.title.clone(),
            snippet: extract_snippet(&m.content, terms, SNIPPET_WORDS),
            updated_at: m.updated_at,
            is_owner: m.owner_user_id == ctx.user_id,
            group_name: m.group_name.clone(),
            owner_user_id: m.owner_user_id.clone(),
            scope: m.scope.clone(),
            percent: (score.clamp(0.0, 1.0) * 100.0).round() as i64,
            method,
            content: m.content.clone(),
        });
    }

    let entities = rail_entities(ctx, &hits);
    Ok(SearchOutput {
        hits,
        entities,
        text_fallback: false,
    })
}

/// Graph walk: query tokens → visible entities (LIKE over names) → notes
/// mentioning them (1 hop) → notes linked to those (2 hops). Every note read
/// goes through acl_read; hop-2 paths remember the intermediate note.
fn graph_walk(ctx: &UserCtx, terms: &[String]) -> Result<Vec<GraphCandidate>, String> {
    if terms.is_empty() {
        return Ok(Vec::new());
    }
    let entities = db::match_query_entities(ctx, terms)?;
    if entities.is_empty() {
        return Ok(Vec::new());
    }
    let entity_ids: Vec<String> = entities.iter().map(|e| e.0.clone()).collect();
    let name_of = |id: &str| -> String {
        entities
            .iter()
            .find(|(eid, _, _)| eid == id)
            .map(|(_, n, _)| n.clone())
            .unwrap_or_default()
    };

    let mut out: Vec<GraphCandidate> = Vec::new();
    let hop1 = db::notes_mentioning_entities(ctx, &entity_ids)?;
    for (note_id, entity_id, mention_count) in &hop1 {
        if out.iter().any(|c| &c.note_id == note_id) {
            continue;
        }
        out.push(GraphCandidate {
            note_id: note_id.clone(),
            hops: 1,
            entity_name: name_of(entity_id),
            via_note_id: None,
            score: graph_hit_score(1, *mention_count, 0.0),
        });
    }

    let hop1_ids: Vec<String> = hop1.iter().map(|(id, _, _)| id.clone()).collect();
    for (via_id, note_id, weight) in db::notes_linked_to(ctx, &hop1_ids)? {
        if out.iter().any(|c| c.note_id == note_id) {
            continue;
        }
        let entity_id = hop1
            .iter()
            .find(|(id, _, _)| id == &via_id)
            .map(|(_, eid, _)| eid.clone())
            .unwrap_or_default();
        out.push(GraphCandidate {
            note_id,
            hops: 2,
            entity_name: name_of(&entity_id),
            via_note_id: Some(via_id),
            score: graph_hit_score(2, 1, weight),
        });
    }
    out.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    Ok(out)
}

/// Aggregated visible entities of the result notes (right-rail chips), by
/// occurrence count across results.
fn rail_entities(ctx: &UserCtx, hits: &[SearchHit]) -> Vec<RailEntity> {
    if hits.is_empty() {
        return Vec::new();
    }
    let ids: Vec<String> = hits.iter().map(|h| h.note_id.clone()).collect();
    let mentions = db::graph_mentions(ctx, &ids);
    let mut agg: Vec<RailEntity> = Vec::new();
    for m in &mentions {
        match agg.iter_mut().find(|e| e.id == m.entity_id) {
            Some(e) => e.count += 1,
            None => agg.push(RailEntity {
                id: m.entity_id.clone(),
                name: m.name.clone(),
                entity_type: m.entity_type.clone(),
                count: 1,
            }),
        }
    }
    agg.sort_by_key(|e| std::cmp::Reverse(e.count));
    agg.truncate(MAX_RAIL_ENTITIES);
    agg
}

// =============================================================================
// Tests — pure helpers only (no host fns on the native target)
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_tier_reflects_both_alias_levels() {
        // No llm → text-only, whatever embeddings do.
        assert_eq!(search_tier(false, false), SearchTier::TextOnly);
        assert_eq!(search_tier(false, true), SearchTier::TextOnly);
        // llm but no embeddings → graph + text (still an answer card).
        assert_eq!(search_tier(true, false), SearchTier::GraphText);
        // Both → full vector + graph hybrid.
        assert_eq!(search_tier(true, true), SearchTier::VectorGraph);
    }

    #[test]
    fn tokenize_drops_short_tokens_and_dedups() {
        let t = tokenize_query("co ustaliliśmy z Firma Sp. z o.o. w sprawie pipeline'u? firma");
        assert!(t.contains(&"ustaliliśmy".to_string()));
        assert!(t.contains(&"firma".to_string()));
        assert!(t.contains(&"pipeline".to_string()));
        // "z", "w", "sp", "o", "u" are too short; "firma" appears once.
        assert!(!t.iter().any(|x| x.chars().count() < 3));
        assert_eq!(t.iter().filter(|x| x.as_str() == "firma").count(), 1);
    }

    #[test]
    fn rrf_ranks_notes_present_in_both_lists_first() {
        let vector = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let graph = vec!["c".to_string(), "d".to_string()];
        let fused = rrf_fuse(&[vector, graph], 60.0);
        // "c" gets contributions from both engines and beats single-list "b"/"d".
        let pos = |id: &str| fused.iter().position(|(i, _)| i == id).unwrap();
        assert!(pos("c") < pos("b"));
        assert!(pos("c") < pos("d"));
        // "a" (rank 1 in vector) still beats "d" (rank 2 in graph).
        assert!(pos("a") < pos("d"));
        // No id appears twice.
        assert_eq!(fused.len(), 4);
    }

    #[test]
    fn rrf_never_introduces_ids_outside_the_input_lists() {
        // ACL contract: the input lists are already filtered to the reader's
        // accessible set — fusion must not resurrect a filtered-out note. A
        // note that HAD a vector hit but failed the ACL is absent from the
        // ranked input, so it cannot exist in the fused output either.
        let accessible_vector = vec!["a".to_string()];
        let accessible_graph = vec!["b".to_string()];
        let fused = rrf_fuse(&[accessible_vector, accessible_graph], 60.0);
        assert!(fused.iter().all(|(id, _)| id == "a" || id == "b"));
        assert!(!fused.iter().any(|(id, _)| id == "private_note"));
    }

    #[test]
    fn snippet_centers_on_first_match_and_marks_words_utf8_safely() {
        let content = "Żółta łódź płynęła. Ustaliliśmy że migracja pipeline'u rusza \
                       od lipca i wszystko będzie dobrze w każdym calu projektu.";
        let terms = vec!["pipeline".to_string()];
        let snippet = extract_snippet(content, &terms, 8);
        assert!(snippet.iter().any(|(w, b)| *b && w.to_lowercase().contains("pipeline")));
        // Non-matching words are unmarked.
        assert!(snippet.iter().any(|(w, b)| !*b && w == "migracja"));
        // Window is capped (plus ellipsis markers).
        assert!(snippet.len() <= 10);
        // UTF-8 content with no match still yields a head snippet, uncut chars.
        let s2 = extract_snippet("żółć źrebię łąka", &["brak".to_string()], 2);
        assert_eq!(s2[0].0, "żółć");
    }

    #[test]
    fn snippet_of_empty_content_is_empty() {
        assert!(extract_snippet("", &["x".to_string()], 10).is_empty());
        assert!(extract_snippet("   \n ", &[], 10).is_empty());
    }

    #[test]
    fn text_score_weights_title_hits() {
        let terms = vec!["pipeline".to_string()];
        let title_hit = text_match_score("Pipeline wdrożeniowy", "nic", &terms);
        let content_hit = text_match_score("Notatka", "pipeline pipeline", &terms);
        assert_eq!(title_hit, 3.0);
        assert_eq!(content_hit, 2.0);
    }

    #[test]
    fn graph_scores_decay_with_hops() {
        assert!(graph_hit_score(1, 3, 0.0) > graph_hit_score(2, 1, 1.0));
        assert!(graph_hit_score(2, 1, 0.9) > graph_hit_score(2, 1, 0.1));
        assert!(graph_hit_score(2, 1, 1.0) <= 0.75);
    }

    #[test]
    fn answer_prompt_numbers_sources_and_carries_the_question() {
        let sources = vec![
            ("Spotkanie".to_string(), "treść pierwsza".to_string()),
            ("Rollback".to_string(), "treść druga".to_string()),
        ];
        let prompt = build_answer_prompt("co ustalono?", &sources);
        assert!(prompt.contains("[1] Spotkanie"));
        assert!(prompt.contains("[2] Rollback"));
        assert!(prompt.contains("treść pierwsza"));
        assert!(prompt.contains("Pytanie: co ustalono?"));
        // Citation instruction present (the UI maps [n] chips to sources).
        assert!(prompt.contains("[n]"));
    }

    #[test]
    fn citation_strip_removes_out_of_range_markers_only() {
        assert_eq!(
            strip_out_of_range_citations("fakt [2], plotka [5] i [0].", 4),
            "fakt [2], plotka  i ."
        );
        // Non-marker brackets and huge digit runs out of range.
        assert_eq!(
            strip_out_of_range_citations("[abc] [ [1] [99999999999999999999]", 4),
            "[abc] [ [1] "
        );
        // UTF-8 around markers stays intact.
        assert_eq!(strip_out_of_range_citations("żółć [7]!", 4), "żółć !");
    }

    #[test]
    fn citation_filter_handles_marker_split_across_batches() {
        let mut f = CitationFilter::new(4);
        // Out-of-range marker split at the '[' boundary is still removed.
        let mut out = f.push("odpowiedź [");
        assert_eq!(out, "odpowiedź ");
        out.push_str(&f.push("7] dalej [1]"));
        out.push_str(&f.finish());
        assert_eq!(out, "odpowiedź  dalej [1]");

        // In-range marker split mid-digits survives the re-join.
        let mut f = CitationFilter::new(4);
        let mut out = f.push("[1");
        out.push_str(&f.push("]"));
        out.push_str(&f.finish());
        assert_eq!(out, "[1]");
    }

    #[test]
    fn citation_filter_flushes_dangling_tail_verbatim() {
        let mut f = CitationFilter::new(4);
        let mut out = f.push("koniec [2");
        assert_eq!(out, "koniec ");
        out.push_str(&f.finish());
        assert_eq!(out, "koniec [2");
        // finish() is idempotent — the tail is emitted once.
        assert_eq!(f.finish(), "");
    }

    #[test]
    fn citation_filter_passes_long_bracket_runs_through() {
        let mut f = CitationFilter::new(4);
        // 5+ bracket bytes cannot complete into a removable marker, so the
        // filter must not buffer them forever.
        let mut out = f.push("wzór [12345");
        out.push_str(&f.finish());
        assert_eq!(out, "wzór [12345");
    }

    #[test]
    fn answer_prompt_truncates_long_sources_on_char_boundary() {
        let long = "ż".repeat(ANSWER_SOURCE_CHARS + 50);
        let prompt = build_answer_prompt("q", &[("T".to_string(), long)]);
        // No panic on the multi-byte boundary and the tail is cut.
        assert!(prompt.len() < ANSWER_SOURCE_CHARS * 3);
    }
}
