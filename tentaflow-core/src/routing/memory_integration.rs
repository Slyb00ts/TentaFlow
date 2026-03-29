// =============================================================================
// Plik: routing/memory_integration.rs
// Opis: Integracja Memory Analyzer z Router — analiza zapytan, odpytywanie
//       Memory Engine via QUIC, wstrzykiwanie kontekstu do promptow,
//       asynchroniczny zapis odpowiedzi, cache historii konwersacji.
// =============================================================================

//! Memory Integration - integracja Memory Analyzer z Router
//!
//! Ten modul laczy:
//! - MemoryAnalyzer (bielik-1.5b) - decyduje czy/jak odpytac Memory
//! - Memory Engine (QUIC) - przechowuje graf wiedzy
//! - Router - wstrzykuje memory_context do promptow
//!
//! Flow dla kazdego request:
//! 1. MemoryAnalyzer.analyze_query() -> QueryDecision
//! 2. Jesli should_query=true -> Memory.Query via QUIC
//! 3. Build memory_context z wynikow
//! 4. Inject do system message
//! 5. (async) Po odpowiedzi: MemoryAnalyzer.analyze_for_storage() -> Store via QUIC

use crate::error::{Result, CoreError};
use crate::memory_analyzer::{
    MemoryAnalyzer, MemoryAnalyzerConfig, MemoryContext, MemoryNodeInfo,
    MemoryQueryType, MemoryRelationInfo, QueryDecision, StoreDecision,
};
use crate::api::openai::types::{ChatCompletionRequest, Message, MessageContent};
use crate::routing::service_manager::ServiceManager;
use tentaflow_protocol::{
    AudioOperation, AudioPayload, MemoryOperation, MemoryPayload,
    MemoryQueryType as ProtocolQueryType, MemoryResultType,
    ModelPayload, ModelRequest, ModelResult,
};
use regex::Regex;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tracing::{debug, error, warn};

/// Skompilowane regexy do wykrywania przedstawienia sie / korekty imienia.
/// Kompilowane raz przy pierwszym uzyciu (LazyLock) zamiast przy kazdym request.
static NAME_INTRO_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        r"(?i)(?:ja\s+)?jestem\s+([A-ZĄĆĘŁŃÓŚŹŻ][a-ząćęłńóśźż]+)",
        r"(?i)mam\s+na\s+imi[eę]\s+([A-ZĄĆĘŁŃÓŚŹŻ][a-ząćęłńóśźż]+)",
        r"(?i)nazywam\s+si[eę]\s+([A-ZĄĆĘŁŃÓŚŹŻ][a-ząćęłńóśźż]+)",
        r"(?i)moje\s+imi[eę]\s+to\s+([A-ZĄĆĘŁŃÓŚŹŻ][a-ząćęłńóśźż]+)",
        r"(?i)^to\s+([A-ZĄĆĘŁŃÓŚŹŻ][a-ząćęłńóśźż]+)$",
    ]
    .iter()
    .filter_map(|p| Regex::new(p).ok())
    .collect()
});

static NAME_CORRECTION_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        r"(?i)(?:to\s+)?nie\s+[A-ZĄĆĘŁŃÓŚŹŻ][a-ząćęłńóśźż]+[,\s]+(?:jestem\s+)?([A-ZĄĆĘŁŃÓŚŹŻ][a-ząćęłńóśźż]+)",
        r"(?i)nie\s+mam\s+na\s+imi[eę]\s+[A-ZĄĆĘŁŃÓŚŹŻ][a-ząćęłńóśźż]+[,\s]+(?:tylko\s+)?([A-ZĄĆĘŁŃÓŚŹŻ][a-ząćęłńóśźż]+)",
    ]
    .iter()
    .filter_map(|p| Regex::new(p).ok())
    .collect()
});

/// Maksymalna liczba wiadomosci w historii per sesja
const MAX_HISTORY_MESSAGES: usize = 20;
/// Czas zycia sesji bez aktywnosci (30 minut)
const SESSION_TTL_SECS: u64 = 1800;

/// Czasy operacji Memory Integration (dla logowania)
#[derive(Debug, Clone, Default)]
pub struct MemoryTimings {
    /// Czas analizy zapytania przez bielik-1.5b
    pub query_analysis_ms: Option<u64>,
    /// Czas zapytania do Memory Engine (QUIC)
    pub memory_query_ms: Option<u64>,
}

/// Pojedyncza wiadomosc w historii konwersacji
#[derive(Debug, Clone)]
pub struct ConversationMessage {
    pub role: String,      // "user" lub "assistant"
    pub content: String,
    pub timestamp: Instant,
}

/// Historia konwersacji dla sesji
#[derive(Debug)]
struct SessionHistory {
    messages: VecDeque<ConversationMessage>,
    last_activity: Instant,
}

impl SessionHistory {
    fn new() -> Self {
        Self {
            messages: VecDeque::new(),
            last_activity: Instant::now(),
        }
    }

    fn add_message(&mut self, role: &str, content: &str) {
        self.messages.push_back(ConversationMessage {
            role: role.to_string(),
            content: content.to_string(),
            timestamp: Instant::now(),
        });
        self.last_activity = Instant::now();

        // Zachowaj tylko ostatnie N wiadomosci
        if self.messages.len() > MAX_HISTORY_MESSAGES {
            self.messages.pop_front();
        }
    }

    fn is_expired(&self) -> bool {
        self.last_activity.elapsed() > Duration::from_secs(SESSION_TTL_SECS)
    }
}

/// Cache historii konwersacji per session_id
pub struct ConversationCache {
    sessions: RwLock<HashMap<String, SessionHistory>>,
}

impl ConversationCache {
    pub fn new() -> Self {
        Self {
            sessions: RwLock::new(HashMap::new()),
        }
    }

    /// Dodaje wiadomosc do historii sesji
    pub async fn add_message(&self, session_id: &str, role: &str, content: &str) {
        let mut sessions = self.sessions.write().await;
        if !sessions.contains_key(session_id) {
            sessions.insert(session_id.to_string(), SessionHistory::new());
        }
        let history = sessions.get_mut(session_id).unwrap();
        history.add_message(role, content);
        debug!(
            "ConversationCache: added {} message to session {}, total: {}",
            role,
            session_id,
            history.messages.len()
        );
    }

