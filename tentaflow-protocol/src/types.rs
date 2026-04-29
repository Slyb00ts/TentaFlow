// ============================================================================
// TENTAFLOW PROTOCOL - Universal Model Protocol
// ============================================================================
//
// Uniwersalny protokół komunikacji dla wszystkich typów modeli AI:
// - Embeddings (text → vectors)
// - Completion (text generation, chat)
// - Image (generation, editing)
// - Audio (TTS, STT)
// - Vision (image understanding)
// - RAG (retrieval augmented generation)
//
// Używa rkyv dla zero-copy serialization (ultra-fast, ~10x faster than serde)
// Obsługuje zarówno streaming jak i non-streaming responses
//
// ARCHITEKTURA:
// - Client → Router: ModelRequest (wszystkie typy operacji)
// - Router → RAG: ModelRequest(RAGPayload)
// - Router → Embeddings Engine: ModelRequest(EmbeddingsPayload)
// - Router → LLM: ModelRequest(CompletionPayload)
// - RAG → Router: ModelRequest (callbacks dla embeddings, LLM)
// - Router → Client: ModelResponse lub ModelStreamChunk
//
// ============================================================================

use rkyv::{Archive, Deserialize, Serialize};
use serde::{Deserialize as SerdeDeserialize, Serialize as SerdeSerialize};
use serde_with::{base64::Base64, serde_as};

// ============================================================================
// SHARED TYPES - Used across multiple model types
// ============================================================================

/// Tryb wyszukiwania w RAG Engine.
///
/// Client może wybrać jeden lub więcej trybów (Vec<SearchMode>).
/// RAG wykonuje wyszukiwanie we wszystkich wybranych silnikach i scala wyniki.
///
/// Tryby:
/// - `FullTextSearch`: Wyszukiwanie pełnotekstowe Tantivy (BM25) - dobre dla keyword search
/// - `VectorSearch`: Wyszukiwanie wektorowe HNSW - dobre dla semantic similarity
/// - `HiRAG`: Hierarchical RAG z knowledge graph - dobre dla złożonych zapytań
/// - `GSW`: Graph Semantic Workspace - dobre dla reasoning i kontekstu episodycznego
///
/// Przykład kombinacji:
/// ```rust
/// // Hybrid search: FTS + Vector
/// search_modes: vec![SearchMode::FullTextSearch, SearchMode::VectorSearch]
///
/// // Full power: wszystkie silniki
/// search_modes: vec![
///     SearchMode::FullTextSearch,
///     SearchMode::VectorSearch,
///     SearchMode::HiRAG,
///     SearchMode::GSW,
/// ]
/// ```
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub enum SearchMode {
    /// Wyszukiwanie pełnotekstowe (Tantivy BM25)
    FullTextSearch,

    /// Wyszukiwanie wektorowe (HNSW + embeddings)
    VectorSearch,

    /// Hierarchical RAG (knowledge graph + multi-level retrieval)
    HiRAG,

    /// Graph Semantic Workspace (episodic memory + reasoning)
    GSW,
}

/// Kontekst dla RAG request (historia konwersacji, metadata sesji).
///
/// Opcjonalny - wysyłany gdy klient ma kontekst do przekazania (np. multi-turn conversation).
///
/// Pola:
/// - `messages`: Historia konwersacji (poprzednie wiadomości user/assistant)
/// - `metadata`: Dodatkowy kontekst (user_id, session_id, preferences, etc.)
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
pub struct RAGContext {
    /// Historia konwersacji (poprzednie wiadomości)
    pub messages: Vec<Message>,

    /// Metadata sesji (key-value pairs: user_id, session_id, language, etc.)
    pub metadata: Vec<(String, String)>,
}

/// Pojedyncza wiadomość w konwersacji.
///
/// Używana w historii konwersacji (RAGContext) oraz w callback requests do LLM.
///
/// Pola:
/// - `role`: Rola nadawcy ("system" | "user" | "assistant")
/// - `content`: Treść wiadomości (tekst)
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
pub struct Message {
    /// Rola nadawcy: "system", "user", "assistant"
    pub role: String,

    /// Treść wiadomości (tekst)
    pub content: String,
}

/// Parametry biznesowe RAG pipeline.
///
/// WAŻNE: Zawiera TYLKO parametry biznesowe (top_k, similarity, reranking).
/// NIE zawiera nazw modeli - modele są konfigurowane w plikach config RAG/Router.
///
/// Dlaczego bez nazw modeli?
/// - Modele są częścią infrastruktury, nie API
/// - RAG używa modeli z własnej konfiguracji (config.toml)
/// - Dla callback do Router: RAG wysyła model name z config RAG
/// - Client nie powinien wybierać modeli bezpośrednio (bezpieczeństwo + consistency)
///
/// Parametry:
/// - `top_k`: Ile maksymalnie dokumentów zwrócić (typowo 3-10)
/// - `min_similarity`: Próg podobieństwa (0.0-1.0, zwykle >0.7 dla quality)
/// - `use_reranking`: Czy użyć cross-encoder reranking dla lepszej jakości (opcjonalne)
///
/// Przykład:
/// ```rust
/// let params = RAGParams {
///     top_k: 5,
///     min_similarity: 0.7,
///     use_reranking: Some(true),  // Włącz reranking dla lepszej jakości
/// };
/// ```
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
pub struct RAGParams {
    /// Ile maksymalnie dokumentów/chunków zwrócić (typowo 3-10)
    /// Większe top_k = więcej kontekstu ale większy koszt LLM
    pub top_k: u32,

    /// Próg minimalnego podobieństwa (0.0-1.0, cosine similarity)
    /// Typowo >0.7 dla wysokiej jakości, >0.6 dla szerszego pokrycia
    pub min_similarity: f32,

    /// Czy użyć cross-encoder reranking przed zwróceniem wyników
    /// - Some(true): Przeranguj wyniki (lepsza jakość, wolniejsze)
    /// - Some(false) lub None: Bez rerankingu (szybsze)
    pub use_reranking: Option<bool>,
}

/// Response od RAG Engine do Router.
///
/// WAŻNE: RAG NIE streamuje i NIE decyduje o przetwarzaniu LLM/TTS.
/// RAG zwraca tylko kontekst tekstowy i metadata. Router decyduje co dalej na podstawie flag.
///
/// Dwa przypadki użycia:
/// 1. requires_llm_processing = false: `context_text` to finalna odpowiedź (zwróć user bezpośrednio)
/// 2. requires_llm_processing = true: `context_text` to prompt dla LLM (Router streamuje przez LLM)
///
/// Pola:
/// - `request_id`: UUID z requestu (correlation)
/// - `context_text`: Kontekst tekstowy (albo finalna odpowiedź, albo prompt dla LLM)
/// - `metadata`: Szczegółowe informacje o znalezionych chunkach (sources, scores, content)
/// - `requires_llm_processing`: PASS-THROUGH z requestu (RAG nie zmienia)
/// - `requires_audio_output`: PASS-THROUGH z requestu (RAG nie zmienia)
///
/// Przykład 1 (bez LLM):
/// ```rust
/// RAGResponse {
///     context_text: "Znaleziono 3 dokumenty o Project X: doc1.pdf, doc2.docx, doc3.txt",
///     requires_llm_processing: false,  // Router zwraca to bezpośrednio user
///     ...
/// }
/// ```
///
/// Przykład 2 (z LLM):
/// ```rust
/// RAGResponse {
///     context_text: "Context: [chunk1 content] [chunk2 content] Question: What is Project X?",
///     requires_llm_processing: true,  // Router wysyła to do LLM i streamuje
///     ...
/// }
/// ```
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
pub struct RAGResponse {
    /// UUID requestu (z RAGRequest, dla korelacji)
    pub request_id: String,

    /// Kontekst tekstowy:
    /// - Jeśli requires_llm_processing=false: finalna odpowiedź dla user
    /// - Jeśli requires_llm_processing=true: prompt dla LLM (zawiera znalezione chunki + query)
    pub context_text: String,

    /// Szczegółowe metadata o znalezionych chunkach (sources, scores, content, etc.)
    pub metadata: Vec<RAGChunkMetadata>,

    /// PASS-THROUGH z RAGRequest: Czy Router powinien przetworzyć przez LLM
    /// RAG NIGDY nie zmienia tej wartości - tylko przekazuje z requestu
    pub requires_llm_processing: bool,

    /// PASS-THROUGH z RAGRequest: Czy Router powinien wygenerować audio (TTS)
    /// RAG NIGDY nie zmienia tej wartości - tylko przekazuje z requestu
    pub requires_audio_output: bool,

    /// Nazwa modelu LLM do użycia przez Router dla final generation (jeśli requires_llm_processing=true)
    /// RAG pobiera to ze swojego config.models.generation_model
    /// Przykład: "gpt-oss-20b", "claude-3-5-sonnet"
    pub llm_model: Option<String>,
}

/// Metadata pojedynczego chunka znalezionego przez RAG.
///
/// Zawiera wszystkie informacje o znalezionym fragmencie dokumentu:
/// - Skąd pochodzi (source_file, source_type)
/// - Jak dobrze pasuje (similarity_score, rank)
/// - Co zawiera (chunk_text, chunk_index)
/// - Dodatkowe dane (doc_metadata)
///
/// Pola:
/// - `doc_id`: Identyfikator dokumentu źródłowego
/// - `chunk_id`: Identyfikator chunka w dokumencie
/// - `chunk_index`: Pozycja chunka w dokumencie (0-indexed)
/// - `chunk_text`: Treść chunka (może być skrócona dla wyświetlenia)
/// - `similarity_score`: Score podobieństwa (0.0-1.0, cosine similarity)
/// - `rank`: Pozycja w rankingu (1 = najlepszy)
/// - `source_file`: Ścieżka do pliku źródłowego (np. "/docs/project_x.pdf")
/// - `source_type`: Typ źródła ("pdf" | "docx" | "txt" | "url" | etc.)
/// - `documents`: Lista dokumentów zawierających ten chunk (zawsze >= 1)
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
pub struct RAGChunkMetadata {
    /// Identyfikator chunka
    pub chunk_id: String,

    /// Pozycja chunka w dokumencie (0-indexed)
    pub chunk_index: u32,

    /// Treść chunka (pełna lub skrócona)
    pub chunk_text: String,

    /// Score podobieństwa do query (0.0-1.0, cosine similarity)
    pub similarity_score: f32,

    /// Pozycja w rankingu wyników (1 = najlepszy)
    pub rank: u32,

    /// Ścieżka do pliku źródłowego lub URL
    pub source_file: String,

    /// Typ źródła (pdf, docx, txt, url, etc.)
    pub source_type: String,

    /// Lista dokumentów zawierających ten chunk.
    /// Zawsze co najmniej 1 element.
    /// Jeśli ten sam plik jest przypisany do wielu dokumentów biznesowych,
    /// każdy ma własny doc_id i metadane.
    pub documents: Vec<ChunkDocument>,
}

/// Dokument zawierający dany chunk.
///
/// Jeden chunk może należeć do wielu dokumentów (przez aliasy/soft links).
/// Każdy dokument ma własny doc_id i metadane.
///
/// Przykład: Ten sam załącznik PDF przypisany do "Umowy A" i "Umowy B":
/// - doc_id: "attach_umowa_a", metadata: {parent: "Umowa A"}
/// - doc_id: "attach_umowa_b", metadata: {parent: "Umowa B"}
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
pub struct ChunkDocument {
    /// Identyfikator dokumentu
    pub doc_id: String,

    /// Metadane specyficzne dla tego dokumentu
    pub metadata: Vec<(String, String)>,
}

/// Message dla vision request (może zawierać tekst + obrazy).
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
pub struct VisionMessage {
    /// Rola
    pub role: String,

    /// Content parts (text + images)
    pub content: Vec<VisionContentPart>,
}

/// Content part dla vision (text | image_url).
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
pub enum VisionContentPart {
    /// Text fragment
    Text { text: String },

    /// Image URL (data: URI lub HTTP URL)
    ImageUrl {
        url: String,
        detail: Option<String>,
    },
}


// ============================================================================
// HELPER FUNCTIONS
// ============================================================================

impl Default for RAGParams {
    fn default() -> Self {
        Self {
            top_k: 5,
            min_similarity: 0.7,
            use_reranking: None,
        }
    }
}

impl RAGParams {

    /// Tworzy parametry RAG dla wysokiej jakości (z reranking).
    pub fn high_quality() -> Self {
        Self {
            top_k: 10,
            min_similarity: 0.6,
            use_reranking: Some(true),
        }
    }

    /// Tworzy parametry RAG dla szybkości.
    pub fn fast() -> Self {
        Self {
            top_k: 3,
            min_similarity: 0.75,
            use_reranking: Some(false),
        }
    }
}

// ============================================================================
// QUIC MESSAGE TYPE DISCRIMINATORS
// ============================================================================

/// Discriminator byte dla rozróżnienia typów wiadomości w QUIC.
///
/// Każda wiadomość QUIC zaczyna się od jednego bajtu określającego typ:
/// - Pierwszy bajt to MESSAGE_TYPE_*
/// - Pozostałe bajty to zserializowane dane (rkyv)
///
/// Przykład wysyłania RAGRequest:
/// ```rust
/// let mut bytes = vec![MESSAGE_TYPE_RAG_REQUEST];
/// bytes.extend_from_slice(&rkyv::to_bytes::<rkyv::rancor::Error>(&request)?);
/// stream.write_all(&bytes).await?;
/// ```
pub const MESSAGE_TYPE_RAG_REQUEST: u8 = 0x01;
pub const MESSAGE_TYPE_INGEST_REQUEST: u8 = 0x02;
pub const MESSAGE_TYPE_CANCEL_REQUEST: u8 = 0x03;

// ============================================================================
// CANCELLATION REQUEST: Client → Router (Cancel ongoing operation)
// ============================================================================

/// Request do anulowania trwającej operacji.
///
/// Wysyłany z klienta do Router gdy użytkownik chce przerwać operację (np. naciśnie Stop).
/// Router przerywa streaming i zwalnia zasoby związane z request_id.
///
/// WAŻNE: Cancellation jest "best effort" - operacja może zakończyć się
/// zanim anulowanie dotrze do serwera.
///
/// Przykład użycia:
/// ```rust
/// // Klient wysyła cancel request
/// let cancel = CancelRequest {
///     request_id: "uuid-123".to_string(),
///     reason: Some("User pressed Stop button".to_string()),
/// };
///
/// // Wysyłanie przez QUIC
/// let mut bytes = vec![MESSAGE_TYPE_CANCEL_REQUEST];
/// bytes.extend_from_slice(&rkyv::to_bytes::<rkyv::rancor::Error>(&cancel)?);
/// stream.write_all(&bytes).await?;
/// ```
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
pub struct CancelRequest {
    /// ID requestu do anulowania (musi być aktywny streaming request)
    pub request_id: String,

    /// Opcjonalny powód anulowania (dla logów/debugging)
    pub reason: Option<String>,
}

/// Response po anulowaniu operacji.
///
/// Zwracany po próbie anulowania operacji.
///
/// Statusy:
/// - `Cancelled`: Operacja została pomyślnie anulowana
/// - `NotFound`: Request o podanym ID nie istnieje lub już się zakończył
/// - `AlreadyCompleted`: Request zakończył się przed anulowaniem
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
pub struct CancelResponse {
    /// ID requestu (correlation)
    pub request_id: String,

    /// Czy anulowanie się powiodło
    pub success: bool,

    /// Status anulowania
    pub status: CancellationStatus,

    /// Opcjonalny komunikat (np. "Request already completed")
    pub message: Option<String>,
}

/// Status anulowania operacji.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub enum CancellationStatus {
    /// Operacja została pomyślnie anulowana
    Cancelled,

    /// Request o podanym ID nie istnieje
    NotFound,

    /// Request zakończył się przed anulowaniem
    AlreadyCompleted,
}

// ============================================================================
// DOCUMENT INGESTION: Router → RAG (Document Upload/Management)
// ============================================================================

/// Request do zaindeksowania dokumentu w RAG.
///
/// Wysyłany z Router do RAG gdy klient uploaduje dokument przez HTTP API.
/// Router konwertuje multipart/form-data lub JSON na ten typ i wysyła przez QUIC.
///
/// Pola:
/// - `request_id`: UUID v4 generowane przez Router (dla korelacji)
/// - `document_id`: Unikalny ID dokumentu (podawany przez klienta lub generowany)
/// - `content`: Treść dokumentu (text, file data)
/// - `metadata`: Dodatkowe metadata (title, author, tags, etc.)
/// - `index_flags`: Które indeksy utworzyć (FTS, Vector, Graph, HiRAG, Metadata)
///
/// Przykład:
/// ```rust
/// let request = IngestRequest {
///     request_id: uuid::Uuid::new_v4().to_string(),
///     document_id: "doc_12345".to_string(),
///     content: DocumentContent::FileData {
///         data: pdf_bytes,
///         filename: "raport.pdf".to_string(),
///     },
///     metadata: vec![("title".to_string(), "Raport Q4".to_string())],
///     index_flags: vec!["fts".to_string(), "vector".to_string(), "metadata".to_string()],
/// };
/// ```
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
pub struct IngestRequest {
    /// UUID v4 requestu (generowane przez Router)
    pub request_id: String,

