// =============================================================================
// Plik: routing/embeddings.rs
// Opis: Obsluga zapytan o embeddingi — route_embeddings (OpenAI API),
//       route_embeddings_quic (QUIC protocol), route_embeddings_via_quic
//       (protocol-native), route_rerank_via_quic (reranking).
// =============================================================================

use crate::api::openai::types::{
    EmbeddingData, EmbeddingInput, EmbeddingRequest, EmbeddingResponse, EmbeddingUsage,
};
use crate::error::{CoreError, Result};
use crate::routing::router::Router;

use std::sync::Arc;
use tentaflow_protocol::*;
use tracing::debug;

impl Router {
    /// Routuje embeddings request do odpowiedniego backendu.
    ///
    /// Obsluguje zarowno Single jak i Multiple input, kieruje do backendu
    /// obslugujacego embeddings (QUIC preferowany, HTTP fallback).
    pub async fn route_embeddings(
        &self,
        request: EmbeddingRequest,
    ) -> Result<crate::routing::RouteResult<EmbeddingResponse>> {
        debug!("Routing embeddings dla modelu: {}", request.model);

        let route_result = {
            use crate::routing::middleware::BackendHandle;
            let this = self.clone();
            let req = request.clone();
            self.dispatch_with_fallback(&request.model, 0, |handle| {
                let this = this.clone();
                let req = req.clone();
                let handle = handle.clone();
                async move {
                    match &handle {
                        BackendHandle::QuicEmbedding(name) => {
                            let quic_handle = {
                                this.service_manager
                                    .quic_embedding_services
                                    .read()
                                    .get(name)
                                    .cloned()
                            }
                            .ok_or_else(|| {
                                anyhow::anyhow!("QUIC embedding serwis '{}' nie znaleziony", name)
                            })?;
                            let quic_client = quic_handle.get_client().await.ok_or_else(|| {
                                anyhow::anyhow!("QUIC embedding serwis '{}' nie polaczony", name)
                            })?;
                            debug!("Routing embeddings przez QUIC: {}", name);
                            this.route_embeddings_quic(quic_client, req, name.clone())
                                .await
                        }
                        BackendHandle::Http(name) => {
                            let backend = this
                                .select_http_backend(name)
                                .ok_or_else(|| anyhow::anyhow!("Brak backendow dla {}", name))?;
                            debug!("Wybrany backend dla embeddings: {}", backend.url());
                            let response = backend.embeddings_request(req).await?;
                            debug!(
                                "Embeddings zakonczone: {} embeddingow wygenerowanych",
                                response.data.len()
                            );
                            Ok(response)
                        }
                        _ => Err(anyhow::anyhow!("Nieobslugiwany backend dla embeddings")),
                    }
                }
            })
            .await?
        };

        Ok(route_result)
    }

    /// Routuje embeddings request przez QUIC (TentaFlow.Embeddings).
    ///
    /// Konwertuje EmbeddingRequest -> ModelRequest z EmbeddingsPayload,
    /// wysyla przez QuicClient, konwertuje odpowiedz do EmbeddingResponse.
    pub(crate) async fn route_embeddings_quic(
        &self,
        quic_client: Arc<crate::net::quic::QuicClient>,
        request: EmbeddingRequest,
        model_name: String,
    ) -> Result<EmbeddingResponse> {
        use uuid::Uuid;

        let input_texts = match &request.input {
            EmbeddingInput::Single(text) => vec![text.clone()],
            EmbeddingInput::Multiple(texts) => texts.clone(),
        };

        let text_count = input_texts.len();

        // Mapuj nazwe serwisu Router -> nazwe modelu w Embeddings Engine
        let embeddings_model_name = model_name
            .strip_prefix("embeddings-")
            .unwrap_or(&model_name)
            .to_string();

        debug!("Model mapping: {} -> {}", model_name, embeddings_model_name);

        let model_request = ModelRequest {
            request_id: Uuid::new_v4().to_string(),
            payload: ModelPayload::Embeddings(EmbeddingsPayload {
                model: embeddings_model_name,
                input: input_texts,
                normalize: true,
            }),
            stream: false,
            metadata: None,
            session_id: None,
        };

        debug!(
            "Wysylam embeddings request przez QUIC: {} tekstow",
            text_count
        );

        let model_response = quic_client.send_request(model_request).await?;

        match model_response.result {
            ModelResult::Embeddings(embeddings_result) => {
                let data: Vec<EmbeddingData> = embeddings_result
                    .embeddings
                    .into_iter()
                    .enumerate()
                    .map(|(idx, embedding)| EmbeddingData {
                        object: "embedding".to_string(),
                        index: idx as u32,
                        embedding,
                    })
                    .collect();

                let estimated_tokens = text_count * 50;

                let response = EmbeddingResponse {
                    object: "list".to_string(),
                    data,
                    model: model_name,
                    usage: EmbeddingUsage {
                        prompt_tokens: estimated_tokens as u32,
                        total_tokens: estimated_tokens as u32,
                    },
                };

                debug!(
                    "Embeddings QUIC: {} embeddingow wygenerowanych",
                    response.data.len()
                );

                Ok(response)
            }
            ModelResult::Error(error_info) => Err(CoreError::InternalError {
                message: format!(
                    "Embeddings QUIC error ({}): {}",
                    model_name, error_info.message
                ),
                source: None,
            }
            .into()),
            _ => Err(CoreError::InternalError {
                message: "Unexpected response type from embeddings QUIC".to_string(),
                source: None,
            }
            .into()),
        }
    }

