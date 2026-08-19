// ===== File: flow_engine/node_adapters/project_knowledge.rs —
// ProjectKnowledgeNodeAdapter (node_type "project_knowledge", category
// service). Exposes a Project Studio knowledge base to flows: `search` embeds
// the Text payload through the shared `rag-embeddings` alias (the same space
// the project ingest writes to) and queries the project's `passages`
// namespace; `list_sources` returns the source catalog. The executing user's
// project membership is enforced on every call — a flow acts strictly as the
// user, never as the platform. =====

use anyhow::{anyhow, Result};
use async_trait::async_trait;

use crate::flow_engine::dispatchers::EmbeddingsRequest;
use crate::flow_engine::envelope::{FlowEnvelope, FlowValue, NodeInput};
use crate::flow_engine::node_adapter::{ExecutionContext, NodeAdapter, PortSpec};
use crate::flow_engine::types::{FlowDataType, FlowNode};
use crate::project_studio::{ingest, knowledge};
use crate::services::org::DEFAULT_ORG_ID;

const NODE_TYPE: &str = "project_knowledge";

pub struct ProjectKnowledgeNodeAdapter;

impl ProjectKnowledgeNodeAdapter {
    pub fn new() -> Self {
        Self
    }

    /// The acting user. Project knowledge is ACL'd per member, so an
    /// unattended run (no user in the flow context) is refused outright.
    fn user_scope(ctx: &ExecutionContext) -> Result<&str> {
        ctx.user_id
            .as_deref()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow!("project_knowledge requires a user identity"))
    }

    fn org_scope(ctx: &ExecutionContext) -> String {
        ctx.org_id
            .clone()
            .unwrap_or_else(|| DEFAULT_ORG_ID.to_string())
    }

    /// Node config wins; an empty config falls back to `envelope.meta
    /// ["project_id"]`. The fallback exists for shared system flows (ps-chat):
    /// ONE global flow serves every project, so the project identity must ride
    /// on the envelope seeded by the caller, not be pinned in the graph.
    fn pick_project_id(node: &FlowNode, envelope: &FlowEnvelope) -> Result<String> {
        node.config
            .get("project_id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .or_else(|| {
                envelope
                    .meta
                    .get("project_id")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string())
            })
            .ok_or_else(|| {
                anyhow!(
                    "project_knowledge: no project id — config 'project_id' nor envelope.meta['project_id']"
                )
            })
    }

    fn pick_source_ids(node: &FlowNode) -> Vec<String> {
        node.config
            .get("source_ids")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|x| x.as_str())
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string())
                    .collect()
            })
            .unwrap_or_default()
    }

    fn query_text(envelope: &FlowEnvelope) -> Result<String> {
        envelope
            .payload
            .as_text()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .ok_or_else(|| {
                anyhow!(
                    "project_knowledge: search requires a non-empty Text payload, got {}",
                    envelope.payload.kind()
                )
            })
    }

    async fn op_search(
        node: &FlowNode,
        envelope: &FlowEnvelope,
        ctx: &ExecutionContext,
        project_id: &str,
        out: &mut FlowEnvelope,
    ) -> Result<()> {
        let query = Self::query_text(envelope)?;
        let top_k = knowledge::clamp_top_k(node.config.get("top_k").and_then(|v| v.as_u64()));
        let source_ids = Self::pick_source_ids(node);

        // Same embedding space the project ingest writes: the shared
        // `rag-embeddings` alias resolved by the platform executor.
        let response = ctx
            .embeddings
            .embed(EmbeddingsRequest {
                model: ingest::EMBEDDINGS_ALIAS.to_string(),
                inputs: vec![query],
                dimensions: None,
                encoding_format: None,
                user_id: ctx.user_id.clone(),
                user_role: ctx.user_role.clone(),
                flow_depth: ctx.subflow_depth,
            })
            .await
            .map_err(|e| anyhow!("project_knowledge: query embedding: {e}"))?;
        let query_vec = response
            .vectors
            .into_iter()
            .next()
            .filter(|v| !v.is_empty())
            .ok_or_else(|| anyhow!("project_knowledge: query embedding empty"))?;
        ctx.usage_sink.record(&node.id, response.usage);

        let org = Self::org_scope(ctx);
        let hits = knowledge::search(
            &ctx.vectors,
            &org,
            project_id,
            &query_vec,
            &source_ids,
            top_k,
        )?;

        // Real retrieval hits feed the citations meta the output node / ps-chat
        // consume — same channel the `vector` node uses.
        out.meta.insert(
            "rag_citations".to_string(),
            knowledge::citations_json(&hits, knowledge::DEFAULT_SNIPPET_CHARS),
        );
        // A downstream `llm` node ignores a Json payload (build_messages only
        // appends Text), so the retrieved passages ground the model through
        // `context.system_prompts` — the same channel the `memory` node uses.
        if !hits.is_empty() {
            out.context.system_prompts.push(knowledge::context_block(
                &hits,
                knowledge::DEFAULT_SNIPPET_CHARS,
            ));
        }
        out.payload = FlowValue::Json(knowledge::hits_to_json(
            &hits,
            knowledge::DEFAULT_SNIPPET_CHARS,
        ));
        Ok(())
    }
}

impl Default for ProjectKnowledgeNodeAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl NodeAdapter for ProjectKnowledgeNodeAdapter {
    fn node_type(&self) -> &str {
        NODE_TYPE
    }
    fn input_ports(&self) -> Vec<PortSpec> {
        vec![PortSpec::new("in", FlowDataType::Any)]
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
            .ok_or_else(|| anyhow!("project_knowledge: missing input edge"))?;
        let envelope = &input.envelope;
        let mut out: FlowEnvelope = (**envelope).clone();

        let user_id = Self::user_scope(ctx)?;
        let org = Self::org_scope(ctx);
        let project_id = Self::pick_project_id(node, envelope)?;
        // Membership gate BEFORE any operation — a non-member gets the same
        // error as a missing project (no existence leak).
        knowledge::require_member(&org, &project_id, user_id)?;

        match node
            .config
            .get("operation")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or("search")
        {
            "search" => Self::op_search(node, envelope, ctx, &project_id, &mut out).await?,
            "list_sources" => {
                out.payload = FlowValue::Json(knowledge::list_sources_json(&project_id)?);
            }
            other => {
                return Err(anyhow!(
                    "project_knowledge: unknown 'operation'='{other}' (search|list_sources)"
                ))
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow_engine::dispatchers::{EmbeddingsDispatcher, EmbeddingsResponse};
    use crate::flow_engine::envelope::TokenUsage;
    use crate::flow_engine::node_adapter::test_support::{stub_ctx, stub_vectors};
    use crate::project_studio::{project_db, repository};
    use crate::services::vector::backend::{Field, FieldSpec, Metric, UpsertItem};
    use serde_json::json;
    use std::sync::Arc;
    use tentaflow_sdk_spec::{FieldType, FieldValue};

    struct FakeEmbeddings {
        vector: Vec<f32>,
    }

    #[async_trait]
    impl EmbeddingsDispatcher for FakeEmbeddings {
        async fn embed(&self, _req: EmbeddingsRequest) -> Result<EmbeddingsResponse> {
            Ok(EmbeddingsResponse {
                vectors: vec![self.vector.clone()],
                usage: TokenUsage::default(),
            })
        }
    }

    fn node(config: serde_json::Value) -> FlowNode {
        FlowNode {
            id: "pk1".into(),
            node_type: NODE_TYPE.into(),
            config,
            position: None,
            label: None,
            region: None,
        }
    }

    fn input(payload: FlowValue) -> NodeInput {
        let mut env = FlowEnvelope::empty();
        env.payload = payload;
        NodeInput {
            from_node_id: "trigger".into(),
            from_port: "full".into(),
            envelope: Arc::new(env),
        }
    }

    /// Registers the process-global Project Studio registry (once per test
    /// binary) and creates one project with an existing on-disk directory.
    /// Returns the fresh project id (unique per call — the registry pool is a
    /// shared OnceLock).
    fn seed_project(owner: &str, members: &[(&str, &str)]) -> String {
        let root =
            std::env::temp_dir().join(format!("tf-ps-know-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&root).expect("create registry root");
        let _ = crate::project_studio::db::init(&root.join("projects.db"))
            .expect("init project studio registry");

        let project_id = uuid::Uuid::new_v4().to_string();
        let dir = root.join("projects").join(&project_id);
        std::fs::create_dir_all(&dir).expect("create project dir");
        let member_rows: Vec<(String, String)> = members
            .iter()
            .map(|(u, r)| (u.to_string(), r.to_string()))
            .collect();
        repository::create_project(
            &project_id,
            "org-1",
            &format!("proj-{project_id}"),
            "",
            "custom",
            r#"["knowledge"]"#,
            owner,
            &dir.to_string_lossy(),
            &member_rows,
        )
        .expect("create project");
        project_id
    }

    fn upsert_passage(
        vectors: &crate::services::vector::NamespaceManager,
        project_id: &str,
        source_id: &str,
        text: &str,
        vec: &[f32],
    ) {
        let scope = crate::project_studio::ingest::vector_scope(project_id);
        let specs = vec![
            FieldSpec {
                name: "doc_id".into(),
                field_type: FieldType::Str,
                indexed: true,
            },
            FieldSpec {
                name: "chunk_index".into(),
                field_type: FieldType::Int,
                indexed: true,
            },
            FieldSpec {
                name: "text".into(),
                field_type: FieldType::Str,
                indexed: false,
            },
            FieldSpec {
                name: "source_id".into(),
                field_type: FieldType::Str,
                indexed: true,
            },
            FieldSpec {
                name: "path".into(),
                field_type: FieldType::Str,
                indexed: false,
            },
            FieldSpec {
                name: "location".into(),
                field_type: FieldType::Str,
                indexed: false,
            },
        ];
        let fields = vec![
            Field {
                name: "doc_id".into(),
                value: FieldValue::Str("file-1".into()),
            },
            Field {
                name: "chunk_index".into(),
                value: FieldValue::Int(0),
            },
            Field {
                name: "text".into(),
                value: FieldValue::Str(text.into()),
            },
            Field {
                name: "source_id".into(),
                value: FieldValue::Str(source_id.into()),
            },
            Field {
                name: "path".into(),
                value: FieldValue::Str("docs/a.md".into()),
            },
            Field {
                name: "location".into(),
                value: FieldValue::Str(String::new()),
            },
        ];
        let items = [UpsertItem {
            ref_id: 1,
            vector: vec,
            fields: &fields,
            sparse: None,
        }];
        vectors
            .upsert_batch_with_quota(
                "org-1",
                &scope,
                crate::project_studio::ingest::VECTOR_NAMESPACE,
                vec.len() as u32,
                Metric::Cosine,
                &specs,
                false,
                &items,
                None,
            )
            .expect("upsert passage");
    }

    #[tokio::test]
    async fn missing_user_identity_is_an_error() {
        let ctx = stub_ctx();
        let err = ProjectKnowledgeNodeAdapter::new()
            .execute(
                &node(json!({"project_id": "p", "operation": "search"})),
                &[input(FlowValue::Text("q".into()))],
                &ctx,
            )
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("requires a user identity"),
            "was: {err}"
        );
    }

    #[tokio::test]
    async fn non_member_gets_uniform_denial() {
        let project_id = seed_project("owner-1", &[]);
        let mut ctx = stub_ctx();
        ctx.user_id = Some("intruder".into());
        ctx.org_id = Some("org-1".into());
        let err = ProjectKnowledgeNodeAdapter::new()
            .execute(
                &node(json!({"project_id": project_id, "operation": "list_sources"})),
                &[input(FlowValue::Empty)],
                &ctx,
            )
            .await
            .unwrap_err();
        // Non-member and missing project produce the SAME message (no leak).
        let missing = ProjectKnowledgeNodeAdapter::new()
            .execute(
                &node(json!({"project_id": "no-such-project", "operation": "list_sources"})),
                &[input(FlowValue::Empty)],
                &ctx,
            )
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), missing.to_string());
        assert!(err.to_string().contains("not found or access denied"));
    }

    #[tokio::test]
    async fn search_returns_hits_and_citations_for_member() {
        let project_id = seed_project("owner-1", &[("member-1", "viewer")]);
        let pool = project_db::open(&project_id).expect("open project pool");
        repository::create_source(&pool, "src-1", "document", "Specs", "{}", "owner-1")
            .expect("create source");

        let mut ctx = stub_ctx();
        ctx.user_id = Some("member-1".into());
        ctx.org_id = Some("org-1".into());
        ctx.embeddings = Arc::new(FakeEmbeddings {
            vector: vec![1.0, 0.0, 0.0],
        });
        let vectors = stub_vectors();
        ctx.vectors = vectors.clone();
        upsert_passage(
            &vectors,
            &project_id,
            "src-1",
            "real project passage",
            &[1.0, 0.0, 0.0],
        );

        let out = ProjectKnowledgeNodeAdapter::new()
            .execute(
                &node(json!({"project_id": project_id, "operation": "search", "top_k": 5})),
                &[input(FlowValue::Text("what do the specs say?".into()))],
                &ctx,
            )
            .await
            .expect("search");

        let hits = match &out.payload {
            FlowValue::Json(v) => v["hits"].as_array().cloned().expect("hits array"),
            other => panic!("expected Json, got {other:?}"),
        };
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0]["source_id"].as_str(), Some("src-1"));
        assert_eq!(hits[0]["source_name"].as_str(), Some("Specs"));
        assert_eq!(hits[0]["file_path"].as_str(), Some("docs/a.md"));
        assert_eq!(hits[0]["snippet"].as_str(), Some("real project passage"));
        assert!(hits[0]["score"].as_f64().is_some());

        let cites = out
            .meta
            .get("rag_citations")
            .and_then(|c| c.as_array())
            .cloned()
            .expect("rag_citations set");
        assert_eq!(cites.len(), 1);
        assert_eq!(cites[0]["doc_id"].as_str(), Some("file-1"));
        assert_eq!(cites[0]["chunk_index"].as_i64(), Some(0));
        assert_eq!(cites[0]["text"].as_str(), Some("real project passage"));

        // The passages ground a downstream llm through system_prompts.
        assert_eq!(out.context.system_prompts.len(), 1);
        assert!(out.context.system_prompts[0].contains("real project passage"));
        assert!(out.context.system_prompts[0].contains("[1]"));
    }