    /// Unikalny ID dokumentu (np. "doc_12345", UUID, hash)
    pub document_id: String,

    /// Treść dokumentu do zaindeksowania
    pub content: DocumentContent,

    /// Metadata dokumentu (key-value pairs: title, author, tags, etc.)
    pub metadata: Vec<(String, String)>,

    /// Lista indeksów do utworzenia (fts, vector, graph, hirag, metadata)
    /// Jeśli puste, RAG użyje wszystkich dostępnych indeksów
    pub index_flags: Vec<String>,
}

/// Dane binarne pliku dla DocumentContent.
///
/// Używa base64 encoding dla serde (JSON API) i raw bytes dla rkyv (QUIC).
#[serde_as]
#[derive(Archive, Deserialize, Serialize, Debug, Clone, SerdeDeserialize, SerdeSerialize)]
pub struct FileDataContent {
    /// Surowe bajty pliku (base64 w JSON, raw bytes w QUIC)
    #[serde_as(as = "Base64")]
    pub data: Vec<u8>,
    /// Nazwa pliku z rozszerzeniem (np. "dokument.pdf")
    pub filename: String,
}

/// Treść dokumentu do zaindeksowania.
///
/// Obsługuje różne źródła treści:
/// - Text: Bezpośredni tekst (np. z JSON API)
/// - FileData: Surowe bajty pliku + nazwa (np. z multipart/form-data upload)
///
/// UWAGA: FilePath nie jest wspierana w protokole QUIC (tylko Text i FileData)
/// bo Router otrzymuje dane od klienta przez HTTP, nie ścieżki do plików.
///
/// Format JSON dla FileData używa base64 encoding:
/// ```json
/// {
///   "FileData": {
///     "data": "SGVsbG8gd29ybGQh",  // base64
///     "filename": "test.txt"
///   }
/// }
/// ```
#[derive(Archive, Deserialize, Serialize, Debug, Clone, SerdeDeserialize, SerdeSerialize)]
pub enum DocumentContent {
    /// Czysty tekst (dla JSON API)
    Text(String),

    /// Dane binarne pliku z nazwą (dla multipart upload)
    /// - data: Surowe bajty pliku (base64 w JSON, raw w QUIC)
    /// - filename: Nazwa pliku z rozszerzeniem (do detekcji MIME type)
    FileData(FileDataContent),
}

/// Response po zaindeksowaniu dokumentu.
///
/// Zawiera:
/// - Potwierdzenie ID dokumentu
/// - Statystyki (liczba chunków, wektorów)
/// - Które indeksy zostały utworzone
/// - Metryki wydajności (czasy przetwarzania)
///
/// Przykład:
/// ```rust
/// let response = IngestResponse {
///     request_id: "uuid-123".to_string(),
///     document_id: "doc_12345".to_string(),
///     status: IngestionStatus::Success,
///     chunk_count: 42,
///     vector_count: 42,
///     indexed_in: vec!["fts".to_string(), "vector".to_string(), "metadata".to_string()],
///     metrics: IngestMetrics {
///         file_processing_ms: 1523,
///         chunking_ms: 234,
///         embedding_ms: 876,
///         fts_indexing_ms: 45,
///         vector_indexing_ms: 123,
///         graph_indexing_ms: 0,
///         total_ms: 2801,
///         embedding_tokens_per_sec: Some(1250.5),
///     },
///     error: None,
/// };
/// ```
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
pub struct IngestResponse {
    /// UUID requestu (correlation z IngestRequest)
    pub request_id: String,

    /// ID zaindeksowanego dokumentu
    pub document_id: String,

    /// Status operacji (Success, Duplicate, Error)
    pub status: IngestionStatus,

    /// Liczba chunków utworzonych
    pub chunk_count: u32,

    /// Liczba wektorów utworzonych
    pub vector_count: u32,

    /// Które indeksy zostały utworzone (fts, vector, graph, hirag, metadata)
    pub indexed_in: Vec<String>,

    /// Metryki wydajności operacji
    pub metrics: IngestMetrics,

    /// Komunikat błędu (jeśli status=Error)
    pub error: Option<String>,
}

/// Status operacji ingestion.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq)]
pub enum IngestionStatus {
    /// Sukces - dokument zaindeksowany
    Success,

    /// Duplikat - dokument z taką zawartością już istnieje (pominięto)
    Duplicate,

    /// Zaktualizowano - dokument z tym ID istniał i został zaktualizowany
    Updated,

    /// Dowiązanie - dokument z taką zawartością już istnieje pod innym ID,
    /// utworzono alias/referencję zamiast re-indeksowania
    LinkedToDuplicate,

    /// Błąd - operacja się nie powiodła
    Error,
}

/// Metryki operacji ingestion.
///
/// Zawiera czasy wszystkich faz przetwarzania dokumentu:
/// - file_processing_ms: Ekstrakcja tekstu z pliku (PDF/Office/OCR/STT)
/// - chunking_ms: Semantyczne dzielenie na fragmenty
/// - embedding_ms: Generowanie embeddingów
/// - fts_indexing_ms: Indeksowanie full-text search (Tantivy)
/// - vector_indexing_ms: Indeksowanie wektorowe (HNSW)
/// - graph_indexing_ms: Indeksowanie grafowe (GSW)
/// - total_ms: Całkowity czas operacji
/// - embedding_tokens_per_sec: Przepustowość embeddingów (jeśli applicable)
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
pub struct IngestMetrics {
    /// Czas przetwarzania pliku (PDF/Office/OCR/STT) w ms
    pub file_processing_ms: u64,

    /// Czas chunkowania w ms
    pub chunking_ms: u64,

    /// Czas generowania embeddingów w ms
    pub embedding_ms: u64,

    /// Czas indeksowania FTS w ms
    pub fts_indexing_ms: u64,

    /// Czas indeksowania wektorów w ms
    pub vector_indexing_ms: u64,

    /// Czas indeksowania grafu w ms
    pub graph_indexing_ms: u64,

    /// Całkowity czas operacji w ms
    pub total_ms: u64,

    /// Tokeny na sekundę (dla embeddingów)
    pub embedding_tokens_per_sec: Option<f32>,
}

// ============================================================================
// UNIFIED MODEL PROTOCOL - Universal Format dla wszystkich modeli
// ============================================================================
//
// Uniwersalny protokół obsługujący:
// - Embeddings (text → vectors)
// - Completion (text generation, chat)
// - Image (generation, editing)
// - Audio (TTS, STT, music)
// - Vision (image understanding)
// - RAG (retrieval augmented generation)
//
// Wspiera tryb streaming i non-streaming dla wszystkich typów.
//
// ============================================================================

/// Uniwersalny request envelope dla wszystkich operacji modelowych.
///
/// Używany dla:
/// - Client → Router (główne API)
/// - Router → RAG (retrieval requests)
/// - Router → Embeddings Engine (embedding requests)
/// - Router → LLM (completion requests)
/// - RAG → Router (callback requests)
///
/// # Przykład użycia - Embeddings:
/// ```rust
/// let request = ModelRequest {
///     request_id: uuid::Uuid::new_v4().to_string(),
///     payload: ModelPayload::Embeddings(EmbeddingsPayload {
///         model: "gemma".to_string(),
///         input: vec!["Hello world".to_string()],
///         normalize: true,
///     }),
///     stream: false,
///     metadata: None,
/// };
/// ```
///
/// # Przykład użycia - Chat Completion:
/// ```rust
/// let request = ModelRequest {
///     request_id: uuid::Uuid::new_v4().to_string(),
///     payload: ModelPayload::Completion(CompletionPayload {
///         model: "gpt-4".to_string(),
///         messages: vec![Message { role: "user".to_string(), content: "Hello!".to_string() }],
///         temperature: Some(0.7),
///         max_tokens: Some(1000),
///         stream: true,
///         ..Default::default()
///     }),
///     stream: true,
///     metadata: None,
/// };
/// ```
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
pub struct ModelRequest {
    /// Unikalny ID requestu (UUID v4)
    pub request_id: String,

    /// Payload dla danego typu modelu
    pub payload: ModelPayload,

    /// Czy odpowiedź ma być streamowana
    /// - true: Odpowiedź przychodzi jako seria ModelStreamChunk
    /// - false: Odpowiedź przychodzi jako pojedynczy ModelResponse
    pub stream: bool,

    /// Opcjonalne metadata (document metadata, etc.)
    pub metadata: Option<Vec<(String, String)>>,

    /// ID sesji użytkownika (dla Memory i kontekstu konwersacji)
    /// Router używa tego do:
    /// - Odczytu kontekstu z Memory przed wywołaniem modelu
    /// - Zapisu wyników do Memory po odpowiedzi modelu
    pub session_id: Option<String>,
}

/// Payload dla różnych typów modeli.
///
/// Każdy wariant zawiera specyficzne dane dla danego typu operacji.
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
pub enum ModelPayload {
    /// Embeddings - konwersja tekstu na wektory
    Embeddings(EmbeddingsPayload),

    /// Completion - generacja tekstu (chat, completion)
    Completion(CompletionPayload),

    /// Image - generacja lub edycja obrazów
    Image(ImagePayload),

    /// Audio - TTS (text-to-speech) lub STT (speech-to-text)
    Audio(AudioPayload),

    /// Vision - rozumienie obrazów
    Vision(VisionPayload),

    /// RAG - retrieval augmented generation
    RAG(RAGPayload),

    /// Rerank - rerankowanie dokumentów względem zapytania (cross-encoder)
    Rerank(RerankPayload),

    /// Memory - operacje na pamięci AI (graf wiedzy, embeddingi, multi-hop reasoning)
    Memory(MemoryPayload),

    /// PrefixCacheInit - inicjalizacja KV cache dla promptów systemowych
    /// Wysyłane przez Router przy połączeniu z LLM Engine
    PrefixCacheInit(PrefixCacheInitRequest),

    /// MeetingEvent - eventy od Meeting Bota do Routera (summary, action items).
    /// Bot otwiera strumień reverse QUIC i wysyła ModelRequest z tym payloadem;
    /// router odbiera w `dispatch_reverse_request` i persistuje do DB.
    MeetingEvent(MeetingEventData),

    /// PromptFetch - pobranie treści promptu z DB routera po `prompt_id` + język.
    /// Używane przez kontenery (np. meeting-bot) żeby nie duplikować treści
    /// promptów po stronie obrazów. Router odpowiada `ModelResult::PromptFetched`.
    PromptFetch(PromptFetchRequest),

    /// Browser - operacje na aktywnej stronie Chromium w kontenerze teams-bot
    /// (screenshot przez CDP, snapshot DOM). Router adresuje bota przez
    /// `ServiceManager::get_quic_llm_client("meeting-bot-{session_id}")`.
    Browser(BrowserPayload),
}

/// Operacje inspekcji uruchomionej strony przeglądarki w kontenerze bota.
/// Wspólne użycie: diagnostyka meetingu z dashboardu (co bot aktualnie widzi).
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct BrowserPayload {
    pub operation: BrowserOperation,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub enum BrowserOperation {
    /// PNG screenshot. `full_page=true` scrolluje i składa całą stronę;
    /// `false` zwraca tylko viewport (znacznie szybsze, bez stitchowania).
    Screenshot { full_page: bool },
    /// Zwraca `document.documentElement.outerHTML` po aktualnym renderingu.
    Dom,
}

// ============================================================================
// PROMPT FETCH PAYLOAD
// ============================================================================

/// Request odczytu promptu z DB routera. Router robi fallback na `pl`
/// gdy wariant w żądanym języku nie istnieje (zgodnie z `repository::find_prompt`).
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
pub struct PromptFetchRequest {
    /// Identyfikator promptu (np. `transcription_summarization`).
    pub prompt_id: String,
    /// Kod języka ISO-639-1 (np. `pl`, `en`, `de`).
    pub language: String,
}

// ============================================================================
// MEETING EVENT PAYLOAD
// ============================================================================

/// Event od Meeting Bota do Routera. Niesie meeting_key (publiczny identyfikator
/// sesji) + timestamp + typowy payload (summary albo action items).
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq)]
pub struct MeetingEventData {
    /// meeting_key z tabeli meeting_sessions. Router resolvuje do session_id.
    pub meeting_key: String,
    /// Unix epoch ms w momencie wygenerowania eventu po stronie bota.
    pub timestamp_ms: i64,
    /// Typ eventu + payload.
    pub payload: MeetingEventPayload,
}

/// Warianty eventów meeting. Każdy wariant niesie dane do tej samej sesji
/// (adresowanej przez `MeetingEventData::meeting_key`).
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq)]
pub enum MeetingEventPayload {
    /// Nowe podsumowanie wygenerowane przez timer bota. Router insertuje
    /// do `meeting_summaries` (append-only historia).
    SummaryUpdate {
        decisions_text: String,
        summary_text: String,
        /// Rozwiązany LLM model po alias resolution (nazwa silnika/modelu).
        model: String,
    },
    /// Lista action items wykryta przez bota. Router upsertuje po content_hash
    /// (owner+task), więc powtórzone pozycje tylko odświeżają deadline.
    ActionItemsUpdate {
        items: Vec<MeetingActionItemData>,
    },
    /// Pojedynczy fragment transkrypcji wygenerowany przez STT na bocie.
    /// Niesie metadane diarization/model surowo — router przed broadcastem
    /// może wzbogacić `speaker_name`/`is_enrolled` z DB voice_profiles.
    /// Persist chunków transkryptu leci osobną ścieżką przez transcript_store
    /// (ModelRequest::Audio z metadata `meeting_id`), ten wariant służy live
    /// broadcastowi do dashboardu.
    TranscriptEntry {
        /// Surowe id z diarization ("SPEAKER_00"…) albo profile_id po match.
        speaker_id: String,
        /// Display name jeśli dostępny (z Teams DOM albo z voice_profile).
        speaker_name: Option<String>,
        /// True gdy speaker_id pochodzi z enrolled voice_profile.
        is_enrolled: bool,
        /// Pewność dopasowania speaker_id (0.0..1.0) jeśli znana.
        speaker_confidence: Option<f32>,
        text: String,
        /// ISO-639-1 (np. "pl", "en") jeśli STT zwrócił język.
        language: Option<String>,
        /// Rozwiązany model STT po alias resolution.
        resolved_stt_model: String,
        /// Czas end-to-end STT (wysłanie audio → otrzymanie tekstu) w ms.
        latency_ms: u64,
    },
    /// Pełny snapshot rosteru po stronie bota. Każda emisja ZASTĘPUJE
    /// poprzedni stan (snapshot, nie diff). Bot wysyła go raz na DOM scan
    /// zamiast N osobnych eventów per uczestnik — O(N) RT → O(1).
    /// Pusta lista = nikt poza botem nie jest widoczny.
    RosterSnapshot {
        entries: Vec<RosterEntry>,
    },
    /// Info o modelach używanych w sesji. Bot wysyła raz po join_meeting.
    /// Router przed broadcastem rozwija aliasy (`teams-stt` → rzeczywisty
    /// engine) — pola tu trzymają aliasy takie jak bot je dostał z configu.
    BackendUpdate {
        stt_model: String,
        tts_model: String,
        summarization_model: String,
        /// Model diarization hardcoded w mesh configu routera (np. `pyannote-3.1`).
        diarization_model: String,
        streaming_latency_ms: Option<u32>,
        enrolled_speakers: Option<u32>,
        total_participants: Option<u32>,
    },
    /// Etap cyklu życia sesji meeting bota. Emitowany przez bota przy przejściach
    /// kluczowych fazami (spawn kontenera → chromium → prejoin → joined). Router
    /// persistuje aktualny stage w `meeting_sessions.lifecycle_stage`, broadcast
    /// do GUI idzie przez ten sam kanał co pozostałe MeetingEventPayload.
    LifecycleUpdate {
        /// Jeden ze stage stringów — patrz `LIFECYCLE_*` constants.
        stage: String,
        /// Opcjonalny komunikat błędu lub informacja diagnostyczna (np. przy
        /// `LIFECYCLE_FAILED` pełny tekst błędu z `join_meeting`).
        details: Option<String>,
    },
    /// Klatka wideo per-uczestnik scrappowana z `<video>` elementu kafelka
    /// w DOM Teams. Bot wysyła ją w cyklu (domyślnie 1 fps) wyłącznie dla
    /// kafelków z aktywnym `MediaStream` (kamera włączona). `jpeg` to surowe
    /// bajty już zdekodowane z base64 — transport JS→Rust idzie przez
    /// `__tentaflowEvent` (JSON-only binding), więc base64 jest jedynie
    /// kodowaniem na granicy CDP; po stronie Rust żyje już binarnie i rkyv
    /// pcha to jako `Vec<u8>` bez ponownego kodowania.
    VideoFrame {
        /// `data-tid` kafelka — w obecnym Teams to nazwa uczestnika, traktujemy
        /// jednak jako opaque id (może się zmienić w przyszłych wersjach UI).
        participant_id: String,
        /// Display name z `tileDisplayName` — None gdy DOM nie udostępnia
        /// czytelnej nazwy (skrajny edge case przy świeżo dołączającym kafelku).
        name: Option<String>,
        /// Unix epoch ms momentu capture po stronie strony.
        ts_ms: u64,
        /// JPEG bytes (q≈0.6, max ~320px szerokości).
        jpeg: Vec<u8>,
    },
    /// Atrybuty rozpoznane z klatki wideo uczestnika (emocje + wiek + płeć).
    /// Router emituje ten wariant po inferencji vision modeli na klatce z
    /// `VideoFrame` (throttle 1 inference / 2s per uczestnik). Nie persistujemy
    /// do DB — broadcast wyłącznie live do dashboardu obok pozostałych
    /// MeetingEventPayload.
    ParticipantAttributes {
        /// `data-tid` kafelka — taki sam jak w `VideoFrame.participant_id`.
        participant_id: String,
        /// Display name z `VideoFrame.name`. GUI używa go do dopasowania do
        /// rosteru po nazwie, gdy `participant_id` z DOM Teams nie pokrywa
        /// się ze `speaker_id` z diarization.
        name: Option<String>,
        ts_ms: u64,
        /// Etykieta emocji z 8-klasowego AffectNet HSEmotion ("Happiness",
        /// "Neutral", "Sadness", "Surprise", "Fear", "Anger", "Disgust",
        /// "Contempt"). None gdy detector nie znalazł twarzy lub emotion
        /// engine nie był podpięty — GUI traktuje to jako "wyczyść badge".
        emotion: Option<String>,
        /// Pewność emocji [0..1] po EWMA smoothing.
        emotion_confidence: Option<f32>,
        /// Wiek w latach (regresja MiVOLO).
        age: Option<f32>,
        /// Prawdopodobieństwo płci męskiej [0..1] z MiVOLO (>0.5 mężczyzna).
        gender_male_prob: Option<f32>,
    },
}

