// =============================================================================
// Plik: net/iroh_client/client.rs
// Opis: Cienki wrapper na `tentaflow_transport::ServiceClient`. Udostepnia API
//       zgodne z dotychczasowym `QuicClient` (pole `url`, `endpoint_id_hex`,
//       `auto_reconnect` itd.) zeby callery w `routing/*` i `services/*` dzialaly
//       bez zmian — caly ruch do peera idzie przez wspolny iroh+rkyv kanal.
// =============================================================================

use std::sync::Arc;
use std::time::Duration;

use futures::stream::Stream;
use iroh::endpoint::{RecvStream, SendStream};
use iroh::{Endpoint, EndpointId};
use tokio::sync::watch;
use tracing::{debug, info};

use crate::error::CoreError;
use tentaflow_protocol::{
    ModelPayload, ModelRequest, ModelResponse, ModelResult, ModelStreamChunk,
    PrefixCacheInitRequest, PrefixCacheInitResponse,
};
use tentaflow_transport::{
    build_client_endpoint, parse_iroh_url, read_frame, ServiceClient, ServiceClientConfig,
    TransportError, ALPN_SERVICE,
};

/// Konfiguracja klienta iroh dla pojedynczego serwisu TentaFlow.
/// Pola `server_name`, `alpn`, `tls_ca`, `skip_tls_verify` zachowane dla
/// kompatybilnosci — iroh autentykuje peerow po `EndpointId`, nie uzywa SNI
/// ani zewnetrznego CA bundle.
#[derive(Debug, Clone)]
pub struct IrohServiceConfig {
    pub name: String,
    /// `iroh://<hex-endpoint-id>` albo czysty 64-znakowy hex.
    pub url: String,
    pub timeout_ms: u64,
    pub server_name: Option<String>,
    pub alpn: String,
    pub auto_reconnect: bool,
    pub reconnect_interval_ms: u64,
    pub keepalive_interval_ms: u64,
    pub tls_ca: Option<String>,
    pub skip_tls_verify: bool,
}

impl Default for IrohServiceConfig {
    fn default() -> Self {
        Self {
            name: String::new(),
            url: String::new(),
            timeout_ms: 30_000,
            server_name: None,
            alpn: "tentaflow-service/v1".to_string(),
            auto_reconnect: true,
            reconnect_interval_ms: 2_000,
            keepalive_interval_ms: 10_000,
            tls_ca: None,
            skip_tls_verify: false,
        }
    }
}

impl IrohServiceConfig {
    fn resolve_endpoint_id(&self) -> Result<EndpointId, CoreError> {
        parse_iroh_url(&self.url).map_err(map_transport_err_cfg)
    }
}

/// Klient iroh dla serwisow TentaFlow.
pub struct IrohServiceClient {
    config: Arc<IrohServiceConfig>,
    inner: Arc<ServiceClient>,
    /// Endpoint trzymany tu zeby zyl tak dlugo jak klient.
    _endpoint: Endpoint,
    shutdown_rx: watch::Receiver<bool>,
}

impl IrohServiceClient {
    pub async fn connect(
        config: IrohServiceConfig,
        shutdown_rx: watch::Receiver<bool>,
    ) -> Result<Self, CoreError> {
        let endpoint_id = config.resolve_endpoint_id()?;

        let endpoint = build_client_endpoint(vec![ALPN_SERVICE.to_vec()])
            .await
            .map_err(map_transport_err)?;

        let mut svc_cfg = ServiceClientConfig::new(&config.name, endpoint_id);
        svc_cfg.request_timeout = Duration::from_millis(config.timeout_ms);
        svc_cfg.auto_reconnect = config.auto_reconnect;
        svc_cfg.reconnect_interval = Duration::from_millis(config.reconnect_interval_ms.max(500));

        let inner = ServiceClient::connect(endpoint.clone(), svc_cfg, shutdown_rx.clone())
            .await
            .map_err(map_transport_err)?;

        info!(name = %config.name, "iroh service client polaczony");

        Ok(Self {
            config: Arc::new(config),
            inner: Arc::new(inner),
            _endpoint: endpoint,
            shutdown_rx,
        })
    }

    pub fn name(&self) -> &str {
        &self.config.name
    }

    pub fn url(&self) -> &str {
        &self.config.url
    }

    pub fn endpoint_id(&self) -> EndpointId {
        self.inner.endpoint_id()
    }

