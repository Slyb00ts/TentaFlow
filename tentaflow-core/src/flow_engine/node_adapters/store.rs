// =============================================================================
// Plik: flow_engine/node_adapters/store.rs
// Opis: StoreNodeAdapter — zapis chunków z embeddingami do przestrzeni wektorowej
//       (ctx.vectors), scoped do (org, addon_instance, namespace). Odtwarza
//       transakcyjny cleanup z addona RAG: cleanup-then-reingest (kasuje stare
//       wektory dokumentu PRZED zapisem) + cleanup-on-failure (kasuje wszystko
//       co już zapisał, gdy któryś chunk padnie). Bez modelu.
// =============================================================================

use anyhow::{anyhow, Result};
use async_trait::async_trait;

use crate::flow_engine::envelope::{FlowEnvelope, FlowValue, NodeInput};
use crate::flow_engine::node_adapter::{ExecutionContext, NodeAdapter, PortSpec};
use crate::flow_engine::types::{FlowDataType, FlowNode};
use crate::services::org::DEFAULT_ORG_ID;
use crate::services::vector::backend::{Field, FieldSpec, Metric, UpsertItem};
use tentaflow_sdk_spec::{FieldType, FieldValue};

const NODE_TYPE: &str = "store";

/// Pojedynczy chunk przygotowany do zapisu: deterministyczny ref_id (z doc_id +
/// chunk_index) + wektor + tekst.
struct PreparedChunk {
    ref_id: u64,
    chunk_index: u64,
    vector: Vec<f32>,
    text: String,
}

pub struct StoreNodeAdapter;

impl StoreNodeAdapter {
    pub fn new() -> Self {
        Self
    }

    /// Org-scope: `ctx.org_id` gdy `Some`, w p.p. `DEFAULT_ORG_ID` (lustro vector.rs).
    fn org_scope(ctx: &ExecutionContext) -> String {
        ctx.org_id
            .clone()
            .unwrap_or_else(|| DEFAULT_ORG_ID.to_string())
    }

    /// Tożsamość instancji addona — bez niej węzeł nie wie w którą przestrzeń
    /// pisać, więc odmawia (lustro vector.rs::addon_scope).
    fn addon_scope(ctx: &ExecutionContext) -> Result<&str> {
        ctx.addon_id.as_deref().ok_or_else(|| {
            anyhow!(
                "store: brak tożsamości addona (ctx.addon_id=None) — węzeł store \
                 wymaga wywołania flow JAKO MODEL przez addon (RAG E1.0)"
            )
        })
    }

    fn pick_namespace(node: &FlowNode) -> Result<String> {
        node.config
            .get("namespace")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .ok_or_else(|| anyhow!("store: brak wymaganego 'namespace' w node.config"))
    }

    fn pick_metric(node: &FlowNode) -> Result<Metric> {
        match node.config.get("metric").and_then(|v| v.as_str()) {
            None => Ok(Metric::Cosine),
            Some(s) => Metric::parse(s)
                .ok_or_else(|| anyhow!("store: nieznana metryka '{s}' (cosine|euclidean|dot)")),
        }
    }

    /// doc_id z node.config albo z envelope.meta["doc_id"]/["collection_id"
    /// kontekstu]. To klucz tożsamości dokumentu — po nim robimy
    /// cleanup-then-reingest. Brak → błąd (bez doc_id nie ma stabilnego
    /// re-ingestu ani izolacji per-dokument).
    fn pick_doc_id(node: &FlowNode, envelope: &FlowEnvelope) -> Result<String> {
        if let Some(d) = node
            .config
            .get("doc_id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            return Ok(d.to_string());
        }
        if let Some(d) = envelope
            .meta
            .get("doc_id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            return Ok(d.to_string());
        }
        Err(anyhow!(
            "store: brak 'doc_id' (node.config ani envelope.meta) — wymagany do \
             cleanup-then-reingest i izolacji per-dokument"
        ))
    }

    /// collection_id z node.config/meta (opcjonalny) — zapisywany jako pole
    /// wektora, by retrieval mógł filtrować per-kolekcja (lustro vector.rs
    /// merge_collection_filter).
    fn pick_collection_id(node: &FlowNode, envelope: &FlowEnvelope) -> Option<String> {
        node.config
            .get("collection_id")
            .and_then(|v| v.as_str())
            .or_else(|| envelope.meta.get("collection_id").and_then(|v| v.as_str()))
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
    }