/// Etapy cyklu życia sesji meeting bota. Używane w
/// `MeetingEventPayload::LifecycleUpdate::stage` oraz w kolumnie
/// `meeting_sessions.lifecycle_stage`. Zachować tę listę jako single source
/// of truth — bot i router trzymają się tych samych stringów.
pub const LIFECYCLE_CONTAINER_SPAWNED: &str = "container_spawned";
pub const LIFECYCLE_BROWSER_LAUNCHED: &str = "browser_launched";
pub const LIFECYCLE_NAVIGATING: &str = "navigating";
pub const LIFECYCLE_PREJOIN_READY: &str = "prejoin_ready";
pub const LIFECYCLE_JOINING: &str = "joining";
/// Bot kliknal Join, ale host jeszcze go nie wpuscil — ekran "Someone in the
/// meeting will let you in soon". Wyemitowane po Joining gdy DOM lobby jest
/// rozpoznany. Pozwala GUI rozroznic "czekamy w lobby" od pelnego JOINED.
pub const LIFECYCLE_LOBBY_WAITING: &str = "lobby_waiting";
pub const LIFECYCLE_JOINED: &str = "joined";
pub const LIFECYCLE_FAILED: &str = "failed";

/// Pojedynczy uczestnik w `RosterSnapshot`. `status` to stringified enum
/// — bot wysyła `"joined"`, `"left"`, `"speaking"`. Dashboard odfiltrowuje
/// nieznane warianty bez błędu.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq)]
pub struct RosterEntry {
    pub speaker_id: String,
    pub speaker_name: Option<String>,
    pub status: String,
    /// Sekundy od ostatniej mowy; `None` gdy jeszcze nie mówił.
    pub last_spoken_ago_sec: Option<u32>,
    /// Czy kafelek ma aktywny strumień wideo (kamera włączona). Pochodzi
    /// z `data-stream-type=Video` lub obecności żywego `<video>` elementu.
    /// GUI wykorzystuje to żeby pokazać badge kamery i decydować czy ma
    /// sens renderować podgląd `VideoFrame` dla tego uczestnika.
    pub has_video: bool,
    /// Czy kafelek raportuje aktywny strumień audio. Wyznaczane z obecności
    /// `data-stream-type=Audio` w DOM oraz odsłuchu mute markerów.
    pub has_audio: bool,
    /// Uczestnik widoczny wśród kafelków sceny (MixedStage / only-videos).
    /// Może być `false` gdy ktoś jest tylko w panelu rosteru bez kamery.
    pub in_stage: bool,
    /// Uczestnik widoczny w panelu rosteru/People. Wraz z `in_stage=false`
    /// daje GUI sygnał że to off-camera participant — kluczowe dla pełnej
    /// listy uczestników (scena gubi nieaktywnych mówców).
    pub in_roster: bool,
}

/// Pojedynczy action item przesyłany w `ActionItemsUpdate`.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct MeetingActionItemData {
    pub owner: String,
    pub task: String,
    pub deadline: Option<String>,
}

/// Frame pushowany przez binary-WS do dashboard GUI po każdym sukcesie
/// `persist_meeting_event`. Zawiera ten sam payload co bot wysłał routerowi —
/// subscriberzy po stronie GUI (live widok meetingu) renderują go bez
/// konieczności odpytywania DB. Filtrowanie po ownership (user_id ↔
/// meeting_sessions.owner_user_id) dzieje się server-side w writer task,
/// więc frame nigdy nie dotrze do niepowołanego usera.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq)]
pub struct MeetingLiveEvent {
    /// meeting_key sesji — adresuje która sesja, GUI route’uje do widoku.
    pub meeting_key: String,
    /// Unix epoch ms w momencie oryginalnej emisji przez bota (reuse z
    /// `MeetingEventData::timestamp_ms`).
    pub timestamp_ms: i64,
    /// Ten sam payload który przeszedł persist/log w routerze.
    pub payload: MeetingEventPayload,
}

// ============================================================================
// EMBEDDINGS PAYLOAD
// ============================================================================

/// Payload dla embeddings request.
///
/// Konwertuje tekst na wektory numeryczne (embeddings).
///
/// # Przykład:
/// ```rust
/// let payload = EmbeddingsPayload {
///     model: "gemma".to_string(),
///     input: vec!["Hello".to_string(), "World".to_string()],
///     normalize: true,
/// };
/// ```
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
pub struct EmbeddingsPayload {
    /// Nazwa modelu (np. "gemma", "qwen", "text-embedding-3-small")
    pub model: String,

    /// Teksty do zakodowania (batch processing)
    pub input: Vec<String>,

    /// Czy normalizować wektory do unit length (L2 norm = 1.0)
    /// Domyślnie: true (zalecane dla większości zastosowań)
    pub normalize: bool,
}

// ============================================================================
// RERANK PAYLOAD
// ============================================================================

/// Payload dla rerank request (cross-encoder rerankowanie dokumentów).
///
/// Używa cross-encoder modelu do obliczenia relevance score
/// dla każdego dokumentu względem zapytania.
///
/// Cross-encoder jest dokładniejszy niż bi-encoder (embeddings),
/// bo przetwarza query+document razem zamiast osobno.
///
/// # Przykład:
/// ```rust
/// let payload = RerankPayload {
///     model: "bge-reranker-v2-m3".to_string(),
///     query: "What is machine learning?".to_string(),
///     documents: vec![
///         "Machine learning is a subset of AI...".to_string(),
///         "Deep learning uses neural networks...".to_string(),
///     ],
///     top_n: Some(5),
///     return_documents: false,
/// };
/// ```
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
pub struct RerankPayload {
    /// Nazwa modelu reranking (np. "bge-reranker-v2-m3", "ms-marco-MiniLM")
    pub model: String,

    /// Zapytanie względem którego rerankować dokumenty
    pub query: String,

    /// Lista dokumentów do rerankowania
    pub documents: Vec<String>,

    /// Ile najlepszych dokumentów zwrócić (None = wszystkie)
    pub top_n: Option<usize>,

    /// Czy zwrócić tekst dokumentów w wyniku (domyślnie false)
    pub return_documents: bool,
}

// ============================================================================
// COMPLETION PAYLOAD
// ============================================================================

/// Payload dla completion request (text generation, chat).
///
/// Obsługuje zarówno chat completion jak i text completion.
///
/// # Przykład - Chat:
/// ```rust
/// let payload = CompletionPayload {
///     model: "gpt-4".to_string(),
///     messages: vec![
///         Message { role: "system".to_string(), content: "You are helpful assistant".to_string() },
///         Message { role: "user".to_string(), content: "Hello!".to_string() },
///     ],
///     temperature: Some(0.7),
///     max_tokens: Some(1000),
///     stream: true,
///     ..Default::default()
/// };
/// ```
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
pub struct CompletionPayload {
    /// Nazwa modelu (np. "gpt-4-turbo", "claude-3-5-sonnet", "deepseek-v3")
    pub model: String,

    /// Pre-formatted prompt (jeśli klient zastosował chat template).
    /// Jeśli ustawione, `messages` są ignorowane i używany jest ten prompt.
    /// Jeśli None, serwer formatuje `messages` używając template modelu.
    pub prompt: Option<String>,

    /// Messages dla chat completion (używane gdy prompt = None)
    pub messages: Vec<Message>,

    /// Temperature (0.0 - 2.0, default: 1.0)
    /// - 0.0: Deterministyczne (zawsze ten sam output)
    /// - 1.0: Balanced creativity
    /// - 2.0: Bardzo kreatywne (losowe)
    pub temperature: Option<f32>,

    /// Maksymalna liczba tokenów w odpowiedzi
    pub max_tokens: Option<u32>,

    /// Top-p sampling (nucleus sampling, 0.0 - 1.0)
    pub top_p: Option<f32>,

    /// Stop sequences (zatrzymaj generację gdy napotkasz te stringi)
    pub stop: Option<Vec<String>>,

    /// Presence penalty (-2.0 - 2.0)
    /// Pozytywne wartości zmniejszają powtarzanie tematów
    pub presence_penalty: Option<f32>,

    /// Frequency penalty (-2.0 - 2.0)
    /// Pozytywne wartości zmniejszają powtarzanie tokenów
    pub frequency_penalty: Option<f32>,

    /// Opcje TTS dla streaming audio response.
    /// Jeśli ustawione, Router będzie generował AudioChunk dla każdego zdania.
    /// Klient otrzymuje TextDelta + AudioChunk chunki przeplatane.
    pub tts_options: Option<TTSStreamingOptions>,

    /// Opcje Memory dla integracji z TentaFlow.Memory.
    /// Jeśli ustawione, Router odpyta Memory przed wywołaniem modelu
    /// i zapisze wyniki po odpowiedzi.
    pub memory_options: Option<MemoryOptions>,

    /// Audio input dla konwersacji głosowych.
    /// Jeśli podane, Router najpierw przetworzy przez STT (transkrypcja)
    /// i speaker identification, a następnie wyśle do LLM.
    /// Audio powinno być w formacie WAV.
    pub audio_input: Option<Vec<u8>>,

    /// Prefix Cache ID dla KV cache reuse.
    ///
    /// Jeśli ustawione, LLM Server użyje zapisanego KV cache dla tego ID
    /// zamiast obliczać od nowa. Używane dla stałych system promptów.
    ///
    /// Flow:
    /// 1. Pierwszy request z danym prefix_cache_id → oblicz i zapisz KV cache
    /// 2. Kolejne requesty z tym samym ID → użyj zapisanego cache
    ///
    /// UWAGA: prefix_cache_id powinien być hash/identyfikator STAŁEJ części prompta
    /// (system message). Zmienna część (user message) jest obliczana normalnie.
    ///
    /// Przykład:
    /// - prefix_cache_id: "jarvis_system_v1"
    /// - prefix_text: "Jesteś Jarvis - inteligentnym asystentem..." (stała część)
    /// - messages: tylko nowa wiadomość user
    pub prefix_cache_id: Option<String>,

    /// Tekst prefixu do cache'owania (używane z prefix_cache_id).
    ///
    /// Jeśli prefix_cache_id jest ustawione ale cache nie istnieje,
    /// ten tekst zostanie użyty do utworzenia cache.
    /// Jeśli cache istnieje, ten tekst jest ignorowany.
    pub prefix_text: Option<String>,
}

/// Opcje Memory dla integracji z TentaFlow.Memory.
///
/// Gdy ustawione w CompletionPayload.memory_options, Router:
/// 1. Odpytuje Memory o kontekst przed wywołaniem modelu
/// 2. Wstrzykuje kontekst do system message
/// 3. Zapisuje nowe fakty z odpowiedzi do Memory (async)
#[derive(Archive, Deserialize, Serialize, Debug, Clone, Default)]
pub struct MemoryOptions {
    /// Czy pamięć jest włączona (domyślnie true jeśli session_id podane)
    pub enabled: Option<bool>,

    /// Identyfikator sesji rozmowy (UUID) - używany do śledzenia kontekstu
    pub session_id: Option<String>,

    /// ID rozpoznanej osoby z STT (dla integracji głosowej)
    pub person_id: Option<String>,

    /// Poziom pewności rozpoznania głosu (0.0-1.0)
    pub speaker_confidence: Option<f32>,

    /// Czy zapisywać nowe informacje do Memory (domyślnie true)
    pub store_enabled: Option<bool>,

    /// Czy odpytywać Memory przed modelem (domyślnie true)
    pub query_enabled: Option<bool>,
}

/// Opcje streaming TTS dla completion response.
///
/// Gdy ustawione w CompletionPayload.tts_options, Router:
/// 1. Generuje tekst z LLM (streaming)
/// 2. Po każdym zdaniu (lub N tokenach) generuje audio chunk
/// 3. Wysyła TextDelta + AudioChunk przeplatane do klienta
///
/// Przykład:
/// ```rust
/// let tts = TTSStreamingOptions {
///     model: "sherpa-tts".to_string(),
///     voice: "jarvis".to_string(),
///     format: Some("opus".to_string()),  // opus dla niskiej latencji
///     speed: None,
/// };
/// ```
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
pub struct TTSStreamingOptions {
    /// Model TTS (np. "sherpa-tts", "tts-1")
    pub model: String,

    /// Głos do użycia (np. "jarvis", "justyna", "alloy")
    pub voice: String,

    /// Format audio (domyślnie "opus" dla niskiej latencji streaming)
    /// - "opus": Dobry dla streaming (mała latencja, dobre quality)
    /// - "mp3": Kompatybilny wszędzie
    /// - "wav": Brak kompresji
    pub format: Option<String>,

    /// Prędkość mówienia (0.25 - 4.0, default: 1.0)
    pub speed: Option<f32>,
}

impl Default for CompletionPayload {
    fn default() -> Self {
        Self {
            model: String::new(),
            prompt: None,
            messages: Vec::new(),
            temperature: Some(1.0),
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
        }
    }
}

// ============================================================================
// IMAGE PAYLOAD
// ============================================================================

/// Payload dla image request (generation, editing, variation).
///
/// # Przykład - Generation:
/// ```rust
/// let payload = ImagePayload {
///     operation: ImageOperation::Generate {
///         model: "dall-e-3".to_string(),
///         prompt: "A futuristic city at sunset".to_string(),
///         size: Some("1024x1024".to_string()),
///         quality: Some("hd".to_string()),
///         n: Some(1),
///     },
/// };
/// ```
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
pub struct ImagePayload {
    /// Typ operacji na obrazie
    pub operation: ImageOperation,
}

/// Operacje na obrazach.
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
pub enum ImageOperation {
    /// Generuj obraz z promptu (DALL-E, Stable Diffusion)
    Generate {
        /// Model (np. "dall-e-3", "stable-diffusion-xl")
        model: String,

        /// Prompt opisujący obraz
        prompt: String,

        /// Rozmiar (np. "1024x1024", "1792x1024")
        size: Option<String>,

        /// Jakość ("standard", "hd")
        quality: Option<String>,

        /// Liczba obrazów do wygenerowania
        n: Option<u32>,
    },

    /// Edytuj obraz (inpainting, outpainting)
    Edit {
        /// Model
        model: String,

        /// Oryginalny obraz (PNG, base64 lub raw bytes)
        image: Vec<u8>,

        /// Maska (opcjonalna, PNG, przezroczystość = obszar do edycji)
        mask: Option<Vec<u8>>,

        /// Prompt opisujący zmiany
        prompt: String,

        /// Rozmiar wyniku
        size: Option<String>,

        /// Liczba wariantów
        n: Option<u32>,
    },

    /// Stwórz warianty obrazu
    Variation {
        /// Model
        model: String,

        /// Oryginalny obraz
        image: Vec<u8>,

        /// Liczba wariantów
        n: Option<u32>,

        /// Rozmiar
        size: Option<String>,
    },
}

// ============================================================================
// AUDIO PAYLOAD
// ============================================================================

/// Payload dla audio request (TTS, STT).
///
/// # Przykład - TTS:
/// ```rust
/// let payload = AudioPayload {
///     operation: AudioOperation::TTS {
///         model: "tts-1-hd".to_string(),
///         input: "Hello, world!".to_string(),
///         voice: "alloy".to_string(),
///         format: Some("mp3".to_string()),
///         speed: Some(1.0),
///         language: Some("en".to_string()),
///     },
/// };
/// ```
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
pub struct AudioPayload {
    /// Typ operacji audio
    pub operation: AudioOperation,
}

