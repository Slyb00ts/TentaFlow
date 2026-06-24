// =============================================================================
// Plik: routing/documents.rs
// Opis: Protocol-native sender dla typed surface `Documents` (`/v1/infer`).
//       Lustro `route_rerank_via_quic`: gdy peer wysyła `DocumentInferPayload`
//       przez mesh reverse-stream, ten węzeł wykonuje detekcję struktury
//       dokumentu przez `executor.execute_document_infer` (route do lokalnego
//       serwisu Documents albo dalej w mesh) i zwraca `ModelResult::Documents`.
//       Fundament flow-ingestu RAG (node-adaptery page_detect/table/ocr — PARTIA 2).
// =============================================================================

use crate::error::{CoreError, Result};
use crate::routing::router::Router;

use tentaflow_protocol::*;

impl Router {
    /// Protocol-native document-infer API używane przez `mesh/inference_proxy.rs`
    /// gdy peer wysyła `DocumentInferPayload` przez reverse stream. Lustro
    /// `route_rerank_via_quic`: ten sam executor co lokalny `/v1/infer`,
    /// mesh-forward guard (`hop_count = MAX_HOP_COUNT`) przeciw re-forward loopowi.
    pub async fn route_documents_via_protocol(
        &self,
        payload: &DocumentInferPayload,
    ) -> Result<ModelResponse> {
        use crate::services::runtime::context::ExecutionContext;
        use crate::services::runtime::executor::DocumentInferRequest;

        let executor =
            self.executor
                .read()
                .clone()
                .ok_or_else(|| CoreError::AllBackendsUnavailable {
                    model_name: payload.model.clone(),
                })?;

        let request = DocumentInferRequest {
            model: payload.model.clone(),
            image_bytes: payload.image_bytes.clone(),
            mime: payload.mime.clone(),
            task: payload.task.clone(),
            flow_depth: 0,
        };

        let mut exec_ctx = ExecutionContext {
            hop_count: crate::services::runtime::context::MAX_HOP_COUNT,
            ..ExecutionContext::default()
        };

        let response = match executor.execute_document_infer(request, &mut exec_ctx).await {
            Ok(r) => r,
            Err(e) => {
                return Err(crate::routing::embeddings::executor_err_to_core(e, &payload.model).into())
            }
        };

        Ok(ModelResponse {
            request_id: uuid::Uuid::new_v4().to_string(),
            result: ModelResult::Documents(DocumentInferResult {
                regions: response.regions,
            }),
            metrics: None,
        })
    }
}