    /// Routuje embeddings request przez QUIC - wersja dla protocol types.
    pub async fn route_embeddings_via_quic(
        &self,
        model: &str,
        texts: Vec<String>,
    ) -> Result<tentaflow_protocol::ModelResponse> {
        debug!("route_embeddings_via_quic: START model={}", model);

        let model_name = self.resolve_model_alias(model);

        debug!(
            "route_embeddings_via_quic: resolved model={}, texts={}",
            model_name,
            texts.len()
        );

        let quic_handle = {
            self.service_manager
                .quic_embedding_services
                .read()
                .get(&model_name)
                .cloned()
        }
        .ok_or_else(|| CoreError::ModelNotFound {
            model_name: model_name.clone(),
        })?;

        let quic_client =
            quic_handle
                .get_client()
                .await
                .ok_or_else(|| CoreError::AllBackendsUnavailable {
                    model_name: model_name.clone(),
                })?;

        // Mapuj nazwe serwisu Router -> nazwe modelu w Embeddings Engine
        let embeddings_model_name = model_name
            .strip_prefix("tentaflow-embeddings-")
            .unwrap_or(&model_name)
            .to_string();

        let request_id = uuid::Uuid::new_v4().to_string();

        let model_request = ModelRequest {
            request_id: request_id.clone(),
            payload: ModelPayload::Embeddings(EmbeddingsPayload {
                model: embeddings_model_name,
                input: texts,
                normalize: true,
            }),
            stream: false,
            metadata: None,
            session_id: None,
        };

        debug!("route_embeddings_via_quic: wysylam request...");
        let response = quic_client.send_request(model_request).await?;
        debug!("route_embeddings_via_quic: odpowiedz otrzymana");

        Ok(response)
    }

    /// Routuje request rerankingu przez QUIC.
    pub async fn route_rerank_via_quic(
        &self,
        payload: &tentaflow_protocol::RerankPayload,
    ) -> Result<tentaflow_protocol::ModelResponse> {
        debug!("route_rerank_via_quic: START model={}", payload.model);

        let model_name = self.resolve_model_alias(&payload.model);

        let rerank_quic_handle = {
            self.service_manager
                .quic_embedding_services
                .read()
                .get(&model_name)
                .cloned()
        };
        let quic_client = if let Some(quic_handle) = rerank_quic_handle {
            quic_handle
                .get_client()
                .await
                .ok_or_else(|| CoreError::AllBackendsUnavailable {
                    model_name: model_name.clone(),
                })?
        } else {
            return Err(CoreError::ModelNotFound {
                model_name: model_name.clone(),
            }
            .into());
        };

        let request_id = uuid::Uuid::new_v4().to_string();

        let model_request = ModelRequest {
            request_id: request_id.clone(),
            payload: ModelPayload::Rerank(RerankPayload {
                model: payload.model.clone(),
                query: payload.query.clone(),
                documents: payload.documents.clone(),
                top_n: payload.top_n,
                return_documents: payload.return_documents,
            }),
            stream: false,
            metadata: None,
            session_id: None,
        };

        debug!("route_rerank_via_quic: wysylam request...");
        let response = quic_client.send_request(model_request).await?;
        debug!("route_rerank_via_quic: odpowiedz otrzymana");

        Ok(response)
    }
}