/// Operacje audio.
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
pub enum AudioOperation {
    /// Text-to-Speech (TTS)
    TTS {
        /// Model (np. "tts-1", "tts-1-hd")
        model: String,

        /// Text do zamiany na audio
        input: String,

        /// Głos ("alloy", "echo", "fable", "onyx", "nova", "shimmer")
        voice: String,

        /// Format audio ("mp3", "opus", "aac", "flac", "wav", "pcm")
        format: Option<String>,

        /// Prędkość (0.25 - 4.0, default: 1.0)
        speed: Option<f32>,

        /// Język syntezy (ISO-639-1, np. "en", "pl", "fr", "es", "de").
        /// None = backend wybiera domyślny.
        language: Option<String>,
    },

    /// Speech-to-Text (STT)
    STT {
        /// Model (np. "whisper-1", "whisper-large-v3")
        model: String,

        /// Audio data (raw bytes - MP3, WAV, M4A, etc.)
        audio_data: Vec<u8>,

        /// Język (ISO-639-1, np. "en", "pl", "de")
        /// Jeśli None, Whisper automatycznie wykryje język
        language: Option<String>,

        /// Format odpowiedzi:
        /// - "json" - prosty JSON z tekstem (domyślny)
        /// - "text" - surowy tekst
        /// - "srt" - napisy SRT
        /// - "vtt" - napisy WebVTT
        /// - "verbose_json" - szczegółowy JSON z segmentami, timestamps, no_speech_prob
        response_format: Option<String>,

        /// Prompt dla kontekstu (opcjonalny)
        prompt: Option<String>,

        /// Temperature (0.0 - 1.0)
        temperature: Option<f32>,

        /// Granularność timestampów (tylko dla verbose_json):
        /// - "segment" - timestamps per segment (domyślny)
        /// - "word" - timestamps per word
        timestamp_granularities: Option<Vec<String>>,

        // === OPCJE FILTROWANIA ===

        /// Próg no_speech_prob do filtrowania halucynacji
        /// Segmenty z no_speech_prob >= threshold zostaną odfiltrowane
        /// None = brak filtrowania
        /// Typowe wartości: 0.5 - 0.8
        no_speech_threshold: Option<f32>,

        /// Minimalny avg_logprob dla segmentu
        /// Segmenty z avg_logprob < threshold zostaną odfiltrowane
        /// None = brak filtrowania (typowo: -1.0)
        avg_logprob_threshold: Option<f32>,

        /// Maksymalny compression_ratio dla segmentu
        /// Segmenty z compression_ratio > threshold zostaną odfiltrowane
        /// None = brak filtrowania (typowo: 2.4)
        compression_ratio_threshold: Option<f32>,
    },

    // =========================================================================
    // SPEAKER IDENTIFICATION - Zarządzanie bazą głosów
    // =========================================================================

    /// Rejestruje nowego mówcę z próbek audio.
    ///
    /// Wymaga co najmniej jednej próbki audio (zalecane 3-5).
    /// Embedding jest ekstrahowany z każdej próbki i zapisywany w bazie.
    ///
    /// # Przykład:
    /// ```rust
    /// AudioOperation::SpeakerEnroll {
    ///     speaker_id: "jan_kowalski".to_string(),
    ///     speaker_name: "Jan Kowalski".to_string(),
    ///     audio_samples: vec![audio1_bytes, audio2_bytes],
    ///     metadata: vec![("department".to_string(), "IT".to_string())],
    /// }
    /// ```
    SpeakerEnroll {
        /// Unikalny identyfikator mówcy (np. "jan_kowalski", UUID)
        speaker_id: String,

        /// Wyświetlana nazwa mówcy (np. "Jan Kowalski")
        speaker_name: String,

        /// Próbki audio do rejestracji (raw bytes - WAV, MP3, etc.)
        /// Zalecane: 3-5 próbek po min. 3 sekundy każda
        audio_samples: Vec<Vec<u8>>,

        /// Metadata mówcy (key-value pairs: department, role, etc.)
        metadata: Vec<(String, String)>,
    },

    /// Dodaje próbki audio do istniejącego mówcy.
    ///
    /// Embeddingi są dodawane do istniejących, centroid jest przeliczany.
    SpeakerAddSamples {
        /// ID mówcy do aktualizacji
        speaker_id: String,

        /// Nowe próbki audio do dodania
        audio_samples: Vec<Vec<u8>>,
    },

    /// Usuwa mówcę z bazy głosów.
    SpeakerRemove {
        /// ID mówcy do usunięcia
        speaker_id: String,
    },

    /// Aktualizuje nazwę (imię) mówcy w bazie głosów.
    ///
    /// Używane gdy użytkownik koryguje swoje imię (np. "mam na imię Piotr, nie Jan").
    SpeakerUpdateName {
        /// ID mówcy do aktualizacji
        speaker_id: String,

        /// Nowa nazwa mówcy
        new_name: String,
    },

    /// Wyświetla listę zarejestrowanych mówców.
    SpeakerList,

    /// Wyświetla informacje o bazie głosów (liczba mówców, embedding dim, etc.)
    SpeakerInfo,

    /// Identyfikuje mówcę na podstawie audio.
    ///
    /// Zwraca najbliższego mówcę jeśli similarity >= threshold.
    SpeakerIdentify {
        /// Audio do identyfikacji (raw bytes)
        audio_data: Vec<u8>,

        /// Próg similarity (0.0-1.0, domyślnie 0.75)
        /// Niższy = więcej fałszywych pozytywów
        /// Wyższy = więcej nierozpoznanych
        threshold: Option<f32>,
    },

    /// Weryfikuje czy audio należy do podanego mówcy.
    ///
    /// Zwraca similarity i czy przekracza próg.
    SpeakerVerify {
        /// ID mówcy do weryfikacji
        speaker_id: String,

        /// Audio do weryfikacji (raw bytes)
        audio_data: Vec<u8>,

        /// Próg similarity (0.0-1.0, domyślnie 0.75)
        threshold: Option<f32>,
    },

    // =========================================================================
    // VOICE RECOGNITION FLOW - Extended operations
    // =========================================================================

    /// Identyfikuje mówcę z poziomem pewności (confidence level).
    ///
    /// W przeciwieństwie do SpeakerIdentify, ta operacja:
    /// - Zwraca confidence_level (HIGH/MEDIUM/LOW)
    /// - Ustawia needs_confirmation gdy wynik niepewny (0.60-0.85)
    /// - Używana w interaktywnym flow rozpoznawania
    ///
    /// Flow:
    /// - similarity >= 0.85: HIGH confidence, auto-recognize
    /// - 0.60 <= similarity < 0.85: MEDIUM confidence, needs confirmation
    /// - similarity < 0.60: LOW confidence, treat as unknown
    SpeakerIdentifyWithConfidence {
        /// Audio do identyfikacji (raw bytes)
        audio_data: Vec<u8>,

        /// Próg dla HIGH confidence (domyślnie 0.85)
        high_threshold: Option<f32>,

        /// Próg dla MEDIUM confidence (domyślnie 0.60)
        medium_threshold: Option<f32>,

        /// Metadata audio (device_info, environment, etc.)
        audio_metadata: Option<Vec<(String, String)>>,
    },

    /// Potwierdza tożsamość mówcy i opcjonalnie dodaje próbkę głosu.
    ///
    /// Używane po SpeakerIdentifyWithConfidence gdy needs_confirmation=true.
    /// Jeśli add_sample=true, embedding z audio jest dodawany do bazy mówcy
    /// (continuous learning).
    SpeakerConfirmIdentity {
        /// ID mówcy do potwierdzenia
        speaker_id: String,

        /// Audio z oryginalnej identyfikacji (do dodania jako próbka)
        audio_data: Option<Vec<u8>>,

        /// Czy dodać próbkę do bazy mówcy (continuous learning)
        add_sample: bool,

        /// Metadata dla nowej próbki
        sample_metadata: Option<Vec<(String, String)>>,
    },

    /// Ręczne linkowanie głosu do osoby w Memory.
    ///
    /// Używane gdy:
    /// - Nowa osoba przedstawia się ("Jarvis, to ja Jan")
    /// - Ręczna korekta błędnej identyfikacji
    /// - Łączenie voice_id z istniejącą osobą w Memory
    SpeakerLinkToMemory {
        /// ID mówcy w bazie głosów STT
        speaker_id: String,

        /// ID węzła osoby w Memory Graph
        memory_node_id: u64,

        /// Voice ID do zapisania w Memory (może być = speaker_id)
        voice_id: String,
    },

    // =========================================================================
    // WAKE WORD DETECTION - "Jarvis" activation
    // =========================================================================

    /// Wykrywa słowo aktywacji (wake word) w audio.
    ///
    /// Domyślnie słucha na "Jarvis", ale można skonfigurować inne słowa.
    /// Używa lekkiego modelu keyword spotting zoptymalizowanego pod kątem
    /// niskiego zużycia zasobów i szybkiego czasu odpowiedzi.
    ///
    /// # Przykład:
    /// ```rust
    /// AudioOperation::WakeWordDetect {
    ///     audio_data: audio_bytes,
    ///     wake_words: None,  // domyślnie "Jarvis"
    ///     sensitivity: Some(0.5),
    ///     return_audio_after: true,  // zwróć audio po wake word
    /// }
    /// ```
    WakeWordDetect {
        /// Audio do analizy (raw bytes - WAV, PCM)
        audio_data: Vec<u8>,

        /// Lista wake words do wykrywania (domyślnie ["Jarvis"])
        /// Case-insensitive, obsługuje warianty ("Jarvis", "jarvis", "JARVIS")
        wake_words: Option<Vec<String>>,

        /// Czułość detekcji (0.0-1.0, domyślnie 0.5)
        /// - Wyższa = więcej detekcji, więcej false positives
        /// - Niższa = mniej detekcji, mniej false positives
        sensitivity: Option<f32>,

        /// Czy zwrócić audio po wake word (do dalszego STT)
        /// True = zwraca audio od momentu wake word do końca
        return_audio_after: bool,
    },

    /// Konfiguruje wake word detector dla sesji.
    ///
    /// Ustawienia persystują przez czas trwania sesji QUIC.
    /// Używane do optymalizacji continuous listening.
    WakeWordConfigure {
        /// Lista aktywnych wake words
        wake_words: Vec<String>,

        /// Czułość detekcji (0.0-1.0)
        sensitivity: f32,

        /// Minimalny czas między detekcjami (ms) - debouncing
        /// Domyślnie 500ms
        min_detection_interval_ms: Option<u32>,

        /// Czy włączyć Voice Activity Detection przed wake word
        /// True = oszczędza CPU, wykrywa tylko gdy ktoś mówi
        vad_enabled: Option<bool>,

        /// Próg VAD (0.0-1.0, domyślnie 0.3)
        vad_threshold: Option<f32>,
    },

    /// Streaming wake word detection.
    ///
    /// Przyjmuje strumień audio i zwraca zdarzenia detekcji.
    /// Optymalne dla continuous listening (np. smart speaker).
    ///
    /// Flow:
    /// 1. Client otwiera stream i wysyła audio chunks
    /// 2. Server analizuje każdy chunk
    /// 3. Gdy wykryje wake word - zwraca WakeWordDetected event
    /// 4. Opcjonalnie kontynuuje listening do następnego wake word
    WakeWordStreamStart {
        /// Konfiguracja sesji (opcjonalne - użyje domyślnych)
        wake_words: Option<Vec<String>>,
        sensitivity: Option<f32>,
        vad_enabled: Option<bool>,
    },

    /// Chunk audio dla streaming wake word detection.
    WakeWordStreamChunk {
        /// Audio chunk (raw PCM bytes)
        audio_data: Vec<u8>,

        /// Timestamp chunka (ms od początku streamu)
        timestamp_ms: u64,
    },

    /// Kończy sesję streaming wake word detection.
    WakeWordStreamStop,

    // ========================================================================
    // CONVERSATION SESSION OPERATIONS
    // ========================================================================

    /// Rozpoczyna sesję konwersacyjną.
    ///
    /// Sesja konwersacyjna to wysokopoziomowa abstrakcja nad wake word + STT.
    /// Pozwala na naturalną interakcję głosową bez powtarzania wake word.
    ///
    /// # Tryby sesji:
    /// - **AlwaysOn**: Zawsze aktywna, bez potrzeby wake word
    /// - **WakeWordTimeout**: Aktywowana wake word, kończy się po czasie ciszy
    /// - **WakeWordExplicitStop**: Aktywowana wake word, kończy się frazą stop
    ///
    /// # Flow:
    /// 1. Client wysyła ConversationStart z konfiguracją
    /// 2. Server zwraca session_id
    /// 3. Client wysyła audio przez ConversationAudio
    /// 4. Server zwraca eventy: WakeWordDetected, Transcription, SessionEnded
    /// 5. Client może zakończyć jawnie przez ConversationEnd
    ConversationStart {
        /// Konfiguracja sesji
        config: ConversationSessionConfig,
    },

    /// Wysyła audio do aktywnej sesji konwersacyjnej.
    ///
    /// Serwer automatycznie:
    /// - Wykrywa wake word (jeśli sesja nieaktywna)
    /// - Transkrybuje mowę (jeśli sesja aktywna)
    /// - Wykrywa stop phrases (w trybie WakeWordExplicitStop)
    /// - Monitoruje ciszę (w trybie WakeWordTimeout)
    ConversationAudio {
        /// ID sesji (z ConversationStartResult)
        session_id: String,

        /// Audio chunk (raw PCM 16kHz mono)
        audio_data: Vec<u8>,

        /// Timestamp chunka (ms od początku streamu)
        timestamp_ms: u64,
    },

    /// Kończy sesję konwersacyjną.
    ///
    /// Użyj gdy:
    /// - User nacisnął przycisk "Stop"
    /// - Aplikacja się zamyka
    /// - Chcesz wymusić zakończenie
    ConversationEnd {
        /// ID sesji do zakończenia
        session_id: String,

        /// Powód zakończenia (do logowania)
        reason: Option<String>,
    },

    /// Pobiera status aktywnej sesji.
    ConversationStatus {
        /// ID sesji
        session_id: String,
    },
}

// ============================================================================
// VISION PAYLOAD
// ============================================================================

/// Payload dla vision request (image understanding).
///
/// # Przykład:
/// ```rust
/// let payload = VisionPayload {
///     model: "gpt-4-vision-preview".to_string(),
///     messages: vec![VisionMessage {
///         role: "user".to_string(),
///         content: vec![
///             VisionContentPart::Text { text: "What's in this image?".to_string() },
///             VisionContentPart::ImageUrl { url: "data:image/png;base64,...".to_string(), detail: Some("high".to_string()) },
///         ],
///     }],
///     max_tokens: Some(1000),
/// };
/// ```
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
pub struct VisionPayload {
    /// Model (np. "gpt-4-vision-preview", "claude-3-5-sonnet")
    pub model: String,

    /// Messages z obrazami
    pub messages: Vec<VisionMessage>,

    /// Maksymalna liczba tokenów
    pub max_tokens: Option<u32>,

    /// Temperature
    pub temperature: Option<f32>,
}

// ============================================================================
// RAG PAYLOAD
// ============================================================================

/// Payload dla RAG request (Retrieval Augmented Generation).
///
/// To jest specjalny workflow który łączy retrieval z generation.
///
/// # Przykład:
/// ```rust
/// let payload = RAGPayload {
///     query: "What is Project X?".to_string(),
///     context: None,
///     params: RAGParams::default(),
///     requires_llm_processing: true,
///     requires_audio_output: false,
///     search_modes: vec![SearchMode::VectorSearch, SearchMode::HiRAG],
/// };
/// ```
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
pub struct RAGPayload {
    /// Zapytanie użytkownika
    pub query: String,

    /// Opcjonalny kontekst (historia konwersacji)
    pub context: Option<RAGContext>,

    /// Parametry retrieval (top_k, min_similarity, reranking)
    pub params: RAGParams,

    /// Czy wymaga przetworzenia przez LLM po retrieval
    pub requires_llm_processing: bool,

    /// Czy wygenerować audio output (TTS)
    pub requires_audio_output: bool,

    /// Tryby wyszukiwania (FTS, Vector, HiRAG, GSW)
    pub search_modes: Vec<SearchMode>,
}

// ============================================================================
// MEMORY PAYLOAD
// ============================================================================

/// Payload dla Memory request (AI Brain Memory System).
///
/// Memory system zawiera:
/// - Graf wiedzy (koncepty, relacje, atrybuty)
/// - HNSW index dla semantic search
/// - Multi-hop reasoning engine
/// - Session memory (working memory + consolidation)
///
/// # Przykład - Store:
/// ```rust
/// let payload = MemoryPayload {
///     operation: MemoryOperation::Store {
///         session_id: "session-123".to_string(),
///         facts: vec![
///             MemoryFact {
///                 subject: "dog".to_string(),
///                 relation: "is_a".to_string(),
///                 object: "animal".to_string(),
///                 confidence: 1.0,
///                 source: Some("user".to_string()),
///             },
///         ],
///     },
/// };
/// ```
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
pub struct MemoryPayload {
    /// Operacja na pamięci
    pub operation: MemoryOperation,
}