    /// Parsuje wejściowe chunki+embeddingi z payloadu Json. Akceptujemy
    /// `{chunks:[{index,text,embedding|vector:[f32]}]}` — upstream embed dokłada
    /// wektor do każdego chunka. WALIDACJA-PRZED-ZAPISEM: wszystkie chunki
    /// parsujemy i sprawdzamy PRZED jakimkolwiek zapisem (zły chunk w batchu → nic
    /// nie zapisane, jak w vector.rs::op_upsert).
    fn parse_chunks(envelope: &FlowEnvelope, doc_id: &str) -> Result<Vec<PreparedChunk>> {
        let obj = match &envelope.payload {
            FlowValue::Json(v) => v,
            other => {
                return Err(anyhow!(
                    "store: payload musi być Json{{chunks:[...]}}, dostał {}",
                    other.kind()
                ))
            }
        };
        let items = obj
            .get("chunks")
            .and_then(|v| v.as_array())
            .ok_or_else(|| anyhow!("store: payload Json bez 'chunks' (tablica)"))?;
        if items.is_empty() {
            return Err(anyhow!("store: pusta lista 'chunks'"));
        }

        let mut prepared = Vec::with_capacity(items.len());
        let mut dim: Option<usize> = None;
        for (i, item) in items.iter().enumerate() {
            let chunk_index = item
                .get("index")
                .and_then(|v| v.as_u64())
                .unwrap_or(i as u64);
            let text = item
                .get("text")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("store: chunk[{i}] brak 'text'"))?
                .to_string();
            let vector_raw = item
                .get("embedding")
                .or_else(|| item.get("vector"))
                .and_then(|v| v.as_array())
                .ok_or_else(|| {
                    anyhow!("store: chunk[{i}] brak 'embedding'/'vector' (upstream embed musi dołożyć wektor)")
                })?;
            let vector: Vec<f32> = vector_raw
                .iter()
                .enumerate()
                .map(|(j, v)| {
                    v.as_f64()
                        .map(|f| f as f32)
                        .ok_or_else(|| anyhow!("store: chunk[{i}] embedding[{j}] nie jest liczbą"))
                })
                .collect::<Result<_>>()?;
            if vector.is_empty() {
                return Err(anyhow!("store: chunk[{i}] embedding jest pusty"));
            }
            match dim {
                None => dim = Some(vector.len()),
                Some(d) if d != vector.len() => {
                    return Err(anyhow!(
                        "store: chunk[{i}] dim {} != dim {d} wcześniejszych chunków",
                        vector.len()
                    ))
                }
                Some(_) => {}
            }
            prepared.push(PreparedChunk {
                ref_id: crate::services::vector::doc_vectors::ref_id_for(doc_id, chunk_index),
                chunk_index,
                vector,
                text,
            });
        }
        Ok(prepared)
    }
}

impl Default for StoreNodeAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl NodeAdapter for StoreNodeAdapter {
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
            .ok_or_else(|| anyhow!("store: brak krawędzi wejściowej"))?;
        let envelope = &input.envelope;

        let org = Self::org_scope(ctx);
        let addon = Self::addon_scope(ctx)?;
        let namespace = Self::pick_namespace(node)?;
        let metric = Self::pick_metric(node)?;
        let doc_id = Self::pick_doc_id(node, envelope)?;
        let collection_id = Self::pick_collection_id(node, envelope);

        // Faza 1: pełna walidacja chunków PRZED jakimkolwiek zapisem.
        let prepared = Self::parse_chunks(envelope, &doc_id)?;
        let dim = prepared[0].vector.len() as u32;

        // Schema pól metadanych (stała dla namespace): doc_id, chunk_index, text,
        // opcjonalnie collection_id. Te same nazwy czyta retrieval (vector.rs
        // citations_from_hits / merge_collection_filter).
        let mut field_specs = vec![
            FieldSpec {
                name: "doc_id".to_string(),
                field_type: FieldType::Str,
                indexed: true,
            },
            FieldSpec {
                name: "chunk_index".to_string(),
                field_type: FieldType::Int,
                indexed: true,
            },
            FieldSpec {
                name: "text".to_string(),
                field_type: FieldType::Str,
                indexed: false,
            },
        ];
        if collection_id.is_some() {
            field_specs.push(FieldSpec {
                name: "collection_id".to_string(),
                field_type: FieldType::Str,
                indexed: true,
            });
        }

