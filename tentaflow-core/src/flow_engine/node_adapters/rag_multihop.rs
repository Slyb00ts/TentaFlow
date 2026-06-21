// =============================================================================
// Plik: flow_engine/node_adapters/rag_multihop.rs
// Opis: Węzły pętli multi-hop RAG (E2.2). Trzy adaptery składające ciało pętli
//       retrieval-round wykonywanej przez `loop` block:
//         * `rag_query_seed`  — ustawia payload = bieżące pod-pytanie
//           (`meta.rag_current_query`, init = oryginalne pytanie z payloadu),
//           żeby embeddings/vector szukały po właściwym zapytaniu tego hopu.
//         * `rag_accumulate`  — akumuluje pasaże zrerankowane W TYM hopie
//           (`meta.rag_citations`) do `meta.rag_accumulated` z DEDUP po
//           (doc_id, chunk_index) i CAP całkowitej liczby; buduje payload =
//           kontekst (pytanie + zakumulowane pasaże) dla sędziego LLM.
//         * `rag_judge`       — parsuje odpowiedź sędziego LLM `{enough,
//           next_query}` z payloadu i ustawia `meta.harness_done=true` (starczy)
//           albo `meta.rag_current_query = next_query` (kolejny hop).
//       Akumulacja w meta PRZETRWA iteracje pętli, bo `loop` block podaje
//       output-envelope iteracji N jako input iteracji N+1 (subflow_runner).
// =============================================================================

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde_json::Value;

use crate::flow_engine::envelope::{FlowEnvelope, FlowValue, NodeInput};
use crate::flow_engine::node_adapter::{ExecutionContext, NodeAdapter, PortSpec};
use crate::flow_engine::types::{FlowDataType, FlowNode};

/// Twardy cap całkowitej liczby zakumulowanych pasaży (anti-DoS / kontrola
/// rozmiaru kontekstu LLM). Po przekroczeniu trzymamy `MAX_ACCUMULATED`
/// najlepszych po score — pętla multi-hop nie może w nieskończoność rozdymać
/// kontekstu finalnej odpowiedzi.
pub const MAX_ACCUMULATED: usize = 20;

/// Twardy cap długości pod-pytania (`next_query`) zwróconego przez sędziego LLM.
/// Sędzia czyta payload pochodzący z pasaży, więc prompt-injection mógłby wymusić
/// gigantyczne pod-pytanie idące dalej do embed/search — przycinamy je char-safe.
pub const MAX_NEXT_QUERY_CHARS: usize = 1024;

/// Meta-klucz: oryginalne pytanie użytkownika (stash przez `rag_query_seed` w
/// pierwszym hopie). Sędzia i finalna odpowiedź zawsze widzą ORYGINALNE pytanie,
/// nie bieżące pod-pytanie.
const META_ORIGINAL_QUESTION: &str = "rag_original_question";
/// Meta-klucz: bieżące pod-pytanie tego hopu (init = oryginalne pytanie).
const META_CURRENT_QUERY: &str = "rag_current_query";
/// Meta-klucz: lista zakumulowanych pasaży (rośnie między hopami).
const META_ACCUMULATED: &str = "rag_accumulated";
/// Meta-klucz: cytaty zrerankowane W TYM hopie (ustawiane przez reranker/vector).
const META_CITATIONS: &str = "rag_citations";
/// Meta-klucz harnessu pętli — `true` kończy pętlę (`loop` until).
const META_HARNESS_DONE: &str = "harness_done";

// =============================================================================
// Czysta logika akumulacji + dedup (testowalna bez ABI/DB)
// =============================================================================

/// Klucz dedup pasaża: `(doc_id, chunk_index)` w formie znormalizowanej do
/// stringa (oba pola mogą być Null/Int/Str w cytatach z vector/reranker).
fn dedup_key(passage: &Value) -> String {
    let doc = passage.get("doc_id").cloned().unwrap_or(Value::Null);
    let chunk = passage.get("chunk_index").cloned().unwrap_or(Value::Null);
    format!("{doc}|{chunk}")
}

/// Score pasaża (malejąco = lepszy). Brak/nie-liczba → `f64::MIN`, żeby pasaże
/// bez score lądowały na końcu przy cappingu.
fn passage_score(passage: &Value) -> f64 {
    passage
        .get("score")
        .and_then(|v| v.as_f64())
        .unwrap_or(f64::MIN)
}