/// Operacje na pamięci AI.
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
pub enum MemoryOperation {
    /// Zapisz fakty do pamięci (working memory → session memory)
    ///
    /// Fakty są najpierw dodawane do working memory, potem konsolidowane
    /// do session memory i grafu wiedzy.
    Store {
        /// ID sesji (dla izolacji pamięci między sesjami)
        session_id: String,

        /// Fakty do zapisania
        facts: Vec<MemoryFact>,

        /// Opcjonalny kontekst (np. embedding query dla klastrowania)
        context_embedding: Option<Vec<f32>>,
    },

    /// Zapytanie do pamięci (multi-hop reasoning na grafie wiedzy)
    ///
    /// Wykonuje spreading activation + HNSW search + constraint satisfaction.
    Query {
        /// ID sesji
        session_id: String,

        /// Treść zapytania (tekstowa)
        query: String,

        /// Opcjonalny embedding zapytania (jeśli klient ma własny model)
        /// Jeśli None, Memory użyje callback do Router dla embeddings
        query_embedding: Option<Vec<f32>>,

        /// Typ zapytania (What, HowTo, Why, Similar, etc.)
        query_type: MemoryQueryType,

        /// Maksymalna głębokość przeszukiwania grafu
        max_depth: Option<u32>,

        /// Maksymalna liczba wyników
        top_k: Option<u32>,

        /// Czy dołączyć ścieżkę rozumowania w wynikach
        include_reasoning: Option<bool>,
    },

    /// Wymuś konsolidację pamięci (working → session → long-term)
    ///
    /// Normalnie konsolidacja działa automatycznie, ale można ją wymusić.
    Consolidate {
        /// ID sesji
        session_id: String,

        /// Czy konsolidować wszystkie sesje (admin operation)
        consolidate_all: bool,
    },

    /// Pobierz statystyki pamięci
    Stats {
        /// ID sesji (None = globalne statystyki)
        session_id: Option<String>,
    },

    /// Wyczyść pamięć sesji
    Clear {
        /// ID sesji do wyczyszczenia
        session_id: String,

        /// Czy zachować long-term memory (tylko wyczyść working/session)
        preserve_long_term: bool,
    },

    /// Dodaj explicit feedback do faktu/węzła
    Feedback {
        /// ID sesji
        session_id: String,

        /// ID węzła w grafie
        node_id: u64,

        /// Typ feedbacku
        feedback_type: MemoryFeedbackType,

        /// Wartość feedbacku (np. -1.0 do 1.0)
        value: f32,
    },

    /// Linkuj voice ID z węzłem osoby (dla speaker identification)
    ///
    /// Pozwala na mapowanie głosu użytkownika (STT speaker_id) z węzłem Person w grafie.
    LinkVoice {
        /// ID sesji
        session_id: String,

        /// ID węzła osoby w grafie
        node_id: u64,

        /// Voice ID z STT (speaker identification)
        voice_id: String,
    },

    /// Znajdź węzeł osoby po voice ID
    FindByVoice {
        /// ID sesji
        session_id: String,

        /// Voice ID do wyszukania
        voice_id: String,
    },

    /// Aktualizuje nazwę osoby w grafie (po korekcie imienia przez użytkownika).
    ///
    /// Używane gdy użytkownik mówi np. "jestem Piotr, nie Jan".
    /// Aktualizuje label węzła Person i opcjonalnie tworzy relację "WasPreviouslyKnownAs".
    UpdatePersonName {
        /// ID sesji
        session_id: String,

        /// Voice ID osoby (alternatywa dla node_id)
        voice_id: Option<String>,

        /// ID węzła osoby (alternatywa dla voice_id)
        node_id: Option<u64>,

        /// Nowa nazwa osoby
        new_name: String,

        /// Czy zachować historię poprzedniej nazwy (tworzy relację WasPreviouslyKnownAs)
        preserve_history: bool,
    },

}

/// Fakt do zapisania w pamięci.
///
/// Reprezentuje trójkę (subject, relation, object) z opcjonalnymi metadanymi.
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
pub struct MemoryFact {
    /// Podmiot (subject) - np. "dog", "Mars", "user_preference"
    pub subject: String,

    /// Relacja - np. "is_a", "has_property", "located_in", "prefers"
    pub relation: String,

    /// Obiekt (object) - np. "animal", "red", "space", "dark_mode"
    pub object: String,

    /// Pewność faktu (0.0-1.0)
    pub confidence: f32,

    /// Źródło faktu (opcjonalne) - np. "user", "inference", "document:doc123"
    pub source: Option<String>,

    /// Dodatkowe metadane (key-value pairs)
    pub metadata: Option<Vec<(String, String)>>,
}

/// Typ zapytania do pamięci.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub enum MemoryQueryType {
    /// "What is X?" - attribute recall
    What,

    /// "What can do X?" - find things with capability
    WhatCanDo,

    /// "What is X used for?" - find purposes
    WhatFor,

    /// "Where is X?" - find location
    Where,

    /// "How to X?" - find requirements/steps
    HowTo,

    /// "Why X?" - find causes
    Why,

    /// "Similar to X" - semantic similarity search
    Similar,

    /// Custom pattern match (advanced)
    Pattern,
}

/// Typ feedbacku dla węzła pamięci.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub enum MemoryFeedbackType {
    /// Użytkownik potwierdził że fakt jest poprawny
    Positive,

    /// Użytkownik zanegował fakt
    Negative,

    /// Użytkownik uznał fakt za istotny (boost priority)
    Important,

    /// Użytkownik uznał fakt za nieistotny (lower priority)
    Irrelevant,
}

// ============================================================================
// MODEL RESPONSE
// ============================================================================

/// Uniwersalny response envelope dla wszystkich operacji modelowych.
///
/// Używany tylko dla non-streaming responses (stream=false).
/// Dla streaming responses używamy ModelStreamChunk.
///
/// # Przykład:
/// ```rust
/// let response = ModelResponse {
///     request_id: "uuid-123".to_string(),
///     result: ModelResult::Embeddings(EmbeddingsResult {
///         embeddings: vec![vec![0.1, 0.2, 0.3]],
///         dimensions: 768,
///         model: "gemma".to_string(),
///     }),
///     metrics: Some(ModelMetrics { /* ... */ }),
/// };
/// ```
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
pub struct ModelResponse {
    /// ID requestu (correlation)
    pub request_id: String,

    /// Wynik operacji lub error
    pub result: ModelResult,

    /// Metryki wydajności (opcjonalne)
    pub metrics: Option<ModelMetrics>,
}

/// Wyniki dla różnych typów modeli.
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
pub enum ModelResult {
    /// Embeddings result
    Embeddings(EmbeddingsResult),

    /// Completion result (non-streaming)
    Completion(CompletionResult),

    /// Image result
    Image(ImageResult),

    /// Audio result
    Audio(AudioResult),

    /// Vision result
    Vision(VisionResult),

    /// RAG result
    RAG(RAGResult),

    /// Rerank result
    Rerank(RerankResult),

    /// Memory result
    Memory(MemoryResult),

    /// PrefixCacheInit result
    PrefixCacheInit(PrefixCacheInitResponse),

    /// PromptFetched - treść promptu odczytana z DB routera (wraz z
    /// rozwiązanym językiem — może się różnić od żądanego jeśli zadziałał fallback).
    PromptFetched(PromptFetchResponse),

    /// Browser - wynik `BrowserPayload` (screenshot, DOM, albo błąd).
    Browser(BrowserResult),

    /// Error
    Error(ErrorInfo),
}

/// Wynik operacji browser wykonanej przez teams-bot.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub enum BrowserResult {
    /// Surowy PNG (bajty). Rozmiar ograniczony framingiem rkyv (16 MiB).
    Screenshot { png: Vec<u8> },
    /// `outerHTML` dokumentu.
    Dom { html: String },
    /// Błąd po stronie bota — np. brak aktywnej strony albo timeout CDP.
    Error { message: String },
}

/// Odpowiedź na `PromptFetchRequest`. `resolved_language` mówi kontenerowi
/// który wariant faktycznie został zwrócony — np. gdy żądał `de`, a DB ma
/// tylko `pl`, tu będzie `pl`.
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
pub struct PromptFetchResponse {
    pub content: String,
    pub name: String,
    pub resolved_language: String,
}

/// Result dla embeddings.
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
pub struct EmbeddingsResult {
    /// Wektory embeddingów (batch x dimensions)
    pub embeddings: Vec<Vec<f32>>,

    /// Rozmiar wektora (dimensions)
    pub dimensions: usize,

    /// Nazwa użytego modelu
    pub model: String,
}

/// Result dla rerank.
///
/// Zawiera posortowaną listę dokumentów z ich relevance scores.
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
pub struct RerankResult {
    /// Lista wyników rerankingu posortowana malejąco po score
    pub results: Vec<RerankResultItem>,

    /// Nazwa użytego modelu
    pub model: String,
}

/// Pojedynczy wynik rerankingu dla dokumentu.
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
pub struct RerankResultItem {
    /// Index dokumentu w oryginalnej liście (0-indexed)
    pub index: usize,

    /// Relevance score (0.0 - 1.0, wyższy = bardziej relevantny)
    pub relevance_score: f32,

    /// Tekst dokumentu (opcjonalnie, jeśli return_documents=true)
    pub document: Option<String>,
}

/// Result dla completion (non-streaming).
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
pub struct CompletionResult {
    /// Wygenerowany tekst (content)
    pub text: String,

    /// Reasoning content (chain of thought) - dla modeli reasoning jak DeepSeek R1, OpenAI o1
    pub reasoning_content: Option<String>,

    /// Nazwa użytego modelu
    pub model: String,

    /// Finish reason ("stop", "length", "content_filter")
    pub finish_reason: Option<String>,

    /// Tool calls (function calling) - lista wywołań funkcji
    pub tool_calls: Option<Vec<ToolCallResult>>,

    // === INTENT ANALYZER FIELDS (Bielik 1.5B) ===

    /// Wykryta intencja główna (Introduction, ToolCall, Conversation, etc.)
    pub detected_intent: Option<String>,

    /// Wykryte wywołania narzędzi z wynikami wykonania
    pub detected_tools: Option<Vec<DetectedToolCall>>,

    /// Transkrybowany tekst z audio input (jeśli był audio)
    pub transcribed_text: Option<String>,

    /// ID rozpoznanego mówcy
    pub speaker_id: Option<String>,

    /// Nazwa rozpoznanego mówcy
    pub speaker_name: Option<String>,
}

/// Pojedyncze wywołanie funkcji w odpowiedzi modelu.
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
pub struct ToolCallResult {
    /// ID wywołania
    pub id: String,

    /// Typ: "function"
    pub tool_type: String,

    /// Nazwa funkcji
    pub function_name: String,

    /// Argumenty funkcji (JSON string)
    pub arguments: String,
}

// ============================================================================
// DETECTED TOOLS - Wyniki z Intent Analyzer (Bielik 1.5B)
// ============================================================================

/// Wykryte wywołanie narzędzia z Intent Analyzer.
/// Różni się od ToolCallResult tym, że zawiera wyniki wykonania i walidacji.
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
pub struct DetectedToolCall {
    /// ID wywołania (uuid)
    pub call_id: String,

    /// Nazwa narzędzia (calendar_add, email_send, web_search, etc.)
    pub tool_name: String,

    /// Parametry narzędzia (JSON string)
    pub parameters: String,

    /// Czy wywołanie było kompletne (wszystkie wymagane parametry)
    pub is_complete: bool,

    /// Brakujące parametry (jeśli niekompletne)
    pub missing_params: Option<Vec<String>>,

    /// Wynik wykonania (jeśli is_complete=true)
    pub execution_result: Option<DetectedToolExecutionResult>,

    /// Pytanie uzupełniające (jeśli brakuje parametrów)
    pub follow_up_question: Option<String>,
}

/// Wynik wykonania wykrytego narzędzia.
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
pub struct DetectedToolExecutionResult {
    /// Czy wykonanie się powiodło
    pub success: bool,

    /// Wiadomość zwrotna
    pub message: String,

    /// Dane zwrotne (JSON string, opcjonalne)
    pub data: Option<String>,

    /// Błąd (jeśli success=false)
    pub error: Option<String>,
}

/// Result dla image.
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
pub struct ImageResult {
    /// Wygenerowane obrazy (PNG/JPEG bytes)
    pub images: Vec<Vec<u8>>,

    /// Nazwa użytego modelu
    pub model: String,
}

/// Result dla audio.
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
pub struct AudioResult {
    /// Audio data (dla TTS) lub transcribed text (dla STT)
    pub data: AudioResultData,

    /// Nazwa użytego modelu
    pub model: String,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
pub enum AudioResultData {
    /// Audio bytes (MP3, WAV, etc.) - dla TTS
    Audio(Vec<u8>),

    /// Transcribed text - dla STT
    Text(String),

    /// Detailed transcription - dla STT z verbose_json
    Detailed {
        text: String,
        segments: Vec<TranscriptionSegment>,
        language: String,
        duration: f32,
        /// Liczba odfiltrowanych segmentów (halucynacji)
        filtered_segments_count: Option<u32>,
    },

    // =========================================================================
    // SPEAKER IDENTIFICATION RESULTS
    // =========================================================================

    /// Wynik rejestracji/aktualizacji mówcy (SpeakerEnroll, SpeakerAddSamples)
    SpeakerEnrollResult {
        /// ID mówcy
        speaker_id: String,
        /// Nazwa mówcy
        speaker_name: String,
        /// Liczba przetworzonych próbek audio
        samples_processed: u32,
        /// Liczba pomyślnie wyekstrahowanych embeddingów
        embeddings_added: u32,
        /// Czy to była nowa rejestracja (true) czy aktualizacja (false)
        is_new: bool,
    },

    /// Wynik usunięcia mówcy (SpeakerRemove)
    SpeakerRemoveResult {
        /// ID usuniętego mówcy
        speaker_id: String,
        /// Czy usunięcie się powiodło
        success: bool,
    },

    /// Wynik aktualizacji nazwy mówcy (SpeakerUpdateName)
    SpeakerUpdateNameResult {
        /// ID mówcy
        speaker_id: String,
        /// Stara nazwa
        old_name: String,
        /// Nowa nazwa
        new_name: String,
        /// Czy aktualizacja się powiodła
        success: bool,
    },

    /// Lista mówców (SpeakerList)
    SpeakerListResult {
        /// Lista (id, name) zarejestrowanych mówców
        speakers: Vec<(String, String)>,
        /// Łączna liczba mówców
        total_count: u32,
    },

    /// Informacje o bazie głosów (SpeakerInfo)
    SpeakerInfoResult {
        /// Liczba zarejestrowanych mówców
        speaker_count: u32,
        /// Wymiar embeddingów (192 dla ECAPA-TDNN)
        embedding_dim: u32,
        /// Próg similarity używany w bazie
        similarity_threshold: f32,
    },

    /// Wynik identyfikacji mówcy (SpeakerIdentify)
    SpeakerIdentifyResult {
        /// Czy rozpoznano mówcę (similarity >= threshold)
        is_match: bool,
        /// ID rozpoznanego mówcy (None jeśli !is_match)
        speaker_id: Option<String>,
        /// Nazwa rozpoznanego mówcy (None jeśli !is_match)
        speaker_name: Option<String>,
        /// Similarity score (0.0-1.0, cosine similarity)
        similarity: f32,
        /// Użyty próg similarity
        threshold: f32,
    },

    /// Wynik weryfikacji mówcy (SpeakerVerify)
    SpeakerVerifyResult {
        /// ID weryfikowanego mówcy
        speaker_id: String,
        /// Czy weryfikacja pozytywna (similarity >= threshold)
        is_verified: bool,
        /// Similarity score z docelowym mówcą
        similarity: f32,
        /// Użyty próg
        threshold: f32,
        /// Jeśli wykryto innego mówcę - jego ID
        detected_speaker_id: Option<String>,
    },

    // =========================================================================
    // VOICE RECOGNITION FLOW RESULTS
    // =========================================================================

    /// Wynik identyfikacji z poziomem pewności (SpeakerIdentifyWithConfidence)
    SpeakerIdentifyWithConfidenceResult {
        /// Czy rozpoznano mówcę (jakikolwiek match powyżej medium_threshold)
        is_match: bool,

        /// ID rozpoznanego mówcy (None jeśli !is_match)
        speaker_id: Option<String>,

        /// Nazwa rozpoznanego mówcy (None jeśli !is_match)
        speaker_name: Option<String>,

        /// Similarity score (0.0-1.0)
        similarity: f32,

        /// Poziom pewności: "HIGH", "MEDIUM", "LOW"
        /// HIGH: similarity >= high_threshold (np. 0.85)
        /// MEDIUM: medium_threshold <= similarity < high_threshold
        /// LOW: similarity < medium_threshold (np. 0.60)
        confidence_level: String,

        /// Czy wymaga potwierdzenia użytkownika (true gdy confidence_level == "MEDIUM")
        needs_confirmation: bool,

        /// Użyty próg high
        high_threshold: f32,

        /// Użyty próg medium
        medium_threshold: f32,

        /// Sugestia wiadomości do użytkownika (np. "Czy to ty, Jan?")
        confirmation_message: Option<String>,
    },