    /// Pobiera historie konwersacji dla sesji
    pub async fn get_history(&self, session_id: &str) -> Vec<ConversationMessage> {
        let sessions = self.sessions.read().await;
        sessions
            .get(session_id)
            .map(|h| h.messages.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Czysci przeterminowane sesje
    pub async fn cleanup_expired(&self) {
        let mut sessions = self.sessions.write().await;
        let before = sessions.len();
        sessions.retain(|_, h| !h.is_expired());
        let removed = before - sessions.len();
        if removed > 0 {
            debug!("ConversationCache: cleaned up {} expired sessions", removed);
        }
    }
}

impl Default for ConversationCache {
    fn default() -> Self {
        Self::new()
    }
}

/// Memory Integration - glowny interfejs dla integracji Memory z Router
pub struct MemoryIntegration {
    /// Memory Analyzer (bielik-1.5b)
    analyzer: MemoryAnalyzer,
    /// Service Manager - dostep do QUIC clients
    service_manager: Arc<ServiceManager>,
    /// Cache historii konwersacji per session_id
    conversation_cache: Arc<ConversationCache>,
}

impl MemoryIntegration {
    /// Tworzy nowa integracje Memory
    pub fn new(service_manager: Arc<ServiceManager>, config: Option<MemoryAnalyzerConfig>) -> Self {
        Self {
            analyzer: MemoryAnalyzer::new(service_manager.clone(), config),
            conversation_cache: service_manager.conversation_cache.clone(),
            service_manager,
        }
    }

    /// Zwraca referencje do cache historii (do uzycia przez Router)
    pub fn conversation_cache(&self) -> Arc<ConversationCache> {
        self.conversation_cache.clone()
    }

    /// Przetwarza request przed wyslaniem do glownego modelu
    ///
    /// Flow:
    /// 1. Sprawdz czy memory_options.enabled
    /// 2. Wywolaj MemoryAnalyzer.analyze_query()
    /// 3. Jesli should_query -> odpytaj Memory via QUIC
    /// 4. Zbuduj memory_context
    /// 5. Zmodyfikuj request (inject context do system message)
    ///
    /// Zwraca: (zmodyfikowany request, query_decision, timings)
    pub async fn process_request(
        &self,
        mut request: ChatCompletionRequest,
    ) -> Result<(ChatCompletionRequest, Option<QueryDecision>, MemoryTimings)> {
        let mut timings = MemoryTimings::default();

        // Wyciagnij wszystkie potrzebne dane z memory_opts zanim zrobimy mutable operations
        let (session_id, person_id, speaker_confidence, session_context) = {
            match &request.memory_options {
                Some(opts) if opts.enabled.unwrap_or(true) && opts.query_enabled.unwrap_or(true) => {
                    let session_id = opts.session_id.clone().unwrap_or_else(|| "default".to_string());
                    let person_id = opts.person_id.clone();
                    let speaker_confidence = opts.speaker_confidence.unwrap_or(0.0);
                    let session_context = opts.session_context.clone();
                    (session_id, person_id, speaker_confidence, session_context)
                }
                _ => {
                    debug!("Memory disabled lub query_enabled=false, pomijam");
                    return Ok((request, None, timings));
                }
            }
        };

        // Wyciagnij ostatnia wiadomosc uzytkownika
        let user_message = self.extract_last_user_message(&request.messages);
        if user_message.is_empty() {
            debug!("Brak wiadomosci uzytkownika, pomijam Memory");
            return Ok((request, None, timings));
        }

        // === CONVERSATION HISTORY: Dodaj user message i wstrzyknij historie ===
        // Najpierw pobierz poprzednia historie (przed dodaniem aktualnej wiadomosci)
        let history = self.conversation_cache.get_history(&session_id).await;
        let is_first_message = history.is_empty();

        if !is_first_message {
            debug!(
                "Injecting {} messages from conversation history for session {}",
                history.len(),
                session_id
            );
            self.inject_conversation_history(&mut request, &history);
        }

        // Oblicz raz - uzywane w inject_session_context i ponizej przy obsludze nieznanego glosu
        let message_is_noise = self.is_likely_noise(&user_message);

        // Wstrzyknij kontekst sesji - informuje LLM czy to nowa rozmowa czy kontynuacja
        self.inject_session_context(&mut request, is_first_message, message_is_noise);

        // Dodaj aktualna wiadomosc user do historii
        self.conversation_cache
            .add_message(&session_id, "user", &user_message)
            .await;

        // === KROK -1: Sprawdz czy uzytkownik koryguje swoje imie ===
        // Jesli mamy person_id (rozpoznany glos, niepusty) i uzytkownik mowi "jestem X" lub "mam na imie X",
        // to zaktualizuj imie w bazie glosow i w Memory
        if let Some(ref voice_id) = person_id {
            // Pomijamy puste voice_id
            if voice_id.is_empty() {
                debug!("Skipping name correction - empty voice_id");
            } else if let Some(new_name) = self.detect_name_correction(&user_message) {
                debug!(
                    "Name correction detected: '{}' for voice_id={}",
                    new_name, voice_id
                );

                // Aktualizuj asynchronicznie - nie blokujemy odpowiedzi
                let voice_id_clone = voice_id.clone();
                let session_id_clone = session_id.clone();
                let new_name_clone = new_name.clone();
                let service_manager = self.service_manager.clone();

                tokio::spawn(async move {
                    // 1. Aktualizuj speaker_name w STT service (baza glosow)
                    if let Err(e) = Self::update_speaker_name_static(
                        &service_manager,
                        &voice_id_clone,
                        &new_name_clone,
                    ).await {
                        error!("Failed to update speaker name in voice DB: {}", e);
                    } else {
                        debug!("Speaker name updated in voice DB: {} -> {}", voice_id_clone, new_name_clone);
                    }

                    // 2. Aktualizuj w Memory (graf wiedzy)
                    if let Err(e) = Self::update_person_name_in_memory_static(
                        &service_manager,
                        &session_id_clone,
                        &voice_id_clone,
                        &new_name_clone,
                    ).await {
                        error!("Failed to update person name in Memory: {}", e);
                    } else {
                        debug!("Person name updated in Memory: voice_id={} -> {}", voice_id_clone, new_name_clone);
                    }
                });
            }
        }

        let t_total_start = Instant::now();

        // === KROK 0: Obsluga rozpoznawania glosu ===
        let mut user_recognized = false;

        if let Some(person_id) = &person_id {
            // Tylko dla wysokiej pewnosci (>0.60) probuj rozpoznac
            if speaker_confidence > 0.60 {
                debug!(
                    "Voice recognition: person_id={}, confidence={:.2}",
                    person_id, speaker_confidence
                );

                let t_voice = Instant::now();
                match self.get_person_context_by_voice(&session_id, person_id).await {
                    Ok((person_context, person_name)) => {
                        debug!(
                            "get_person_context_by_voice took {:?}",
                            t_voice.elapsed()
                        );
                        if !person_context.formatted_context.is_empty() {
                            // Wstrzyknij kontekst osoby do request
                            self.inject_memory_context(&mut request, &person_context);
                            user_recognized = true;

                            debug!(
                                "Person context injected for: {} (confidence: {:.2})",
                                person_name.as_deref().unwrap_or("unknown"),
                                speaker_confidence
                            );

                            // Jesli bardzo wysoka pewnosc (>0.85), dodaj personalizacje do system message
                            if speaker_confidence > 0.85 {
                                if let Some(name) = person_name {
                                    self.inject_personalization(&mut request, &name, is_first_message);
                                }
                            }
                        } else {
                            // Memory nie ma kontekstu dla tej osoby - sprawdz czy mamy nazwe z Memory
                            // i uzyj jej do personalizacji
                            debug!("Memory returned empty context for person_id");
                            if speaker_confidence > 0.85 {
                                if let Some(name) = person_name {
                                    debug!("Using person_name from Memory (empty context): {}", name);
                                    self.inject_personalization(&mut request, &name, is_first_message);
                                    user_recognized = true;
                                }
                            }
                        }
                    }
                    Err(e) => {
                        debug!(
                            "get_person_context_by_voice FAILED after {:?}: {}",
                            t_voice.elapsed(), e
                        );
                        warn!("Failed to get person context: {} - continuing without it", e);
                    }
                }

                // FALLBACK: Jesli Memory nie ma kontekstu ale mamy speaker_name z STT,
                // wstrzyknij personalizacje na podstawie samego rozpoznania glosu
                if !user_recognized && speaker_confidence > 0.85 {
                    // Pobierz speaker_name z memory_opts (ustawione przez Router po speaker identification)
                    // Musimy sklonowac przed mutable borrow
                    let stt_speaker_name = request.memory_options
                        .as_ref()
                        .and_then(|opts| opts.speaker_name.clone());

                    if let Some(name) = stt_speaker_name {
                        debug!(
                            "Fallback personalization: speaker_name={} from STT (Memory empty)",
                            name
                        );
                        self.inject_personalization(&mut request, &name, is_first_message);
                        user_recognized = true;
                    }
                }
            }
        }

        // Jesli uzytkownik nierozpoznany (brak person_id lub niski confidence), dodaj info do system message
        // Ale jesli speaker zostal rozpoznany (person_id + confidence > 0.60), to nie jest "nieznany"
        let speaker_known = person_id.as_ref().map(|id| !id.is_empty()).unwrap_or(false) && speaker_confidence > 0.60;

        // Sprawdz czy jest hint o potwierdzeniu tozsamosci (MEDIUM confidence)
        // session_context moze zawierac hint np. "Prawdopodobnie Jan (85%) - zapytaj o potwierdzenie"
        let has_confirmation_hint = session_context.as_ref()
            .map(|ctx| ctx.contains("Prawdopodobnie") || ctx.contains("zapytaj o potwierdzenie"))
            .unwrap_or(false);

        // === NOWA LOGIKA: Inteligentna obsluga nieznanego glosu ===
        // 1. Szum/krzaki - ignoruj, nie pytaj o imie
        // 2. Przedstawienie sie ("jestem Marek") - zarejestruj glos z tym imieniem
        // 3. Nowy glos w trakcie rozmowy - delikatnie zapytaj
        // 4. Nowy glos na poczatku - pelne przedstawienie

        let self_intro_name = self.detect_self_introduction(&user_message);

        if !user_recognized && !speaker_known {
            if message_is_noise {
                // SZUM/KRZAKI - nie pytaj o imie, to tylko artefakt audio
                // LLM powinien poprosic o powtorzenie dzieki inject_session_context
                debug!("Detected noise/garbage in STT output, NOT asking for name");
                // Nie wstrzykuj unknown_user_context
            } else if let Some(ref intro_name) = self_intro_name {
                // Uzytkownik sie przedstawil - zarejestruj glos i potwierdz
                debug!("New speaker introduced themselves as: {}", intro_name);
                self.inject_new_speaker_introduced(&mut request, intro_name);

                // TODO: Tutaj powinnismy tez zarejestrowac glos z tym imieniem
                // Na razie zostawiamy to do store_analysis ktory wykryje przedstawienie
            } else if has_confirmation_hint {
                // MEDIUM confidence - mamy kandydata, ale potrzebujemy potwierdzenia
                self.inject_medium_confidence_context(&mut request, session_context.as_deref().unwrap_or(""));
                debug!("Medium confidence context injected (needs confirmation)");
            } else if !is_first_message {
                // Nowy glos W TRAKCIE rozmowy - delikatnie zapytaj
                debug!("New voice detected during conversation, asking gently");
                self.inject_new_voice_during_conversation(&mut request);
            } else {
                // LOW confidence na poczatku rozmowy - pelne przedstawienie
                self.inject_unknown_user_context(&mut request);
                debug!("Unknown user context injected (speaker not recognized, first message)");
            }
        }

        // Wyciagnij session_context (dla REFINE/EXPAND) - bez hintow o tozsamosci
        let session_context_for_analyzer = if has_confirmation_hint {
            None // Nie przekazuj hintow o tozsamosci do Memory Analyzer
        } else {
            session_context.as_deref()
        };

        // === KROK 1: Memory Analyzer - decyzja o zapytaniu (bielik-1.5b) ===
        let t_analyze = Instant::now();
        let query_decision = match self.analyzer.analyze_query(&user_message, session_context_for_analyzer, person_id.as_deref()).await {
            Ok(decision) => {
                debug!(
                    "Memory Analyzer analyze_query took {:?} (should_query={}, type={:?})",
                    t_analyze.elapsed(), decision.should_query, decision.query_type
                );
                decision
            }
            Err(e) => {
                debug!(
                    "Memory Analyzer analyze_query FAILED after {:?}: {}",
                    t_analyze.elapsed(), e
                );
                warn!("MemoryAnalyzer error: {} - uzywam fallback", e);
                MemoryAnalyzer::fallback_query_decision()
            }
        };
        timings.query_analysis_ms = Some(t_analyze.elapsed().as_millis() as u64);

        // Jesli nie trzeba odpytywac Memory - zwroc oryginalny request
        if !query_decision.should_query {
            debug!(
                "Memory Integration total: {:?} (no query needed)",
                t_total_start.elapsed()
            );
            return Ok((request, Some(query_decision), timings));
        }

        // === KROK 2: Odpytaj Memory via QUIC ===
        let t_query = Instant::now();
        let memory_context = match self.query_memory(&session_id, &query_decision).await {
            Ok(ctx) => {
                debug!(
                    "query_memory took {:?} ({} nodes, {} relations)",
                    t_query.elapsed(), ctx.nodes.len(), ctx.relations.len()
                );
                ctx
            }
            Err(e) => {
                debug!(
                    "query_memory FAILED after {:?}: {}",
                    t_query.elapsed(), e
                );
                warn!("Memory query error: {} - kontynuuje bez kontekstu", e);
                MemoryContext::default()
            }
        };
        timings.memory_query_ms = Some(t_query.elapsed().as_millis() as u64);

        // === KROK 3: Inject memory_context do request ===
        if !memory_context.formatted_context.is_empty() {
            self.inject_memory_context(&mut request, &memory_context);
            debug!(
                "Memory context injected: {} nodes, {} relations",
                memory_context.nodes.len(),
                memory_context.relations.len()
            );
        }

        debug!(
            "Memory Integration total: {:?}",
            t_total_start.elapsed()
        );

        Ok((request, Some(query_decision), timings))
    }

    /// Przetwarza odpowiedz modelu i zapisuje do Memory (async)
    ///
    /// Wywolywane PO otrzymaniu odpowiedzi od glownego modelu.
    /// Spawns async task - nie blokuje odpowiedzi do uzytkownika.
    ///
    /// Ta funkcja rowniez zapisuje odpowiedz assistant do conversation_cache!
    pub fn process_response_async(
        &self,
        request: &ChatCompletionRequest,
        response_text: &str,
        _query_decision: Option<QueryDecision>,
    ) {
        // Pobierz session_id nawet jesli memory_store jest wylaczony
        // - potrzebujemy go do conversation_cache
        let session_id = request
            .memory_options
            .as_ref()
            .and_then(|opts| opts.session_id.clone())
            .unwrap_or_else(|| "default".to_string());

        // === ZAWSZE zapisz assistant response do conversation_cache ===
        // Niezaleznie od ustawien memory_store
        let cache = self.conversation_cache.clone();
        let response_for_cache = response_text.to_string();
        let session_for_cache = session_id.clone();

        tokio::spawn(async move {
            cache
                .add_message(&session_for_cache, "assistant", &response_for_cache)
                .await;
            debug!(
                "Stored assistant response in conversation cache for session {}",
                session_for_cache
            );
        });

        // Sprawdz czy Memory store jest wlaczony i wyciagnij potrzebne pola
        let (speaker_id, speaker_name) = match &request.memory_options {
            Some(opts) if opts.enabled.unwrap_or(true) && opts.store_enabled.unwrap_or(true) => {
                (opts.person_id.clone(), opts.speaker_name.clone())
            }
            _ => {
                debug!("Memory store disabled, pomijam (ale conversation_cache zapisane)");
                return;
            }
        };

        let user_message = self.extract_last_user_message(&request.messages);
        if user_message.is_empty() {
            return;
        }

        let response_text = response_text.to_string();
        let service_manager = self.service_manager.clone();

        // Spawn async task - nie czekamy na wynik
        debug!(
            "Memory Store task spawning: session={}, speaker_id={:?}, user_msg_len={}, ai_resp_len={}",
            session_id,
            speaker_id,
            user_message.len(),
            response_text.len()
        );

        tokio::spawn(async move {
            debug!("Memory Store task started for session: {}, speaker: {:?}", session_id, speaker_id);

            // Stworz nowy analyzer w task (nie mozemy przenosic self)
            let analyzer = MemoryAnalyzer::new(service_manager.clone(), None);

            // Analizuj co zapisac - z informacja o mowcy (speaker_id + speaker_name)!
            debug!("Memory Store: calling analyze_for_storage_with_speaker, speaker_id={:?}, speaker_name={:?}", speaker_id, speaker_name);
            let store_decision = match analyzer.analyze_for_storage_with_speaker(
                &user_message,
                &response_text,
                speaker_id.as_deref(),
                speaker_name.as_deref(),
            ).await
            {
                Ok(decision) => {
                    debug!(
                        "Memory Store: analyze_for_storage OK - should_store={}, entities={}, relations={}, facts={}",
                        decision.should_store,
                        decision.entities.len(),
                        decision.relations.len(),
                        decision.facts.len()
                    );
                    // Log szczegoly relacji dla debugowania
                    for (i, rel) in decision.relations.iter().enumerate() {
                        debug!(
                            "Memory Store: relation[{}]: from='{}', to='{}', type='{}'",
                            i, rel.from, rel.to, rel.relation_type
                        );
                    }
                    decision
                }
                Err(e) => {
                    warn!("Memory Store: MemoryAnalyzer error: {} - pomijam zapis", e);
                    return;
                }
            };

            if !store_decision.should_store {
                debug!("Memory Store: should_store=false, reasoning={:?}", store_decision.reasoning);
                return;
            }

            // Zapisz do Memory via QUIC
            debug!("Memory Store: calling store_to_memory_static...");
            if let Err(e) =
                Self::store_to_memory_static(&service_manager, &session_id, &store_decision).await
            {
                warn!("Memory Store error: {}", e);
            } else {
                debug!(
                    "Memory stored: {} entities, {} relations, {} facts",
                    store_decision.entities.len(),
                    store_decision.relations.len(),
                    store_decision.facts.len()
                );
            }
        });
    }

    /// Odpytuje Memory via QUIC
    async fn query_memory(
        &self,
        session_id: &str,
        decision: &QueryDecision,
    ) -> Result<MemoryContext> {
        let t_start = Instant::now();

        // Znajdz QUIC client dla Memory
        let quic_client = self.get_memory_client().await?;
        debug!("query_memory: get_memory_client took {:?}", t_start.elapsed());

        // Konwertuj query_type
        // Protocol uzywa: What, WhatCanDo, WhatFor, Where, HowTo, Why, Similar, Pattern
        let protocol_query_type = match decision.query_type {
            MemoryQueryType::NewSearch => ProtocolQueryType::What,
            MemoryQueryType::Refine => ProtocolQueryType::HowTo,
            MemoryQueryType::Expand => ProtocolQueryType::Similar,
            MemoryQueryType::None => return Ok(MemoryContext::default()),
        };

        // Zbuduj query string z search_terms
        let query = decision.search_terms.join(" ");
        if query.is_empty() {
            debug!("query_memory: empty query, returning default");
            return Ok(MemoryContext::default());
        }

        debug!(
            "query_memory: sending query '{}' type={:?}",
            query, protocol_query_type
        );

        // Przygotuj request
        let request_id = uuid::Uuid::new_v4().to_string();
        let model_request = ModelRequest {
            request_id: request_id.clone(),
            payload: ModelPayload::Memory(MemoryPayload {
                operation: MemoryOperation::Query {
                    session_id: session_id.to_string(),
                    query: query.clone(),
                    query_embedding: None, // Memory Engine wygeneruje embedding
                    query_type: protocol_query_type,
                    max_depth: Some(3),
                    top_k: Some(10),
                    include_reasoning: Some(true),
                },
            }),
            stream: false,
            metadata: None,
            session_id: Some(session_id.to_string()),
        };

        // Wyslij przez QUIC
        let t_quic = Instant::now();
        debug!("query_memory: sending QUIC request for '{}'...", query);
        let response = quic_client.send_request(model_request).await?;
        debug!(
            "query_memory: QUIC request took {:?}",
            t_quic.elapsed()
        );

        // Parsuj odpowiedz
        let result = self.parse_memory_response(response);
        debug!(
            "query_memory: total {:?}",
            t_start.elapsed()
        );
        result
    }

    /// Znajduje osobe po voice_id (speaker_id z STT) i pobiera jej kontekst
    ///
    /// Uzywane gdy STT rozpozna glos i przesle person_id w MemoryOptions.
    /// Zwraca: (MemoryContext z informacjami o osobie, nazwa osoby)
    pub async fn get_person_context_by_voice(
        &self,
        session_id: &str,
        voice_id: &str,
    ) -> Result<(MemoryContext, Option<String>)> {
        let t_start = Instant::now();

        // Krok 1: Znajdz osobe po voice_id
        let quic_client = self.get_memory_client().await?;
        debug!("get_memory_client took {:?}", t_start.elapsed());

        let t_find = Instant::now();
        let request_id = uuid::Uuid::new_v4().to_string();
        let find_request = ModelRequest {
            request_id: request_id.clone(),
            payload: ModelPayload::Memory(MemoryPayload {
                operation: MemoryOperation::FindByVoice {
                    session_id: session_id.to_string(),
                    voice_id: voice_id.to_string(),
                },
            }),
            stream: false,
            metadata: None,
            session_id: Some(session_id.to_string()),
        };

        debug!("FindByVoice request: voice_id={}", voice_id);
        let response = quic_client.send_request(find_request).await?;
        debug!("FindByVoice QUIC request took {:?}", t_find.elapsed());

        // Parsuj wynik FindByVoice
        let find_result = match response.result {
            ModelResult::Memory(memory_result) => match memory_result.result_type {
                MemoryResultType::FindByVoice(result) => result,
                _ => {
                    debug!("Unexpected Memory result type for FindByVoice");
                    return Ok((MemoryContext::default(), None));
                }
            },
            ModelResult::Error(err) => {
                warn!("FindByVoice error: {}", err.message);
                return Ok((MemoryContext::default(), None));
            }
            _ => {
                warn!("Unexpected response type for FindByVoice");
                return Ok((MemoryContext::default(), None));
            }
        };

        // Jesli nie znaleziono osoby
        if !find_result.found || find_result.node_id.is_none() {
            debug!(
                "FindByVoice: person not found for voice_id={} (total: {:?})",
                voice_id, t_start.elapsed()
            );
            return Ok((MemoryContext::default(), None));
        }

        let person_name = find_result.person_name.clone();
        let node_id = find_result.node_id.unwrap();

        debug!(
            "Found person by voice: {} (node_id={}, type={:?})",
            person_name.as_deref().unwrap_or("unknown"),
            node_id,
            find_result.node_type
        );

        // Krok 2: Pobierz kontekst osoby (relacje, fakty o niej)
        let t_context = Instant::now();
        let person_context = self
            .query_person_context(session_id, node_id, person_name.as_deref())
            .await?;
        debug!("query_person_context took {:?}", t_context.elapsed());

        debug!(
            "get_person_context_by_voice total: {:?}",
            t_start.elapsed()
        );

        Ok((person_context, person_name))
    }

    /// Odpytuje Memory o kontekst konkretnej osoby (jej relacje, fakty)
    async fn query_person_context(
        &self,
        session_id: &str,
        person_node_id: u64,
        person_name: Option<&str>,
    ) -> Result<MemoryContext> {
        let quic_client = self.get_memory_client().await?;

        // Zapytanie o relacje i fakty zwiazane z ta osoba
        let query = format!(
            "kontekst osoby {} relacje fakty preferencje",
            person_name.unwrap_or("unknown")
        );

        let request_id = uuid::Uuid::new_v4().to_string();
        let model_request = ModelRequest {
            request_id: request_id.clone(),
            payload: ModelPayload::Memory(MemoryPayload {
                operation: MemoryOperation::Query {
                    session_id: session_id.to_string(),
                    query,
                    query_embedding: None,
                    query_type: ProtocolQueryType::What,
                    max_depth: Some(2), // Plytsze przeszukiwanie dla kontekstu osoby
                    top_k: Some(5),
                    include_reasoning: Some(true),
                },
            }),
            stream: false,
            metadata: None,
            session_id: Some(session_id.to_string()),
        };

        debug!(
            "Query person context: node_id={}, name={:?}",
            person_node_id, person_name
        );
        let response = quic_client.send_request(model_request).await?;

        // Parsuj odpowiedz
        let mut context = self.parse_memory_response(response)?;

        // Oznacz kontekst jako personalny
        if !context.formatted_context.is_empty() && person_name.is_some() {
            context.formatted_context = format!(
                "[KONTEKST OSOBY: {}]\n{}\n[KONIEC KONTEKSTU OSOBY]",
                person_name.unwrap(),
                context
                    .formatted_context
                    .replace("[KONTEKST Z PAMIĘCI]", "")
                    .replace("[KONIEC KONTEKSTU]", "")
                    .trim()
            );
        }

        Ok(context)
    }

    /// Parsuje odpowiedz z Memory i buduje MemoryContext
    fn parse_memory_response(
        &self,
        response: tentaflow_protocol::ModelResponse,
    ) -> Result<MemoryContext> {
        match response.result {
            ModelResult::Memory(memory_result) => {
                let mut context = MemoryContext::default();

                // MemoryResult zawiera result_type enum
                match memory_result.result_type {
                    MemoryResultType::Query(query_result) => {
                        // Konwertuj answers do nodes
                        for answer in query_result.answers {
                            // Konwertuj Option<Vec<(String, String)>> do HashMap
                            let attributes: std::collections::HashMap<String, String> = answer
                                .attributes
                                .unwrap_or_default()
                                .into_iter()
                                .collect();

                            context.nodes.push(MemoryNodeInfo {
                                id: answer.node_id,
                                name: answer.label.clone(),
                                node_type: answer.node_type.clone(),
                                attributes,
                                last_accessed: None,
                            });

                            // Dodaj fakt z odpowiedzia (bez score i type - to techniczne szczegoly)
                            // LLM dostanie instrukcje jak interpretowac te dane
                            if answer.score >= 0.5 {
                                context.facts.push(answer.label.clone());
                            }
                        }

                        // Konwertuj reasoning_paths do relations
                        if let Some(paths) = query_result.reasoning_paths {
                            for path in paths {
                                for step in path.steps {
                                    context.relations.push(MemoryRelationInfo {
                                        from_name: step.from_label,
                                        to_name: step.to_label,
                                        relation_type: step.relation,
                                        metadata: std::collections::HashMap::new(),
                                    });
                                }
                            }
                        }

                        debug!(
                            "Memory query result: {} nodes, {} relations, stats: {:?}",
                            context.nodes.len(),
                            context.relations.len(),
                            query_result.query_stats
                        );
                    }
                    MemoryResultType::Store(store_result) => {
                        debug!(
                            "Memory store result: {} facts stored, {} nodes created",
                            store_result.facts_stored, store_result.nodes_created
                        );
                    }
                    MemoryResultType::Stats(stats_result) => {
                        debug!(
                            "Memory stats: {} nodes, {} edges",
                            stats_result.total_nodes, stats_result.total_edges
                        );
                    }
                    _ => {
                        debug!("Other Memory result type received");
                    }
                }

                // Formatuj kontekst
                context.formatted_context = self.format_memory_context(&context);

                Ok(context)
            }
            ModelResult::Error(err) => {
                warn!("Memory query error: {:?} - {}", err.error_type, err.message);
                Ok(MemoryContext::default())
            }
            _ => {
                warn!("Unexpected Memory response type");
                Ok(MemoryContext::default())
            }
        }
    }

    /// Formatuje MemoryContext jako tekst do wstawienia do prompta
    ///
    /// Dane sa w formacie "encja relacja wartosc" np. "jan_kowalski HasAge 42".
    /// LLM dostaje instrukcje jak naturalnie interpretowac te dane.
    fn format_memory_context(&self, context: &MemoryContext) -> String {
        use crate::prompt_registry::main_llm;

        let mut data_parts = Vec::new();

        // Relacje z reasoning paths
        if !context.relations.is_empty() {
            let rels_str: Vec<String> = context
                .relations
                .iter()
                .map(|r| format!("- {} {} {}", r.from_name, r.relation_type, r.to_name))
                .collect();
            data_parts.push(rels_str.join("\n"));
        }

        // Fakty z wyszukiwania
        if !context.facts.is_empty() {
            data_parts.push(context.facts.iter().map(|f| format!("- {}", f)).collect::<Vec<_>>().join("\n"));
        }

        if data_parts.is_empty() {
            String::new()
        } else {
            let combined_context = data_parts.join("\n");
            let registry = &self.service_manager.prompt_registry;
            let mut params = HashMap::new();
            params.insert("context", combined_context.as_str());

            registry.require_template(main_llm::MEMORY_CONTEXT_TEMPLATE, &params)
        }
    }

    /// Wstrzykuje historie konwersacji do messages
    ///
    /// Dodaje poprzednie wiadomosci user/assistant PO system message ale PRZED aktualna wiadomoscia user.
    /// To pozwala LLM miec pelny kontekst poprzednich wymian.
    fn inject_conversation_history(
        &self,
        request: &mut ChatCompletionRequest,
        history: &[ConversationMessage],
    ) {
        if history.is_empty() {
            return;
        }

        // Znajdz pozycje gdzie wstawic historie:
        // - Po system message (jesli jest)
        // - Przed ostatnia wiadomoscia user (aktualna)
        let insert_pos = request
            .messages
            .iter()
            .position(|m| m.role == "system")
            .map(|i| i + 1)
            .unwrap_or(0);

        // Konwertuj historie na Messages
        let history_messages: Vec<Message> = history
            .iter()
            .map(|msg| Message {
                role: msg.role.clone(),
                content: Some(MessageContent::Text(msg.content.clone())),
                ..Default::default()
            })
            .collect();

        // Wstaw historie w odpowiednie miejsce (split_off + extend zamiast insert w petli)
        let tail = request.messages.split_off(insert_pos);
        request.messages.extend(history_messages);
        request.messages.extend(tail);

        debug!(
            "Injected {} history messages at position {}",
            history.len(),
            insert_pos
        );
    }

    /// Dopisuje tekst do system message (lub tworzy nowy jesli nie istnieje)
    fn append_to_system_message(request: &mut ChatCompletionRequest, text: &str) {
        for msg in &mut request.messages {
            if msg.role == "system" {
                if let Some(MessageContent::Text(ref mut content)) = msg.content {
                    content.push_str(text);
                }
                return;
            }
        }

        // Brak system message - dodaj nowy na poczatku
        request.messages.insert(
            0,
            Message {
                role: "system".to_string(),
                content: Some(MessageContent::Text(text.trim_start().to_string())),
                ..Default::default()
            },
        );
    }

    /// Wstrzykuje memory_context do request (modyfikuje system message)
    fn inject_memory_context(&self, request: &mut ChatCompletionRequest, context: &MemoryContext) {
        if context.formatted_context.is_empty() {
            return;
        }

        let text = format!("\n\n{}", context.formatted_context);
        Self::append_to_system_message(request, &text);
    }

    /// Wstrzykuje personalizacje dla rozpoznanej osoby
    ///
    /// Dodaje informacje o tym kto mowi do system message.
    /// Uzywane gdy speaker_confidence > 0.85 (bardzo wysoka pewnosc rozpoznania).
    fn inject_personalization(&self, request: &mut ChatCompletionRequest, person_name: &str, is_first_message: bool) {
        use crate::prompt_registry::main_llm;

        let registry = &self.service_manager.prompt_registry;
        let mut params = HashMap::new();
        params.insert("name", person_name);

        let personalization = if is_first_message {
            registry.require_template(main_llm::PERSONALIZATION_FIRST_TEMPLATE, &params)
        } else {
            registry.require_template(main_llm::PERSONALIZATION_CONTINUE_TEMPLATE, &params)
        };

        Self::append_to_system_message(request, &personalization);
    }

    /// Wstrzykuje kontekst sesji - czy to nowa rozmowa czy kontynuacja
    ///
    /// Zapobiega sytuacji gdy LLM wita sie przy kazdej wiadomosci.
    fn inject_session_context(
        &self,
        request: &mut ChatCompletionRequest,
        is_first_message: bool,
        is_noise: bool,
    ) {
        use crate::prompt_registry::main_llm;

        let registry = &self.service_manager.prompt_registry;

        let prompt_id = if is_first_message {
            main_llm::SESSION_START
        } else if is_noise {
            main_llm::SESSION_UNCLEAR
        } else {
            main_llm::SESSION_CONTINUE
        };

        let context = registry.require_content(prompt_id);

        Self::append_to_system_message(request, &context);
    }

    /// Sprawdza czy wiadomosc wyglada na szum/niezrozumiala (krzaki z STT)
    ///
    /// Wykrywa:
    /// - Bardzo krotkie wiadomosci (< 3 znaki)
    /// - Same znaki specjalne/liczby
    /// - Powtarzajace sie znaki (aaaaaa, mmmmm)
    /// - Typowe artefakty STT (hm, yyy, eee)
    /// - Niezrozumiale ciagi znakow (wysoki stosunek spolgloskek, brak slow)
    fn is_likely_noise(&self, message: &str) -> bool {
        let trimmed = message.trim();

        // Bardzo krotka wiadomosc
        if trimmed.len() < 3 {
            return true;
        }

        // Same znaki specjalne lub liczby
        if trimmed.chars().all(|c| !c.is_alphabetic()) {
            return true;
        }

        // Powtarzajace sie znaki (np. "aaaaaa", "mmmmm")
        let char_count = trimmed.chars().count();
        if char_count > 3 {
            if let Some(first) = trimmed.chars().next().and_then(|c| c.to_lowercase().next()) {
                if trimmed.chars().all(|c| c.to_lowercase().next() == Some(first)) {
                    return true;
                }
            }
        }

        // Typowe artefakty STT
        let noise_patterns = [
            "hm", "hmm", "hmmm", "yyy", "eee", "aaa", "mmm",
            "...", "???", "!!!", "aha", "mhm", "uhm", "ehm",
            "uch", "och", "ach", "ech", "yhy", "no", "ee",
            "ha", "he", "ho", "hej", "hę", "hą",
        ];
        let lower = trimmed.to_lowercase();
        for pattern in noise_patterns {
            if lower == pattern || (lower.starts_with(pattern) && lower.len() < pattern.len() + 3) {
                return true;
            }
        }

        // Wykryj "krzaki" - niezrozumiale ciagi bez prawdziwych slow
        // Jesli wiadomosc jest krotka (<15 znakow) i nie zawiera zadnego sensownego slowa
        if trimmed.len() < 15 {
            let has_real_word = self.contains_real_word(&lower);
            if !has_real_word {
                return true;
            }
        }

        // Wysoki stosunek spolgloskek do samogloskek (typowe dla krzakow STT)
        let vowels = lower.chars().filter(|c| "aeiouyąęó".contains(*c)).count();
        let consonants = lower.chars().filter(|c| c.is_alphabetic() && !"aeiouyąęó".contains(*c)).count();
        if consonants > 0 && vowels > 0 {
            let ratio = consonants as f32 / vowels as f32;
            // Normalny tekst ma stosunek ~1.5-2.5, krzaki czesto >4
            if ratio > 4.0 && trimmed.len() < 20 {
                return true;
            }
        }

        false
    }

    /// Sprawdza czy tekst zawiera przynajmniej jedno sensowne polskie/angielskie slowo
    fn contains_real_word(&self, text: &str) -> bool {
        // Lista podstawowych slow ktore oznaczaja sensowna wypowiedz
        let common_words = [
            // Polskie
            "tak", "nie", "co", "jak", "gdzie", "kiedy", "czy", "ale", "to", "ja", "ty",
            "on", "ona", "ono", "my", "wy", "oni", "jest", "są", "był", "była", "będzie",
            "mam", "masz", "ma", "mamy", "chcę", "chcesz", "chce", "mogę", "możesz", "może",
            "cześć", "hej", "witaj", "dzień", "dobry", "wieczór", "proszę", "dziękuję",
            "przepraszam", "tak", "dobrze", "okej", "super", "fajnie", "świetnie",
            "jestem", "nazywam", "imię", "wiem", "rozumiem", "pamiętam", "zapamiętaj",
            "powiedz", "opowiedz", "zrób", "pomóż", "znajdź", "pokaż", "daj",
            // Angielskie (na wypadek code-switching)
            "yes", "no", "ok", "okay", "hello", "hi", "bye", "thanks", "please",
            "what", "how", "where", "when", "why", "who", "the", "and", "or",
        ];

        for word in text.split_whitespace() {
            // Usun znaki interpunkcyjne z poczatku i konca
            let clean_word = word.trim_matches(|c: char| !c.is_alphabetic());
            if clean_word.len() >= 2 && common_words.contains(&clean_word) {
                return true;
            }
        }
        false
    }

    /// Wykrywa czy uzytkownik sie przedstawia w wiadomosci
    ///
    /// Wzorce: "jestem X", "mam na imie X", "nazywam sie X", "to ja, X"
    /// Zwraca Some(imie) jesli wykryto przedstawienie
    fn detect_self_introduction(&self, message: &str) -> Option<String> {
        let lower = message.to_lowercase();

        // Wzorce przedstawienia sie
        let patterns = [
            ("jestem ", 7),
            ("mam na imię ", 12),
            ("nazywam się ", 12),
            ("to ja, ", 7),
            ("to ja ", 6),
            ("mówi ", 5),
            ("tu ", 3),
        ];

        for (pattern, skip_len) in patterns {
            if let Some(pos) = lower.find(pattern) {
                let after_pattern = &message[pos + skip_len..];
                // Wyciagnij pierwsze slowo (imie) - kapitalizowane
                let name: String = after_pattern
                    .split(|c: char| c.is_whitespace() || c == ',' || c == '.' || c == '!')
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_string();

                // Sprawdz czy to wyglada na imie (3-15 znakow, zaczyna sie wielka lub mala)
                if name.len() >= 2 && name.len() <= 15 && name.chars().next().map(|c| c.is_alphabetic()).unwrap_or(false) {
                    // Nie zwracaj jesli to typowe slowo a nie imie
                    let not_names = ["tu", "tutaj", "teraz", "zawsze", "sam", "sama", "też", "także"];
                    if !not_names.contains(&name.to_lowercase().as_str()) {
                        return Some(name);
                    }
                }
            }
        }
        None
    }

    /// Wstrzykuje kontekst nieznanego uzytkownika
    ///
    /// Dodaje informacje ze rozmowca nie zostal rozpoznany i AI powinno sie przedstawic
    /// oraz zapytac o imie przy powitaniu.
    fn inject_unknown_user_context(&self, request: &mut ChatCompletionRequest) {
        use crate::prompt_registry::main_llm;

        let registry = &self.service_manager.prompt_registry;
        let unknown_user_context = registry.require_content(main_llm::UNKNOWN_USER_STRONG);

        Self::append_to_system_message(request, &unknown_user_context);
    }

    /// Wstrzykuje kontekst dla MEDIUM confidence rozpoznania glosu
    ///
    /// Dodaje informacje ze glos jest podobny do kogos z bazy, ale wymaga potwierdzenia.
    fn inject_medium_confidence_context(&self, request: &mut ChatCompletionRequest, hint: &str) {
        use crate::prompt_registry::main_llm;

        let registry = &self.service_manager.prompt_registry;

        // Wyciagnij imie z hinta jesli mozliwe
        // Format hinta: "Prawdopodobnie Jan (75.0%) - zapytaj o potwierdzenie"
        let name_hint = if hint.starts_with("Prawdopodobnie ") {
            hint.split(" (").next()
                .map(|s| s.replace("Prawdopodobnie ", ""))
                .unwrap_or_default()
        } else {
            String::new()
        };

        let context = if !name_hint.is_empty() {
            let mut params = HashMap::new();
            params.insert("name", name_hint.as_str());
            registry.require_template(main_llm::MEDIUM_CONFIDENCE_KNOWN_TEMPLATE, &params)
        } else {
            registry.require_content(main_llm::MEDIUM_CONFIDENCE_UNKNOWN).to_string()
        };

        Self::append_to_system_message(request, &context);
    }

    /// Wstrzykuje kontekst gdy nowy rozmowca sie przedstawil
    ///
    /// Uzytkownik powiedzial "jestem X" - potwierdz i zapamietaj
    fn inject_new_speaker_introduced(&self, request: &mut ChatCompletionRequest, name: &str) {
        use crate::prompt_registry::main_llm;

        let registry = &self.service_manager.prompt_registry;
        let mut params = HashMap::new();
        params.insert("name", name);

        let context = registry.require_template(main_llm::NEW_SPEAKER_INTRODUCED_TEMPLATE, &params);

        Self::append_to_system_message(request, &context);
    }

    /// Wstrzykuje kontekst dla nowego glosu wykrytego w trakcie rozmowy
    ///
    /// Delikatnie pyta kto dolaczyl do rozmowy (bez pelnego przedstawienia)
    fn inject_new_voice_during_conversation(&self, request: &mut ChatCompletionRequest) {
        use crate::prompt_registry::main_llm;

        let registry = &self.service_manager.prompt_registry;
        let context = registry.require_content(main_llm::NEW_VOICE_DURING_CONVERSATION);

        Self::append_to_system_message(request, &context);
    }

    /// Wyciaga ostatnia wiadomosc uzytkownika
    fn extract_last_user_message(&self, messages: &[Message]) -> String {
        messages
            .iter()
            .rev()
            .find(|m| m.role == "user")
            .and_then(|m| m.content.as_ref())
            .map(|content| match content {
                MessageContent::Text(text) => text.clone(),
                MessageContent::Parts(parts) => parts
                    .iter()
                    .filter_map(|p| {
                        if let crate::api::openai::types::ContentPart::Text { text } = p {
                            Some(text.clone())
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(" "),
            })
            .unwrap_or_default()
    }

    /// Znajduje QUIC client dla Memory w service_manager
    async fn find_memory_client(service_manager: &ServiceManager) -> Result<Arc<crate::net::quic::QuicClient>> {
        let memory_handles: Vec<_> = service_manager.quic_memory_services.read().values().cloned().collect();
        for handle in memory_handles {
            let client_guard = handle.client.read().await;
            if let Some(client) = client_guard.as_ref() {
                return Ok(client.clone());
            }
        }

        Err(CoreError::AllBackendsUnavailable {
            model_name: "memory".to_string(),
        }
        .into())
    }

    /// Pobiera QUIC client dla Memory (async - wymaga locka)
    async fn get_memory_client(&self) -> Result<Arc<crate::net::quic::QuicClient>> {
        Self::find_memory_client(&self.service_manager).await
    }

    /// Zapisuje do Memory via QUIC (static version for async task)
    async fn store_to_memory_static(
        service_manager: &ServiceManager,
        session_id: &str,
        decision: &StoreDecision,
    ) -> Result<()> {
        let quic_client = Self::find_memory_client(service_manager).await?;

        // Konwertuj entities/relations/facts do MemoryFact format
        let mut facts = Vec::new();

        // Helper do konwersji HashMap na Vec<(String, String)>
        fn hashmap_to_vec(map: &std::collections::HashMap<String, String>) -> Vec<(String, String)> {
            map.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
        }

        // Encje jako fakty (subject IsA type)
        for entity in &decision.entities {
            facts.push(tentaflow_protocol::MemoryFact {
                subject: entity.name.clone(),
                relation: "IsA".to_string(),
                object: format!("{:?}", entity.entity_type),
                confidence: entity.confidence,
                source: Some("memory_analyzer".to_string()),
                metadata: Some(hashmap_to_vec(&entity.attributes)),
            });
        }

        // Relacje jako fakty
        for relation in &decision.relations {
            facts.push(tentaflow_protocol::MemoryFact {
                subject: relation.from.clone(),
                relation: relation.relation_type.clone(),
                object: relation.to.clone(),
                confidence: relation.confidence,
                source: Some("memory_analyzer".to_string()),
                metadata: Some(hashmap_to_vec(&relation.metadata)),
            });
        }

        // Fakty tekstowe (subject=fact, relation=States, object=text)
        for fact in &decision.facts {
            facts.push(tentaflow_protocol::MemoryFact {
                subject: "fact".to_string(),
                relation: "States".to_string(),
                object: fact.text.clone(),
                confidence: fact.confidence,
                source: Some("memory_analyzer".to_string()),
                metadata: None,
            });
        }

        if facts.is_empty() {
            return Ok(());
        }

        // Przygotuj request
        let request_id = uuid::Uuid::new_v4().to_string();
        let model_request = ModelRequest {
            request_id: request_id.clone(),
            payload: ModelPayload::Memory(MemoryPayload {
                operation: MemoryOperation::Store {
                    session_id: session_id.to_string(),
                    facts,
                    context_embedding: None,
                },
            }),
            stream: false,
            metadata: None,
            session_id: Some(session_id.to_string()),
        };

        // Wyslij przez QUIC
        debug!("Memory store: request_id={}", request_id);
        let response = quic_client.send_request(model_request).await?;

        // Sprawdz odpowiedz
        match response.result {
            ModelResult::Memory(_) => Ok(()),
            ModelResult::Error(err) => {
                Err(CoreError::InternalError {
                    message: format!("Memory store error: {}", err.message),
                    source: None,
                }
                .into())
            }
            _ => Ok(()),
        }
    }

    /// Wykrywa czy uzytkownik przedstawia sie lub koryguje swoje imie.
    ///
    /// Wzorce (case-insensitive):
    /// - "jestem X", "jestem X.", "ja jestem X"
    /// - "mam na imie X", "mam na imie X"
    /// - "nazywam sie X", "nazywam sie X"
    /// - "moje imie to X", "moje imie to X"
    /// - "nie jestem X, jestem Y" -> zwraca Y
    /// - "to nie Jan, jestem Piotr" -> zwraca Piotr
    ///
    /// Zwraca: Some(imie) jesli wykryto, None w przeciwnym razie
    fn detect_name_correction(&self, message: &str) -> Option<String> {
        for re in NAME_INTRO_PATTERNS.iter() {
            if let Some(caps) = re.captures(message) {
                if let Some(name_match) = caps.get(1) {
                    let name = name_match.as_str().to_string();
                    if self.is_valid_name(&name) {
                        debug!("Detected name introduction: '{}'", name);
                        return Some(name);
                    }
                }
            }
        }

        for re in NAME_CORRECTION_PATTERNS.iter() {
            if let Some(caps) = re.captures(message) {
                if let Some(name_match) = caps.get(1) {
                    let name = name_match.as_str().to_string();
                    if self.is_valid_name(&name) {
                        debug!("Detected name correction: '{}'", name);
                        return Some(name);
                    }
                }
            }
        }

        None
    }

    /// Sprawdza czy tekst jest prawidlowym imieniem.
    ///
    /// Filtruje slowa ktore nie sa imionami (zaimki, czasowniki, etc.)
    fn is_valid_name(&self, name: &str) -> bool {
        // Minimalna dlugosc
        if name.len() < 2 {
            return false;
        }

        // Maksymalna dlugosc (imiona rzadko maja wiecej niz 15 znakow)
        if name.len() > 15 {
            return false;
        }

        // Wyklucz popularne slowa ktore nie sa imionami
        let excluded_words: &[&str] = &[
            "tu", "tam", "tak", "nie", "już", "jeszcze", "bardzo", "dobrze",
            "tutaj", "teraz", "potem", "później", "dzisiaj", "wczoraj",
            "pewien", "pewna", "pewne", "jakiś", "jakieś", "każdy",
            "gotowy", "gotowa", "zajęty", "zajęta",
        ];

        let name_lower = name.to_lowercase();
        if excluded_words.contains(&name_lower.as_str()) {
            return false;
        }

        // Imie powinno zaczynac sie wielka litera
        let first_char = name.chars().next().unwrap();
        first_char.is_uppercase()
    }

    /// Aktualizuje speaker_name w bazie glosow STT (static version for async task)
    async fn update_speaker_name_static(
        service_manager: &ServiceManager,
        speaker_id: &str,
        new_name: &str,
    ) -> Result<()> {
        // Znajdz STT client
        let stt_client = service_manager
            .get_first_quic_stt_client()
            .await
            .ok_or_else(|| CoreError::AllBackendsUnavailable {
                model_name: "stt".to_string(),
            })?;

        // Utworz request SpeakerUpdateName
        let request_id = uuid::Uuid::new_v4().to_string();
        let model_request = ModelRequest {
            request_id: request_id.clone(),
            payload: ModelPayload::Audio(AudioPayload {
                operation: AudioOperation::SpeakerUpdateName {
                    speaker_id: speaker_id.to_string(),
                    new_name: new_name.to_string(),
                },
            }),
            stream: false,
            metadata: None,
            session_id: None,
        };

        debug!(
            "Updating speaker name: speaker_id={}, new_name={}",
            speaker_id, new_name
        );

        let response = stt_client.send_request(model_request).await?;

        match response.result {
            ModelResult::Audio(audio_result) => {
                match audio_result.data {
                    tentaflow_protocol::AudioResultData::SpeakerUpdateNameResult {
                        success, old_name, new_name, ..
                    } => {
                        if success {
                            debug!("Speaker name updated: '{}' -> '{}'", old_name, new_name);
                            Ok(())
                        } else {
                            Err(CoreError::InternalError {
                                message: format!(
                                    "Failed to update speaker name: {} -> {}",
                                    old_name, new_name
                                ),
                                source: None,
                            }
                            .into())
                        }
                    }
                    _ => {
                        warn!("Unexpected audio result type for SpeakerUpdateName");
                        Ok(()) // Nie blokuj - moze serwis nie wspiera tej operacji
                    }
                }
            }
            ModelResult::Error(err) => {
                Err(CoreError::InternalError {
                    message: format!("SpeakerUpdateName error: {}", err.message),
                    source: None,
                }
                .into())
            }
            _ => {
                warn!("Unexpected response type for SpeakerUpdateName");
                Ok(())
            }
        }
    }

    /// Aktualizuje nazwe osoby w Memory (static version for async task)
    async fn update_person_name_in_memory_static(
        service_manager: &ServiceManager,
        session_id: &str,
        voice_id: &str,
        new_name: &str,
    ) -> Result<()> {
        let quic_client = Self::find_memory_client(service_manager).await?;

        // Utworz request UpdatePersonName
        let request_id = uuid::Uuid::new_v4().to_string();
        let model_request = ModelRequest {
            request_id: request_id.clone(),
            payload: ModelPayload::Memory(MemoryPayload {
                operation: MemoryOperation::UpdatePersonName {
                    session_id: session_id.to_string(),
                    voice_id: Some(voice_id.to_string()),
                    node_id: None,
                    new_name: new_name.to_string(),
                    preserve_history: true, // Zachowaj poprzednia nazwe jako alias
                },
            }),
            stream: false,
            metadata: None,
            session_id: Some(session_id.to_string()),
        };

        debug!(
            "Updating person name in Memory: voice_id={}, new_name={}",
            voice_id, new_name
        );

        let response = quic_client.send_request(model_request).await?;

        match response.result {
            ModelResult::Memory(memory_result) => {
                match memory_result.result_type {
                    MemoryResultType::UpdatePersonName(result) => {
                        if result.success {
                            debug!(
                                "Person name updated in Memory: '{}' -> '{}' (node_id={})",
                                result.old_name, result.new_name, result.node_id
                            );
                            Ok(())
                        } else {
                            Err(CoreError::InternalError {
                                message: format!(
                                    "Failed to update person name in Memory: {} -> {}",
                                    result.old_name, result.new_name
                                ),
                                source: None,
                            }
                            .into())
                        }
                    }
                    _ => {
                        warn!("Unexpected Memory result type for UpdatePersonName");
                        Ok(())
                    }
                }
            }
            ModelResult::Error(err) => {
                Err(CoreError::InternalError {
                    message: format!("UpdatePersonName error: {}", err.message),
                    source: None,
                }
                .into())
            }
            _ => {
                warn!("Unexpected response type for UpdatePersonName");
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_memory_context_empty() {
        let context = MemoryContext::default();
        let formatted = format_test_context(&context);
        assert!(formatted.is_empty());
    }

    #[test]
    fn test_format_memory_context_with_nodes() {
        let mut context = MemoryContext::default();
        context.nodes.push(MemoryNodeInfo {
            id: 1,
            name: "Marek".to_string(),
            node_type: "Person".to_string(),
            attributes: [("role".to_string(), "developer".to_string())]
                .into_iter()
                .collect(),
            last_accessed: None,
        });

        let formatted = format_test_context(&context);
        assert!(formatted.contains("Marek"));
        assert!(formatted.contains("Person"));
        assert!(formatted.contains("developer"));
    }

    fn format_test_context(context: &MemoryContext) -> String {
        let mut parts = Vec::new();

        if !context.nodes.is_empty() {
            let nodes_str: Vec<String> = context
                .nodes
                .iter()
                .map(|n| format!("- {} ({})", n.name, n.node_type))
                .collect();
            parts.push(format!("OSOBY/ENCJE:\n{}", nodes_str.join("\n")));
        }

        if parts.is_empty() {
            String::new()
        } else {
            format!("[KONTEKST]\n{}\n[KONIEC]", parts.join("\n\n"))
        }
    }
}