/// Łączy `existing` (dotychczas zakumulowane) z `incoming` (pasaże tego hopu):
/// dedup po (doc_id, chunk_index) z zachowaniem WYŻSZEGO score, sort malejąco po
/// score, cap do `MAX_ACCUMULATED`. Czysta funkcja — fundament inwariantu, że
/// finalne cytaty = wszystkie zakumulowane pasaże (dedup, top wg score).
pub fn merge_accumulated(existing: &[Value], incoming: &[Value]) -> Vec<Value> {
    use std::collections::HashMap;

    // Mapa klucz→pasaż z najlepszym dotąd score (zachowuje kolejność wstawień
    // przez równoległy wektor kluczy — deterministyczny wynik przy równych score).
    let mut by_key: HashMap<String, Value> = HashMap::new();
    let mut order: Vec<String> = Vec::new();

    for passage in existing.iter().chain(incoming.iter()) {
        // Pomijamy pasaże bez treści (nie da się ich pokazać / zacytować).
        if passage.get("text").and_then(|v| v.as_str()).is_none() {
            continue;
        }
        let key = dedup_key(passage);
        match by_key.get(&key) {
            Some(prev) if passage_score(prev) >= passage_score(passage) => {
                // Zachowaj poprzedni (lepszy lub równy score).
            }
            Some(_) => {
                by_key.insert(key, passage.clone());
            }
            None => {
                order.push(key.clone());
                by_key.insert(key, passage.clone());
            }
        }
    }

    let mut merged: Vec<Value> = order
        .into_iter()
        .filter_map(|k| by_key.remove(&k))
        .collect();

    // Sort malejąco po score (stabilny względem kolejności wstawień dla remisów).
    merged.sort_by(|a, b| passage_score(b).total_cmp(&passage_score(a)));
    merged.truncate(MAX_ACCUMULATED);
    merged
}

/// Wyciąga tablicę pasaży z wartości meta (None gdy brak / nie-tablica).
fn passages_from_meta(envelope: &FlowEnvelope, key: &str) -> Vec<Value> {
    envelope
        .meta
        .get(key)
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default()
}

/// Buduje tekst kontekstu dla LLM (sędziego / finalnej odpowiedzi) z pytania i
/// zakumulowanych pasaży. Format jawnie numerowany, żeby model mógł cytować.
fn build_context_text(question: &str, accumulated: &[Value]) -> String {
    let mut s = String::new();
    s.push_str("Pytanie: ");
    s.push_str(question);
    s.push_str("\n\nKontekst (zakumulowane pasaże):\n");
    for (i, p) in accumulated.iter().enumerate() {
        let text = p.get("text").and_then(|v| v.as_str()).unwrap_or("");
        let doc = p
            .get("doc_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| p.get("doc_id").map(|v| v.to_string()).unwrap_or_default());
        let chunk = p
            .get("chunk_index")
            .map(|v| v.to_string())
            .unwrap_or_default();
        s.push_str(&format!("[{i}] (doc={doc} chunk={chunk}) {text}\n"));
    }
    s
}

// =============================================================================
// rag_query_seed — ustawia payload = bieżące pod-pytanie tego hopu
// =============================================================================

pub struct RagQuerySeedNodeAdapter;

impl RagQuerySeedNodeAdapter {
    pub fn new() -> Self {
        Self
    }
}

impl Default for RagQuerySeedNodeAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl NodeAdapter for RagQuerySeedNodeAdapter {
    fn node_type(&self) -> &str {
        "rag_query_seed"
    }
    fn input_ports(&self) -> Vec<PortSpec> {
        vec![PortSpec::new("in", FlowDataType::Any)]
    }
    fn output_ports(&self) -> Vec<PortSpec> {
        vec![PortSpec::new("full", FlowDataType::Text)]
    }

    async fn execute(
        &self,
        _node: &FlowNode,
        inputs: &[NodeInput],
        _ctx: &ExecutionContext,
    ) -> Result<FlowEnvelope> {
        let input = inputs
            .first()
            .ok_or_else(|| anyhow!("rag_query_seed: brak krawędzi wejściowej"))?;
        let mut out: FlowEnvelope = (*input.envelope).clone();

        // Pierwszy hop: meta nie ma jeszcze rag_current_query — bierzemy
        // oryginalne pytanie z payloadu Text (trigger → ten węzeł). Stash
        // oryginalnego pytania w meta, żeby sędzia/finalna odpowiedź zawsze
        // widziały je, nie bieżące pod-pytanie. Kolejne hopy: rag_current_query
        // ustawione przez rag_judge poprzedniego hopu.
        let current = out
            .meta
            .get(META_CURRENT_QUERY)
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .or_else(|| out.payload.as_text().map(|s| s.to_string()))
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| {
                anyhow!(
                    "rag_query_seed: brak pytania — ani meta.rag_current_query, ani payload Text"
                )
            })?;

        out.meta
            .entry(META_ORIGINAL_QUESTION.to_string())
            .or_insert_with(|| Value::String(current.clone()));
        out.meta
            .insert(META_CURRENT_QUERY.to_string(), Value::String(current.clone()));
        out.payload = FlowValue::Text(current);
        Ok(out)
    }
}

