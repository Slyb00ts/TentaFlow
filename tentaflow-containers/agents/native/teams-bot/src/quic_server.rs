// =============================================================================
// Plik: quic_server.rs
// Opis: Serwer iroh kontenera meeting bot. Router laczy sie po `EndpointId`,
//       wysyla `ModelRequest` w length-prefixed rkyv; kontener odpowiada
//       `ModelResponse` albo strumieniuje `ModelStreamChunk`. Z tego samego
//       `Connection` kontener moze inicjowac `accept_bi → open_bi` w odwrotna
//       strone — `RouterClient` wysyla STT/TTS requesty do routera.
// =============================================================================

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use iroh::endpoint::Connection;
use iroh::SecretKey;
use tentaflow_protocol::*;
use tentaflow_transport::{
    build_server_endpoint, read_frame, write_frame, ServerEndpointConfig, ALPN_SERVICE,
};
use tokio::sync::{mpsc, oneshot, watch};
use tracing::{debug, error, info, warn};

/// Komenda sterujaca spotkaniem — wysylana z iroh do glownej petli w main.rs
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

/// Klient do wysylania requestow z sidecara do routera na istniejacym `Connection`.
/// Sidecar trzyma `Connection` ktore router nawiazal i `open_bi` w druga strone
/// otwiera nowy bidi stream obslugiwany przez routera (reverse_listener).
///
/// `current_meeting_id` ustawiane przy JoinMeeting, uzywane w ModelRequest.metadata
/// zeby router wiedzial do ktorego meetingu przypisac STT/diarization.
pub struct RouterClient {
    connection: Connection,
    current_meeting_id: Arc<parking_lot::Mutex<Option<String>>>,
}

/// Wynik wywolania `RouterClient::chat_completion` — text + rozwiazana nazwa
/// modelu (po alias resolution po stronie routera).
pub struct ChatCompletionResult {
    pub content: String,
    pub resolved_model: String,
}

impl RouterClient {
    pub fn new(connection: Connection) -> Self {
        Self {
            connection,
            current_meeting_id: Arc::new(parking_lot::Mutex::new(None)),
        }
    }

    pub fn set_meeting_id(&self, meeting_id: String) {
        *self.current_meeting_id.lock() = Some(meeting_id);
    }

    pub fn clear_meeting_id(&self) {
        *self.current_meeting_id.lock() = None;
    }

    pub fn current_meeting_id(&self) -> Option<String> {
        self.current_meeting_id.lock().clone()
    }

    /// Wysyla `ModelRequest` do routera i czeka na `ModelResponse`. Format
    /// ramki w obu kierunkach: `[u32 BE length][rkyv payload]`.
    pub async fn send_request(&self, request: &ModelRequest) -> Result<ModelResponse> {
        let (mut send, mut recv) = self
            .connection
            .open_bi()
            .await
            .map_err(|e| anyhow::anyhow!("open_bi do routera: {e}"))?;

        write_frame(&mut send, request)
            .await
            .map_err(|e| anyhow::anyhow!("write ModelRequest: {e}"))?;
        send.finish().ok();

        read_frame::<ModelResponse>(&mut recv)
            .await
            .map_err(|e| anyhow::anyhow!("read ModelResponse: {e}"))?
            .ok_or_else(|| anyhow::anyhow!("router zamknal stream bez odpowiedzi"))
    }

