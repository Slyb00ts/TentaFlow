// =============================================================================
// Plik: quic_server.rs
// Opis: Serwer QUIC — kontener nasluchuje na polaczenia od routera TentaFlow.
//       Wzorzec identyczny z Solutio.AI.STT/LLM — serwer QUIC + rkyv.
//       Router laczy sie jako klient, wysyla ModelRequest, kontener odpowiada
//       ModelResponse (lub strumieniuje ModelStreamChunk).
//       RouterClient umozliwia wysylanie requestow z sidecara do routera
//       na tym samym polaczeniu QUIC (reverse direction).
// =============================================================================

use anyhow::{Context, Result};
use quinn::{Endpoint, ServerConfig as QuinnServerConfig};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use std::net::SocketAddr;
use std::sync::Arc;
use tentaflow_protocol::*;
use tokio::sync::{mpsc, oneshot, watch};
use tracing::{debug, error, info, warn};

/// Komenda sterujaca spotkaniem — wysylana z QUIC do glownej petli w main.rs
pub enum MeetingCommand {
    JoinMeeting {
        meeting_url: String,
        response_tx: oneshot::Sender<String>,
    },
    LeaveMeeting {
        response_tx: oneshot::Sender<String>,
    },
    GetStatus {
        response_tx: oneshot::Sender<String>,
    },
}

/// Klient do wysylania requestow z sidecara do routera
/// na istniejacym polaczeniu QUIC (reverse direction).
/// Router polaczyl sie jako klient, ale QUIC jest bidirektionalny —
/// serwer moze otworzyc nowy stream do klienta.
pub struct RouterClient {
    connection: quinn::Connection,
}

impl RouterClient {
    pub fn new(connection: quinn::Connection) -> Self {
        Self { connection }
    }

    /// Wysyla ModelRequest do routera i czeka na ModelResponse.
    /// Otwiera nowy stream bidirektionalny na istniejacym polaczeniu.
    pub async fn send_request(&self, request: &ModelRequest) -> Result<ModelResponse> {
        let (mut send, mut recv) = self.connection.open_bi().await
            .map_err(|e| anyhow::anyhow!("Nie udalo sie otworzyc streamu do routera: {}", e))?;

        let request_bytes = rkyv::to_bytes::<rkyv::rancor::Error>(request)
            .map_err(|e| anyhow::anyhow!("Blad serializacji ModelRequest: {}", e))?;

        send.write_all(&request_bytes).await?;
        send.finish()?;

        let response_bytes = recv.read_to_end(10 * 1024 * 1024).await
            .context("Nie udalo sie odczytac odpowiedzi od routera")?;

        let archived = rkyv::access::<ArchivedModelResponse, rkyv::rancor::Error>(&response_bytes)
            .map_err(|e| anyhow::anyhow!("Blad walidacji rkyv ModelResponse: {}", e))?;

        let response: ModelResponse =
            rkyv::deserialize::<ModelResponse, rkyv::rancor::Error>(archived)
                .map_err(|e| anyhow::anyhow!("Blad deserializacji rkyv ModelResponse: {}", e))?;

        Ok(response)
    }

    /// Wysyla audio do STT przez router (alias podany w `model`).
    pub async fn transcribe(
        &self,
        audio_pcm: &[i16],
        model: &str,
        language: Option<String>,
    ) -> Result<String> {
        let audio_bytes: Vec<u8> = audio_pcm.iter()
            .flat_map(|s| s.to_le_bytes())
            .collect();

        let request = ModelRequest {
            request_id: uuid::Uuid::new_v4().to_string(),
            payload: ModelPayload::Audio(AudioPayload {
                operation: AudioOperation::STT {
                    model: model.to_string(),
                    audio_data: audio_bytes,
                    language,
                    response_format: None,
                    prompt: None,
                    temperature: None,
                    timestamp_granularities: None,
                    no_speech_threshold: None,
                    avg_logprob_threshold: None,
                    compression_ratio_threshold: None,
                },
            }),
            stream: false,
            metadata: None,
            session_id: None,
        };

        let response = self.send_request(&request).await?;

        match response.result {
            ModelResult::Audio(audio) => match audio.data {
                AudioResultData::Text(text) => Ok(text),
                AudioResultData::Detailed { text, .. } => Ok(text),
                _ => anyhow::bail!("STT zwrocil nieoczekiwany typ danych"),
            },
            ModelResult::Error(e) => anyhow::bail!("STT blad: {}", e.message),
            _ => anyhow::bail!("Nieoczekiwany typ odpowiedzi STT"),
        }
    }

    /// Wysyla tekst do TTS przez router (alias podany w `model`).
    pub async fn synthesize(
        &self,
        text: &str,
        voice: &str,
        model: &str,
    ) -> Result<Vec<u8>> {
        let request = ModelRequest {
            request_id: uuid::Uuid::new_v4().to_string(),
            payload: ModelPayload::Audio(AudioPayload {
                operation: AudioOperation::TTS {
                    model: model.to_string(),
                    input: text.to_string(),
                    voice: voice.to_string(),
                    format: Some("pcm".to_string()),
                    speed: None,
                },
            }),
            stream: false,
            metadata: None,
            session_id: None,
        };

        let response = self.send_request(&request).await?;

        match response.result {
            ModelResult::Audio(audio) => match audio.data {
                AudioResultData::Audio(bytes) => Ok(bytes),
                _ => anyhow::bail!("TTS zwrocil nieoczekiwany typ danych"),
            },
            ModelResult::Error(e) => anyhow::bail!("TTS blad: {}", e.message),
            _ => anyhow::bail!("Nieoczekiwany typ odpowiedzi TTS"),
        }
    }
}

