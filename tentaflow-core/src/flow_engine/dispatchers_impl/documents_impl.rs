// =============================================================================
// Plik: flow_engine/dispatchers_impl/documents_impl.rs
// Opis: DocumentsDispatcherImpl — wrapper nad
//       `services::runtime::executor::ModelRuntimeExecutor::execute_document_infer`.
//       Node-adaptery flow-ingestu RAG widzą tylko narrow trait; failover aliasu
//       (np. `rag-detect`) jest TEN SAM co rerank/embeddings (A1). Świeży runtime
//       `ExecutionContext` per call. To fundament PARTII 2 (page_detect/table/ocr).
// =============================================================================

use async_trait::async_trait;

use super::ModelRuntimeSlot;
use crate::flow_engine::dispatchers::DocumentsDispatcher;
use crate::services::runtime::context::ExecutionContext as RuntimeContext;
use crate::services::runtime::executor::DocumentInferRequest;

use tentaflow_protocol::DocumentInferResult;

pub struct DocumentsDispatcherImpl {
    runtime: ModelRuntimeSlot,
}

impl DocumentsDispatcherImpl {
    pub fn new(runtime: ModelRuntimeSlot) -> Self {
        Self { runtime }
    }
}

#[async_trait]
impl DocumentsDispatcher for DocumentsDispatcherImpl {
    async fn infer(
        &self,
        model: &str,
        image: &[u8],
        mime: &str,
        task: &str,
    ) -> Result<DocumentInferResult, String> {
        if image.is_empty() {
            return Err("DocumentsDispatcher: empty image".to_string());
        }

        let request = DocumentInferRequest {
            model: model.to_string(),
            image_bytes: image.to_vec(),
            mime: mime.to_string(),
            task: task.to_string(),
            flow_depth: 0,
        };

        let mut rctx = RuntimeContext::default();
        let runtime = self
            .runtime
            .read()
            .as_ref()
            .cloned()
            .ok_or_else(|| "DocumentsDispatcher: ModelRuntimeExecutor not wired".to_string())?;
        let response = runtime
            .execute_document_infer(request, &mut rctx)
            .await
            .map_err(|e| format!("DocumentsDispatcher: {e}"))?;

        Ok(DocumentInferResult {
            regions: response.regions,
        })
    }
}