    /// Wysyla audio do STT przez router (alias podany w `model`).
    /// `extra_metadata` — dodatkowe pary klucz/wartosc (roster, active_speaker,
    /// timestamp_ms) — doklejane obok `meeting_id`.
    pub async fn transcribe(
        &self,
        audio_pcm: &[i16],
        model: &str,
        language: Option<String>,
        extra_metadata: Vec<(String, String)>,
    ) -> Result<String> {
        // Zero-copy reinterpretacja i16 -> u8 LE w jednym memcpy zamiast 2N alokacji
        // i osobnych zapisow per sample. Bezpieczne na little-endian (x86_64,
        // aarch64 — wszystkie hosty na ktorych deployujemy teams-bota).
        let audio_bytes: Vec<u8> = bytemuck::cast_slice::<i16, u8>(audio_pcm).to_vec();

        let mut meta: Vec<(String, String)> = Vec::new();
        if let Some(mid) = self.current_meeting_id() {
            meta.push(("meeting_id".to_string(), mid));
        }
        meta.extend(extra_metadata);
        let metadata = if meta.is_empty() { None } else { Some(meta) };

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
            metadata,
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

    /// Wysyla chat.completions request przez router. Router rozwiazuje alias
    /// -> konkretny model i zwraca wygenerowany tekst + rozwiazana nazwa modelu.
    /// Uzywane przez summarizer do generowania podsumowan transkryptu.
    pub async fn chat_completion(
        &self,
        model_alias: &str,
        messages: Vec<(String, String)>,
    ) -> Result<ChatCompletionResult> {
        let msgs: Vec<Message> = messages
            .into_iter()
            .map(|(role, content)| Message { role, content })
            .collect();

        let mut meta: Vec<(String, String)> = Vec::new();
        if let Some(mid) = self.current_meeting_id() {
            meta.push(("meeting_id".to_string(), mid));
        }
        let metadata = if meta.is_empty() { None } else { Some(meta) };

        let request = ModelRequest {
            request_id: uuid::Uuid::new_v4().to_string(),
            payload: ModelPayload::Completion(CompletionPayload {
                model: model_alias.to_string(),
                prompt: None,
                messages: msgs,
                temperature: Some(0.2),
                max_tokens: Some(1024),
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
            metadata,
            session_id: None,
        };

        let response = self.send_request(&request).await?;
        match response.result {
            ModelResult::Completion(c) => Ok(ChatCompletionResult {
                content: c.text,
                resolved_model: c.model,
            }),
            ModelResult::Error(e) => anyhow::bail!("chat_completion blad: {}", e.message),
            _ => anyhow::bail!("Nieoczekiwany typ odpowiedzi chat_completion"),
        }
    }

    /// Pobiera treść promptu z DB routera po `prompt_id` + język. Router robi
    /// fallback na `pl` jeśli wariant w żądanym języku nie istnieje.
    /// Zwraca treść promptu (pole `content`) — `name` i `resolved_language`
    /// są ignorowane, bo bot potrzebuje tylko treści do system message.
    pub async fn fetch_prompt(&self, prompt_id: &str, language: &str) -> Result<String> {
        let request = ModelRequest {
            request_id: uuid::Uuid::new_v4().to_string(),
            payload: ModelPayload::PromptFetch(PromptFetchRequest {
                prompt_id: prompt_id.to_string(),
                language: language.to_string(),
            }),
            stream: false,
            metadata: None,
            session_id: None,
        };

        let response = self.send_request(&request).await?;
        match response.result {
            ModelResult::PromptFetched(p) => Ok(p.content),
            ModelResult::Error(e) => anyhow::bail!("PromptFetch blad: {}", e.message),
            _ => anyhow::bail!("Nieoczekiwany typ odpowiedzi PromptFetch"),
        }
    }

    /// Wysyla MeetingEvent (SummaryUpdate albo ActionItemsUpdate) do routera.
    /// Router persistuje w tabelach `meeting_summaries` / `meeting_action_items`
    /// przez `persist_meeting_event` w reverse_request.rs.
    pub async fn send_meeting_event(
        &self,
        meeting_key: &str,
        timestamp_ms: i64,
        payload: MeetingEventPayload,
    ) -> Result<()> {
        let request = ModelRequest {
            request_id: uuid::Uuid::new_v4().to_string(),
            payload: ModelPayload::MeetingEvent(MeetingEventData {
                meeting_key: meeting_key.to_string(),
                timestamp_ms,
                payload,
            }),
            stream: false,
            metadata: None,
            session_id: None,
        };

        let response = self.send_request(&request).await?;
        match response.result {
            ModelResult::Error(e) => anyhow::bail!("MeetingEvent blad: {}", e.message),
            _ => Ok(()),
        }
    }

    /// Streamujace chat completion: otwiera bidi stream do routera z
    /// `stream=true`, czyta `ModelStreamChunk` w petli i wola `on_delta(text)`
    /// dla kazdego `TextDelta`. Zwraca pelny zaakumulowany tekst (do logow /
    /// summary). Pozwala botowi parsowac granice zdan i odpalac TTS dla
    /// pierwszego zdania zanim LLM dokonczy generowanie reszty.
    ///
    /// Kontrakt callbacka: synchronny, nie powinien blokowac (router
    /// generuje delty tak szybko jak LLM je produkuje).
    pub async fn chat_completion_stream<F>(
        &self,
        model_alias: &str,
        messages: Vec<(String, String)>,
        mut on_delta: F,
    ) -> Result<String>
    where
        F: FnMut(&str),
    {
        let msgs: Vec<Message> = messages
            .into_iter()
            .map(|(role, content)| Message { role, content })
            .collect();

        let mut meta: Vec<(String, String)> = Vec::new();
        if let Some(mid) = self.current_meeting_id() {
            meta.push(("meeting_id".to_string(), mid));
        }
        let metadata = if meta.is_empty() { None } else { Some(meta) };

        let request = ModelRequest {
            request_id: uuid::Uuid::new_v4().to_string(),
            payload: ModelPayload::Completion(CompletionPayload {
                model: model_alias.to_string(),
                prompt: None,
                messages: msgs,
                temperature: Some(0.2),
                max_tokens: Some(1024),
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
            stream: true,
            metadata,
            session_id: None,
        };

        let (mut send, mut recv) = self
            .connection
            .open_bi()
            .await
            .map_err(|e| anyhow::anyhow!("open_bi do routera (chat stream): {e}"))?;

        write_frame(&mut send, &request)
            .await
            .map_err(|e| anyhow::anyhow!("write ModelRequest (chat stream): {e}"))?;
        send.finish().ok();

        let mut full_text = String::with_capacity(512);
        loop {
            let chunk = read_frame::<ModelStreamChunk>(&mut recv)
                .await
                .map_err(|e| anyhow::anyhow!("read ModelStreamChunk (chat): {e}"))?;
            let Some(chunk) = chunk else {
                // Strumien zamkniety bez Done — traktujemy jako koniec
                // (router moze zakonczyc FIN-em po ostatnim TextDelta).
                return Ok(full_text);
            };
            match chunk.chunk {
                StreamChunkType::TextDelta(delta) => {
                    on_delta(&delta);
                    full_text.push_str(&delta);
                }
                StreamChunkType::Done { .. } => return Ok(full_text),
                StreamChunkType::Error(err) => {
                    anyhow::bail!("chat stream blad: {}", err.message);
                }
                // Pozostale typy (Metadata, ReasoningDelta, IntentInfo) nie sa
                // tu konsumowane — bot karmi sentence-buffer wylacznie wlasciwa
                // tresc odpowiedzi.
                other => {
                    debug!("chat stream: pominiety chunk: {:?}", other);
                }
            }
        }
    }

    /// Streamujaca synteza mowy: otwiera bidi stream do routera, wysyla
    /// `ModelRequest` z `stream=true`, czyta `ModelStreamChunk` w petli.
    /// Dla kazdego `AudioChunk(bytes)` wola `on_chunk(pcm)` — caller
    /// dostaje raw PCM (16 kHz mono i16 LE) i moze pchac do mikrofonu na
    /// biezaco zamiast czekac na pelny bufor.
    ///
    /// Kontrakt callbacka: zwroc `Ok(())` zeby kontynuowac, `Err(_)` zeby
    /// zerwac strumien. Callback nie powinien blokowac — backend produkuje
    /// chunki tak szybko jak potrafi sie.
    pub async fn synthesize_stream<F>(
        &self,
        text: &str,
        voice: &str,
        model: &str,
        mut on_chunk: F,
    ) -> Result<()>
    where
        F: FnMut(Vec<u8>) -> Result<()>,
    {
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
            stream: true,
            metadata: None,
            session_id: None,
        };

        let (mut send, mut recv) = self
            .connection
            .open_bi()
            .await
            .map_err(|e| anyhow::anyhow!("open_bi do routera (TTS stream): {e}"))?;

        write_frame(&mut send, &request)
            .await
            .map_err(|e| anyhow::anyhow!("write ModelRequest (TTS stream): {e}"))?;
        send.finish().ok();

        loop {
            let chunk = read_frame::<ModelStreamChunk>(&mut recv)
                .await
                .map_err(|e| anyhow::anyhow!("read ModelStreamChunk: {e}"))?;
            let Some(chunk) = chunk else {
                // Strumien zamkniety bez Done — traktujemy jako koniec
                // (sztywne wymagania na Done dawalyby false-positive errory
                // przy benignym FIN od routera).
                return Ok(());
            };
            match chunk.chunk {
                StreamChunkType::AudioChunk(pcm) => {
                    on_chunk(pcm)?;
                }
                StreamChunkType::Done { .. } => return Ok(()),
                StreamChunkType::Error(err) => {
                    anyhow::bail!("TTS stream blad: {}", err.message);
                }
                // Pozostale typy (TextDelta, Metadata itp.) dla TTS sa
                // nieoczekiwane — logujemy i ignorujemy zamiast bail!,
                // zeby przejsciowe nieoczekiwane chunki nie zrywaly sesji.
                other => {
                    debug!("TTS stream: nieoczekiwany typ chunka: {:?}", other);
                }
            }
        }
    }
}

/// Konfiguracja serwera iroh kontenera
#[derive(Debug, Clone)]
pub struct ContainerTransportConfig {
    pub port: u16,
    pub secret_key_path: Option<String>,
    pub secret_key_hex: Option<String>,
    pub enable_lan_discovery: bool,
    pub enable_dht_discovery: bool,
}

impl Default for ContainerTransportConfig {
    fn default() -> Self {
        Self {
            port: 5000,
            secret_key_path: None,
            secret_key_hex: None,
            enable_lan_discovery: true,
            enable_dht_discovery: true,
        }
    }
}

/// Slot na aktywną stronę Chromium — dzielony między main loopem (ustawia po
/// join, czyści po leave) a handlerami QUIC (czytają przy
/// `ModelPayload::Browser`). `chromiumoxide::Page` jest tanią Arc-kopią, więc
/// trzymanie jej tutaj nie rywalizuje z main loopem.
pub type PageSlot = Arc<tokio::sync::Mutex<Option<chromiumoxide::Page>>>;

/// Serwer iroh meeting bota.
pub struct MeetingQuicServer {
    config: ContainerTransportConfig,
    transcript_tx: mpsc::UnboundedSender<(String, String, u64)>,
    transcript_rx: Arc<tokio::sync::Mutex<mpsc::UnboundedReceiver<(String, String, u64)>>>,
    router_client: Arc<tokio::sync::Mutex<Option<Arc<RouterClient>>>>,
    command_tx: mpsc::UnboundedSender<MeetingCommand>,
    command_rx: Arc<tokio::sync::Mutex<Option<mpsc::UnboundedReceiver<MeetingCommand>>>>,
    page: PageSlot,
}

impl MeetingQuicServer {
    pub fn new(config: ContainerTransportConfig) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        Self {
            config,
            transcript_tx: tx,
            transcript_rx: Arc::new(tokio::sync::Mutex::new(rx)),
            router_client: Arc::new(tokio::sync::Mutex::new(None)),
            command_tx: cmd_tx,
            command_rx: Arc::new(tokio::sync::Mutex::new(Some(cmd_rx))),
            page: Arc::new(tokio::sync::Mutex::new(None)),
        }
    }

    /// Handle do slotu aktywnej strony Chromium. Main loop używa go do
    /// `set`/`clear` przy join/leave; QUIC handler czyta przy browser capture.
    pub fn page_slot(&self) -> PageSlot {
        self.page.clone()
    }

    pub fn transcript_sender(&self) -> mpsc::UnboundedSender<(String, String, u64)> {
        self.transcript_tx.clone()
    }

    pub async fn command_receiver(&self) -> Option<mpsc::UnboundedReceiver<MeetingCommand>> {
        self.command_rx.lock().await.take()
    }

    pub async fn router_client(&self) -> Option<Arc<RouterClient>> {
        self.router_client.lock().await.clone()
    }

    pub fn router_client_handle(&self) -> Arc<tokio::sync::Mutex<Option<Arc<RouterClient>>>> {
        self.router_client.clone()
    }

    /// Uruchamia iroh endpoint i nasluchuje na polaczenia od routera.
    pub async fn run(&self, mut shutdown_rx: watch::Receiver<bool>) -> Result<()> {
        let secret_key = if let Some(hex) = self.config.secret_key_hex.as_deref() {
            load_secret_key_from_hex(hex)?
        } else {
            load_or_generate_secret_key(self.config.secret_key_path.as_deref())?
        };
        let bind_addr: SocketAddr = format!("0.0.0.0:{}", self.config.port)
            .parse()
            .context("Nieprawidlowy bind addr")?;

        let endpoint = build_server_endpoint(ServerEndpointConfig {
            secret_key,
            bind_addr,
            alpns: vec![ALPN_SERVICE.to_vec()],
            relay_url: None,
            enable_lan_discovery: self.config.enable_lan_discovery,
            enable_dht_discovery: self.config.enable_dht_discovery,
        })
        .await
        .context("iroh endpoint bind")?;

        info!(
            endpoint_id = %endpoint.id().fmt_short(),
            port = self.config.port,
            "Meeting bot iroh endpoint nasluchuje"
        );

        loop {
            tokio::select! {
                incoming = endpoint.accept() => {
                    let Some(incoming) = incoming else {
                        warn!("iroh endpoint zwrocil None");
                        break;
                    };

                    let transcript_rx = self.transcript_rx.clone();
                    let router_client_slot = self.router_client.clone();
                    let command_tx = self.command_tx.clone();
                    let page_slot = self.page.clone();

                    tokio::spawn(async move {
                        match incoming.await {
                            Ok(connection) => {
                                let remote = connection.remote_id();
                                debug!(remote = %remote.fmt_short(), "Router polaczony");
                                Self::handle_connection(
                                    connection,
                                    transcript_rx,
                                    router_client_slot,
                                    command_tx,
                                    page_slot,
                                ).await;
                            }
                            Err(e) => {
                                error!("iroh handshake nieudany: {e}");
                            }
                        }
                    });
                }
                _ = shutdown_rx.changed() => {
                    if *shutdown_rx.borrow() {
                        info!("Zamykam iroh endpoint...");
                        endpoint.close().await;
                        info!("iroh endpoint zamkniety");
                        break;
                    }
                }
            }
        }
        Ok(())
    }

    /// Obsluguje pojedyncze polaczenie od routera.
    async fn handle_connection(
        connection: Connection,
        transcript_rx: Arc<tokio::sync::Mutex<mpsc::UnboundedReceiver<(String, String, u64)>>>,
        router_client_slot: Arc<tokio::sync::Mutex<Option<Arc<RouterClient>>>>,
        command_tx: mpsc::UnboundedSender<MeetingCommand>,
        page_slot: PageSlot,
    ) {
        let remote = connection.remote_id();

        // Zapisz RouterClient — to samo polaczenie sluzy do przyjmowania
        // requestow (accept_bi) i do wysylania requestow (open_bi).
        let client = Arc::new(RouterClient::new(connection.clone()));
        {
            let mut slot = router_client_slot.lock().await;
            *slot = Some(client);
        }
        info!(remote = %remote.fmt_short(), "RouterClient gotowy");

        loop {
            match connection.accept_bi().await {
                Ok((send, recv)) => {
                    let transcript_rx = transcript_rx.clone();
                    let command_tx = command_tx.clone();
                    let page_slot = page_slot.clone();
                    tokio::spawn(async move {
                        if let Err(e) = Self::handle_stream(send, recv, transcript_rx, command_tx, page_slot).await {
                            debug!("handle_stream: {e}");
                        }
                    });
                }
                Err(e) => {
                    debug!(remote = %remote.fmt_short(), "Polaczenie zamkniete: {e}");
                    break;
                }
            }
        }

        let mut slot = router_client_slot.lock().await;
        *slot = None;
        info!(remote = %remote.fmt_short(), "RouterClient usuniety");
    }

    /// Obsluguje pojedynczy bidi stream — odczytuje `ModelRequest`, dispatch,
    /// odsyla `ModelResponse` albo strumieniuje `ModelStreamChunk`.
    async fn handle_stream(
        mut send: iroh::endpoint::SendStream,
        mut recv: iroh::endpoint::RecvStream,
        transcript_rx: Arc<tokio::sync::Mutex<mpsc::UnboundedReceiver<(String, String, u64)>>>,
        command_tx: mpsc::UnboundedSender<MeetingCommand>,
        page_slot: PageSlot,
    ) -> Result<()> {
        let request: ModelRequest = read_frame(&mut recv)
            .await
            .map_err(|e| anyhow::anyhow!("read ModelRequest: {e}"))?
            .ok_or_else(|| anyhow::anyhow!("peer zamknal stream bez requestu"))?;

        debug!(
            request_id = %request.request_id,
            stream = request.stream,
            "ModelRequest odebrany"
        );

        if request.stream {
            Self::handle_streaming_request(&request, &mut send, transcript_rx).await?;
        } else {
            let response = if let ModelPayload::Browser(payload) = &request.payload {
                Self::process_browser(&request.request_id, payload, &page_slot).await
            } else if let Some(cmd_response) =
                Self::try_handle_tool_command(&request, &command_tx).await
            {
                cmd_response
            } else {
                Self::process_request(&request)
            };

            write_frame(&mut send, &response)
                .await
                .map_err(|e| anyhow::anyhow!("write ModelResponse: {e}"))?;
            send.finish().ok();
        }
        Ok(())
    }

    /// Obsluguje `ModelPayload::Browser` — screenshot albo snapshot DOM aktywnej
    /// strony Chromium. Brak aktywnej strony, timeout CDP albo blad evaluate
    /// mapuja sie na `BrowserResult::Error` (tunnel dashboard↔bot zostaje).
    async fn process_browser(
        request_id: &str,
        payload: &BrowserPayload,
        page_slot: &PageSlot,
    ) -> ModelResponse {
        use std::time::Duration;
        use tokio::time::timeout;

        // Krotki timeout na samo pozyczenie strony — main loop robi swap tylko
        // przy join/leave, wiec zwykle to non-blocking; 1s zabezpiecza przed
        // deadlockiem gdyby main loop trzymal slot dluzej.
        let guard = match timeout(Duration::from_secs(1), page_slot.lock()).await {
            Ok(g) => g,
            Err(_) => return browser_error(request_id, "page slot lock timeout"),
        };
        let Some(page) = guard.as_ref() else {
            return browser_error(request_id, "no active page");
        };

        // Klonujemy handle (Page = Arc) i dropujemy guard przed dlugimi await —
        // nie blokujemy join/leave podczas screenshotu full_page.
        let page = page.clone();
        drop(guard);

        // 10s budzet CDP: full_page screenshot na Teams UI potrafi trwac kilka
        // sekund (scroll + stitch). Dashboard caller (backend) ma wlasny 12s.
        let deadline = Duration::from_secs(10);
        let result = match &payload.operation {
            BrowserOperation::Screenshot { full_page } => {
                let fp = *full_page;
                match timeout(deadline, capture_screenshot(&page, fp)).await {
                    Ok(Ok(png)) => BrowserResult::Screenshot { png },
                    Ok(Err(e)) => BrowserResult::Error { message: format!("screenshot: {e}") },
                    Err(_) => BrowserResult::Error { message: "screenshot timeout (10s)".into() },
                }
            }
            BrowserOperation::Dom => {
                match timeout(deadline, capture_dom(&page)).await {
                    Ok(Ok(html)) => BrowserResult::Dom { html },
                    Ok(Err(e)) => BrowserResult::Error { message: format!("dom: {e}") },
                    Err(_) => BrowserResult::Error { message: "dom snapshot timeout (10s)".into() },
                }
            }
        };

        ModelResponse {
            request_id: request_id.to_string(),
            result: ModelResult::Browser(result),
            metrics: None,
        }
    }

    /// Sprawdza czy request zawiera komende narzedzia w polu prompt.
    async fn try_handle_tool_command(
        request: &ModelRequest,
        command_tx: &mpsc::UnboundedSender<MeetingCommand>,
    ) -> Option<ModelResponse> {
        let payload = match &request.payload {
            ModelPayload::Completion(p) => p,
            _ => return None,
        };

        let prompt = payload.prompt.as_deref()?;
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
                    return Some(error_resp(
                        &request.request_id,
                        ErrorType::InvalidRequest,
                        "Brak meeting_url w parametrach join_meeting",
                    ));
                }

                info!(meeting_url = %meeting_url, "Komenda: join_meeting");
                MeetingCommand::JoinMeeting {
                    meeting_url,
                    response_tx,
                }
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
                warn!(tool = tool, "Nieznana komenda");
                return Some(error_resp(
                    &request.request_id,
                    ErrorType::InvalidRequest,
                    &format!("Nieznana komenda: {tool}"),
                ));
            }
        };

        if command_tx.send(cmd).is_err() {
            return Some(error_resp(
                &request.request_id,
                ErrorType::InternalError,
                "Kanal komend zamkniety",
            ));
        }

        let result_text = match tokio::time::timeout(std::time::Duration::from_secs(300), response_rx).await {
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

    /// Przetwarza jednorazowy ModelRequest.
    fn process_request(request: &ModelRequest) -> ModelResponse {
        match &request.payload {
            ModelPayload::Completion(payload) => {
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
            ModelPayload::Audio(payload) => match &payload.operation {
                AudioOperation::STT { .. } => ModelResponse {
                    request_id: request.request_id.clone(),
                    result: ModelResult::Audio(AudioResult {
                        data: AudioResultData::Text(String::new()),
                        model: "meeting-bot-stt".to_string(),
                    }),
                    metrics: None,
                },
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
                _ => error_resp(
                    &request.request_id,
                    ErrorType::InvalidRequest,
                    "Meeting bot nie obsluguje tej operacji audio",
                ),
            },
            _ => error_resp(
                &request.request_id,
                ErrorType::InvalidRequest,
                "Meeting bot nie obsluguje tego typu requestu",
            ),
        }
    }

    /// Streaming — wysyla chunki transkrypcji na zywo przez length-prefixed rkyv.
    async fn handle_streaming_request(
        request: &ModelRequest,
        send: &mut iroh::endpoint::SendStream,
        transcript_rx: Arc<tokio::sync::Mutex<mpsc::UnboundedReceiver<(String, String, u64)>>>,
    ) -> Result<()> {
        let request_id = &request.request_id;
        debug!(request_id = %request_id, "Streaming: start");

        let mut rx = transcript_rx.lock().await;
        while let Some((speaker, text, _timestamp_ms)) = rx.recv().await {
            let chunk_text = format!("[{}]: {}\n", speaker, text);
            let chunk = ModelStreamChunk {
                request_id: request_id.clone(),
                chunk: StreamChunkType::TextDelta(chunk_text),
            };
            write_frame(send, &chunk)
                .await
                .map_err(|e| anyhow::anyhow!("write chunk: {e}"))?;
        }

        let done_chunk = ModelStreamChunk {
            request_id: request_id.clone(),
            chunk: StreamChunkType::Done { final_metrics: None },
        };
        write_frame(send, &done_chunk)
            .await
            .map_err(|e| anyhow::anyhow!("write done chunk: {e}"))?;
        send.finish().ok();

        debug!(request_id = %request_id, "Streaming: zakonczony");
        Ok(())
    }
}

/// Laduje Ed25519 `SecretKey` z pliku albo generuje nowy (i zapisuje, jesli
/// `path` podana). Brak `path` = ephemeral — po restarcie nowy `EndpointId`.
/// Dekoduje Ed25519 secret key z 64-znakowego hex. Używany gdy env
/// `BOT_SECRET_KEY_HEX` jest ustawione (np. kontener odpalony z MeetingManagera).
fn load_secret_key_from_hex(hex: &str) -> Result<SecretKey> {
    let trimmed = hex.trim();
    if trimmed.len() != 64 {
        anyhow::bail!(
            "BOT_SECRET_KEY_HEX ma {} znakow, wymagane 64 (32 bajty hex)",
            trimmed.len()
        );
    }
    let bytes = hex::decode(trimmed).context("dekodowanie BOT_SECRET_KEY_HEX")?;
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&bytes);
    info!("Wczytano Ed25519 secret key z env BOT_SECRET_KEY_HEX");
    Ok(SecretKey::from_bytes(&arr))
}

fn load_or_generate_secret_key(path: Option<&str>) -> Result<SecretKey> {
    let Some(path) = path else {
        warn!("Brak secret_key_path — generuje ephemeral key");
        return Ok(SecretKey::generate());
    };

    let path = Path::new(path);
    if path.exists() {
        let bytes = std::fs::read(path)
            .with_context(|| format!("wczytanie klucza z {}", path.display()))?;
        if bytes.len() != 32 {
            anyhow::bail!(
                "plik klucza {} ma {} bajtow, wymagane 32",
                path.display(),
                bytes.len()
            );
        }
        let mut arr = [0u8; 32];
        arr.copy_from_slice(&bytes);
        info!(path = %path.display(), "wczytano Ed25519 secret key");
        return Ok(SecretKey::from_bytes(&arr));
    }

    let key = SecretKey::generate();
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).ok();
        }
    }
    std::fs::write(path, key.to_bytes())
        .with_context(|| format!("zapis klucza do {}", path.display()))?;
    info!(path = %path.display(), "wygenerowano i zapisano Ed25519 secret key");
    Ok(key)
}