/// Konfiguracja serwera QUIC kontenera
#[derive(Debug, Clone)]
pub struct ContainerQuicConfig {
    /// Port UDP do nasluchiwania
    pub port: u16,

    /// Sciezka do certyfikatu TLS (PEM). None = self-signed
    pub tls_cert: Option<String>,

    /// Sciezka do klucza prywatnego TLS (PEM). None = self-signed
    pub tls_key: Option<String>,

    /// Maksymalna liczba rownoczesnych strumieni na polaczenie
    pub max_streams: u32,

    /// Timeout bezczynnosci (ms)
    pub idle_timeout_ms: u64,
}

impl Default for ContainerQuicConfig {
    fn default() -> Self {
        Self {
            port: 5000,
            tls_cert: None,
            tls_key: None,
            max_streams: 64,
            idle_timeout_ms: 30_000,
        }
    }
}

/// Serwer QUIC kontenera meeting bot.
///
/// Nasluchuje na polaczenia od routera TentaFlow, przetwarza ModelRequest
/// i zwraca ModelResponse. Dla transkrypcji strumieniowej wysyla
/// kolejne ModelStreamChunk z length-prefix.
pub struct MeetingQuicServer {
    config: ContainerQuicConfig,
    /// Kanal do wysylania transkrypcji na zywo (speaker, text, timestamp_ms)
    transcript_tx: mpsc::UnboundedSender<(String, String, u64)>,
    /// Kanal do odbierania transkrypcji (uzywany w handle_stream)
    transcript_rx: Arc<tokio::sync::Mutex<mpsc::UnboundedReceiver<(String, String, u64)>>>,
    /// Klient do routera — ustawiany gdy router sie polacy
    router_client: Arc<tokio::sync::Mutex<Option<Arc<RouterClient>>>>,
    /// Kanal komend sterujacych spotkaniem (QUIC -> main.rs)
    command_tx: mpsc::UnboundedSender<MeetingCommand>,
    command_rx: Arc<tokio::sync::Mutex<Option<mpsc::UnboundedReceiver<MeetingCommand>>>>,
}