    /// Wynik potwierdzenia tożsamości (SpeakerConfirmIdentity)
    SpeakerConfirmIdentityResult {
        /// ID mówcy
        speaker_id: String,

        /// Czy potwierdzenie zaakceptowano
        confirmed: bool,

        /// Czy dodano próbkę głosu (continuous learning)
        sample_added: bool,

        /// Liczba próbek po dodaniu (jeśli sample_added=true)
        total_samples: Option<u32>,
    },

    /// Wynik linkowania do Memory (SpeakerLinkToMemory)
    SpeakerLinkToMemoryResult {
        /// ID mówcy w STT
        speaker_id: String,

        /// ID węzła w Memory
        memory_node_id: u64,

        /// Voice ID zapisany w Memory
        voice_id: String,

        /// Czy linkowanie się powiodło
        success: bool,
    },

    // =========================================================================
    // WAKE WORD DETECTION RESULTS
    // =========================================================================

    /// Wynik detekcji wake word (WakeWordDetect)
    WakeWordDetectResult {
        /// Czy wykryto wake word
        detected: bool,

        /// Które wake word zostało wykryte (None jeśli !detected)
        detected_word: Option<String>,

        /// Poziom pewności detekcji (0.0-1.0)
        confidence: f32,

        /// Timestamp wykrycia w audio (ms od początku)
        timestamp_ms: Option<u64>,

        /// Audio po wake word (jeśli return_audio_after=true)
        /// Może być użyte bezpośrednio do STT
        audio_after: Option<Vec<u8>>,

        /// Długość audio po wake word (ms)
        audio_after_duration_ms: Option<u64>,
    },

    /// Wynik konfiguracji wake word (WakeWordConfigure)
    WakeWordConfigureResult {
        /// Czy konfiguracja się powiodła
        success: bool,

        /// Aktywne wake words
        active_wake_words: Vec<String>,

        /// Aktualna czułość
        sensitivity: f32,

        /// Aktualny interwał detekcji (ms)
        min_detection_interval_ms: u32,

        /// Czy VAD jest włączony
        vad_enabled: bool,
    },

    /// Wynik rozpoczęcia streamu wake word (WakeWordStreamStart)
    WakeWordStreamStartResult {
        /// ID sesji streamingu
        session_id: String,

        /// Czy sesja rozpoczęta pomyślnie
        success: bool,

        /// Aktywne wake words w sesji
        active_wake_words: Vec<String>,
    },

    /// Zdarzenie detekcji w streamie (zwracane gdy wykryto wake word)
    WakeWordStreamEvent {
        /// ID sesji
        session_id: String,

        /// Które wake word wykryto
        detected_word: String,

        /// Poziom pewności (0.0-1.0)
        confidence: f32,

        /// Timestamp w streamie (ms)
        timestamp_ms: u64,

        /// Audio po wake word (do dalszego przetwarzania)
        audio_after: Option<Vec<u8>>,
    },

    /// Wynik zatrzymania streamu (WakeWordStreamStop)
    WakeWordStreamStopResult {
        /// ID sesji
        session_id: String,

        /// Liczba wykrytych wake words w sesji
        total_detections: u32,

        /// Czas trwania sesji (ms)
        session_duration_ms: u64,
    },

    // ========================================================================
    // CONVERSATION SESSION RESULTS
    // ========================================================================

    /// Wynik rozpoczęcia sesji konwersacyjnej (ConversationStart)
    ConversationStartResult {
        /// ID sesji (używaj w ConversationAudio/End)
        session_id: String,

        /// Czy sesja rozpoczęta pomyślnie
        success: bool,

        /// Początkowy stan sesji
        initial_state: SessionState,

        /// Aktywna konfiguracja
        config: ConversationSessionConfig,

        /// Komunikat (błąd lub informacja)
        message: Option<String>,
    },

    /// Event z sesji konwersacyjnej (zwracany na ConversationAudio)
    ///
    /// Może być wielokrotnie wysyłany dla jednego audio chunk:
    /// - Transcription gdy rozpoznano mowę
    /// - SessionActivated gdy wykryto wake word
    /// - SessionDeactivated gdy wykryto stop lub timeout
    ConversationEventResult {
        /// ID sesji
        session_id: String,

        /// Typ eventu
        event_data: ConversationEvent,

        /// Timestamp eventu (ms od początku sesji)
        timestamp_ms: u64,
    },

    /// Wynik przetwarzania audio w sesji konwersacyjnej
    ///
    /// Zwracany na każdy chunk audio. Zawiera:
    /// - Stan sesji (Active/Inactive)
    /// - Opcjonalną transkrypcję (gdy zebrano wystarczająco dużo audio)
    /// - Eventy (wake word, stop phrase, etc.)
    ConversationAudioResult {
        /// ID sesji
        session_id: String,

        /// Aktualny stan sesji
        state: SessionState,

        /// Transkrypcja (jeśli wykonano)
        transcription: Option<String>,

        /// Confidence transkrypcji (jeśli wykonano)
        confidence: Option<f32>,

        /// Eventy wygenerowane podczas przetwarzania
        events: Vec<ConversationEvent>,
    },

    /// Wynik zakończenia sesji (ConversationEnd)
    ConversationEndResult {
        /// ID sesji
        session_id: String,

        /// Czy zakończono pomyślnie
        success: bool,

        /// Statystyki sesji
        stats: ConversationSessionStats,
    },

    /// Wynik statusu sesji (ConversationStatus)
    ConversationStatusResult {
        /// ID sesji
        session_id: String,

        /// Czy sesja istnieje
        exists: bool,

        /// Info o sesji (jeśli istnieje)
        info: Option<ConversationSessionInfo>,
    },
}

// ============================================================================
// CONVERSATION SESSION - Tryby pracy asystenta głosowego
// ============================================================================

/// Tryb pracy asystenta głosowego.
///
/// Określa jak asystent reaguje na aktywację i deaktywację:
/// - `AlwaysOn` - zawsze słucha, bez potrzeby wake word
/// - `WakeWordTimeout` - wake word aktywuje, timeout deaktywuje
/// - `WakeWordExplicitStop` - wake word aktywuje, explicit stop deaktywuje
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub enum SessionMode {
    /// Zawsze aktywny - nie wymaga wake word.
    ///
    /// Idealne dla:
    /// - Dedykowany asystent w pokoju
    /// - Smart speaker
    /// - Aplikacje gdzie prywatność nie jest priorytetem
    AlwaysOn,

    /// Aktywacja przez wake word, deaktywacja przez timeout.
    ///
    /// Po wykryciu wake word sesja jest aktywna przez określony czas.
    /// Jeśli przez `silence_timeout_ms` nie ma tekstu (tylko szumy/cisza),
    /// sesja się kończy.
    ///
    /// Idealne dla:
    /// - Prywatność (nie nagrywa gdy nie aktywowany)
    /// - Oszczędność zasobów (nie przetwarza ciągle)
    WakeWordTimeout {
        /// Timeout ciszy w ms (domyślnie 30000 = 30s)
        silence_timeout_ms: u32,
    },

    /// Aktywacja przez wake word, deaktywacja przez explicit stop phrase.
    ///
    /// Sesja trwa do momentu wypowiedzenia frazy kończącej
    /// (np. "dzięki Jarvis, to koniec").
    ///
    /// Idealne dla:
    /// - Długie rozmowy
    /// - Sesje pracy z asystentem
    /// - Gdy timeout byłby irytujący
    WakeWordExplicitStop,
}

impl Default for SessionMode {
    fn default() -> Self {
        SessionMode::WakeWordTimeout {
            silence_timeout_ms: 30_000, // 30 sekund
        }
    }
}

/// Stan sesji rozmowy z asystentem.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq, Default)]
pub enum SessionState {
    /// Nieaktywny - czeka na wake word (lub zawsze aktywny w AlwaysOn)
    #[default]
    Inactive,

    /// Aktywny - słucha i przetwarza komendy
    Active,

    /// Przetwarzanie - asystent generuje odpowiedź
    Processing,

    /// Mówi - asystent odtwarza odpowiedź TTS
    Speaking,
}

/// Konfiguracja sesji rozmowy.
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
pub struct ConversationSessionConfig {
    /// Tryb pracy asystenta
    pub mode: SessionMode,

    /// Wake words (dla trybów WakeWord*)
    pub wake_words: Vec<String>,

    /// Stop phrases (dla trybu WakeWordExplicitStop)
    /// Np. ["dzięki jarvis to koniec", "ok jarvis wystarczy"]
    pub stop_phrases: Vec<String>,

    /// Czułość detekcji wake word (0.0-1.0)
    pub wake_word_sensitivity: f32,

    /// Czy używać VAD do wykrywania mowy
    pub vad_enabled: bool,

    /// Próg VAD (0.0-1.0)
    pub vad_threshold: f32,

    /// Czy odtwarzać dźwięk potwierdzenia aktywacji
    pub play_activation_sound: bool,

    /// Czy odtwarzać dźwięk deaktywacji
    pub play_deactivation_sound: bool,
}

impl Default for ConversationSessionConfig {
    fn default() -> Self {
        Self {
            mode: SessionMode::default(),
            wake_words: vec![
                "jarvis".to_string(),
                "hej jarvis".to_string(),
                "cześć jarvis".to_string(),
                "ok jarvis".to_string(),
            ],
            stop_phrases: vec![
                "dzięki jarvis to koniec".to_string(),
                "ok jarvis wystarczy".to_string(),
                "jarvis koniec".to_string(),
                "to wszystko jarvis".to_string(),
                "jarvis dziękuję".to_string(),
                "dziękuję jarvis".to_string(),
            ],
            wake_word_sensitivity: 0.5,
            vad_enabled: true,
            vad_threshold: 0.3,
            play_activation_sound: true,
            play_deactivation_sound: true,
        }
    }
}

/// Informacje o aktywnej sesji rozmowy.
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
pub struct ConversationSessionInfo {
    /// ID sesji
    pub session_id: String,

    /// Aktualny stan
    pub state: SessionState,

    /// Tryb sesji
    pub mode: SessionMode,

    /// Timestamp rozpoczęcia (Unix ms)
    pub started_at_ms: u64,

    /// Timestamp ostatniej aktywności (Unix ms)
    pub last_activity_ms: u64,

    /// Liczba przetworzonych wypowiedzi w sesji
    pub utterance_count: u32,

    /// ID rozpoznanego mówcy (jeśli zidentyfikowany)
    pub speaker_id: Option<String>,

    /// Imię rozpoznanego mówcy
    pub speaker_name: Option<String>,
}

/// Statystyki zakończonej sesji rozmowy.
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
pub struct ConversationSessionStats {
    /// Całkowity czas trwania sesji (ms)
    pub total_duration_ms: u64,

    /// Czas aktywnej rozmowy (ms) - bez ciszy
    pub active_duration_ms: u64,

    /// Liczba wypowiedzi użytkownika
    pub utterance_count: u32,

    /// Łączna liczba słów
    pub total_words: u32,

    /// Liczba wykrytych wake words
    pub wake_word_detections: u32,

    /// Liczba wykrytych stop phrases
    pub stop_phrase_detections: u32,

    /// Średni confidence transkrypcji
    pub avg_transcription_confidence: f32,
}

impl Default for ConversationSessionStats {
    fn default() -> Self {
        Self {
            total_duration_ms: 0,
            active_duration_ms: 0,
            utterance_count: 0,
            total_words: 0,
            wake_word_detections: 0,
            stop_phrase_detections: 0,
            avg_transcription_confidence: 0.0,
        }
    }
}

/// Zdarzenia sesji rozmowy (dla powiadomień client).
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
pub enum ConversationEvent {
    /// Sesja została aktywowana (wake word detected lub AlwaysOn start)
    SessionActivated {
        session_id: String,
        mode: SessionMode,
        /// Jak została aktywowana
        activation_reason: ActivationReason,
    },

    /// Sesja została deaktywowana
    SessionDeactivated {
        session_id: String,
        /// Powód deaktywacji
        deactivation_reason: DeactivationReason,
        /// Czas trwania sesji (ms)
        duration_ms: u64,
        /// Liczba wypowiedzi w sesji
        utterance_count: u32,
    },

    /// Wykryto mowę (początek wypowiedzi)
    SpeechStarted {
        session_id: String,
        timestamp_ms: u64,
    },

    /// Koniec mowy (koniec wypowiedzi)
    SpeechEnded {
        session_id: String,
        timestamp_ms: u64,
        /// Czas trwania wypowiedzi (ms)
        duration_ms: u64,
    },

    /// Transkrypcja wypowiedzi
    UtteranceTranscribed {
        session_id: String,
        text: String,
        /// Czy wykryto stop phrase
        is_stop_phrase: bool,
        /// Confidence transkrypcji
        confidence: f32,
    },

    /// Timeout - zbliża się koniec sesji (ostrzeżenie)
    TimeoutWarning {
        session_id: String,
        /// Ile ms do timeout
        remaining_ms: u32,
    },

    /// Błąd w sesji
    SessionError {
        session_id: String,
        error_message: String,
    },
}

/// Powód aktywacji sesji.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq)]
pub enum ActivationReason {
    /// Wake word wykryty
    WakeWord {
        detected_phrase: String,
        confidence: f32,
    },
    /// Tryb AlwaysOn - automatyczna aktywacja
    AlwaysOn,
    /// Manualna aktywacja (np. przycisk)
    Manual,
}

/// Powód deaktywacji sesji.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub enum DeactivationReason {
    /// Timeout ciszy
    SilenceTimeout {
        silence_duration_ms: u32,
    },
    /// Explicit stop phrase
    StopPhrase {
        detected_phrase: String,
    },
    /// Manualna deaktywacja
    Manual,
    /// Błąd
    Error {
        message: String,
    },
    /// Rozłączenie klienta
    ClientDisconnected,
}

/// Segment transkrypcji z Whisper (verbose_json format).
///
/// Zawiera wszystkie pola z OpenAI Whisper API:
/// - Podstawowe: id, start, end, text
/// - Quality metrics: no_speech_prob, avg_logprob, compression_ratio
/// - Extra: seek, tokens, temperature
///
/// Quality metrics pozwalają filtrować halucynacje:
/// - no_speech_prob > 0.6: Prawdopodobnie halucynacja (brak mowy)
/// - avg_logprob < -1.0: Niska pewność modelu
/// - compression_ratio > 2.4: Nietypowy wzorzec (powtórzenia)
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
pub struct TranscriptionSegment {
    /// ID segmentu (0-indexed)
    pub id: u32,

    /// Pozycja seek w pliku audio
    pub seek: u32,

    /// Czas rozpoczęcia segmentu w sekundach
    pub start: f32,

    /// Czas zakończenia segmentu w sekundach
    pub end: f32,

    /// Transkrybowany tekst
    pub text: String,

    /// Lista tokenów (opcjonalna - może być duża)
    pub tokens: Option<Vec<u32>>,

    /// Temperature użyta do dekodowania tego segmentu
    pub temperature: f32,

    /// Średni log probability dla tokenów w segmencie
    /// Niższe wartości = mniejsza pewność
    /// Typowo: > -0.5 to dobra jakość, < -1.0 to potencjalnie halucynacja
    pub avg_logprob: f32,

    /// Compression ratio dla segmentu
    /// Wysokie wartości (> 2.4) mogą wskazywać na powtórzenia/halucynacje
    pub compression_ratio: f32,

    /// Prawdopodobieństwo braku mowy w tym segmencie (0.0 - 1.0)
    /// Wysokie wartości (> 0.6) sugerują halucynację - brak mowy w audio
    pub no_speech_prob: f32,

    /// Etykieta mówcy z diarization (np. "SPEAKER_00", "Jan Kowalski")
    /// None jeśli diarization wyłączona lub nie wykryto mówcy
    pub speaker_label: Option<String>,

    /// Similarity score z bazy mówców (0.0-1.0, cosine similarity)
    /// None jeśli diarization wyłączona lub brak bazy mówców
    pub speaker_similarity: Option<f32>,

    /// Czy mówca został rozpoznany z bazy (true) czy to anonimowy speaker (false)
    /// None jeśli diarization wyłączona
    pub is_known_speaker: Option<bool>,
}

/// Result dla vision.
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
pub struct VisionResult {
    /// Response text (image understanding)
    pub text: String,

    /// Nazwa użytego modelu
    pub model: String,
}

/// Result dla RAG.
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
pub struct RAGResult {
    /// Kontekst tekstowy lub finalna odpowiedź
    pub context_text: String,

    /// Metadata o znalezionych chunkach
    pub metadata: Vec<RAGChunkMetadata>,

    /// Czy wymaga dalszego przetworzenia przez LLM (pass-through z RAGPayload)
    pub requires_llm_processing: bool,

    /// Czy wygenerować audio output (TTS) (pass-through z RAGPayload)
    pub requires_audio_output: bool,

