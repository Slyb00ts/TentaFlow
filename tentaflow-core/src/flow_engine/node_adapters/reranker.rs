// =============================================================================
// Plik: flow_engine/node_adapters/reranker.rs
// Opis: RerankerNodeAdapter — krok retrievalu RAG między vector-search a LLM.
//       Wejście Json{query, candidates:[{id,text}]} → wyjście Json{ranked:
//       [{id,score,text}]} posortowane malejąco po score, ucięte do top_n.
//       Dispatch przez ctx.reranker (alias `rag-reranker`, /v1/rerank).
// =============================================================================

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use tracing::warn;

use crate::flow_engine::dispatchers::RerankRequest;
use crate::flow_engine::envelope::{FlowEnvelope, FlowValue, NodeInput};
use crate::flow_engine::node_adapter::{ExecutionContext, NodeAdapter, PortSpec};
use crate::flow_engine::types::{FlowDataType, FlowNode};

const NODE_TYPE: &str = "reranker";

/// Domyślny alias modelu rerankera (rozwiązywany przez resolver aliasów A1).
const DEFAULT_MODEL_ALIAS: &str = "rag-reranker";

/// Twardy cap liczby kandydatów wysyłanych do cross-encodera (plan §8 — kontrola
/// wąskiego gardła). Vector-search może zwrócić tysiące kandydatów; reranker
/// jest drogi, więc obcinamy wejście zanim trafi do modelu.
const MAX_CANDIDATES: usize = 200;

/// Pojedynczy kandydat z portu wejściowego — `id` zachowywany 1:1 na wyjściu,
/// `text` to treść pasaża wysyłana do rerankera. `doc_id`/`chunk_index` są
/// opcjonalne (obecne tylko dla wejścia vector-hits RAG); niosą je tu, żeby
/// po rerankingu odtworzyć cytaty w meta["rag_citations"] w nowej kolejności.
/// `vector_score` to surowy score z vector-search (0.0 poza ścieżką RAG) —
/// służy do degradacji do kolejności wektorowej gdy reranker jest niedostępny.
struct Candidate {
    id: String,
    text: String,
    doc_id: serde_json::Value,
    chunk_index: serde_json::Value,
    vector_score: f32,
}

/// Kształt wejścia rozpoznany w `parse_input`. RAG vector-hits ma fallback
/// do kolejności wektorowej (hity niosą `score`); czysty kontrakt
/// `{query,candidates}` go nie ma (brak vector score) — degradacja nie dotyczy.
#[derive(Clone, Copy, PartialEq, Eq)]
enum InputShape {
    Candidates,
    VectorHits,
}

pub struct RerankerNodeAdapter;

impl RerankerNodeAdapter {
    pub fn new() -> Self {
        Self
    }