        // Cleanup-then-reingest: skasuj WSZYSTKIE stare wektory tego dokumentu
        // PRZED zapisem nowych. Re-ingest tego samego doc_id ze ZMIENIONĄ liczbą
        // chunków zostawiłby orphany (stare chunki o wyższym indeksie), gdyby nie
        // ten krok — deterministyczny ref_id nadpisuje tylko chunki o tym samym
        // indeksie. Szukamy istniejących wektorów po filtrze doc_id i kasujemy je.
        // Namespace może jeszcze nie istnieć (pierwszy ingest) — NamespaceNotFound
        // traktujemy jako „nic do sprzątania", nie błąd.
        match ctx.vectors.get(&org, addon, &namespace) {
            Ok(backend) => crate::services::vector::doc_vectors::delete_doc_vectors(
                &*backend,
                &doc_id,
                Some(&prepared[0].vector),
            )
            .map_err(|e| anyhow!("store: cleanup-then-reingest: {e}"))?,
            Err(crate::services::vector::error::VectorError::NamespaceNotFound { .. }) => {
                // Pierwszy ingest do tej przestrzeni — nic do sprzątania.
            }
            Err(e) => return Err(anyhow!("store: cleanup-then-reingest: {e}")),
        }

        // Faza 2: zapis. Cały dokument idzie JEDNYM batched upsertem — zvec buduje
        // graf HNSW znacznie taniej z N doków naraz niż z N pojedynczych insertów
        // (pojedyncze insert+flush per chunk to anty-wzorzec: ~100-150 s na
        // dokument). Metadane chunków budujemy z wyprzedzeniem (muszą przeżyć
        // pożyczające je `UpsertItem`), wektory pożyczamy wprost z `prepared`.
        let mut fields_per_chunk: Vec<Vec<Field>> = Vec::with_capacity(prepared.len());
        for chunk in &prepared {
            let mut field_values = vec![
                Field {
                    name: "doc_id".to_string(),
                    value: FieldValue::Str(doc_id.clone()),
                },
                Field {
                    name: "chunk_index".to_string(),
                    value: FieldValue::Int(chunk.chunk_index as i64),
                },
                Field {
                    name: "text".to_string(),
                    value: FieldValue::Str(chunk.text.clone()),
                },
            ];
            if let Some(cid) = &collection_id {
                field_values.push(Field {
                    name: "collection_id".to_string(),
                    value: FieldValue::Str(cid.clone()),
                });
            }
            fields_per_chunk.push(field_values);
        }

        let items: Vec<UpsertItem<'_>> = prepared
            .iter()
            .zip(fields_per_chunk.iter())
            .map(|(chunk, fields)| UpsertItem {
                ref_id: chunk.ref_id,
                vector: &chunk.vector,
                fields: fields.as_slice(),
                sparse: None,
            })
            .collect();

        if let Err(e) = ctx.vectors.upsert_batch_with_quota(
            &org,
            addon,
            &namespace,
            dim,
            metric,
            &field_specs,
            false,
            &items,
            ctx.vector_home.as_deref(),
        ) {
            // Cleanup-on-failure: batch jest transakcyjny po stronie quoty i
            // robi jeden insert, ale gdyby częściowo zapisał (błąd backendu po
            // wstawieniu części doków), kasujemy wszystkie ref_id dokumentu —
            // zero częściowego ingestu (lustro run_ingest_pipeline z addona).
            let all_refs: Vec<u64> = prepared.iter().map(|c| c.ref_id).collect();
            let cleanup_err = Self::rollback(ctx, &org, addon, &namespace, &all_refs);
            return match cleanup_err {
                Some(ce) => Err(anyhow!(
                    "store: batch upsert dokumentu nieudany: {e}; dodatkowo cleanup nieudany: {ce}"
                )),
                None => Err(anyhow!("store: batch upsert dokumentu nieudany: {e}")),
            };
        }

        // Markdown rekonstrukcji do raportu. Tekstów chunków NIE przepychamy przez
        // ABI (cap 8 MiB → PayloadTooLarge na dużym dokumencie): addon czyta je z
        // przestrzeni wektorowej `passages` po `doc_id` (te same pola doc_id/
        // chunk_index/text, które tu zapisaliśmy) do ekstrakcji grafu.
        let markdown = prepared
            .iter()
            .map(|c| c.text.as_str())
            .collect::<Vec<_>>()
            .join("\n\n");