    /// ps-chat: ONE global system flow serves every project, so the node must
    /// resolve the project from `envelope.meta["project_id"]` when the config
    /// leaves it empty. Missing both is a hard error.
    #[tokio::test]
    async fn project_id_falls_back_to_envelope_meta() {
        let project_id = seed_project("owner-4", &[]);
        let pool = project_db::open(&project_id).expect("open project pool");
        repository::create_source(&pool, "src-m", "document", "Meta docs", "{}", "owner-4")
            .expect("create source");

        let mut ctx = stub_ctx();
        ctx.user_id = Some("owner-4".into());
        ctx.org_id = Some("org-1".into());

        let mut env = FlowEnvelope::empty();
        env.meta
            .insert("project_id".into(), json!(project_id.clone()));
        let out = ProjectKnowledgeNodeAdapter::new()
            .execute(
                &node(json!({"project_id": "", "operation": "list_sources"})),
                &[NodeInput {
                    from_node_id: "trigger".into(),
                    from_port: "full".into(),
                    envelope: Arc::new(env),
                }],
                &ctx,
            )
            .await
            .expect("list_sources via meta project_id");
        match &out.payload {
            FlowValue::Json(v) => {
                assert_eq!(v["sources"][0]["source_id"].as_str(), Some("src-m"));
            }
            other => panic!("expected Json, got {other:?}"),
        }

        // Neither config nor meta → explicit error naming both channels.
        let err = ProjectKnowledgeNodeAdapter::new()
            .execute(
                &node(json!({"operation": "list_sources"})),
                &[input(FlowValue::Empty)],
                &ctx,
            )
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("envelope.meta['project_id']"),
            "was: {err}"
        );
    }

    #[tokio::test]
    async fn list_sources_returns_catalog() {
        let project_id = seed_project("owner-2", &[]);
        let pool = project_db::open(&project_id).expect("open project pool");
        repository::create_source(&pool, "src-a", "document", "Docs", "{}", "owner-2")
            .expect("create source");

        let mut ctx = stub_ctx();
        ctx.user_id = Some("owner-2".into());
        ctx.org_id = Some("org-1".into());

        let out = ProjectKnowledgeNodeAdapter::new()
            .execute(
                &node(json!({"project_id": project_id, "operation": "list_sources"})),
                &[input(FlowValue::Empty)],
                &ctx,
            )
            .await
            .expect("list_sources");
        let sources = match &out.payload {
            FlowValue::Json(v) => v["sources"].as_array().cloned().expect("sources array"),
            other => panic!("expected Json, got {other:?}"),
        };
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0]["source_id"].as_str(), Some("src-a"));
        assert_eq!(sources[0]["name"].as_str(), Some("Docs"));
        assert_eq!(sources[0]["kind"].as_str(), Some("document"));
    }

    #[tokio::test]
    async fn search_on_empty_knowledge_base_returns_no_hits() {
        let project_id = seed_project("owner-3", &[]);
        let mut ctx = stub_ctx();
        ctx.user_id = Some("owner-3".into());
        ctx.org_id = Some("org-1".into());
        ctx.embeddings = Arc::new(FakeEmbeddings {
            vector: vec![1.0, 0.0],
        });

        let out = ProjectKnowledgeNodeAdapter::new()
            .execute(
                &node(json!({"project_id": project_id})),
                &[input(FlowValue::Text("anything".into()))],
                &ctx,
            )
            .await
            .expect("search without namespace");
        match &out.payload {
            FlowValue::Json(v) => assert_eq!(v["hits"].as_array().map(|a| a.len()), Some(0)),
            other => panic!("expected Json, got {other:?}"),
        }
    }
}