    fn pick_model(node: &FlowNode, envelope: &FlowEnvelope) -> String {
        if let Some(m) = node
            .config
            .get("model")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            return m.to_string();
        }
        if let Some(m) = envelope
            .meta
            .get("rerank_model")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            return m.to_string();
        }
        DEFAULT_MODEL_ALIAS.to_string()
    }

    /// Wyciąga `query` + listę kandydatów z payload Json. Adapter toleruje DWA
    /// kształty wejścia:
    ///   1. kontrakt rerankera `{query, candidates:[{id,text}]}` — query bierze
    ///      z payloadu (`query`),
    ///   2. RAG vector-hits `{hits:[{ref_id, score, fields:{text,doc_id,
    ///      chunk_index}}]}` — query bierze z `meta["rag_query_text"]` (tekst
    ///      pytania zachowany przez embeddings node, bo po embeddingu payload to
    ///      wektor, nie tekst), a hity mapuje na kandydatów `{id:ref_id,
    ///      text:fields.text}` + niesie doc_id/chunk_index do odtworzenia cytatów.
    /// Wybór kształtu jest jawny po obecności klucza `candidates`/`hits` — bez
    /// zgadywania. `id` opcjonalny w kontrakcie #1 (fallback do pozycji).
    /// Obecność OBU kluczy jednocześnie to niejednoznaczność (realny vector
    /// output nie ma `candidates`) → czytelny błąd zamiast cichego wyboru.
    fn parse_input(envelope: &FlowEnvelope) -> Result<(String, Vec<Candidate>, InputShape)> {
        let obj = match &envelope.payload {
            FlowValue::Json(v) => v,
            other => {
                return Err(anyhow!(
                    "reranker adapter: payload must be Json, got {}",
                    other.kind()
                ))
            }
        };

        let has_candidates = obj.get("candidates").is_some();
        let has_hits = obj.get("hits").is_some();
        if has_candidates && has_hits {
            return Err(anyhow!(
                "reranker adapter: ambiguous reranker input (both candidates and hits)"
            ));
        }
        if has_candidates {
            let (query, candidates) = Self::parse_candidates_input(obj)?;
            return Ok((query, candidates, InputShape::Candidates));
        }
        if has_hits {
            let (query, candidates) = Self::parse_vector_hits_input(obj, envelope)?;
            return Ok((query, candidates, InputShape::VectorHits));
        }
        Err(anyhow!(
            "reranker adapter: payload Json must carry 'candidates' (\\{{query,candidates\\}}) \
             or 'hits' (vector-search output)"
        ))
    }

    /// Kontrakt #1: `{query, candidates:[{id,text}]}`.
    fn parse_candidates_input(
        obj: &serde_json::Value,
    ) -> Result<(String, Vec<Candidate>)> {
        let query = obj
            .get("query")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow!("reranker adapter: missing non-empty 'query' in payload"))?
            .to_string();

        let arr = obj
            .get("candidates")
            .and_then(|v| v.as_array())
            .ok_or_else(|| anyhow!("reranker adapter: missing 'candidates' array in payload"))?;

        let mut candidates = Vec::with_capacity(arr.len());
        for (pos, item) in arr.iter().enumerate() {
            let text = item
                .get("text")
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    anyhow!("reranker adapter: candidate[{pos}] missing string 'text'")
                })?
                .to_string();
            let id = item
                .get("id")
                .and_then(|v| v.as_str().map(|s| s.to_string()).or_else(|| v.as_u64().map(|n| n.to_string())))
                .unwrap_or_else(|| pos.to_string());
            candidates.push(Candidate {
                id,
                text,
                doc_id: serde_json::Value::Null,
                chunk_index: serde_json::Value::Null,
                vector_score: 0.0,
            });
        }
        Ok((query, candidates))
    }

    /// Kontrakt #2: RAG vector-hits `{hits:[{ref_id, fields:{text,...}}]}` +
    /// query z `meta["rag_query_text"]`. Hity bez `fields.text` są pomijane
    /// (nie da się ich zrerankować ani zacytować), nie wywracają węzła.
    fn parse_vector_hits_input(
        obj: &serde_json::Value,
        envelope: &FlowEnvelope,
    ) -> Result<(String, Vec<Candidate>)> {
        let query = envelope
            .meta
            .get("rag_query_text")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                anyhow!(
                    "reranker adapter: vector-hits input wymaga query w meta['rag_query_text'] \
                     (tekst pytania stashowany przez embeddings node)"
                )
            })?
            .to_string();

        let arr = obj
            .get("hits")
            .and_then(|v| v.as_array())
            .ok_or_else(|| anyhow!("reranker adapter: 'hits' must be an array"))?;

        let mut candidates = Vec::with_capacity(arr.len());
        for hit in arr {
            let fields = hit.get("fields");
            let Some(text) = fields
                .and_then(|f| f.get("text"))
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
            else {
                continue;
            };
            let id = hit
                .get("ref_id")
                .and_then(|v| v.as_u64().map(|n| n.to_string()).or_else(|| v.as_str().map(|s| s.to_string())))
                .unwrap_or_else(|| text.to_string());
            let doc_id = fields
                .and_then(|f| f.get("doc_id"))
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            let chunk_index = fields
                .and_then(|f| f.get("chunk_index"))
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            let vector_score = hit
                .get("score")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0) as f32;
            candidates.push(Candidate {
                id,
                text: text.to_string(),
                doc_id,
                chunk_index,
                vector_score,
            });
        }
        Ok((query, candidates))
    }

    /// Bug 2 (degradacja RAG) — gdy reranker niedostępny, buduje wynik z
    /// kandydatów posortowanych malejąco po vector score, ucięty do top_n.
    /// Emituje `{ranked:[{id,score,text}]}` ze score=vector_score oraz
    /// nadpisuje meta["rag_citations"] (z vector score) — RAG odpowiada dalej.
    /// Wołane WYŁĄCZNIE na ścieżce vector-hits (gdzie vector_score jest realny).
    fn degrade_to_vector_order(
        mut out: FlowEnvelope,
        mut candidates: Vec<Candidate>,
        top_n: Option<usize>,
    ) -> FlowEnvelope {
        candidates.sort_by(|a, b| b.vector_score.total_cmp(&a.vector_score));
        if let Some(n) = top_n {
            candidates.truncate(n);
        }

        let ranked_json: Vec<serde_json::Value> = candidates
            .iter()
            .map(|c| {
                serde_json::json!({
                    "id": c.id,
                    "score": c.vector_score,
                    "text": c.text,
                })
            })
            .collect();

        let citations: Vec<serde_json::Value> = candidates
            .iter()
            .map(|c| {
                serde_json::json!({
                    "doc_id": c.doc_id,
                    "chunk_index": c.chunk_index,
                    "text": c.text,
                    "score": c.vector_score,
                })
            })
            .collect();
        out.meta.insert(
            "rag_citations".to_string(),
            serde_json::Value::Array(citations),
        );

        out.payload = FlowValue::Json(serde_json::json!({ "ranked": ranked_json }));
        out
    }
}