// =============================================================================
// rag_accumulate — dedup + akumulacja pasaży tego hopu, build kontekstu sędziego
// =============================================================================

pub struct RagAccumulateNodeAdapter;

impl RagAccumulateNodeAdapter {
    pub fn new() -> Self {
        Self
    }
}

impl Default for RagAccumulateNodeAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl NodeAdapter for RagAccumulateNodeAdapter {
    fn node_type(&self) -> &str {
        "rag_accumulate"
    }
    fn input_ports(&self) -> Vec<PortSpec> {
        vec![PortSpec::new("in", FlowDataType::Any)]
    }
    fn output_ports(&self) -> Vec<PortSpec> {
        vec![PortSpec::new("full", FlowDataType::Text)]
    }

    async fn execute(
        &self,
        _node: &FlowNode,
        inputs: &[NodeInput],
        _ctx: &ExecutionContext,
    ) -> Result<FlowEnvelope> {
        let input = inputs
            .first()
            .ok_or_else(|| anyhow!("rag_accumulate: brak krawędzi wejściowej"))?;
        let mut out: FlowEnvelope = (*input.envelope).clone();

        // Pasaże tego hopu = rerankowane cytaty (reranker nadpisał rag_citations
        // rerankowaną kolejnością + score cross-encodera). Łączymy z dotąd
        // zakumulowanymi, dedup po (doc_id, chunk_index), cap MAX_ACCUMULATED.
        let existing = passages_from_meta(&out, META_ACCUMULATED);
        let incoming = passages_from_meta(&out, META_CITATIONS);
        let merged = merge_accumulated(&existing, &incoming);

        out.meta.insert(
            META_ACCUMULATED.to_string(),
            Value::Array(merged.clone()),
        );

        // Payload = kontekst (oryginalne pytanie + zakumulowane pasaże) dla
        // sędziego LLM. Sędzia ocenia, czy STARCZA do odpowiedzi na ORYGINALNE
        // pytanie — dlatego budujemy kontekst wokół niego, nie pod-pytania.
        let question = out
            .meta
            .get(META_ORIGINAL_QUESTION)
            .and_then(|v| v.as_str())
            .or_else(|| out.meta.get(META_CURRENT_QUERY).and_then(|v| v.as_str()))
            .unwrap_or("")
            .to_string();
        out.payload = FlowValue::Text(build_context_text(&question, &merged));
        Ok(out)
    }
}

// =============================================================================
// rag_judge — parsuje {enough,next_query} sędziego, ustawia harness_done/next
// =============================================================================

pub struct RagJudgeNodeAdapter;

impl RagJudgeNodeAdapter {
    pub fn new() -> Self {
        Self
    }
}

impl Default for RagJudgeNodeAdapter {
    fn default() -> Self {
        Self::new()
    }
}

/// Wynik parsowania werdyktu sędziego. Czysta struktura — testowana osobno.
#[derive(Debug, Clone, PartialEq)]
pub struct JudgeVerdict {
    pub enough: bool,
    pub next_query: Option<String>,
}

