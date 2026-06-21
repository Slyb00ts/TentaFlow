// =============================================================================
// Plik: flow_engine/node_adapters/reranker.rs
// Opis: RerankerNodeAdapter — krok retrievalu RAG między vector-search a LLM.
//       Wejście Json{query, candidates:[{id,text}]} → wyjście Json{ranked:
//       [{id,score,text}]} posortowane malejąco po score, ucięte do top_n.
//       Dispatch przez ctx.reranker (alias `rag-reranker`, /v1/rerank).
// =============================================================================

use anyhow::{anyhow, Result};
use async_trait::async_trait;

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
/// `text` to treść pasaża wysyłana do rerankera.
struct Candidate {
    id: String,
    text: String,
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

    /// Wyciąga `query` + listę kandydatów z payload `Json{query, candidates}`.
    /// `id` opcjonalny w wejściu — gdy brak, używamy pozycji jako id (string),
    /// żeby zawsze móc zmapować wynik z powrotem.
    fn parse_input(envelope: &FlowEnvelope) -> Result<(String, Vec<Candidate>)> {
        let obj = match &envelope.payload {
            FlowValue::Json(v) => v,
            other => {
                return Err(anyhow!(
                    "reranker adapter: payload must be Json, got {}",
                    other.kind()
                ))
            }
        };

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
            candidates.push(Candidate { id, text });
        }
        Ok((query, candidates))
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
        let (query, mut candidates) = Self::parse_input(envelope)?;

        // top_n: node.config -> envelope.meta. Bez wartości = wszystkie.
        let top_n = node
            .config
            .get("top_n")
            .and_then(|v| v.as_u64())
            .or_else(|| envelope.meta.get("top_n").and_then(|v| v.as_u64()))
            .map(|n| n as usize);

        let mut out: FlowEnvelope = (**envelope).clone();

        // Pusty zestaw kandydatów → pusty wynik (NIE błąd, NIE panic) — flow
        // retrievalu może legalnie nie znaleźć nic do zrangowania.
        if candidates.is_empty() {
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

        let response = ctx
            .reranker
            .rerank(req)
            .await
            .map_err(|e| anyhow!("reranker adapter: dispatcher failed: {e}"))?;

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
            .into_iter()
            .map(|(score, cand)| {
                serde_json::json!({
                    "id": cand.id,
                    "score": score,
                    "text": cand.text,
                })
            })
            .collect();

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
}