impl Default for RerankerNodeAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl NodeAdapter for RerankerNodeAdapter {
    fn node_type(&self) -> &str {
        NODE_TYPE
    }
    fn input_ports(&self) -> Vec<PortSpec> {
        vec![PortSpec::new("in", FlowDataType::Json)]
    }
    fn output_ports(&self) -> Vec<PortSpec> {
        vec![PortSpec::new("full", FlowDataType::Json)]
    }

    async fn execute(
        &self,
        node: &FlowNode,
        inputs: &[NodeInput],
        ctx: &ExecutionContext,
    ) -> Result<FlowEnvelope> {
        let input = inputs
            .first()
            .ok_or_else(|| anyhow!("reranker adapter: missing input edge"))?;
        let envelope = &input.envelope;

        let model = Self::pick_model(node, envelope);
        let (query, mut candidates, shape) = Self::parse_input(envelope)?;

        // top_n: node.config -> envelope.meta. Bez wartości = wszystkie.
        let top_n = node
            .config
            .get("top_n")
            .and_then(|v| v.as_u64())
            .or_else(|| envelope.meta.get("top_n").and_then(|v| v.as_u64()))
            .map(|n| n as usize);

        let mut out: FlowEnvelope = (**envelope).clone();
        let is_rag = shape == InputShape::VectorHits;

        // Pusty zestaw kandydatów → pusty wynik (NIE błąd, NIE panic) — flow
        // retrievalu może legalnie nie znaleźć nic do zrangowania. Inwariant
        // (bug 1): na ścieżce RAG cytaty ZAWSZE odzwierciedlają to co realnie
        // poszło do LLM. Brak kandydatów = zero cytatów → wyczyść stare
        // vector-hity stashowane w meta przez vector node (inaczej output
        // emitowałby nierankowane cytaty mimo pustego kontekstu).
        if candidates.is_empty() {
            if is_rag {
                out.meta.insert(
                    "rag_citations".to_string(),
                    serde_json::Value::Array(Vec::new()),
                );
            }
            out.payload = FlowValue::Json(serde_json::json!({ "ranked": [] }));
            return Ok(out);
        }

        // Cap kandydatów PRZED dispatchem (kontrola wąskiego gardła §8).
        if candidates.len() > MAX_CANDIDATES {
            candidates.truncate(MAX_CANDIDATES);
        }

        // Cap top_n do liczby dostępnych kandydatów — backend nie powinien
        // zwrócić więcej wpisów niż dokumentów.
        let effective_top_n = top_n.map(|n| n.min(candidates.len()) as u32);

        let documents: Vec<String> = candidates.iter().map(|c| c.text.clone()).collect();
        let req = RerankRequest {
            model,
            query,
            documents,
            top_n: effective_top_n,
            user_id: ctx.user_id.clone(),
            user_role: ctx.user_role.clone(),
            flow_depth: ctx.subflow_depth,
        };

        // Bug 2 (resilience): gdy dispatch rerankera zawiedzie (wszystkie
        // targety failover `rag-reranker` padły) i jesteśmy na ścieżce RAG,
        // degradujemy do kolejności vector-score (hity są już vector-scored) —
        // RAG ma odpowiadać także bez rerankera. Czysty kontrakt
        // {query,candidates} nie ma vector score, więc tam błąd jak dotąd.
        let response = match ctx.reranker.rerank(req).await {
            Ok(resp) => resp,
            Err(e) if is_rag => {
                warn!("reranker niedostępny, degradacja do vector order: {e}");
                return Ok(Self::degrade_to_vector_order(out, candidates, top_n));
            }
            Err(e) => return Err(anyhow!("reranker adapter: dispatcher failed: {e}")),
        };

        ctx.usage_sink.record(&node.id, response.usage);

        // Mapuj index → kandydat (id+text). Dispatcher gwarantuje malejące
        // sortowanie, ale i tak sortujemy obronnie po score; index spoza
        // zakresu to błąd kontraktu backendu.
        let mut ranked: Vec<(f32, &Candidate)> = Vec::with_capacity(response.results.len());
        for r in &response.results {
            let cand = candidates.get(r.index).ok_or_else(|| {
                anyhow!(
                    "reranker adapter: backend index {} out of range ({} candidates)",
                    r.index,
                    candidates.len()
                )
            })?;
            ranked.push((r.score, cand));
        }
        ranked.sort_by(|a, b| b.0.total_cmp(&a.0));

        // Ucięcie do top_n na wypadek gdyby backend zignorował parametr.
        if let Some(n) = top_n {
            ranked.truncate(n);
        }

        let ranked_json: Vec<serde_json::Value> = ranked
            .iter()
            .map(|(score, cand)| {
                serde_json::json!({
                    "id": cand.id,
                    "score": score,
                    "text": cand.text,
                })
            })
            .collect();

        // RAG E2.1 — cytaty mają teraz odzwierciedlać RERANKOWANĄ kolejność i
        // score cross-encodera (lepszy sygnał niż surowy dystans wektorowy).
        // Inwariant (bug 1): na ścieżce vector-hits (RAG) ZAWSZE nadpisujemy
        // meta["rag_citations"] tym, co realnie poszło do LLM — także pustą
        // listą gdy reranker zwrócił 0 wyników. Inaczej output emitowałby
        // STARE, nierankowane vector-hity stashowane przez vector node.
        // Dla czystego kontraktu {query,candidates} nie dotykamy cytatów.
        if is_rag {
            let citations: Vec<serde_json::Value> = ranked
                .iter()
                .map(|(score, cand)| {
                    serde_json::json!({
                        "doc_id": cand.doc_id,
                        "chunk_index": cand.chunk_index,
                        "text": cand.text,
                        "score": score,
                    })
                })
                .collect();
            out.meta.insert(
                "rag_citations".to_string(),
                serde_json::Value::Array(citations),
            );
        }

        out.payload = FlowValue::Json(serde_json::json!({ "ranked": ranked_json }));
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow_engine::dispatchers::{
        RerankDispatcher, RerankRequest, RerankResponse, RerankResult,
    };
    use crate::flow_engine::envelope::TokenUsage;
    use crate::flow_engine::node_adapter::test_support::stub_ctx;
    use async_trait::async_trait;
    use serde_json::json;
    use std::sync::{Arc, Mutex};

    fn node(config: serde_json::Value) -> FlowNode {
        FlowNode {
            id: "r1".into(),
            node_type: NODE_TYPE.into(),
            config,
            position: None,
            label: None,
            region: None,
        }
    }

    fn input(envelope: FlowEnvelope) -> NodeInput {
        NodeInput {
            from_node_id: "trigger".into(),
            from_port: "full".into(),
            envelope: Arc::new(envelope),
        }
    }

    /// Reranker który przypisuje score = długość tekstu (krótszy = wyżej gdy
    /// odwrócimy). Zwraca wyniki w kolejności WEJŚCIOWEJ (out-of-order po
    /// score) — adapter musi sam posortować malejąco.
    struct LenReranker {
        last_doc_count: Mutex<usize>,
        last_top_n: Mutex<Option<u32>>,
        last_flow_depth: Mutex<u8>,
    }

    #[async_trait]
    impl RerankDispatcher for LenReranker {
        async fn rerank(&self, req: RerankRequest) -> Result<RerankResponse> {
            *self.last_doc_count.lock().unwrap() = req.documents.len();
            *self.last_top_n.lock().unwrap() = req.top_n;
            *self.last_flow_depth.lock().unwrap() = req.flow_depth;
            // Score = liczba znaków; zwracamy w kolejności wejściowej.
            let results = req
                .documents
                .iter()
                .enumerate()
                .map(|(i, d)| RerankResult {
                    index: i,
                    score: d.len() as f32,
                })
                .collect();
            Ok(RerankResponse {
                results,
                usage: TokenUsage::default(),
            })
        }
    }

    fn payload(query: &str, candidates: serde_json::Value) -> FlowEnvelope {
        let mut env = FlowEnvelope::empty();
        env.payload = FlowValue::Json(json!({ "query": query, "candidates": candidates }));
        env
    }

    /// RAG E2.1 — envelope w kształcie vector-hits (output vector node) z
    /// query w meta["rag_query_text"] (stashowanym przez embeddings node).
    fn vector_hits_env(query: &str, hits: serde_json::Value) -> FlowEnvelope {
        let mut env = FlowEnvelope::empty();
        env.payload = FlowValue::Json(json!({
            "op": "search",
            "namespace": "passages",
            "hits": hits,
        }));
        env.meta.insert(
            "rag_query_text".into(),
            serde_json::Value::String(query.to_string()),
        );
        env
    }

    #[tokio::test]
    async fn sorts_descending_by_score_and_preserves_id_text() {
        let env = payload(
            "q",
            json!([
                {"id": "a", "text": "short"},
                {"id": "b", "text": "much longer passage"},
                {"id": "c", "text": "mid len"},
            ]),
        );
        let mut ctx = stub_ctx();
        ctx.reranker = Arc::new(LenReranker {
            last_doc_count: Mutex::new(0),
            last_top_n: Mutex::new(None),
            last_flow_depth: Mutex::new(0),
        });

        let out = RerankerNodeAdapter::new()
            .execute(&node(json!({"model": "rag-reranker"})), &[input(env)], &ctx)
            .await
            .unwrap();

        let ranked = match out.payload {
            FlowValue::Json(v) => v.get("ranked").and_then(|r| r.as_array()).cloned().unwrap(),
            other => panic!("expected Json, got {other:?}"),
        };
        assert_eq!(ranked.len(), 3);
        // Najdłuższy tekst ma najwyższy score → "b" pierwszy.
        assert_eq!(ranked[0]["id"], "b");
        assert_eq!(ranked[0]["text"], "much longer passage");
        // Malejąco: b > c > a.
        let scores: Vec<f64> = ranked
            .iter()
            .map(|r| r["score"].as_f64().unwrap())
            .collect();
        assert!(scores[0] >= scores[1] && scores[1] >= scores[2]);
        assert_eq!(ranked[2]["id"], "a");
    }

    #[tokio::test]
    async fn truncates_to_top_n() {
        let env = payload(
            "q",
            json!([
                {"id": "a", "text": "aaaa"},
                {"id": "b", "text": "bbbbbbbb"},
                {"id": "c", "text": "cc"},
            ]),
        );
        let mut ctx = stub_ctx();
        let fake = Arc::new(LenReranker {
            last_doc_count: Mutex::new(0),
            last_top_n: Mutex::new(None),
            last_flow_depth: Mutex::new(0),
        });
        ctx.reranker = fake.clone();

        let out = RerankerNodeAdapter::new()
            .execute(&node(json!({"top_n": 2})), &[input(env)], &ctx)
            .await
            .unwrap();

        let ranked = match out.payload {
            FlowValue::Json(v) => v.get("ranked").and_then(|r| r.as_array()).cloned().unwrap(),
            other => panic!("expected Json, got {other:?}"),
        };
        assert_eq!(ranked.len(), 2);
        assert_eq!(ranked[0]["id"], "b");
        // top_n przekazane do dispatchera (capnięte do liczby kandydatów).
        assert_eq!(*fake.last_top_n.lock().unwrap(), Some(2));
    }

    #[tokio::test]
    async fn empty_candidates_returns_empty_ranked_not_error() {
        let env = payload("q", json!([]));
        let ctx = stub_ctx();
        let out = RerankerNodeAdapter::new()
            .execute(&node(json!({})), &[input(env)], &ctx)
            .await
            .unwrap();
        let ranked = match out.payload {
            FlowValue::Json(v) => v.get("ranked").and_then(|r| r.as_array()).cloned().unwrap(),
            other => panic!("expected Json, got {other:?}"),
        };
        assert!(ranked.is_empty());
    }

    #[tokio::test]
    async fn caps_candidate_count_before_dispatch() {
        let big: Vec<serde_json::Value> = (0..MAX_CANDIDATES + 50)
            .map(|i| json!({"id": i.to_string(), "text": format!("doc {i}")}))
            .collect();
        let env = payload("q", json!(big));
        let mut ctx = stub_ctx();
        let fake = Arc::new(LenReranker {
            last_doc_count: Mutex::new(0),
            last_top_n: Mutex::new(None),
            last_flow_depth: Mutex::new(0),
        });
        ctx.reranker = fake.clone();

        RerankerNodeAdapter::new()
            .execute(&node(json!({})), &[input(env)], &ctx)
            .await
            .unwrap();

        // Dispatcher dostał najwyżej MAX_CANDIDATES dokumentów.
        assert_eq!(*fake.last_doc_count.lock().unwrap(), MAX_CANDIDATES);
    }

    #[tokio::test]
    async fn rejects_non_json_payload() {
        let mut env = FlowEnvelope::empty();
        env.payload = FlowValue::Text("not json".into());
        let ctx = stub_ctx();
        let err = RerankerNodeAdapter::new()
            .execute(&node(json!({})), &[input(env)], &ctx)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("must be Json"));
    }