    /// Wysyla `ModelRequest` i czeka na pelny `ModelResponse`.
    pub async fn send_request(&self, request: ModelRequest) -> Result<ModelResponse, CoreError> {
        self.inner.request(request).await.map_err(map_transport_err)
    }

    /// Otwiera bidi stream do peera, wysyla `ModelRequest`, zwraca oba strumienie.
    /// Caller czyta kolejne `ModelStreamChunk` ramki (length-prefixed rkyv).
    pub async fn send_request_stream_raw(
        &self,
        request: ModelRequest,
    ) -> Result<(SendStream, RecvStream), CoreError> {
        self.inner.open_bi(request).await.map_err(map_transport_err)
    }

    /// Wysyla `ModelRequest` i zwraca `Stream` kolejnych `ModelStreamChunk` (STT,
    /// chat streaming itd.). Stream konczy sie gdy peer zamknie `recv`.
    pub async fn send_request_stream(
        &self,
        request: ModelRequest,
    ) -> Result<impl Stream<Item = Result<ModelStreamChunk, CoreError>> + Send, CoreError> {
        let (_send, mut recv) = self
            .inner
            .open_bi(request)
            .await
            .map_err(map_transport_err)?;

        Ok(async_stream::try_stream! {
            loop {
                match read_frame::<ModelStreamChunk>(&mut recv).await {
                    Ok(Some(chunk)) => yield chunk,
                    Ok(None) => break,
                    Err(e) => {
                        Err(map_transport_err(e))?;
                        break;
                    }
                }
            }
        })
    }

    /// Wysyla `PrefixCacheInitRequest` do peera, opakowujac go w `ModelRequest`,
    /// i zwraca `PrefixCacheInitResponse`.
    pub async fn send_prefix_cache_init(
        &self,
        request: PrefixCacheInitRequest,
    ) -> Result<PrefixCacheInitResponse, CoreError> {
        let model_request = ModelRequest {
            request_id: request.request_id.clone(),
            payload: ModelPayload::PrefixCacheInit(request),
            stream: false,
            metadata: None,
            session_id: None,
        };

        let response = self.send_request(model_request).await?;

        match response.result {
            ModelResult::PrefixCacheInit(r) => Ok(r),
            ModelResult::Error(err) => Err(CoreError::BackendError {
                backend_url: self.config.name.clone(),
                message: err.message,
                source: err.details.map(anyhow::Error::msg),
            }),
            _ => Err(CoreError::InternalError {
                message: "Nieoczekiwany wariant ModelResult dla PrefixCacheInit".into(),
                source: None,
            }),
        }
    }

    /// Zamyka klienta.
    pub async fn shutdown(self: Arc<Self>) {
        debug!(name = %self.config.name, "iroh service client shutdown");
        let _ = &self.shutdown_rx;
    }

    /// Zwraca wewnetrzny `ServiceClient` — do zaawansowanych uzyc (np. ensure_connection).
    pub fn inner(&self) -> Arc<ServiceClient> {
        Arc::clone(&self.inner)
    }

    /// Zwraca aktualne polaczenie iroh (z auto-reconnect jesli zamkniete).
    /// Uzywane przez `reverse_request` do `accept_bi` na istniejacym polaczeniu.
    pub async fn iroh_connection(&self) -> Result<iroh::endpoint::Connection, CoreError> {
        self.inner
            .ensure_connection()
            .await
            .map_err(map_transport_err)
    }

    pub async fn send_and_wait_legacy_bytes(
        &self,
        _payload: Vec<u8>,
    ) -> Result<Vec<u8>, CoreError> {
        Err(CoreError::InternalError {
            message: "legacy raw-bytes API nie jest wspierane — uzywaj `send_request`".into(),
            source: None,
        })
    }
}

fn map_transport_err(e: TransportError) -> CoreError {
    let msg = e.to_string();
    match e {
        TransportError::InvalidConfig(_) => CoreError::ConfigError {
            message: msg.clone(),
            source: anyhow::anyhow!(msg),
        },
        TransportError::Serialize(_) | TransportError::Deserialize(_) => CoreError::InternalError {
            message: msg,
            source: None,
        },
        _ => CoreError::NetworkError {
            message: msg.clone(),
            source: anyhow::anyhow!(msg),
        },
    }
}

fn map_transport_err_cfg(e: TransportError) -> CoreError {
    let msg = e.to_string();
    CoreError::ConfigError {
        message: msg.clone(),
        source: anyhow::anyhow!(msg),
    }
}