impl MeetingQuicServer {
    pub fn new(config: ContainerQuicConfig) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        Self {
            config,
            transcript_tx: tx,
            transcript_rx: Arc::new(tokio::sync::Mutex::new(rx)),
            router_client: Arc::new(tokio::sync::Mutex::new(None)),
            command_tx: cmd_tx,
            command_rx: Arc::new(tokio::sync::Mutex::new(Some(cmd_rx))),
        }
    }

    /// Zwraca nadawce transkrypcji — inne moduly moga wysylac transkrypcje
    /// ktore serwer przekaze do routera w streamingu
    pub fn transcript_sender(&self) -> mpsc::UnboundedSender<(String, String, u64)> {
        self.transcript_tx.clone()
    }

    /// Zwraca odbiorce komend sterujacych — main.rs konsumuje komendy
    /// i wykonuje akcje na przegladarce (join/leave/status).
    /// Mozna wywolac tylko raz — drugie wywolanie zwroci None.
    pub async fn command_receiver(&self) -> Option<mpsc::UnboundedReceiver<MeetingCommand>> {
        self.command_rx.lock().await.take()
    }

    /// Zwraca klienta do routera (jesli router jest polaczony).
    /// Inne moduly uzywaja tego do wysylania STT/TTS requestow.
    pub async fn router_client(&self) -> Option<Arc<RouterClient>> {
        self.router_client.lock().await.clone()
    }

    /// Zwraca uchwyt do pola router_client — pozwala innym modulom
    /// sprawdzac/czekac na polaczenie routera bez async.
    pub fn router_client_handle(&self) -> Arc<tokio::sync::Mutex<Option<Arc<RouterClient>>>> {
        self.router_client.clone()
    }

    /// Uruchamia serwer QUIC i nasluchuje na polaczenia od routera.
    pub async fn run(&self, mut shutdown_rx: watch::Receiver<bool>) -> Result<()> {
        let server_config = self.create_server_config()?;
        let bind_addr: SocketAddr = format!("0.0.0.0:{}", self.config.port)
            .parse()
            .context("Nieprawidlowy adres bind")?;

        let endpoint = Endpoint::server(server_config, bind_addr)?;
        info!(port = self.config.port, "Serwer QUIC kontenera nasluchuje");

        loop {
            tokio::select! {
                conn = endpoint.accept() => {
                    match conn {
                        Some(incoming) => {
                            let transcript_rx = self.transcript_rx.clone();
                            let router_client_slot = self.router_client.clone();
                            let command_tx = self.command_tx.clone();
                            tokio::spawn(async move {
                                match incoming.await {
                                    Ok(connection) => {
                                        debug!(
                                            remote = %connection.remote_address(),
                                            "Router polaczony"
                                        );
                                        Self::handle_connection(
                                            connection,
                                            transcript_rx,
                                            router_client_slot,
                                            command_tx,
                                        ).await;
                                    }
                                    Err(e) => {
                                        error!("Polaczenie QUIC nieudane: {}", e);
                                    }
                                }
                            });
                        }
                        None => {
                            warn!("Endpoint QUIC zamkniety");
                            break;
                        }
                    }
                }
                _ = shutdown_rx.changed() => {
                    info!("Zamykanie serwera QUIC...");
                    endpoint.close(0u32.into(), b"container shutdown");
                    endpoint.wait_idle().await;
                    info!("Serwer QUIC zamkniety");
                    break;
                }
            }
        }

        Ok(())
    }

    /// Obsluguje pojedyncze polaczenie od routera.
    /// Akceptuje strumienie bidirektionalne w petli.
    /// Tworzy RouterClient z tego samego Connection — inne moduly
    /// moga go uzywac do wysylania requestow do routera (reverse direction).
    async fn handle_connection(
        connection: quinn::Connection,
        transcript_rx: Arc<tokio::sync::Mutex<mpsc::UnboundedReceiver<(String, String, u64)>>>,
        router_client_slot: Arc<tokio::sync::Mutex<Option<Arc<RouterClient>>>>,
        command_tx: mpsc::UnboundedSender<MeetingCommand>,
    ) {
        let remote = connection.remote_address();
        debug!(remote = %remote, "handle_connection: start");

        // Zapisz RouterClient — to samo polaczenie sluzy do przyjmowania
        // requestow (accept_bi) i do wysylania requestow (open_bi).
        let client = Arc::new(RouterClient::new(connection.clone()));
        {
            let mut slot = router_client_slot.lock().await;
            *slot = Some(client);
        }
        info!(remote = %remote, "RouterClient gotowy — mozna wysylac requesty do routera");

        loop {
            match connection.accept_bi().await {
                Ok((send, recv)) => {
                    let transcript_rx = transcript_rx.clone();
                    let command_tx = command_tx.clone();
                    tokio::spawn(async move {
                        if let Err(e) = Self::handle_stream(send, recv, transcript_rx, command_tx).await {
                            error!("Blad obslugi strumienia: {}", e);
                        }
                    });
                }
                Err(e) => {
                    debug!(remote = %remote, "Polaczenie zamkniete: {}", e);
                    break;
                }
            }
        }

        // Polaczenie zamkniete — usun RouterClient
        {
            let mut slot = router_client_slot.lock().await;
            *slot = None;
        }
        info!(remote = %remote, "RouterClient usuniety — polaczenie zamkniete");
    }

    /// Obsluguje pojedynczy strumien bidirektionalny (request -> response).
    ///
    /// Odczytuje ModelRequest, przetwarza i odsyla ModelResponse.
    /// Dla streamingu wysyla kolejne ModelStreamChunk z length-prefix
    /// (4 bajty BE dlugosci + rkyv payload).
    async fn handle_stream(
        mut send: quinn::SendStream,
        mut recv: quinn::RecvStream,
        transcript_rx: Arc<tokio::sync::Mutex<mpsc::UnboundedReceiver<(String, String, u64)>>>,
        command_tx: mpsc::UnboundedSender<MeetingCommand>,
    ) -> Result<()> {
        // Odczytaj request (max 10MB)
        let request_bytes = recv
            .read_to_end(10 * 1024 * 1024)
            .await
            .context("Nie udalo sie odczytac requestu")?;

        if request_bytes.is_empty() {
            anyhow::bail!("Pusty request");
        }

        debug!(len = request_bytes.len(), "Odebrano request");

        // Deserializacja ModelRequest (rkyv zero-copy)
        let archived = rkyv::access::<ArchivedModelRequest, rkyv::rancor::Error>(&request_bytes)
            .map_err(|e| anyhow::anyhow!("Blad walidacji rkyv ModelRequest: {}", e))?;

        let request: ModelRequest =
            rkyv::deserialize::<ModelRequest, rkyv::rancor::Error>(archived)
                .map_err(|e| anyhow::anyhow!("Blad deserializacji rkyv ModelRequest: {}", e))?;

        debug!(
            request_id = %request.request_id,
            stream = request.stream,
            "ModelRequest odebrany"
        );

        if request.stream {
            // Tryb strumieniowy — wysylaj chunki z transkrypcja na zywo
            Self::handle_streaming_request(&request, &mut send, transcript_rx).await?;
        } else {
            // Sprawdz czy Completion zawiera komende narzedzia w polu prompt
            let response = if let Some(cmd_response) = Self::try_handle_tool_command(&request, &command_tx).await {
                cmd_response
            } else {
                Self::process_request(&request)
            };

            let response_bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&response)
                .map_err(|e| anyhow::anyhow!("Blad serializacji rkyv ModelResponse: {}", e))?;

            send.write_all(&response_bytes).await?;
            send.finish()?;
        }

        Ok(())
    }

    /// Sprawdza czy request zawiera komende narzedzia w polu prompt.
    /// Jesli tak — wysyla komende przez kanal i czeka na odpowiedz.
    /// Zwraca None jesli prompt nie zawiera komendy narzedzia.
    async fn try_handle_tool_command(
        request: &ModelRequest,
        command_tx: &mpsc::UnboundedSender<MeetingCommand>,
    ) -> Option<ModelResponse> {
        let payload = match &request.payload {
            ModelPayload::Completion(p) => p,
            _ => return None,
        };

        let prompt = payload.prompt.as_deref()?;

        // Parsuj JSON z pola prompt
        let json: serde_json::Value = serde_json::from_str(prompt).ok()?;
        let tool = json.get("tool")?.as_str()?;

        let (response_tx, response_rx) = oneshot::channel();

        let cmd = match tool {
            "teams-bot.join_meeting" => {
                let meeting_url = json
                    .get("params")
                    .and_then(|p| p.get("meeting_url"))
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();

                if meeting_url.is_empty() {
                    return Some(ModelResponse {
                        request_id: request.request_id.clone(),
                        result: ModelResult::Error(ErrorInfo {
                            error_type: ErrorType::InvalidRequest,
                            message: "Brak meeting_url w parametrach join_meeting".to_string(),
                            details: None,
                        }),
                        metrics: None,
                    });
                }

                info!(meeting_url = %meeting_url, "Komenda: join_meeting");
                MeetingCommand::JoinMeeting { meeting_url, response_tx }
            }
            "teams-bot.leave_meeting" => {
                info!("Komenda: leave_meeting");
                MeetingCommand::LeaveMeeting { response_tx }
            }
            "teams-bot.get_status" => {
                debug!("Komenda: get_status");
                MeetingCommand::GetStatus { response_tx }
            }
            _ => {
                warn!(tool = tool, "Nieznana komenda narzedzia");
                return Some(ModelResponse {
                    request_id: request.request_id.clone(),
                    result: ModelResult::Error(ErrorInfo {
                        error_type: ErrorType::InvalidRequest,
                        message: format!("Nieznana komenda: {}", tool),
                        details: None,
                    }),
                    metrics: None,
                });
            }
        };

        if command_tx.send(cmd).is_err() {
            return Some(ModelResponse {
                request_id: request.request_id.clone(),
                result: ModelResult::Error(ErrorInfo {
                    error_type: ErrorType::InternalError,
                    message: "Kanal komend zamkniety — sidecar konczy prace".to_string(),
                    details: None,
                }),
                metrics: None,
            });
        }

        // Czekaj na odpowiedz z glownej petli (max 5 minut — join moze trwac)
        let result_text = match tokio::time::timeout(
            std::time::Duration::from_secs(300),
            response_rx,
        ).await {
            Ok(Ok(text)) => text,
            Ok(Err(_)) => "Blad: kanal odpowiedzi zamkniety".to_string(),
            Err(_) => "Blad: timeout oczekiwania na wykonanie komendy".to_string(),
        };

        Some(ModelResponse {
            request_id: request.request_id.clone(),
            result: ModelResult::Completion(CompletionResult {
                text: result_text,
                reasoning_content: None,
                model: "meeting-bot".to_string(),
                finish_reason: Some("stop".to_string()),
                tool_calls: None,
                detected_intent: None,
                detected_tools: None,
                transcribed_text: None,
                speaker_id: None,
                speaker_name: None,
            }),
            metrics: None,
        })
    }

    /// Przetwarza jednorazowy ModelRequest i zwraca ModelResponse.
    fn process_request(request: &ModelRequest) -> ModelResponse {
        match &request.payload {
            ModelPayload::Completion(payload) => {
                // Meeting bot moze odpowiadac na pytania o spotkanie
                let response_text = format!(
                    "Meeting bot kontener — odebrano completion request: {} wiadomosci",
                    payload.messages.len()
                );

                ModelResponse {
                    request_id: request.request_id.clone(),
                    result: ModelResult::Completion(CompletionResult {
                        text: response_text,
                        reasoning_content: None,
                        model: "meeting-bot".to_string(),
                        finish_reason: Some("stop".to_string()),
                        tool_calls: None,
                        detected_intent: None,
                        detected_tools: None,
                        transcribed_text: None,
                        speaker_id: None,
                        speaker_name: None,
                    }),
                    metrics: None,
                }
            }
            ModelPayload::Audio(payload) => {
                match &payload.operation {
                    AudioOperation::STT { .. } => {
                        // STT — kontener moze przetwarzac audio z spotkania
                        ModelResponse {
                            request_id: request.request_id.clone(),
                            result: ModelResult::Audio(AudioResult {
                                data: AudioResultData::Text(String::new()),
                                model: "meeting-bot-stt".to_string(),
                            }),
                            metrics: None,
                        }
                    }
                    AudioOperation::TTS { input, voice, .. } => {
                        debug!(text_len = input.len(), voice = %voice, "TTS request");
                        ModelResponse {
                            request_id: request.request_id.clone(),
                            result: ModelResult::Audio(AudioResult {
                                data: AudioResultData::Audio(Vec::new()),
                                model: "meeting-bot-tts".to_string(),
                            }),
                            metrics: None,
                        }
                    }
                    _ => {
                        warn!(
                            request_id = %request.request_id,
                            "Nieobslugiwana operacja audio"
                        );
                        ModelResponse {
                            request_id: request.request_id.clone(),
                            result: ModelResult::Error(ErrorInfo {
                                error_type: ErrorType::InvalidRequest,
                                message: "Meeting bot nie obsluguje tej operacji audio".to_string(),
                                details: None,
                            }),
                            metrics: None,
                        }
                    }
                }
            }
            _ => {
                // Nieobslugiwany typ payload
                warn!(
                    request_id = %request.request_id,
                    "Nieobslugiwany typ payload"
                );
                ModelResponse {
                    request_id: request.request_id.clone(),
                    result: ModelResult::Error(ErrorInfo {
                        error_type: ErrorType::InvalidRequest,
                        message: "Meeting bot nie obsluguje tego typu requestu".to_string(),
                        details: None,
                    }),
                    metrics: None,
                }
            }
        }
    }

    /// Obsluguje streaming request — wysyla chunki transkrypcji na zywo.
    ///
    /// Format ramki: [4 bajty BE dlugosci][rkyv ModelStreamChunk]
    async fn handle_streaming_request(
        request: &ModelRequest,
        send: &mut quinn::SendStream,
        transcript_rx: Arc<tokio::sync::Mutex<mpsc::UnboundedReceiver<(String, String, u64)>>>,
    ) -> Result<()> {
        let request_id = &request.request_id;
        debug!(request_id = %request_id, "Streaming: start");

        let mut rx = transcript_rx.lock().await;

        // Wysylaj transkrypcje jako TextDelta dopoki kanal jest otwarty
        while let Some((speaker, text, _timestamp_ms)) = rx.recv().await {
            let chunk_text = format!("[{}]: {}\n", speaker, text);

            let chunk = ModelStreamChunk {
                request_id: request_id.clone(),
                chunk: StreamChunkType::TextDelta(chunk_text),
            };

            Self::send_stream_chunk(send, &chunk).await?;
        }

        // Koniec streamu
        let done_chunk = ModelStreamChunk {
            request_id: request_id.clone(),
            chunk: StreamChunkType::Done {
                final_metrics: None,
            },
        };
        Self::send_stream_chunk(send, &done_chunk).await?;
        send.finish()?;

        debug!(request_id = %request_id, "Streaming: zakonczony");
        Ok(())
    }

    /// Wysyla pojedynczy chunk z length-prefix przez QUIC.
    async fn send_stream_chunk(
        send: &mut quinn::SendStream,
        chunk: &ModelStreamChunk,
    ) -> Result<()> {
        let chunk_bytes = rkyv::to_bytes::<rkyv::rancor::Error>(chunk)
            .map_err(|e| anyhow::anyhow!("Blad serializacji rkyv ModelStreamChunk: {}", e))?;

        let len = chunk_bytes.len() as u32;
        let mut frame = Vec::with_capacity(4 + chunk_bytes.len());
        frame.extend_from_slice(&len.to_be_bytes());
        frame.extend_from_slice(&chunk_bytes);
        send.write_all(&frame).await?;

        Ok(())
    }

    /// Tworzy konfiguracje serwera QUIC z certyfikatami TLS.
    fn create_server_config(&self) -> Result<QuinnServerConfig> {
        let (certs, key) = match (&self.config.tls_cert, &self.config.tls_key) {
            (Some(cert_path), Some(key_path)) => {
                info!("Ladowanie certyfikatow TLS z plikow");
                let cert_data = std::fs::read(cert_path)
                    .with_context(|| format!("Nie udalo sie odczytac certyfikatu: {}", cert_path))?;
                let key_data = std::fs::read(key_path)
                    .with_context(|| format!("Nie udalo sie odczytac klucza: {}", key_path))?;

                let certs: Vec<CertificateDer<'static>> =
                    rustls_pemfile::certs(&mut cert_data.as_slice())
                        .collect::<std::result::Result<Vec<_>, _>>()
                        .context("Nie udalo sie sparsowac certyfikatu")?;

                let key: PrivateKeyDer<'static> =
                    rustls_pemfile::private_key(&mut key_data.as_slice())
                        .context("Nie udalo sie sparsowac klucza")?
                        .ok_or_else(|| anyhow::anyhow!("Brak klucza prywatnego w pliku"))?;

                (certs, key)
            }
            _ => {
                info!("Generowanie self-signed certyfikatu TLS");
                Self::generate_self_signed()?
            }
        };

        let mut server_crypto = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(certs, key)
            .context("Nie udalo sie skonfigurowac TLS")?;

        // ALPN — identyfikacja protokolu TentaFlow
        server_crypto.alpn_protocols = vec![b"tentaflow".to_vec()];

        let mut server_config = QuinnServerConfig::with_crypto(Arc::new(
            quinn::crypto::rustls::QuicServerConfig::try_from(server_crypto)?,
        ));

        let mut transport = quinn::TransportConfig::default();
        transport.max_concurrent_bidi_streams(self.config.max_streams.into());
        transport.max_idle_timeout(Some(
            std::time::Duration::from_millis(self.config.idle_timeout_ms)
                .try_into()
                .context("Nieprawidlowa wartosc idle_timeout_ms")?,
        ));
        transport.keep_alive_interval(Some(std::time::Duration::from_secs(15)));

        server_config.transport_config(Arc::new(transport));

        Ok(server_config)
    }

    /// Generuje self-signed certyfikat EC P-256 (do dewelopmentu).
    fn generate_self_signed() -> Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)> {
        let mut params = rcgen::CertificateParams::new(vec!["localhost".to_string()])
            .context("Nie udalo sie utworzyc parametrow certyfikatu")?;
        params
            .distinguished_name
            .push(rcgen::DnType::CommonName, "tentaflow-meeting-bot");

        let key_pair = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256)
            .context("Nie udalo sie wygenerowac klucza")?;

        let cert = params
            .self_signed(&key_pair)
            .context("Nie udalo sie wygenerowac certyfikatu self-signed")?;

        let cert_der = CertificateDer::from(cert.der().to_vec());
        let key_der = PrivateKeyDer::try_from(key_pair.serialize_der())
            .map_err(|e| anyhow::anyhow!("Blad konwersji klucza DER: {:?}", e))?;

        Ok((vec![cert_der], key_der))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// Tworzy klienta QUIC z self-signed certyfikatem (pomija weryfikacje TLS).
    async fn create_test_client(server_addr: SocketAddr) -> Result<quinn::Connection> {
        let mut client_crypto = rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(SkipServerVerification))
            .with_no_client_auth();

        client_crypto.alpn_protocols = vec![b"tentaflow".to_vec()];

        let client_config = quinn::ClientConfig::new(Arc::new(
            quinn::crypto::rustls::QuicClientConfig::try_from(client_crypto)?,
        ));

        let mut endpoint = Endpoint::client("0.0.0.0:0".parse()?)?;
        endpoint.set_default_client_config(client_config);

        let connection = endpoint
            .connect(server_addr, "localhost")?
            .await
            .context("Polaczenie z serwerem QUIC nieudane")?;

        Ok(connection)
    }

    /// Pomija weryfikacje certyfikatu serwera (do testow z self-signed).
    #[derive(Debug)]
    struct SkipServerVerification;

    impl rustls::client::danger::ServerCertVerifier for SkipServerVerification {
        fn verify_server_cert(
            &self,
            _end_entity: &rustls::pki_types::CertificateDer<'_>,
            _intermediates: &[rustls::pki_types::CertificateDer<'_>],
            _server_name: &rustls::pki_types::ServerName<'_>,
            _ocsp_response: &[u8],
            _now: rustls::pki_types::UnixTime,
        ) -> std::result::Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
            Ok(rustls::client::danger::ServerCertVerified::assertion())
        }

        fn verify_tls12_signature(
            &self,
            _message: &[u8],
            _cert: &rustls::pki_types::CertificateDer<'_>,
            _dss: &rustls::DigitallySignedStruct,
        ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
            Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
        }

        fn verify_tls13_signature(
            &self,
            _message: &[u8],
            _cert: &rustls::pki_types::CertificateDer<'_>,
            _dss: &rustls::DigitallySignedStruct,
        ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
            Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
        }

        fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
            vec![
                rustls::SignatureScheme::ECDSA_NISTP256_SHA256,
                rustls::SignatureScheme::ECDSA_NISTP384_SHA384,
                rustls::SignatureScheme::ED25519,
            ]
        }
    }

    /// Instaluje CryptoProvider dla rustls (wymagane w testach).
    fn ensure_crypto_provider() {
        let _ = rustls::crypto::ring::default_provider().install_default();
    }

    /// Uruchamia serwer QUIC na losowym porcie, zwraca adres i kanal shutdown.
    async fn start_test_server() -> Result<(SocketAddr, watch::Sender<bool>, Arc<MeetingQuicServer>)> {
        ensure_crypto_provider();
        let config = ContainerQuicConfig {
            port: 0, // losowy port
            ..Default::default()
        };

        let server = Arc::new(MeetingQuicServer::new(config));
        let server_config = server.create_server_config()?;
        let bind_addr: SocketAddr = "127.0.0.1:0".parse()?;
        let endpoint = Endpoint::server(server_config, bind_addr)?;
        let local_addr = endpoint.local_addr()?;

        let (shutdown_tx, shutdown_rx) = watch::channel(false);

        let server_clone = server.clone();
        tokio::spawn(async move {
            let transcript_rx = server_clone.transcript_rx.clone();
            let router_client_slot = server_clone.router_client.clone();
            let command_tx = server_clone.command_tx.clone();
            let mut shutdown_rx = shutdown_rx;

            loop {
                tokio::select! {
                    conn = endpoint.accept() => {
                        match conn {
                            Some(incoming) => {
                                let transcript_rx = transcript_rx.clone();
                                let router_client_slot = router_client_slot.clone();
                                let command_tx = command_tx.clone();
                                tokio::spawn(async move {
                                    match incoming.await {
                                        Ok(connection) => {
                                            MeetingQuicServer::handle_connection(
                                                connection,
                                                transcript_rx,
                                                router_client_slot,
                                                command_tx,
                                            ).await;
                                        }
                                        Err(_) => {}
                                    }
                                });
                            }
                            None => break,
                        }
                    }
                    _ = shutdown_rx.changed() => {
                        endpoint.close(0u32.into(), b"test shutdown");
                        endpoint.wait_idle().await;
                        break;
                    }
                }
            }
        });

        // Krótkie opoznienie zeby serwer sie uruchomil
        tokio::time::sleep(Duration::from_millis(50)).await;

        Ok((local_addr, shutdown_tx, server))
    }

    #[tokio::test]
    async fn quic_server_client_completion_roundtrip() {
        // Test 1: Klient laczy sie z serwerem, wysyla ModelRequest Completion,
        // odbiera ModelResponse i weryfikuje poprawnosc odpowiedzi.

        // Arrange
        let (addr, shutdown_tx, _server) = start_test_server().await
            .expect("Serwer powinien sie uruchomic");

        let connection = create_test_client(addr).await
            .expect("Klient powinien sie polaczyc");

        let request = ModelRequest {
            request_id: "test-completion-001".to_string(),
            payload: ModelPayload::Completion(CompletionPayload {
                model: "meeting-bot".to_string(),
                prompt: None,
                messages: vec![
                    Message {
                        role: "user".to_string(),
                        content: "Czesc".to_string(),
                    },
                ],
                temperature: None,
                max_tokens: None,
                top_p: None,
                stop: None,
                presence_penalty: None,
                frequency_penalty: None,
                tts_options: None,
                memory_options: None,
                audio_input: None,
                prefix_cache_id: None,
                prefix_text: None,
            }),
            stream: false,
            metadata: None,
            session_id: None,
        };

        // Act — wysylamy request przez bidirektionalny stream
        let (mut send, mut recv) = connection.open_bi().await
            .expect("Otwarcie streamu powinno sie udac");

        let request_bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&request)
            .expect("Serializacja powinna sie udac");

        send.write_all(&request_bytes).await
            .expect("Zapis powinien sie udac");
        send.finish().expect("Zakonczenie wysylania powinno sie udac");

        let response_bytes = recv.read_to_end(10 * 1024 * 1024).await
            .expect("Odczyt odpowiedzi powinien sie udac");

        let archived = rkyv::access::<ArchivedModelResponse, rkyv::rancor::Error>(&response_bytes)
            .expect("Walidacja rkyv powinna sie udac");

        let response: ModelResponse =
            rkyv::deserialize::<ModelResponse, rkyv::rancor::Error>(archived)
                .expect("Deserializacja powinna sie udac");

        // Assert
        assert_eq!(response.request_id, "test-completion-001");
        match &response.result {
            ModelResult::Completion(result) => {
                assert!(result.text.contains("1 wiadomosci"));
                assert_eq!(result.model, "meeting-bot");
                assert_eq!(result.finish_reason.as_deref(), Some("stop"));
            }
            other => panic!("Oczekiwano Completion, otrzymano: {:?}", std::mem::discriminant(other)),
        }

        // Cleanup
        connection.close(0u32.into(), b"test done");
        let _ = shutdown_tx.send(true);
    }

    #[tokio::test]
    async fn quic_server_client_stt_request_returns_audio_result() {
        // Serwer odpowiada na STT request typem Audio

        // Arrange
        let (addr, shutdown_tx, _server) = start_test_server().await
            .expect("Serwer powinien sie uruchomic");

        let connection = create_test_client(addr).await
            .expect("Klient powinien sie polaczyc");

        let request = ModelRequest {
            request_id: "test-stt-001".to_string(),
            payload: ModelPayload::Audio(AudioPayload {
                operation: AudioOperation::STT {
                    model: "whisper-1".to_string(),
                    audio_data: vec![0u8; 100],
                    language: Some("pl".to_string()),
                    response_format: None,
                    prompt: None,
                    temperature: None,
                    timestamp_granularities: None,
                    no_speech_threshold: None,
                    avg_logprob_threshold: None,
                    compression_ratio_threshold: None,
                },
            }),
            stream: false,
            metadata: None,
            session_id: None,
        };

        // Act
        let (mut send, mut recv) = connection.open_bi().await.unwrap();
        let request_bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&request).unwrap();
        send.write_all(&request_bytes).await.unwrap();
        send.finish().unwrap();

        let response_bytes = recv.read_to_end(10 * 1024 * 1024).await.unwrap();
        let archived = rkyv::access::<ArchivedModelResponse, rkyv::rancor::Error>(&response_bytes).unwrap();
        let response: ModelResponse = rkyv::deserialize::<ModelResponse, rkyv::rancor::Error>(archived).unwrap();

        // Assert
        assert_eq!(response.request_id, "test-stt-001");
        match &response.result {
            ModelResult::Audio(audio) => {
                assert_eq!(audio.model, "meeting-bot-stt");
            }
            other => panic!("Oczekiwano Audio, otrzymano: {:?}", std::mem::discriminant(other)),
        }

        connection.close(0u32.into(), b"test done");
        let _ = shutdown_tx.send(true);
    }

    #[tokio::test]
    async fn quic_server_unsupported_payload_returns_error() {
        // Nieobslugiwany typ payload zwraca ModelResult::Error

        // Arrange
        let (addr, shutdown_tx, _server) = start_test_server().await
            .expect("Serwer powinien sie uruchomic");

        let connection = create_test_client(addr).await
            .expect("Klient powinien sie polaczyc");

        let request = ModelRequest {
            request_id: "test-unsupported-001".to_string(),
            payload: ModelPayload::Embeddings(EmbeddingsPayload {
                model: "test".to_string(),
                input: vec!["test".to_string()],
                normalize: true,
            }),
            stream: false,
            metadata: None,
            session_id: None,
        };

        // Act
        let (mut send, mut recv) = connection.open_bi().await.unwrap();
        let request_bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&request).unwrap();
        send.write_all(&request_bytes).await.unwrap();
        send.finish().unwrap();

        let response_bytes = recv.read_to_end(10 * 1024 * 1024).await.unwrap();
        let archived = rkyv::access::<ArchivedModelResponse, rkyv::rancor::Error>(&response_bytes).unwrap();
        let response: ModelResponse = rkyv::deserialize::<ModelResponse, rkyv::rancor::Error>(archived).unwrap();

        // Assert
        assert_eq!(response.request_id, "test-unsupported-001");
        match &response.result {
            ModelResult::Error(err) => {
                assert_eq!(err.error_type, ErrorType::InvalidRequest);
                assert!(err.message.contains("nie obsluguje"));
            }
            other => panic!("Oczekiwano Error, otrzymano: {:?}", std::mem::discriminant(other)),
        }

        connection.close(0u32.into(), b"test done");
        let _ = shutdown_tx.send(true);
    }

    #[tokio::test]
    async fn reverse_quic_router_client_available_after_connection() {
        // Test 2: Po polaczeniu klienta, RouterClient jest dostepny
        // i mozna otworzyc stream w kierunku klienta (reverse direction).

        // Arrange
        let (addr, shutdown_tx, server) = start_test_server().await
            .expect("Serwer powinien sie uruchomic");

        // Przed polaczeniem — RouterClient nie jest dostepny
        assert!(server.router_client().await.is_none());

        let connection = create_test_client(addr).await
            .expect("Klient powinien sie polaczyc");

        // Wysylamy dummy request zeby handle_connection zapisalo RouterClient
        let (mut send, mut recv) = connection.open_bi().await.unwrap();
        let request = ModelRequest {
            request_id: "handshake".to_string(),
            payload: ModelPayload::Completion(CompletionPayload {
                model: "meeting-bot".to_string(),
                prompt: None,
                messages: vec![Message { role: "user".to_string(), content: "ping".to_string() }],
                temperature: None,
                max_tokens: None,
                top_p: None,
                stop: None,
                presence_penalty: None,
                frequency_penalty: None,
                tts_options: None,
                memory_options: None,
                audio_input: None,
                prefix_cache_id: None,
                prefix_text: None,
            }),
            stream: false,
            metadata: None,
            session_id: None,
        };
        let request_bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&request).unwrap();
        send.write_all(&request_bytes).await.unwrap();
        send.finish().unwrap();
        let _ = recv.read_to_end(10 * 1024 * 1024).await.unwrap();

        // Krótkie opoznienie — handle_connection musi zapisac RouterClient
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Act & Assert — RouterClient powinien byc dostepny
        let router_client = server.router_client().await;
        assert!(router_client.is_some(), "RouterClient powinien byc dostepny po polaczeniu");

        // Test reverse direction: serwer otwiera stream do klienta
        let router_client = router_client.unwrap();

        // Uruchom task klienta ktory akceptuje stream z serwera i odpowiada
        let client_conn = connection.clone();
        let client_task = tokio::spawn(async move {
            let (mut client_send, mut client_recv) = client_conn.accept_bi().await
                .expect("Klient powinien zaakceptowac stream od serwera");

            // Odczytaj request od serwera
            let req_bytes = client_recv.read_to_end(10 * 1024 * 1024).await
                .expect("Odczyt requestu powinien sie udac");

            let archived = rkyv::access::<ArchivedModelRequest, rkyv::rancor::Error>(&req_bytes)
                .expect("Walidacja rkyv powinna sie udac");
            let req: ModelRequest = rkyv::deserialize::<ModelRequest, rkyv::rancor::Error>(archived)
                .expect("Deserializacja powinna sie udac");

            // Wyslij odpowiedz
            let response = ModelResponse {
                request_id: req.request_id.clone(),
                result: ModelResult::Completion(CompletionResult {
                    text: "Odpowiedz z routera".to_string(),
                    reasoning_content: None,
                    model: "router-model".to_string(),
                    finish_reason: Some("stop".to_string()),
                    tool_calls: None,
                    detected_intent: None,
                    detected_tools: None,
                    transcribed_text: None,
                    speaker_id: None,
                    speaker_name: None,
                }),
                metrics: None,
            };

            let resp_bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&response)
                .expect("Serializacja odpowiedzi powinna sie udac");

            client_send.write_all(&resp_bytes).await
                .expect("Zapis odpowiedzi powinien sie udac");
            client_send.finish()
                .expect("Zakonczenie wysylania powinno sie udac");

            req.request_id
        });

        // Serwer wysyla request do klienta przez RouterClient
        let test_request = ModelRequest {
            request_id: "reverse-001".to_string(),
            payload: ModelPayload::Completion(CompletionPayload {
                model: "reverse-test".to_string(),
                prompt: None,
                messages: vec![Message { role: "user".to_string(), content: "reverse test".to_string() }],
                temperature: None,
                max_tokens: None,
                top_p: None,
                stop: None,
                presence_penalty: None,
                frequency_penalty: None,
                tts_options: None,
                memory_options: None,
                audio_input: None,
                prefix_cache_id: None,
                prefix_text: None,
            }),
            stream: false,
            metadata: None,
            session_id: None,
        };

        let response = router_client.send_request(&test_request).await
            .expect("Reverse request powinien sie udac");

        // Assert
        assert_eq!(response.request_id, "reverse-001");
        match &response.result {
            ModelResult::Completion(result) => {
                assert_eq!(result.text, "Odpowiedz z routera");
                assert_eq!(result.model, "router-model");
            }
            other => panic!("Oczekiwano Completion, otrzymano: {:?}", std::mem::discriminant(other)),
        }

        // Weryfikacja ze klient odebrarl poprawne request_id
        let received_request_id = client_task.await.unwrap();
        assert_eq!(received_request_id, "reverse-001");

        // Cleanup
        connection.close(0u32.into(), b"test done");
        let _ = shutdown_tx.send(true);
    }

    #[tokio::test]
    async fn quic_server_multiple_streams_concurrent() {
        // Wiele strumieni rownoczesnie na jednym polaczeniu

        // Arrange
        let (addr, shutdown_tx, _server) = start_test_server().await
            .expect("Serwer powinien sie uruchomic");

        let connection = create_test_client(addr).await
            .expect("Klient powinien sie polaczyc");

        // Act — wysylamy 5 requestow rownoczesnie
        let mut handles = Vec::new();
        for i in 0..5 {
            let conn = connection.clone();
            let handle = tokio::spawn(async move {
                let (mut send, mut recv) = conn.open_bi().await.unwrap();
                let request = ModelRequest {
                    request_id: format!("concurrent-{}", i),
                    payload: ModelPayload::Completion(CompletionPayload {
                        model: "meeting-bot".to_string(),
                        prompt: None,
                        messages: vec![Message {
                            role: "user".to_string(),
                            content: format!("msg-{}", i),
                        }],
                        temperature: None,
                        max_tokens: None,
                        top_p: None,
                        stop: None,
                        presence_penalty: None,
                        frequency_penalty: None,
                        tts_options: None,
                        memory_options: None,
                        audio_input: None,
                        prefix_cache_id: None,
                        prefix_text: None,
                    }),
                    stream: false,
                    metadata: None,
                    session_id: None,
                };
                let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&request).unwrap();
                send.write_all(&bytes).await.unwrap();
                send.finish().unwrap();
                let resp_bytes = recv.read_to_end(10 * 1024 * 1024).await.unwrap();
                let archived = rkyv::access::<ArchivedModelResponse, rkyv::rancor::Error>(&resp_bytes).unwrap();
                let response: ModelResponse = rkyv::deserialize::<ModelResponse, rkyv::rancor::Error>(archived).unwrap();
                response.request_id
            });
            handles.push(handle);
        }

        // Assert — wszystkie odpowiedzi maja poprawne request_id
        let mut received_ids: Vec<String> = Vec::new();
        for handle in handles {
            let id = handle.await.unwrap();
            received_ids.push(id);
        }
        received_ids.sort();

        for i in 0..5 {
            assert_eq!(received_ids[i], format!("concurrent-{}", i));
        }

        connection.close(0u32.into(), b"test done");
        let _ = shutdown_tx.send(true);
    }
}