    /// Domyślny stub `StubReranker` (deterministyczny) działa bez nadpisania —
    /// dowód że inne węzły z `stub_ctx` nie panickują na ctx.reranker.
    #[tokio::test]
    async fn default_stub_reranker_ranks_without_override() {
        let env = payload(
            "q",
            json!([
                {"id": "x", "text": "first"},
                {"id": "y", "text": "second"},
            ]),
        );
        let ctx = stub_ctx();
        let out = RerankerNodeAdapter::new()
            .execute(&node(json!({})), &[input(env)], &ctx)
            .await
            .unwrap();
        let ranked = match out.payload {
            FlowValue::Json(v) => v.get("ranked").and_then(|r| r.as_array()).cloned().unwrap(),
            other => panic!("expected Json, got {other:?}"),
        };
        // StubReranker: pierwszy dokument dostaje najwyższy score.
        assert_eq!(ranked.len(), 2);
        assert_eq!(ranked[0]["id"], "x");
    }

    /// RAG C2 (bug 1, recursion guard): adapter PROPAGUJE `ctx.subflow_depth`
    /// do `RerankRequest.flow_depth`. To ono pozwala dispatcherowi zasiać
    /// runtime-context głębokością, więc self-referencyjny rerank-flow narasta
    /// przez guard zamiast resetować do 0.
    #[tokio::test]
    async fn propagates_subflow_depth_into_request_flow_depth() {
        let env = payload("q", json!([{"id": "a", "text": "doc"}]));
        let mut ctx = stub_ctx();
        ctx.subflow_depth = 2;
        let fake = Arc::new(LenReranker {
            last_doc_count: Mutex::new(0),
            last_top_n: Mutex::new(None),
            last_flow_depth: Mutex::new(0),
        });
        ctx.reranker = fake.clone();

        RerankerNodeAdapter::new()
            .execute(&node(json!({})), &[input(env)], &ctx)
            .await
            .unwrap();

        assert_eq!(*fake.last_flow_depth.lock().unwrap(), 2);
    }