        // page_count: ustawia go `pdf_rasterize` w meta (`pdf_page_count`) i
        // przenosi przez vision_parse_pages -> document_merge -> combine -> chunk
        // -> embed -> store (każdy klonuje base envelope). store, jako terminalny
        // węzeł, przepisuje go do finalnego JSON, by `flow_outcome_to_ingest_response`
        // zwrócił REALNĄ liczbę stron zamiast defaultu 1. Brak meta (obraz/office/
        // tekst — pojedyncza "strona") = mapper defaultuje na 1.
        let page_count = envelope
            .meta
            .get("pdf_page_count")
            .and_then(|v| v.as_u64())
            .filter(|n| *n > 0);

        let mut payload = serde_json::json!({
            "op": "store",
            "namespace": namespace,
            "doc_id": doc_id,
            "written": prepared.len(),
            "markdown": markdown,
            "chunks": prepared.len(),
        });
        if let Some(pc) = page_count {
            payload["page_count"] = serde_json::json!(pc);
        }

        let mut out = (**envelope).clone();
        out.payload = FlowValue::Json(payload);
        out.meta.insert(
            "stored_chunks".to_string(),
            serde_json::json!(prepared.len()),
        );
        Ok(out)
    }
}

impl StoreNodeAdapter {
    /// Kasuje listę ref_id z przestrzeni (cleanup-on-failure). Zwraca `Some(msg)`
    /// gdy KTÓRYKOLWIEK delete padł — caller doklei to do błędu ingestu.
    fn rollback(
        ctx: &ExecutionContext,
        org: &str,
        addon: &str,
        namespace: &str,
        refs: &[u64],
    ) -> Option<String> {
        if refs.is_empty() {
            return None;
        }
        let backend = match ctx.vectors.get(org, addon, namespace) {
            Ok(b) => b,
            Err(e) => return Some(format!("backend niedostępny do rollbacku: {e}")),
        };
        crate::services::vector::doc_vectors::rollback_refs(&*backend, refs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow_engine::node_adapter::test_support::{stub_ctx, stub_vectors};
    use serde_json::json;
    use std::sync::Arc;
    use tentaflow_sdk_spec::Filter;

    fn node(config: serde_json::Value) -> FlowNode {
        FlowNode {
            id: "store-1".into(),
            node_type: NODE_TYPE.into(),
            config,
            position: None,
            label: None,
            region: None,
        }
    }

    fn addon_ctx(
        addon: &str,
        org: &str,
        vectors: Arc<crate::services::vector::NamespaceManager>,
    ) -> ExecutionContext {
        let mut ctx = stub_ctx();
        ctx.addon_id = Some(addon.to_string());
        ctx.org_id = Some(org.to_string());
        ctx.vectors = vectors;
        ctx
    }

    /// Buduje payload {chunks:[{index,text,embedding}]}.
    fn chunks_payload(chunks: &[(u64, &str, Vec<f32>)]) -> FlowValue {
        let arr: Vec<serde_json::Value> = chunks
            .iter()
            .map(|(i, t, v)| json!({"index": i, "text": t, "embedding": v}))
            .collect();
        FlowValue::Json(json!({ "chunks": arr }))
    }

    fn input(payload: FlowValue, meta: serde_json::Value) -> NodeInput {
        let mut env = FlowEnvelope::empty();
        env.payload = payload;
        if let Some(obj) = meta.as_object() {
            for (k, v) in obj {
                env.meta.insert(k.clone(), v.clone());
            }
        }
        NodeInput {
            from_node_id: "embed".into(),
            from_port: "full".into(),
            envelope: Arc::new(env),
        }
    }

    #[tokio::test]
    async fn stores_chunks_and_reports_count() {
        let v = stub_vectors();
        let ctx = addon_ctx("inst-a", "org-1", v);
        let out = StoreNodeAdapter::new()
            .execute(
                &node(json!({"namespace": "passages", "doc_id": "docA"})),
                &[input(
                    chunks_payload(&[
                        (0, "pasaz 0", vec![1.0, 0.0, 0.0]),
                        (1, "pasaz 1", vec![0.0, 1.0, 0.0]),
                    ]),
                    json!({}),
                )],
                &ctx,
            )
            .await
            .unwrap();
        let written = match &out.payload {
            FlowValue::Json(v) => v.get("written").and_then(|n| n.as_u64()).unwrap(),
            other => panic!("expected Json, got {other:?}"),
        };
        assert_eq!(written, 2);
        assert_eq!(
            out.meta.get("stored_chunks").and_then(|n| n.as_u64()),
            Some(2)
        );
    }

    /// Cleanup-then-reingest: drugi ingest tego samego doc_id z MNIEJSZĄ liczbą
    /// chunków nie zostawia orphanów (stary chunk index=1 ma zniknąć).
    #[tokio::test]
    async fn reingest_removes_orphan_chunks() {
        let v = stub_vectors();
        let ctx = addon_ctx("inst-a", "org-1", v);
        let store = StoreNodeAdapter::new();

        // Pierwszy ingest: 3 chunki.
        store
            .execute(
                &node(json!({"namespace": "p", "doc_id": "docA"})),
                &[input(
                    chunks_payload(&[
                        (0, "a", vec![1.0, 0.0, 0.0]),
                        (1, "b", vec![0.0, 1.0, 0.0]),
                        (2, "c", vec![0.0, 0.0, 1.0]),
                    ]),
                    json!({}),
                )],
                &ctx,
            )
            .await
            .unwrap();

        // Re-ingest: tylko 1 chunk. Stare chunki 1 i 2 muszą zniknąć.
        store
            .execute(
                &node(json!({"namespace": "p", "doc_id": "docA"})),
                &[input(
                    chunks_payload(&[(0, "a2", vec![1.0, 0.0, 0.0])]),
                    json!({}),
                )],
                &ctx,
            )
            .await
            .unwrap();

        // Przestrzeń ma dokładnie 1 wektor doc_id=docA.
        let backend = ctx.vectors.get("org-1", "inst-a", "p").unwrap();
        let filter = Filter::Eq("doc_id".to_string(), FieldValue::Str("docA".into()));
        let hits = backend
            .search(&[1.0, 0.0, 0.0], 100, Some(&filter), &["text".to_string()])
            .unwrap();
        assert_eq!(hits.len(), 1, "re-ingest nie zostawia orphanów");
        let text = hits[0]
            .fields
            .iter()
            .find(|f| f.name == "text")
            .map(|f| match &f.value {
                FieldValue::Str(s) => s.clone(),
                _ => String::new(),
            })
            .unwrap();
        assert_eq!(text, "a2", "zostaje nowy chunk, nie stary");
    }

    #[tokio::test]
    async fn doc_id_from_meta_when_config_absent() {
        let v = stub_vectors();
        let ctx = addon_ctx("inst-a", "org-1", v);
        let out = StoreNodeAdapter::new()
            .execute(
                &node(json!({"namespace": "p"})),
                &[input(
                    chunks_payload(&[(0, "x", vec![1.0, 0.0])]),
                    json!({"doc_id": "from-meta"}),
                )],
                &ctx,
            )
            .await
            .unwrap();
        let doc = match &out.payload {
            FlowValue::Json(v) => v
                .get("doc_id")
                .and_then(|d| d.as_str())
                .unwrap()
                .to_string(),
            _ => panic!("expected Json"),
        };
        assert_eq!(doc, "from-meta");
    }

    #[tokio::test]
    async fn missing_doc_id_is_error() {
        let v = stub_vectors();
        let ctx = addon_ctx("inst-a", "org-1", v);
        let err = StoreNodeAdapter::new()
            .execute(
                &node(json!({"namespace": "p"})),
                &[input(
                    chunks_payload(&[(0, "x", vec![1.0, 0.0])]),
                    json!({}),
                )],
                &ctx,
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("doc_id"), "{err}");
    }

    #[tokio::test]
    async fn missing_embedding_in_chunk_is_error_writes_nothing() {
        let v = stub_vectors();
        let ctx = addon_ctx("inst-a", "org-1", v);
        // Drugi chunk bez embeddingu → cały batch odrzucony przed zapisem.
        let payload = FlowValue::Json(json!({"chunks": [
            {"index": 0, "text": "a", "embedding": [1.0, 0.0]},
            {"index": 1, "text": "b"},
        ]}));
        let err = StoreNodeAdapter::new()
            .execute(
                &node(json!({"namespace": "p", "doc_id": "docA"})),
                &[input(payload, json!({}))],
                &ctx,
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("embedding"), "{err}");
        // Namespace nie powstał (walidacja przed zapisem) — search pada.
        assert!(ctx.vectors.get("org-1", "inst-a", "p").is_err());
    }

    #[tokio::test]
    async fn missing_addon_id_is_error() {
        let v = stub_vectors();
        let mut ctx = stub_ctx();
        ctx.vectors = v;
        let err = StoreNodeAdapter::new()
            .execute(
                &node(json!({"namespace": "p", "doc_id": "docA"})),
                &[input(
                    chunks_payload(&[(0, "x", vec![1.0, 0.0])]),
                    json!({}),
                )],
                &ctx,
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("tożsamości addona"), "{err}");
    }
}