async fn capture_screenshot(page: &chromiumoxide::Page, full_page: bool) -> Result<Vec<u8>> {
    use chromiumoxide::cdp::browser_protocol::page::CaptureScreenshotFormat;
    use chromiumoxide::page::ScreenshotParams;
    let params = ScreenshotParams::builder()
        .format(CaptureScreenshotFormat::Png)
        .full_page(full_page)
        .build();
    page.screenshot(params)
        .await
        .map_err(|e| anyhow::anyhow!("page.screenshot: {e}"))
}

async fn capture_dom(page: &chromiumoxide::Page) -> Result<String> {
    let result = page
        .evaluate("document.documentElement.outerHTML")
        .await
        .map_err(|e| anyhow::anyhow!("page.evaluate: {e}"))?;
    let html: String = result
        .into_value()
        .map_err(|e| anyhow::anyhow!("evaluate into_value: {e}"))?;
    Ok(html)
}

fn browser_error(request_id: &str, message: &str) -> ModelResponse {
    ModelResponse {
        request_id: request_id.to_string(),
        result: ModelResult::Browser(BrowserResult::Error {
            message: message.to_string(),
        }),
        metrics: None,
    }
}

fn error_resp(request_id: &str, error_type: ErrorType, message: &str) -> ModelResponse {
    ModelResponse {
        request_id: request_id.to_string(),
        result: ModelResult::Error(ErrorInfo {
            error_type,
            message: message.to_string(),
            details: None,
        }),
        metrics: None,
    }
}