    /// RAG E2.1 — reranker bierze query z meta["rag_query_text"] i kandydatów z
    /// vector-hits (`{hits:[{ref_id,fields:{text}}]}`), mapując ref_id→id,
    /// fields.text→text. Bez tego query gubi się po embeddings (payload=wektor).
    #[tokio::test]
    async fn accepts_vector_hits_input_with_query_from_meta() {
        let env = vector_hits_env(
            "jakie jest pytanie?",
            json!([
                {"ref_id": 10, "score": 0.9, "fields": {"text": "short", "doc_id": "docA", "chunk_index": 0}},
                {"ref_id": 20, "score": 0.5, "fields": {"text": "much longer passage", "doc_id": "docB", "chunk_index": 3}},
            ]),
        );
        let mut ctx = stub_ctx();
        let fake = Arc::new(LenReranker {
            last_doc_count: Mutex::new(0),
            last_top_n: Mutex::new(None),
            last_flow_depth: Mutex::new(0),
        });
        ctx.reranker = fake.clone();

        let out = RerankerNodeAdapter::new()
            .execute(&node(json!({"model": "rag-reranker"})), &[input(env)], &ctx)
            .await
            .unwrap();

        // Dispatcher dostał 2 dokumenty (z fields.text), query niepuste.
        assert_eq!(*fake.last_doc_count.lock().unwrap(), 2);
        let ranked = match &out.payload {
            FlowValue::Json(v) => v.get("ranked").and_then(|r| r.as_array()).cloned().unwrap(),
            other => panic!("expected Json, got {other:?}"),
        };
        // LenReranker: najdłuższy tekst → najwyższy score → ref_id 20 (id="20").
        assert_eq!(ranked[0]["id"], "20");
        assert_eq!(ranked[0]["text"], "much longer passage");
    }