    /// Nazwa modelu LLM do użycia (jeśli requires_llm_processing=true)
    pub llm_model: Option<String>,
}

// ============================================================================
// MEMORY RESULT
// ============================================================================

/// Result dla Memory operations.
///
/// Zawiera wyniki różnych operacji pamięci (Store, Query, Stats, etc.).
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
pub struct MemoryResult {
    /// Typ wyniku operacji
    pub result_type: MemoryResultType,
}

/// Typ wyniku operacji pamięci.
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
pub enum MemoryResultType {
    /// Wynik operacji Store
    Store(MemoryStoreResult),

    /// Wynik operacji Query
    Query(MemoryQueryResult),

    /// Wynik operacji Consolidate
    Consolidate(MemoryConsolidateResult),

    /// Wynik operacji Stats
    Stats(MemoryStatsResult),

    /// Wynik operacji Clear
    Clear(MemoryClearResult),

    /// Wynik operacji Feedback
    Feedback(MemoryFeedbackResult),

    /// Wynik operacji LinkVoice
    LinkVoice(MemoryLinkVoiceResult),

    /// Wynik operacji FindByVoice
    FindByVoice(MemoryFindByVoiceResult),

    /// Wynik operacji UpdatePersonName
    UpdatePersonName(MemoryUpdatePersonNameResult),
}

/// Wynik operacji Store (zapisanie faktów).
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
pub struct MemoryStoreResult {
    /// Liczba zapisanych faktów
    pub facts_stored: u32,

    /// Liczba utworzonych węzłów w grafie
    pub nodes_created: u32,

    /// Liczba utworzonych krawędzi w grafie
    pub edges_created: u32,

    /// ID sesji
    pub session_id: String,
}

/// Wynik operacji Query (zapytanie do pamięci).
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
pub struct MemoryQueryResult {
    /// Lista odpowiedzi (node_id, score, label)
    pub answers: Vec<MemoryAnswer>,

    /// Ścieżki rozumowania (jeśli include_reasoning=true)
    pub reasoning_paths: Option<Vec<MemoryReasoningPath>>,

    /// Statystyki zapytania
    pub query_stats: MemoryQueryStats,
}

/// Pojedyncza odpowiedź z zapytania do pamięci.
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
pub struct MemoryAnswer {
    /// ID węzła w grafie
    pub node_id: u64,

    /// Score relevance (0.0-1.0)
    pub score: f32,

    /// Etykieta węzła (display name)
    pub label: String,

    /// Typ węzła (concept, action, attribute, etc.)
    pub node_type: String,

    /// Opcjonalne dodatkowe atrybuty
    pub attributes: Option<Vec<(String, String)>>,
}

/// Ścieżka rozumowania (jak dotarliśmy do odpowiedzi).
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
pub struct MemoryReasoningPath {
    /// Lista kroków w ścieżce
    pub steps: Vec<MemoryReasoningStep>,

    /// Łączna pewność ścieżki
    pub total_confidence: f32,
}

/// Pojedynczy krok w ścieżce rozumowania.
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
pub struct MemoryReasoningStep {
    /// ID węzła źródłowego
    pub from_node_id: u64,

    /// Etykieta węzła źródłowego
    pub from_label: String,

    /// ID węzła docelowego
    pub to_node_id: u64,

    /// Etykieta węzła docelowego
    pub to_label: String,

    /// Typ relacji
    pub relation: String,

    /// Pewność tego kroku
    pub confidence: f32,
}

/// Statystyki zapytania do pamięci.
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
pub struct MemoryQueryStats {
    /// Liczba odwiedzonych węzłów
    pub nodes_visited: u32,

    /// Liczba przejrzanych krawędzi
    pub edges_traversed: u32,

    /// Maksymalna osiągnięta głębokość
    pub max_depth_reached: u32,

    /// Czas wykonania w mikrosekundach
    pub execution_time_us: u64,

    /// Czy zapytanie zostało przycięte (za dużo wyników)
    pub was_truncated: bool,
}

/// Wynik operacji Consolidate.
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
pub struct MemoryConsolidateResult {
    /// Liczba skonsolidowanych faktów
    pub facts_consolidated: u32,

    /// Liczba usuniętych duplikatów
    pub duplicates_removed: u32,

    /// Liczba wzmocnionych faktów (przez powtórzenie)
    pub facts_reinforced: u32,

    /// ID sesji
    pub session_id: String,
}

/// Wynik operacji Stats.
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
pub struct MemoryStatsResult {
    /// Liczba węzłów w grafie
    pub total_nodes: u64,

    /// Liczba krawędzi w grafie
    pub total_edges: u64,

    /// Liczba aktywnych sesji
    pub active_sessions: u32,

    /// Rozmiar working memory (bajtów)
    pub working_memory_bytes: u64,

    /// Rozmiar session memory (bajtów)
    pub session_memory_bytes: u64,

    /// Rozmiar HNSW index (bajtów)
    pub hnsw_index_bytes: u64,

    /// Wymiar embeddingów
    pub embedding_dimensions: u32,

    /// Liczba wektorów w HNSW
    pub hnsw_vectors_count: u64,
}

/// Wynik operacji Clear.
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
pub struct MemoryClearResult {
    /// Czy operacja się powiodła
    pub success: bool,

    /// Liczba usuniętych faktów
    pub facts_cleared: u32,

    /// ID sesji
    pub session_id: String,
}

/// Wynik operacji Feedback.
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
pub struct MemoryFeedbackResult {
    /// Czy feedback został zastosowany
    pub success: bool,

    /// ID węzła
    pub node_id: u64,

    /// Nowy score węzła (po feedback)
    pub new_score: f32,
}

/// Wynik operacji LinkVoice (linkowanie głosu z osobą).
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
pub struct MemoryLinkVoiceResult {
    /// Czy linkowanie się powiodło
    pub success: bool,

    /// ID węzła osoby
    pub node_id: u64,

    /// Voice ID które zostało przypisane
    pub voice_id: String,
}

/// Wynik operacji FindByVoice (wyszukiwanie osoby po głosie).
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
pub struct MemoryFindByVoiceResult {
    /// Czy znaleziono osobę
    pub found: bool,

    /// ID węzła osoby (jeśli znaleziono)
    pub node_id: Option<u64>,

    /// Nazwa osoby (jeśli znaleziono)
    pub person_name: Option<String>,

    /// Typ węzła
    pub node_type: Option<String>,
}

/// Wynik operacji UpdatePersonName (aktualizacja nazwy osoby).
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
pub struct MemoryUpdatePersonNameResult {
    /// Czy aktualizacja się powiodła
    pub success: bool,

    /// ID węzła osoby
    pub node_id: u64,

    /// Poprzednia nazwa
    pub old_name: String,

    /// Nowa nazwa
    pub new_name: String,

    /// Voice ID (jeśli aktualizowano po voice_id)
    pub voice_id: Option<String>,
}

// ============================================================================
// MODEL STREAM CHUNK
// ============================================================================

/// Uniwersalny chunk dla streamingu.
///
/// Używany dla wszystkich typów streaming responses.
///
/// # Kolejność chunków:
/// 1. Metadata (pierwszy chunk, opcjonalny)
/// 2. Content chunks (TextDelta, AudioChunk, ImageChunk)
/// 3. Done (ostatni chunk, zawsze)
///
/// # Przykład użycia:
/// ```rust
/// // 1. Metadata (opcjonalne)
/// send_chunk(ModelStreamChunk {
///     request_id: "uuid-123".to_string(),
///     chunk: StreamChunkType::Metadata(ModelMetadata { /* ... */ }),
/// });
///
/// // 2. Content chunks
/// for token in llm_stream {
///     send_chunk(ModelStreamChunk {
///         request_id: "uuid-123".to_string(),
///         chunk: StreamChunkType::TextDelta(token),
///     });
/// }
///
/// // 3. Done
/// send_chunk(ModelStreamChunk {
///     request_id: "uuid-123".to_string(),
///     chunk: StreamChunkType::Done { final_metrics: Some(metrics) },
/// });
/// ```
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
pub struct ModelStreamChunk {
    /// ID requestu (correlation)
    pub request_id: String,

    /// Typ chunka
    pub chunk: StreamChunkType,
}

/// Typy chunków dla streamingu.
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
pub enum StreamChunkType {
    /// Metadata (wysyłane jako pierwsze, opcjonalne)
    /// Np. dla RAG: lista źródeł dokumentów
    /// Np. dla completion: info o modelu, parametrach
    Metadata(ModelMetadata),

    /// Text delta (dla completion, vision, RAG)
    /// Kolejne fragmenty generowanego tekstu (content)
    TextDelta(String),

    /// Reasoning delta (dla modeli reasoning jak DeepSeek R1, OpenAI o1)
    /// Kolejne fragmenty chain-of-thought (reasoning_content)
    ReasoningDelta(String),

    /// Tool call delta (dla function calling)
    /// Kolejne fragmenty wywołania funkcji
    ToolCallDelta(ToolCallDeltaChunk),

    /// Audio chunk (dla TTS w streaming mode)
    /// Kolejne fragmenty audio
    AudioChunk(Vec<u8>),

    /// Image chunk (dla image generation w streaming mode)
    /// Progresywne JPEG lub fragmenty obrazu
    ImageChunk(Vec<u8>),

    /// Intent Analyzer info (wysyłane przed tekstem, opcjonalne)
    /// Zawiera wyniki analizy intencji i wykryte narzędzia
    IntentInfo(IntentAnalyzerInfo),

    /// Koniec streamingu (wysyłane jako ostatnie, zawsze)
    Done {
        /// Finalne metryki (opcjonalne)
        final_metrics: Option<ModelMetrics>,
    },

    /// Error podczas streamingu
    Error(ErrorInfo),
}

/// Informacje z Intent Analyzer (Bielik 1.5B) dla streamingu.
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
pub struct IntentAnalyzerInfo {
    /// Wykryta intencja główna
    pub detected_intent: Option<String>,

    /// Wykryte wywołania narzędzi z wynikami
    pub detected_tools: Option<Vec<DetectedToolCall>>,

    /// Transkrybowany tekst z audio input
    pub transcribed_text: Option<String>,

    /// ID rozpoznanego mówcy
    pub speaker_id: Option<String>,

    /// Nazwa rozpoznanego mówcy
    pub speaker_name: Option<String>,
}

/// Chunk dla streaming tool calls.
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
pub struct ToolCallDeltaChunk {
    /// Index tool call w liście
    pub index: u32,

    /// ID wywołania (tylko w pierwszym chunk dla danego index)
    pub id: Option<String>,

    /// Nazwa funkcji (tylko w pierwszym chunk dla danego index)
    pub function_name: Option<String>,

    /// Fragment argumentów (przyrostowe)
    pub arguments_delta: Option<String>,
}

/// Metadata wysyłane na początku streamingu.
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
pub struct ModelMetadata {
    /// Typ modelu/operacji
    pub model_type: String,

    /// Nazwa użytego modelu
    pub model_name: String,

    /// Dodatkowe metadata (key-value pairs)
    /// Np. dla RAG: lista źródeł
    /// Np. dla completion: parametry (temperature, max_tokens)
    pub details: Vec<(String, String)>,
}

// ============================================================================
// METRICS & ERROR
// ============================================================================

/// Wspólne metryki dla wszystkich typów modeli.
///
/// Zawiera podstawowe metryki (latency, throughput) oraz opcjonalne
/// szczegółowe metryki specyficzne dla typu modelu.
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
pub struct ModelMetrics {
    /// Nazwa modelu
    pub model_name: String,

    /// Całkowita latencja w ms (od przyjęcia request do końca response)
    pub latency_ms: u64,

    /// Time To First Token w ms (od przyjęcia request do pierwszego tokena)
    /// Dostępne tylko dla streaming responses
    pub time_to_first_token_ms: Option<u64>,

    /// Liczba przetworzonych tokenów (jeśli applicable)
    pub tokens_processed: Option<usize>,

    /// Przepustowość tokenów/s (jeśli applicable)
    pub throughput_tokens_per_sec: Option<f32>,

    /// Szczegółowe metryki specyficzne dla typu modelu
    pub detailed: Option<DetailedMetrics>,
}

/// Szczegółowe metryki specyficzne dla typu modelu.
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
pub enum DetailedMetrics {
    /// Metryki dla embeddings
    Embeddings {
        tokenization_ms: u64,
        inference_ms: u64,
    },

    /// Metryki dla completion
    Completion {
        prompt_tokens: u32,
        completion_tokens: u32,
        total_tokens: u32,
    },

    /// Metryki dla audio
    Audio {
        audio_duration_sec: Option<f32>,
    },

    /// Metryki dla RAG
    RAG {
        retrieval_ms: u64,
        reranking_ms: Option<u64>,
        chunks_found: u32,
    },
}

/// Error information.
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
pub struct ErrorInfo {
    /// Typ błędu
    pub error_type: ErrorType,

    /// Wiadomość błędu (user-friendly)
    pub message: String,

    /// Szczegóły techniczne (opcjonalne)
    pub details: Option<String>,
}

/// Typy błędów.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq)]
pub enum ErrorType {
    /// Invalid request (nieprawidłowe parametry)
    InvalidRequest,

    /// Model not found
    ModelNotFound,

    /// Rate limit exceeded
    RateLimitExceeded,

    /// Internal server error
    InternalError,

    /// Timeout
    Timeout,

    /// Authentication error
    Unauthorized,

    /// Content filter triggered
    ContentFiltered,
}


// ============================================================================
// PREFIX CACHE - KV Cache dla promptów systemowych
// ============================================================================

/// Kategoria modelu dla prefix cache
#[derive(Archive, Deserialize, Serialize, Debug, Clone, Copy, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub enum PrefixCacheModelCategory {
    /// Główny LLM (bielik-11b) - odpowiedzi użytkownikowi
    MainLlm,
    /// Analyzer LLM (bielik-1.5b) - analiza dla Memory, tools
    AnalyzerLlm,
}

/// Typ prompta w prefix cache
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub enum PrefixCachePromptType {
    /// System prompt - pełny, stały
    System,
    /// Suffix - doklejany do system message
    Suffix,
    /// Template - wymaga formatowania z parametrami
    Template,
}

/// Pojedynczy prompt do zacheowania
#[derive(Archive, Deserialize, Serialize, Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct PrefixCacheEntry {
    /// Unikalny ID prompta (np. "jarvis_system", "query_analysis_system")
    pub id: String,
    /// Kategoria modelu
    pub category: PrefixCacheModelCategory,
    /// Typ prompta
    pub prompt_type: PrefixCachePromptType,
    /// Treść prompta
    pub content: String,
    /// Priorytet cachowania (wyższy = ważniejszy, 0-100)
    pub cache_priority: u8,
}

/// Request do zainicjalizowania prefix cache na silniku LLM
///
/// Wysyłany przy połączeniu Router → LLM Engine.
/// Silnik LLM cachuje KV (Key-Value) dla podanych promptów,
/// eliminując potrzebę przeliczania attention dla identycznych prefixów.
///
/// # Przykład:
/// ```rust
/// let request = PrefixCacheInitRequest {
///     request_id: uuid::Uuid::new_v4().to_string(),
///     model_name: "bielik-11b".to_string(),
///     category: PrefixCacheModelCategory::MainLlm,
///     prompts: vec![
///         PrefixCacheEntry {
///             id: "jarvis_system".to_string(),
///             category: PrefixCacheModelCategory::MainLlm,
///             prompt_type: PrefixCachePromptType::System,
///             content: "Jesteś Jarvis...".to_string(),
///             cache_priority: 100,
///         },
///     ],
/// };
/// ```
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
pub struct PrefixCacheInitRequest {
    /// ID requestu
    pub request_id: String,
    /// Nazwa modelu (np. "bielik-11b", "bielik-1.5b")
    pub model_name: String,
    /// Kategoria modelu
    pub category: PrefixCacheModelCategory,
    /// Lista promptów do zacheowania
    pub prompts: Vec<PrefixCacheEntry>,
}

/// Response po zainicjalizowaniu prefix cache
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
pub struct PrefixCacheInitResponse {
    /// ID requestu (z PrefixCacheInitRequest)
    pub request_id: String,
    /// Czy sukces
    pub success: bool,
    /// Liczba zacheowanych promptów
    pub cached_count: u32,
    /// Błędy dla poszczególnych promptów (jeśli były)
    pub errors: Vec<(String, String)>, // (prompt_id, error_message)
    /// Info o wykorzystanej pamięci cache (MB)
    pub cache_memory_mb: Option<f32>,
}

/// Request do użycia zacheowanego prompta
///
/// Zamiast wysyłać pełny tekst prompta, wysyłamy tylko ID.
/// Silnik LLM używa zacheowanego KV dla tego prompta.
///
/// Używane w CompletionPayload.prefix_cache_id
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
pub struct PrefixCacheUseRequest {
    /// ID zacheowanego prompta (z PrefixCacheEntry.id)
    pub prompt_id: String,
    /// Dodatkowy tekst do dołączenia po zacheowanym prompcie (opcjonalny)
    /// Np. kontekst z Memory, personalizacja
    pub suffix: Option<String>,
}