/// Parsuje odpowiedź sędziego LLM na `{enough:bool, next_query?:string}`.
/// Toleruje JSON owinięty w tekst/markdown (wyłuskuje pierwszy obiekt `{...}`).
/// Brak parsowalnego JSON-a albo `enough` nie-bool → traktujemy jako „starczy"
/// (bezpieczny default: kończymy pętlę zamiast pętlić w nieskończoność na
/// niejasnym werdykcie). `next_query` pusty/whitespace → None.
pub fn parse_judge_verdict(raw: &str) -> JudgeVerdict {
    let json = extract_json_object(raw);
    let Some(v) = json else {
        return JudgeVerdict {
            enough: true,
            next_query: None,
        };
    };
    let enough = v.get("enough").and_then(|x| x.as_bool()).unwrap_or(true);
    let next_query = v
        .get("next_query")
        .and_then(|x| x.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    JudgeVerdict { enough, next_query }
}

/// Wyłuskuje pierwszy obiekt JSON z tekstu (model może owinąć go w prozę albo
/// fence ```json). Najpierw próba parsowania całości, potem wycinek od pierwszego
/// `{` do ostatniego `}`.
fn extract_json_object(raw: &str) -> Option<Value> {
    let trimmed = raw.trim();
    if let Ok(v) = serde_json::from_str::<Value>(trimmed) {
        if v.is_object() {
            return Some(v);
        }
    }
    let start = trimmed.find('{')?;
    let end = trimmed.rfind('}')?;
    if end <= start {
        return None;
    }
    serde_json::from_str::<Value>(&trimmed[start..=end])
        .ok()
        .filter(|v| v.is_object())
}

#[async_trait]
impl NodeAdapter for RagJudgeNodeAdapter {
    fn node_type(&self) -> &str {
        "rag_judge"
    }
    fn input_ports(&self) -> Vec<PortSpec> {
        vec![PortSpec::new("in", FlowDataType::Text)]
    }
    fn output_ports(&self) -> Vec<PortSpec> {
        vec![PortSpec::new("full", FlowDataType::Text)]
    }

    async fn execute(
        &self,
        _node: &FlowNode,
        inputs: &[NodeInput],
        _ctx: &ExecutionContext,
    ) -> Result<FlowEnvelope> {
        let input = inputs
            .first()
            .ok_or_else(|| anyhow!("rag_judge: brak krawędzi wejściowej"))?;
        let mut out: FlowEnvelope = (*input.envelope).clone();

        let raw = out.payload.as_text().unwrap_or_default().to_string();
        let verdict = parse_judge_verdict(&raw);

        // Pod-pytanie sędziego przycinamy do twardego capa (char-boundary-safe):
        // prompt-injection mógłby wstrzyknąć ogromne `next_query` idące do
        // embed/search. Po przycięciu pusty string → brak sensownego pod-pytania.
        let next_query = verdict.next_query.and_then(|next| {
            let capped: String = next.chars().take(MAX_NEXT_QUERY_CHARS).collect();
            let capped = capped.trim().to_string();
            (!capped.is_empty()).then_some(capped)
        });

        match next_query {
            // Kolejny hop: ustaw pod-pytanie; harness_done pozostaje nieustawione
            // (loop until = false), więc pętla wykona następną iterację.
            Some(next) if !verdict.enough => {
                out.meta
                    .insert(META_CURRENT_QUERY.to_string(), Value::String(next));
                out.meta.remove(META_HARNESS_DONE);
            }
            // Starczy (albo sędzia nie podał pod-pytania) → zakończ pętlę. Bez
            // next_query nie ma czego dalej szukać, więc też kończymy — inaczej
            // kolejny hop powtórzyłby identyczne zapytanie (jałowy obrót).
            _ => {
                out.meta
                    .insert(META_HARNESS_DONE.to_string(), Value::Bool(true));
            }
        }
        Ok(out)
    }
}

// =============================================================================
// rag_finalize — po pętli: zbuduj payload-kontekst dla finalnego LLM i przepnij
// zakumulowane pasaże na rag_citations, żeby output je wyemitował
// =============================================================================

pub struct RagFinalizeNodeAdapter;

impl RagFinalizeNodeAdapter {
    pub fn new() -> Self {
        Self
    }
}

impl Default for RagFinalizeNodeAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl NodeAdapter for RagFinalizeNodeAdapter {
    fn node_type(&self) -> &str {
        "rag_finalize"
    }
    fn input_ports(&self) -> Vec<PortSpec> {
        vec![PortSpec::new("in", FlowDataType::Any)]
    }
    fn output_ports(&self) -> Vec<PortSpec> {
        vec![PortSpec::new("full", FlowDataType::Text)]
    }

    async fn execute(
        &self,
        _node: &FlowNode,
        inputs: &[NodeInput],
        _ctx: &ExecutionContext,
    ) -> Result<FlowEnvelope> {
        let input = inputs
            .first()
            .ok_or_else(|| anyhow!("rag_finalize: brak krawędzi wejściowej"))?;
        let mut out: FlowEnvelope = (*input.envelope).clone();

        // Po pętli payload to werdykt sędziego ostatniego hopu — odbudowujemy
        // payload jako kontekst (oryginalne pytanie + WSZYSTKIE zakumulowane
        // pasaże) dla finalnego LLM, który ma na ich podstawie odpowiedzieć.
        let accumulated = passages_from_meta(&out, META_ACCUMULATED);
        let question = out
            .meta
            .get(META_ORIGINAL_QUESTION)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        // GraphRAG (E3.2): finalny kontekst FUZUJE pasaże wektorowe z faktami
        // grafowymi ostatniego hopu (meta.rag_graph_facts), żeby finalny LLM —
        // tak jak sędzia — widział OBA źródła. Brak faktów → sam kontekst pasaży.
        let vector_context = build_context_text(&question, &accumulated);
        let graph_facts = out
            .meta
            .get(crate::flow_engine::node_adapters::rag_graphrag::META_GRAPH_FACTS)
            .and_then(|v| v.as_str())
            .unwrap_or("");
        out.payload = FlowValue::Text(
            crate::flow_engine::node_adapters::rag_graphrag::fuse_context(
                &vector_context,
                graph_facts,
            ),
        );

        // Cytaty finalnej odpowiedzi = WSZYSTKIE zakumulowane pasaże (dedup, top
        // wg score) — przepinamy je na rag_citations, bo output emituje stamtąd.
        out.meta.insert(
            META_CITATIONS.to_string(),
            Value::Array(accumulated),
        );
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow_engine::node_adapter::test_support::stub_ctx;
    use serde_json::json;
    use std::sync::Arc;

    fn node(node_type: &str) -> FlowNode {
        FlowNode {
            id: format!("{node_type}-1"),
            node_type: node_type.into(),
            config: serde_json::Value::Null,
            position: None,
            label: None,
            region: None,
        }
    }

    fn input(env: FlowEnvelope) -> NodeInput {
        NodeInput {
            from_node_id: "prev".into(),
            from_port: "full".into(),
            envelope: Arc::new(env),
        }
    }

    // --- merge_accumulated (dedup + cap) -----------------------------------

    #[test]
    fn merge_dedups_by_doc_and_chunk_keeping_best_score() {
        let existing = vec![json!({"doc_id": "d1", "chunk_index": 0, "text": "a", "score": 0.2})];
        // Ten sam (d1,0) z wyższym score nadpisuje; (d1,1) to nowy pasaż.
        let incoming = vec![
            json!({"doc_id": "d1", "chunk_index": 0, "text": "a-better", "score": 0.9}),
            json!({"doc_id": "d1", "chunk_index": 1, "text": "b", "score": 0.5}),
        ];
        let merged = merge_accumulated(&existing, &incoming);
        assert_eq!(merged.len(), 2, "dedup po (doc_id,chunk_index)");
        // Najlepszy score pierwszy; (d1,0) zachował WYŻSZY score (0.9) i jego tekst.
        assert_eq!(merged[0]["doc_id"], "d1");
        assert_eq!(merged[0]["chunk_index"].as_i64(), Some(0));
        assert_eq!(merged[0]["score"].as_f64(), Some(0.9));
        assert_eq!(merged[0]["text"], "a-better");
        assert_eq!(merged[1]["chunk_index"].as_i64(), Some(1));
    }

    #[test]
    fn merge_keeps_existing_when_incoming_score_is_lower() {
        let existing = vec![json!({"doc_id": "d", "chunk_index": 0, "text": "good", "score": 0.8})];
        let incoming = vec![json!({"doc_id": "d", "chunk_index": 0, "text": "worse", "score": 0.1})];
        let merged = merge_accumulated(&existing, &incoming);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0]["score"].as_f64(), Some(0.8));
        assert_eq!(merged[0]["text"], "good");
    }

    #[test]
    fn merge_caps_total_at_max_accumulated() {
        let incoming: Vec<Value> = (0..(MAX_ACCUMULATED + 10))
            .map(|i| json!({"doc_id": "d", "chunk_index": i, "text": "t", "score": i as f64}))
            .collect();
        let merged = merge_accumulated(&[], &incoming);
        assert_eq!(merged.len(), MAX_ACCUMULATED, "cap całkowitej liczby pasaży");
        // Top po score — najwyższy chunk_index (= najwyższy score) pierwszy.
        assert_eq!(
            merged[0]["chunk_index"].as_i64(),
            Some((MAX_ACCUMULATED + 10 - 1) as i64)
        );
    }

    #[test]
    fn merge_skips_passages_without_text() {
        let incoming = vec![
            json!({"doc_id": "d", "chunk_index": 0, "score": 0.9}),
            json!({"doc_id": "d", "chunk_index": 1, "text": "ok", "score": 0.5}),
        ];
        let merged = merge_accumulated(&[], &incoming);
        assert_eq!(merged.len(), 1, "pasaż bez text pominięty");
        assert_eq!(merged[0]["text"], "ok");
    }

    // --- parse_judge_verdict -----------------------------------------------

    #[test]
    fn judge_parses_enough_true() {
        let v = parse_judge_verdict(r#"{"enough": true}"#);
        assert!(v.enough);
        assert!(v.next_query.is_none());
    }

    #[test]
    fn judge_parses_next_query_when_not_enough() {
        let v = parse_judge_verdict(r#"{"enough": false, "next_query": "kto był prezesem?"}"#);
        assert!(!v.enough);
        assert_eq!(v.next_query.as_deref(), Some("kto był prezesem?"));
    }

    #[test]
    fn judge_extracts_json_wrapped_in_prose() {
        let v = parse_judge_verdict(
            "Oto mój werdykt:\n```json\n{\"enough\": false, \"next_query\": \"X\"}\n```\nGotowe.",
        );
        assert!(!v.enough);
        assert_eq!(v.next_query.as_deref(), Some("X"));
    }

    #[test]
    fn judge_defaults_to_enough_on_unparseable() {
        // Brak JSON → bezpieczny default „starczy" (kończymy pętlę).
        let v = parse_judge_verdict("nie wiem, może wystarczy");
        assert!(v.enough);
        assert!(v.next_query.is_none());
    }

    #[test]
    fn judge_blank_next_query_is_none() {
        let v = parse_judge_verdict(r#"{"enough": false, "next_query": "   "}"#);
        assert!(!v.enough);
        assert!(v.next_query.is_none(), "pusty next_query → None");
    }

    // --- rag_query_seed -----------------------------------------------------

    #[tokio::test]
    async fn query_seed_first_hop_uses_payload_and_stashes_original() {
        let mut env = FlowEnvelope::empty();
        env.payload = FlowValue::Text("oryginalne pytanie".into());
        let out = RagQuerySeedNodeAdapter::new()
            .execute(&node("rag_query_seed"), &[input(env)], &stub_ctx())
            .await
            .unwrap();
        assert_eq!(out.payload.as_text(), Some("oryginalne pytanie"));
        assert_eq!(
            out.meta.get(META_ORIGINAL_QUESTION).and_then(|v| v.as_str()),
            Some("oryginalne pytanie")
        );
        assert_eq!(
            out.meta.get(META_CURRENT_QUERY).and_then(|v| v.as_str()),
            Some("oryginalne pytanie")
        );
    }

    #[tokio::test]
    async fn query_seed_later_hop_uses_meta_current_query() {
        let mut env = FlowEnvelope::empty();
        // Drugi hop: payload to śmieci po poprzednim output, ale meta niesie
        // pod-pytanie i oryginalne pytanie.
        env.payload = FlowValue::Text("stary output".into());
        env.meta.insert(
            META_ORIGINAL_QUESTION.into(),
            json!("oryginalne pytanie"),
        );
        env.meta
            .insert(META_CURRENT_QUERY.into(), json!("pod-pytanie hop 2"));
        let out = RagQuerySeedNodeAdapter::new()
            .execute(&node("rag_query_seed"), &[input(env)], &stub_ctx())
            .await
            .unwrap();
        assert_eq!(out.payload.as_text(), Some("pod-pytanie hop 2"));
        // Oryginalne pytanie NIE zostaje nadpisane pod-pytaniem.
        assert_eq!(
            out.meta.get(META_ORIGINAL_QUESTION).and_then(|v| v.as_str()),
            Some("oryginalne pytanie")
        );
    }

    // --- rag_accumulate -----------------------------------------------------

    #[tokio::test]
    async fn accumulate_grows_across_hops_via_meta() {
        // Hop 1: rag_citations = [d1#0]; brak rag_accumulated.
        let mut env = FlowEnvelope::empty();
        env.meta.insert(META_ORIGINAL_QUESTION.into(), json!("Q"));
        env.meta.insert(
            META_CITATIONS.into(),
            json!([{"doc_id": "d1", "chunk_index": 0, "text": "pasaz1", "score": 0.7}]),
        );
        let out1 = RagAccumulateNodeAdapter::new()
            .execute(&node("rag_accumulate"), &[input(env)], &stub_ctx())
            .await
            .unwrap();
        let acc1 = out1.meta.get(META_ACCUMULATED).and_then(|v| v.as_array()).unwrap();
        assert_eq!(acc1.len(), 1);

        // Hop 2: meta z hopu 1 (akumulacja przetrwała) + nowy cytat [d2#0].
        let mut env2 = out1;
        env2.meta.insert(
            META_CITATIONS.into(),
            json!([{"doc_id": "d2", "chunk_index": 0, "text": "pasaz2", "score": 0.9}]),
        );
        let out2 = RagAccumulateNodeAdapter::new()
            .execute(&node("rag_accumulate"), &[input(env2)], &stub_ctx())
            .await
            .unwrap();
        let acc2 = out2.meta.get(META_ACCUMULATED).and_then(|v| v.as_array()).unwrap();
        assert_eq!(acc2.len(), 2, "akumulacja rośnie między hopami");
        // Payload to kontekst dla sędziego — zawiera pytanie i oba pasaże.
        let ctx_text = out2.payload.as_text().unwrap();
        assert!(ctx_text.contains("Pytanie: Q"));
        assert!(ctx_text.contains("pasaz1") && ctx_text.contains("pasaz2"));
    }

    // --- rag_judge ----------------------------------------------------------

    #[tokio::test]
    async fn judge_sets_harness_done_when_enough() {
        let mut env = FlowEnvelope::empty();
        env.payload = FlowValue::Text(r#"{"enough": true}"#.into());
        let out = RagJudgeNodeAdapter::new()
            .execute(&node("rag_judge"), &[input(env)], &stub_ctx())
            .await
            .unwrap();
        assert_eq!(
            out.meta.get(META_HARNESS_DONE).and_then(|v| v.as_bool()),
            Some(true)
        );
    }

    #[tokio::test]
    async fn judge_sets_next_query_when_not_enough() {
        let mut env = FlowEnvelope::empty();
        env.payload =
            FlowValue::Text(r#"{"enough": false, "next_query": "dalej"}"#.into());
        let out = RagJudgeNodeAdapter::new()
            .execute(&node("rag_judge"), &[input(env)], &stub_ctx())
            .await
            .unwrap();
        assert!(out.meta.get(META_HARNESS_DONE).is_none(), "pętla trwa");
        assert_eq!(
            out.meta.get(META_CURRENT_QUERY).and_then(|v| v.as_str()),
            Some("dalej")
        );
    }

    #[tokio::test]
    async fn judge_caps_next_query_length() {
        // Sędzia (potencjalnie prompt-injected) zwraca olbrzymie pod-pytanie —
        // adapter przycina je do MAX_NEXT_QUERY_CHARS przed zapisem do meta.
        let long = "a".repeat(MAX_NEXT_QUERY_CHARS + 500);
        let mut env = FlowEnvelope::empty();
        env.payload = FlowValue::Text(
            json!({"enough": false, "next_query": long}).to_string(),
        );
        let out = RagJudgeNodeAdapter::new()
            .execute(&node("rag_judge"), &[input(env)], &stub_ctx())
            .await
            .unwrap();
        assert!(out.meta.get(META_HARNESS_DONE).is_none(), "pętla trwa");
        let next = out
            .meta
            .get(META_CURRENT_QUERY)
            .and_then(|v| v.as_str())
            .unwrap();
        assert_eq!(
            next.chars().count(),
            MAX_NEXT_QUERY_CHARS,
            "next_query przycięty do twardego capa"
        );
    }

    #[tokio::test]
    async fn judge_empty_next_query_ends_loop() {
        // Brak sensownego pod-pytania (po trim puste) → kończymy pętlę zamiast
        // odpalać kolejny hop z pustym zapytaniem.
        let mut env = FlowEnvelope::empty();
        env.payload =
            FlowValue::Text(r#"{"enough": false, "next_query": "   "}"#.into());
        let out = RagJudgeNodeAdapter::new()
            .execute(&node("rag_judge"), &[input(env)], &stub_ctx())
            .await
            .unwrap();
        assert_eq!(
            out.meta.get(META_HARNESS_DONE).and_then(|v| v.as_bool()),
            Some(true),
            "pusty next_query → harness_done"
        );
        assert!(
            out.meta.get(META_CURRENT_QUERY).is_none(),
            "brak pod-pytania w meta"
        );
    }

    // --- rag_finalize -------------------------------------------------------

    #[tokio::test]
    async fn finalize_promotes_accumulated_to_citations_and_builds_context() {
        let mut env = FlowEnvelope::empty();
        env.meta.insert(META_ORIGINAL_QUESTION.into(), json!("Q"));
        env.meta.insert(
            META_ACCUMULATED.into(),
            json!([
                {"doc_id": "d1", "chunk_index": 0, "text": "p1", "score": 0.9},
                {"doc_id": "d2", "chunk_index": 1, "text": "p2", "score": 0.5}
            ]),
        );
        // Payload to werdykt sędziego — finalize go nadpisuje kontekstem.
        env.payload = FlowValue::Text(r#"{"enough": true}"#.into());
        let out = RagFinalizeNodeAdapter::new()
            .execute(&node("rag_finalize"), &[input(env)], &stub_ctx())
            .await
            .unwrap();
        // rag_citations = zakumulowane pasaże (output je wyemituje).
        let cites = out.meta.get(META_CITATIONS).and_then(|v| v.as_array()).unwrap();
        assert_eq!(cites.len(), 2);
        assert_eq!(cites[0]["doc_id"], "d1");
        // Payload to kontekst dla finalnego LLM (pytanie + pasaże).
        let ctx_text = out.payload.as_text().unwrap();
        assert!(ctx_text.contains("Pytanie: Q"));
        assert!(ctx_text.contains("p1") && ctx_text.contains("p2"));
    }

    // --- walidacja flow JSON (R1-R10) --------------------------------------

    /// Oba flow RAG (outer query + body retrieval_round) muszą przejść pełną
    /// walidację R1–R10 z rejestrem wszystkich adapterów (w tym nowych węzłów
    /// multi-hop). To strażnik kontraktu: flow nie wejdzie do produkcji
    /// (register_engine_flows woła tę samą `validate`), jeśli kształt jest zły.
    fn validate_flow_json(json: &str) -> Result<(), crate::flow_engine::validation::FlowValidationError> {
        let def: crate::flow_engine::types::FlowDefinition =
            serde_json::from_str(json).expect("flow JSON parsuje się do FlowDefinition");
        let registry = crate::flow_engine::dispatcher::build_registry_for_test();
        crate::flow_engine::validation::validate(&def, &registry)
    }

    #[test]
    fn outer_query_flow_validates() {
        let json = include_str!("../../../addons/rag/flows/query.flow.json");
        validate_flow_json(json).expect("outer query.flow.json musi przejść R1-R10");
    }

    #[test]
    fn body_retrieval_round_flow_validates() {
        let json = include_str!("../../../addons/rag/flows/retrieval_round.flow.json");
        validate_flow_json(json).expect("body retrieval_round.flow.json musi przejść R1-R10");
    }

    /// Outer flow naprawdę używa loop block z `body_flow_engine_id=retrieval_round`
    /// i `max_iterations` <= twardego capu (anti-DoS). Asercja na realnej
    /// konfiguracji, nie na opisie.
    #[test]
    fn outer_flow_loop_config_is_capped_multihop() {
        let json = include_str!("../../../addons/rag/flows/query.flow.json");
        let def: crate::flow_engine::types::FlowDefinition =
            serde_json::from_str(json).unwrap();
        let loop_node = def
            .nodes
            .iter()
            .find(|n| n.node_type == "loop")
            .expect("outer flow ma węzeł loop");
        assert_eq!(
            loop_node.config.get("body_flow_engine_id").and_then(|v| v.as_str()),
            Some("retrieval-round"),
            "loop wskazuje na body retrieval-round przez engine-flow id"
        );
        let max_iter = loop_node
            .config
            .get("max_iterations")
            .and_then(|v| v.as_u64())
            .expect("loop ma max_iterations");
        assert!(max_iter >= 1 && max_iter <= 4, "max_iterations capnięte do <=4: {max_iter}");
        assert_eq!(
            loop_node.config.get("until").and_then(|v| v.as_str()),
            Some("has(meta.harness_done) && meta.harness_done == true"),
            "until czyta sygnał harness_done sędziego"
        );
    }

    #[tokio::test]
    async fn judge_ends_loop_when_not_enough_but_no_next_query() {
        // „not enough" bez next_query nie może pętlić w kółko — kończymy.
        let mut env = FlowEnvelope::empty();
        env.payload = FlowValue::Text(r#"{"enough": false}"#.into());
        let out = RagJudgeNodeAdapter::new()
            .execute(&node("rag_judge"), &[input(env)], &stub_ctx())
            .await
            .unwrap();
        assert_eq!(
            out.meta.get(META_HARNESS_DONE).and_then(|v| v.as_bool()),
            Some(true)
        );
    }
}