    /// Vector-hits bez query w meta → czytelny błąd (reranker nie ma czego rangować).
    #[tokio::test]
    async fn vector_hits_without_query_in_meta_is_error() {
        let mut env = FlowEnvelope::empty();
        env.payload = FlowValue::Json(json!({
            "hits": [{"ref_id": 1, "fields": {"text": "t"}}]
        }));
        let ctx = stub_ctx();
        let err = RerankerNodeAdapter::new()
            .execute(&node(json!({})), &[input(env)], &ctx)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("rag_query_text"), "był: {err}");
    }

    /// RAG E2.1 — po rerankingu meta["rag_citations"] odzwierciedla RERANKOWANĄ
    /// kolejność i score cross-encodera (nie surowy dystans wektorowy). Cytaty
    /// niosą doc_id/chunk_index/text/score z vector hits, ucięte do top_n.
    #[tokio::test]
    async fn rewrites_citations_in_reranked_order_with_reranker_score() {
        let env = vector_hits_env(
            "q",
            json!([
                {"ref_id": 10, "score": 0.1, "fields": {"text": "aa", "doc_id": "docA", "chunk_index": 1}},
                {"ref_id": 20, "score": 0.2, "fields": {"text": "bbbbbb", "doc_id": "docB", "chunk_index": 2}},
                {"ref_id": 30, "score": 0.3, "fields": {"text": "cccc", "doc_id": "docC", "chunk_index": 3}},
            ]),
        );
        let mut ctx = stub_ctx();
        ctx.reranker = Arc::new(LenReranker {
            last_doc_count: Mutex::new(0),
            last_top_n: Mutex::new(None),
            last_flow_depth: Mutex::new(0),
        });

        let out = RerankerNodeAdapter::new()
            .execute(&node(json!({"top_n": 2})), &[input(env)], &ctx)
            .await
            .unwrap();

        let cites = out
            .meta
            .get("rag_citations")
            .and_then(|c| c.as_array())
            .cloned()
            .expect("meta.rag_citations przepisane przez reranker");
        // top_n=2, rerankowana kolejność wg długości tekstu: bbbbbb > cccc.
        assert_eq!(cites.len(), 2);
        assert_eq!(cites[0]["doc_id"], "docB");
        assert_eq!(cites[0]["chunk_index"].as_i64(), Some(2));
        assert_eq!(cites[0]["text"], "bbbbbb");
        assert_eq!(cites[1]["doc_id"], "docC");
        // Score to score rerankera (długość = 6.0 dla "bbbbbb"), malejąco.
        let s0 = cites[0]["score"].as_f64().unwrap();
        let s1 = cites[1]["score"].as_f64().unwrap();
        assert!(s0 >= s1, "cytaty malejąco po score rerankera: {s0} >= {s1}");
    }

    /// Dispatcher który ZAWSZE zawodzi — symuluje padnięcie wszystkich
    /// targetów failover `rag-reranker` (degradacja na ścieżce RAG).
    struct FailingReranker;

    #[async_trait]
    impl RerankDispatcher for FailingReranker {
        async fn rerank(&self, _req: RerankRequest) -> Result<RerankResponse> {
            Err(anyhow!("wszystkie targety rag-reranker padły"))
        }
    }

    /// Bug 1 — vector-hits gdzie WSZYSTKIE hity są bez `fields.text`:
    /// candidates puste → ranked=[] ORAZ meta["rag_citations"]=[] (NIE stare
    /// vector-hity stashowane przez vector node). Inwariant: cytaty odzwierciedlają
    /// realny kontekst LLM.
    #[tokio::test]
    async fn vector_hits_all_without_text_clears_stale_citations() {
        let mut env = vector_hits_env(
            "q",
            json!([
                {"ref_id": 1, "score": 0.9, "fields": {"doc_id": "docA", "chunk_index": 0}},
                {"ref_id": 2, "score": 0.5, "fields": {"text": "", "doc_id": "docB", "chunk_index": 1}},
            ]),
        );
        // Stare cytaty stashowane przez vector node — muszą zostać wyczyszczone.
        env.meta.insert(
            "rag_citations".into(),
            json!([{"doc_id": "stale", "chunk_index": 0, "text": "old", "score": 0.9}]),
        );
        let ctx = stub_ctx();

        let out = RerankerNodeAdapter::new()
            .execute(&node(json!({})), &[input(env)], &ctx)
            .await
            .unwrap();

        let ranked = match &out.payload {
            FlowValue::Json(v) => v.get("ranked").and_then(|r| r.as_array()).cloned().unwrap(),
            other => panic!("expected Json, got {other:?}"),
        };
        assert!(ranked.is_empty());
        let cites = out
            .meta
            .get("rag_citations")
            .and_then(|c| c.as_array())
            .expect("rag_citations nadpisane na []");
        assert!(cites.is_empty(), "stare cytaty muszą zniknąć: {cites:?}");
    }

    /// Bug 2 — gdy reranker padnie na ścieżce RAG, węzeł degraduje do kolejności
    /// vector-score (NIE wywraca flow). Wynik = hity posortowane malejąco po
    /// vector score, ucięte do top_n, + cytaty z vector score.
    #[tokio::test]
    async fn reranker_failure_degrades_to_vector_order() {
        let env = vector_hits_env(
            "q",
            json!([
                {"ref_id": 10, "score": 0.3, "fields": {"text": "low", "doc_id": "docA", "chunk_index": 0}},
                {"ref_id": 20, "score": 0.9, "fields": {"text": "high", "doc_id": "docB", "chunk_index": 1}},
                {"ref_id": 30, "score": 0.6, "fields": {"text": "mid", "doc_id": "docC", "chunk_index": 2}},
            ]),
        );
        let mut ctx = stub_ctx();
        ctx.reranker = Arc::new(FailingReranker);

        let out = RerankerNodeAdapter::new()
            .execute(&node(json!({"top_n": 2})), &[input(env)], &ctx)
            .await
            .expect("RAG degraduje do vector order zamiast wywracać flow");

        let ranked = match &out.payload {
            FlowValue::Json(v) => v.get("ranked").and_then(|r| r.as_array()).cloned().unwrap(),
            other => panic!("expected Json, got {other:?}"),
        };
        // Vector order malejąco: 0.9 (high) > 0.6 (mid), ucięte do top_n=2.
        assert_eq!(ranked.len(), 2);
        assert_eq!(ranked[0]["id"], "20");
        assert_eq!(ranked[0]["text"], "high");
        assert_eq!(ranked[0]["score"].as_f64().unwrap(), 0.9_f32 as f64);
        assert_eq!(ranked[1]["id"], "30");

        // Cytaty z vector score, w tej samej (vector) kolejności.
        let cites = out
            .meta
            .get("rag_citations")
            .and_then(|c| c.as_array())
            .expect("cytaty z degradacji vector order");
        assert_eq!(cites.len(), 2);
        assert_eq!(cites[0]["doc_id"], "docB");
        assert_eq!(cites[0]["score"].as_f64().unwrap(), 0.9_f32 as f64);
    }

    /// Bug 2 — generyczny kontrakt {query,candidates} (brak vector score) NIE ma
    /// fallbacku: gdy reranker padnie → błąd flow (jak dotąd).
    #[tokio::test]
    async fn reranker_failure_on_candidates_input_is_error() {
        let env = payload("q", json!([{"id": "a", "text": "x"}]));
        let mut ctx = stub_ctx();
        ctx.reranker = Arc::new(FailingReranker);
        let err = RerankerNodeAdapter::new()
            .execute(&node(json!({})), &[input(env)], &ctx)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("dispatcher failed"), "był: {err}");
    }

    /// Bug 3 — wejście z OBOMA `candidates` i `hits` → czytelny błąd
    /// (niejednoznaczność), nie ciche wygranie candidates.
    #[tokio::test]
    async fn both_candidates_and_hits_is_ambiguous_error() {
        let mut env = FlowEnvelope::empty();
        env.payload = FlowValue::Json(json!({
            "query": "q",
            "candidates": [{"id": "a", "text": "x"}],
            "hits": [{"ref_id": 1, "score": 0.5, "fields": {"text": "y"}}],
        }));
        let ctx = stub_ctx();
        let err = RerankerNodeAdapter::new()
            .execute(&node(json!({})), &[input(env)], &ctx)
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("ambiguous reranker input"),
            "był: {err}"
        );
    }

    /// Czysty kontrakt {query,candidates} NIE dotyka meta["rag_citations"]
    /// (brak pól RAG) — generyczny rerank poza RAG nie wstrzykuje cytatów.
    #[tokio::test]
    async fn candidates_input_does_not_touch_citations() {
        let env = payload("q", json!([{"id": "a", "text": "x"}]));
        let ctx = stub_ctx();
        let out = RerankerNodeAdapter::new()
            .execute(&node(json!({})), &[input(env)], &ctx)
            .await
            .unwrap();
        assert!(out.meta.get("rag_citations").is_none());
    }
}