// ============================================================================
// SHARED TYPES - Typy wspoldzielone
// ============================================================================

/// Informacje o karcie graficznej
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
pub struct GpuDeviceInfo {
    /// Indeks GPU w systemie
    pub index: u32,
    /// Nazwa karty graficznej
    pub name: String,
    /// Całkowita pamięć VRAM w MB
    pub vram_total_mb: u64,
    /// Wersja sterownika
    pub driver_version: String,
}

/// Dane uwierzytelniania do rejestru Docker
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
pub struct RegistryAuth {
    pub server: String,
    pub username: String,
    pub password: String,
}

#[cfg(test)]
mod ingest_tests {
    use super::*;

    #[test]
    fn test_ingest_serialization() {
        let request = IngestRequest {
            request_id: "test-uuid".to_string(),
            document_id: "doc-123".to_string(),
            content: DocumentContent::Text("Hello world".to_string()),
            metadata: vec![],
            index_flags: vec![],
        };

        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&request).unwrap();
        println!("Serialized {} bytes", bytes.len());
        println!("First 50 bytes: {:02X?}", &bytes[..std::cmp::min(50, bytes.len())]);

        // Check if it looks like ASCII text (which would be wrong for rkyv)
        let ascii_count = bytes.iter()
            .take(50)
            .filter(|&&b| b >= 0x20 && b < 0x7f)
            .count();
        println!("ASCII printable chars in first 50 bytes: {}/50", ascii_count);

        // If more than 90% is printable ASCII, something is wrong
        assert!(ascii_count < 45, "Data looks like ASCII text, not rkyv binary!");
    }

    #[test]
    fn test_ingest_roundtrip() {
        // Create IngestRequest similar to what Client sends
        let request = IngestRequest {
            request_id: "f5e832ea-81a3-4b84-a944-e70a2359f5e8".to_string(),
            document_id: "test-doc-12345678901234567890123456789012".to_string(),
            content: DocumentContent::Text("TentaFlow.AI to zaawansowana platforma sztucznej inteligencji.".to_string()),
            metadata: vec![
                ("source".to_string(), "test".to_string()),
                ("type".to_string(), "description".to_string()),
            ],
            index_flags: vec![],
        };

        // Serialize
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&request).unwrap();
        println!("Roundtrip test: serialized {} bytes", bytes.len());
        println!("First 40 bytes: {:02X?}", &bytes[..std::cmp::min(40, bytes.len())]);

        // Deserialize using access (same as Router does)
        let archived = rkyv::access::<ArchivedIngestRequest, rkyv::rancor::Error>(&bytes)
            .expect("Failed to access ArchivedIngestRequest");

        // Verify fields
        assert_eq!(archived.request_id.as_str(), "f5e832ea-81a3-4b84-a944-e70a2359f5e8");
        assert_eq!(archived.document_id.as_str(), "test-doc-12345678901234567890123456789012");

        // Full deserialize
        let deserialized: IngestRequest = rkyv::deserialize::<IngestRequest, rkyv::rancor::Error>(archived)
            .expect("Failed to deserialize IngestRequest");

        assert_eq!(deserialized.request_id, request.request_id);
        assert_eq!(deserialized.document_id, request.document_id);
        assert_eq!(deserialized.metadata.len(), 2);

        println!("Roundtrip test: SUCCESS!");
    }
}

#[cfg(test)]
mod meeting_event_tests {
    use super::*;

    // Roundtrip SummaryUpdate przez rkyv — wariant ModelPayload::MeetingEvent
    // musi encodować i decodować się zgodnie z archetypowym wzorcem innych variantów.
    #[test]
    fn rkyv_roundtrip_meeting_event_summary_update() {
        let request = ModelRequest {
            request_id: "req-1".to_string(),
            payload: ModelPayload::MeetingEvent(MeetingEventData {
                meeting_key: "mkey-abc".to_string(),
                timestamp_ms: 1_700_000_000_000,
                payload: MeetingEventPayload::SummaryUpdate {
                    decisions_text: "Decyzja X".to_string(),
                    summary_text: "Podsumowanie Y".to_string(),
                    model: "qwen".to_string(),
                },
            }),
            stream: false,
            metadata: None,
            session_id: None,
        };

        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&request).expect("encode");
        let decoded: ModelRequest =
            rkyv::from_bytes::<ModelRequest, rkyv::rancor::Error>(&bytes).expect("decode");

        match decoded.payload {
            ModelPayload::MeetingEvent(ev) => {
                assert_eq!(ev.meeting_key, "mkey-abc");
                assert_eq!(ev.timestamp_ms, 1_700_000_000_000);
                match ev.payload {
                    MeetingEventPayload::SummaryUpdate {
                        decisions_text,
                        summary_text,
                        model,
                    } => {
                        assert_eq!(decisions_text, "Decyzja X");
                        assert_eq!(summary_text, "Podsumowanie Y");
                        assert_eq!(model, "qwen");
                    }
                    _ => panic!("expected SummaryUpdate"),
                }
            }
            _ => panic!("expected MeetingEvent variant"),
        }
    }

    // Roundtrip ActionItemsUpdate z listą >1 item, żeby sprawdzić Vec w rkyv.
    #[test]
    fn rkyv_roundtrip_meeting_event_action_items_update() {
        let request = ModelRequest {
            request_id: "req-2".to_string(),
            payload: ModelPayload::MeetingEvent(MeetingEventData {
                meeting_key: "mkey-xyz".to_string(),
                timestamp_ms: 1_700_000_001_000,
                payload: MeetingEventPayload::ActionItemsUpdate {
                    items: vec![
                        MeetingActionItemData {
                            owner: "Alice".to_string(),
                            task: "prepare report".to_string(),
                            deadline: Some("2026-05-01".to_string()),
                        },
                        MeetingActionItemData {
                            owner: "Bob".to_string(),
                            task: "ship PR".to_string(),
                            deadline: None,
                        },
                    ],
                },
            }),
            stream: false,
            metadata: None,
            session_id: None,
        };

        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&request).expect("encode");
        let decoded: ModelRequest =
            rkyv::from_bytes::<ModelRequest, rkyv::rancor::Error>(&bytes).expect("decode");

        match decoded.payload {
            ModelPayload::MeetingEvent(ev) => {
                assert_eq!(ev.meeting_key, "mkey-xyz");
                match ev.payload {
                    MeetingEventPayload::ActionItemsUpdate { items } => {
                        assert_eq!(items.len(), 2);
                        assert_eq!(items[0].owner, "Alice");
                        assert_eq!(items[0].task, "prepare report");
                        assert_eq!(items[0].deadline.as_deref(), Some("2026-05-01"));
                        assert_eq!(items[1].owner, "Bob");
                        assert_eq!(items[1].deadline, None);
                    }
                    _ => panic!("expected ActionItemsUpdate"),
                }
            }
            _ => panic!("expected MeetingEvent variant"),
        }
    }

    // Roundtrip TranscriptEntry — sprawdza że wszystkie pola (Option<String>,
    // Option<f32>, u64) enkodują się i dekodują stabilnie przez rkyv.
    #[test]
    fn rkyv_roundtrip_meeting_event_transcript_entry() {
        let request = ModelRequest {
            request_id: "req-te-1".to_string(),
            payload: ModelPayload::MeetingEvent(MeetingEventData {
                meeting_key: "mkey-te".to_string(),
                timestamp_ms: 1_700_000_002_000,
                payload: MeetingEventPayload::TranscriptEntry {
                    speaker_id: "SPEAKER_00".to_string(),
                    speaker_name: Some("Alice".to_string()),
                    is_enrolled: true,
                    speaker_confidence: Some(0.87),
                    text: "Zaczynamy spotkanie".to_string(),
                    language: Some("pl".to_string()),
                    resolved_stt_model: "whisper-large-v3".to_string(),
                    latency_ms: 412,
                },
            }),
            stream: false,
            metadata: None,
            session_id: None,
        };

        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&request).expect("encode");
        let decoded: ModelRequest =
            rkyv::from_bytes::<ModelRequest, rkyv::rancor::Error>(&bytes).expect("decode");

        match decoded.payload {
            ModelPayload::MeetingEvent(ev) => match ev.payload {
                MeetingEventPayload::TranscriptEntry {
                    speaker_id,
                    speaker_name,
                    is_enrolled,
                    speaker_confidence,
                    text,
                    language,
                    resolved_stt_model,
                    latency_ms,
                } => {
                    assert_eq!(speaker_id, "SPEAKER_00");
                    assert_eq!(speaker_name.as_deref(), Some("Alice"));
                    assert!(is_enrolled);
                    assert_eq!(speaker_confidence, Some(0.87));
                    assert_eq!(text, "Zaczynamy spotkanie");
                    assert_eq!(language.as_deref(), Some("pl"));
                    assert_eq!(resolved_stt_model, "whisper-large-v3");
                    assert_eq!(latency_ms, 412);
                }
                _ => panic!("expected TranscriptEntry"),
            },
            _ => panic!("expected MeetingEvent variant"),
        }
    }

    // Roundtrip RosterSnapshot — batch z 50 uczestników, mix statusów i pól
    // opcjonalnych. Sprawdza że Vec<RosterEntry> przechodzi rkyv encode/decode
    // bez utraty danych przy realnych rozmiarach burst'u Teams.
    #[test]
    fn rkyv_roundtrip_meeting_event_roster_snapshot() {
        let entries: Vec<RosterEntry> = (0..50)
            .map(|i| RosterEntry {
                speaker_id: format!("SPEAKER_{:02}", i),
                speaker_name: if i % 3 == 0 { None } else { Some(format!("User {}", i)) },
                status: match i % 3 {
                    0 => "joined".to_string(),
                    1 => "speaking".to_string(),
                    _ => "left".to_string(),
                },
                last_spoken_ago_sec: if i % 2 == 0 { Some(i as u32) } else { None },
                has_video: i % 2 == 0,
                has_audio: i % 3 != 0,
                in_stage: i % 4 != 0,
                in_roster: true,
            })
            .collect();

        let request = ModelRequest {
            request_id: "req-rs-1".to_string(),
            payload: ModelPayload::MeetingEvent(MeetingEventData {
                meeting_key: "mkey-rs".to_string(),
                timestamp_ms: 1_700_000_003_000,
                payload: MeetingEventPayload::RosterSnapshot { entries: entries.clone() },
            }),
            stream: false,
            metadata: None,
            session_id: None,
        };

        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&request).expect("encode");
        let decoded: ModelRequest =
            rkyv::from_bytes::<ModelRequest, rkyv::rancor::Error>(&bytes).expect("decode");

        match decoded.payload {
            ModelPayload::MeetingEvent(ev) => match ev.payload {
                MeetingEventPayload::RosterSnapshot { entries: decoded_entries } => {
                    assert_eq!(decoded_entries.len(), 50);
                    assert_eq!(decoded_entries, entries);
                }
                _ => panic!("expected RosterSnapshot"),
            },
            _ => panic!("expected MeetingEvent variant"),
        }
    }

    // Roundtrip BackendUpdate — sprawdza wszystkie None w opcjonalnych liczbach,
    // bo tak bot je wysyła zaraz po join (bez znajomości streaming_latency itp.).
    #[test]
    fn rkyv_roundtrip_meeting_event_backend_update() {
        let request = ModelRequest {
            request_id: "req-bu-1".to_string(),
            payload: ModelPayload::MeetingEvent(MeetingEventData {
                meeting_key: "mkey-bu".to_string(),
                timestamp_ms: 1_700_000_004_000,
                payload: MeetingEventPayload::BackendUpdate {
                    stt_model: "teams-stt".to_string(),
                    tts_model: "teams-tts".to_string(),
                    summarization_model: "teams-summarization".to_string(),
                    diarization_model: "pyannote-3.1".to_string(),
                    streaming_latency_ms: None,
                    enrolled_speakers: None,
                    total_participants: None,
                },
            }),
            stream: false,
            metadata: None,
            session_id: None,
        };

        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&request).expect("encode");
        let decoded: ModelRequest =
            rkyv::from_bytes::<ModelRequest, rkyv::rancor::Error>(&bytes).expect("decode");

        match decoded.payload {
            ModelPayload::MeetingEvent(ev) => match ev.payload {
                MeetingEventPayload::BackendUpdate {
                    stt_model,
                    tts_model,
                    summarization_model,
                    diarization_model,
                    streaming_latency_ms,
                    enrolled_speakers,
                    total_participants,
                } => {
                    assert_eq!(stt_model, "teams-stt");
                    assert_eq!(tts_model, "teams-tts");
                    assert_eq!(summarization_model, "teams-summarization");
                    assert_eq!(diarization_model, "pyannote-3.1");
                    assert_eq!(streaming_latency_ms, None);
                    assert_eq!(enrolled_speakers, None);
                    assert_eq!(total_participants, None);
                }
                _ => panic!("expected BackendUpdate"),
            },
            _ => panic!("expected MeetingEvent variant"),
        }
    }

    // Roundtrip PromptFetchRequest przez rkyv — wariant ModelPayload::PromptFetch.
    #[test]
    fn rkyv_roundtrip_prompt_fetch_request() {
        let request = ModelRequest {
            request_id: "req-pf-1".to_string(),
            payload: ModelPayload::PromptFetch(PromptFetchRequest {
                prompt_id: "transcription_summarization".to_string(),
                language: "en".to_string(),
            }),
            stream: false,
            metadata: None,
            session_id: None,
        };

        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&request).expect("encode");
        let decoded: ModelRequest =
            rkyv::from_bytes::<ModelRequest, rkyv::rancor::Error>(&bytes).expect("decode");

        match decoded.payload {
            ModelPayload::PromptFetch(req) => {
                assert_eq!(req.prompt_id, "transcription_summarization");
                assert_eq!(req.language, "en");
            }
            _ => panic!("expected PromptFetch variant"),
        }
    }

    // Roundtrip PromptFetchResponse — sprawdza wariant ModelResult::PromptFetched.
    #[test]
    fn rkyv_roundtrip_prompt_fetched_response() {
        let response = ModelResponse {
            request_id: "req-pf-2".to_string(),
            result: ModelResult::PromptFetched(PromptFetchResponse {
                content: "You are a meeting assistant.".to_string(),
                name: "Transcription Summarization (EN)".to_string(),
                resolved_language: "en".to_string(),
            }),
            metrics: None,
        };

        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&response).expect("encode");
        let decoded: ModelResponse =
            rkyv::from_bytes::<ModelResponse, rkyv::rancor::Error>(&bytes).expect("decode");

        match decoded.result {
            ModelResult::PromptFetched(p) => {
                assert_eq!(p.content, "You are a meeting assistant.");
                assert_eq!(p.name, "Transcription Summarization (EN)");
                assert_eq!(p.resolved_language, "en");
            }
            _ => panic!("expected PromptFetched variant"),
        }
    }

    // Regression: bajty zserializowanego `ModelResponse` NIE moga walidowac sie
    // jako `ModelStreamChunk`. Ta kolizja byla zrodlem buga "subtree pointer
    // overran range" w chat streaming path: sidecar dla `request.stream=false`
    // zwracal `ModelResponse`, a klient po stronie routera czytal ramki jako
    // `ModelStreamChunk`. `request_id: String` na poczatku obu typow przepuszczal
    // parser az do enum discriminantu (Completion=1 vs TextDelta=1), gdzie rkyv
    // probowal odczytac String z bledem bytes -> pointer overrun.
    #[test]
    fn model_response_bytes_dont_validate_as_stream_chunk() {
        let response = ModelResponse {
            request_id: "test-req-id".to_string(),
            result: ModelResult::Completion(CompletionResult {
                text: "Hello world from the LLM".to_string(),
                reasoning_content: None,
                model: "qwen3.5-0.8b".to_string(),
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

        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&response).expect("encode");

        // Probujemy zwalidowac bajty ModelResponse jako ArchivedModelStreamChunk.
        // To MUSI sie skonczyc bledem walidacji (a NIE panika ani sukcesem).
        let result = rkyv::access::<ArchivedModelStreamChunk, rkyv::rancor::Error>(&bytes);
        assert!(
            result.is_err(),
            "ModelResponse bytes nie powinny walidowac sie jako ModelStreamChunk \
             (kolizja discriminantow ModelResult vs StreamChunkType)"
        );
    }

    // Sanity: roundtrip ModelStreamChunk z TextDelta — kontrola pozytywna do
    // testu wyzej.
    #[test]
    fn rkyv_roundtrip_stream_chunk_text_delta() {
        let chunk = ModelStreamChunk {
            request_id: "test-req-id".to_string(),
            chunk: StreamChunkType::TextDelta("Hello world".to_string()),
        };
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&chunk).expect("encode");
        let decoded: ModelStreamChunk =
            rkyv::from_bytes::<ModelStreamChunk, rkyv::rancor::Error>(&bytes).expect("decode");
        assert_eq!(decoded.request_id, "test-req-id");
        match decoded.chunk {
            StreamChunkType::TextDelta(s) => assert_eq!(s, "Hello world"),
            _ => panic!("expected TextDelta"),
        }
    }
}
