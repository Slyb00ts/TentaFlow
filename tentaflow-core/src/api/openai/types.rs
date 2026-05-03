// =============================================================================
// Plik: api/openai/types.rs
// Opis: Definicje struktur danych dla wszystkich endpointow OpenAI API.
//       Wszystkie struktury sa kompatybilne z oficjalna specyfikacja OpenAI API.
//       Wspiera: Chat Completions, Embeddings, Audio Transcriptions, Vision API.
// Przyklad:
//   let request = ChatCompletionRequest {
//       model: "gpt-4".to_string(),
//       messages: vec![Message { role: "user".to_string(), .. }],
//       ..Default::default()
//   };
// =============================================================================

use serde::{Deserialize, Serialize};

// =============================================================================
// CHAT COMPLETIONS - TEXT & VISION
// =============================================================================

/// Memory-specific options dla integracji z TentaFlow.Memory.
///
/// Opcjonalne pole w ChatCompletionRequest ktore klient moze wypelnic
/// aby wlaczyc pamiec kontekstowa (osoby, projekty, relacje).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MemoryOptions {
    /// Czy pamiec jest wlaczona (domyslnie true jesli session_id podane)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,

    /// Identyfikator sesji rozmowy (UUID) - uzywany do sledzenia kontekstu
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,

    /// ID rozpoznanej osoby z STT (dla integracji glosowej)
    /// Mapowane na person_id w Memory (speaker_id = person_id)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub person_id: Option<String>,

    /// Poziom pewnosci rozpoznania glosu (0.0-1.0)
    /// >0.85: automatycznie rozpoznaj, 0.60-0.85: zapytaj o potwierdzenie, <0.60: nowa osoba
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speaker_confidence: Option<f32>,

    /// Nazwa rozpoznanego mowcy (np. "Piotrek")
    /// Ustawiane gdy speaker_confidence > 0.60
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speaker_name: Option<String>,

    /// Czy zapisywac nowe informacje do Memory (domyslnie true)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub store_enabled: Option<bool>,

    /// Czy odpytywac Memory przed modelem (domyslnie true)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub query_enabled: Option<bool>,

    /// Poprzedni kontekst sesji (dla REFINE/EXPAND queries)
    /// Wypelniane automatycznie przez Router na podstawie historii sesji
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_context: Option<String>,
}

/// Request do /v1/chat/completions
///
/// Obsluguje zarowno zwykly tekst jak i multimodal (vision).
/// Streaming jest kontrolowane przez pole `stream`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatCompletionRequest {
    /// Nazwa modelu (np. "gpt-4-turbo", "claude-3-5-sonnet")
    pub model: String,

    /// Lista wiadomosci w konwersacji
    pub messages: Vec<Message>,

    /// Temperatura (0.0-2.0) - kontroluje losowosc odpowiedzi
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,

    /// Max tokens w odpowiedzi
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,

    /// Top-p sampling (0.0-1.0)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,

    /// Frequency penalty (-2.0 do 2.0)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frequency_penalty: Option<f32>,

    /// Presence penalty (-2.0 do 2.0)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub presence_penalty: Option<f32>,

    /// Stop sequences (max 4)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop: Option<Vec<String>>,

    /// Czy streamowac odpowiedz (SSE)
    #[serde(default)]
    pub stream: bool,

    /// User identifier (dla monitoringu i rate limiting)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,

    /// Response format (np. {"type": "json_object"})
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_format: Option<ResponseFormat>,

    /// Function calling (OpenAI tools)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<Tool>>,

    /// Tool choice ("auto", "none", lub konkretna funkcja)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<ToolChoice>,

    /// Liczba completion choices do wygenerowania (n)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub n: Option<u32>,

    /// Memory-specific options dla integracji z TentaFlow.Memory
    /// Pole niestandardowe - uzywane gdy wlaczona pamiec kontekstowa
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_options: Option<MemoryOptions>,

    /// Audio input dla voice conversation (opcjonalne).
    /// Jesli podane, Router przetworzy przez STT i speaker identification.
    /// Zwraca transkrybowany tekst oraz info o mowcy w response.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio_input: Option<Vec<u8>>,
}

/// Pojedyncza wiadomosc w konwersacji
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Message {
    /// Rola: "system", "user", "assistant", "tool"
    #[serde(default)]
    pub role: String,

    /// Tresc wiadomosci - moze byc String lub Vec<ContentPart> dla multimodal
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub content: Option<MessageContent>,

    /// Reasoning content (dla modeli reasoning jak DeepSeek R1, OpenAI o1)
    /// Zawiera "chain of thought" / proces myslowy modelu
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub reasoning_content: Option<String>,

    /// Nazwa uzytkownika/bota (opcjonalne)
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub name: Option<String>,

    /// Tool calls (dla function calling)
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub tool_calls: Option<Vec<ToolCall>>,

    /// Tool call ID (dla role="tool")
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub tool_call_id: Option<String>,
}

/// Content wiadomosci - moze byc tekstem lub multimodal
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MessageContent {
    /// Zwykly tekst
    Text(String),
    /// Multimodal content (tekst + obrazy)
    Parts(Vec<ContentPart>),
}

/// Czesc zawartosci multimodal (text | image_url)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentPart {
    /// Fragment tekstowy
    Text { text: String },
    /// URL do obrazu (dla vision)
    ImageUrl { image_url: ImageUrl },
}

/// URL obrazu dla vision requests
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageUrl {
    /// URL obrazu (data: URI lub HTTP(S) URL)
    pub url: String,

    /// Detail level: "auto", "low", "high"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Response format specification
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseFormat {
    /// Typ: "text" lub "json_object"
    #[serde(rename = "type")]
    pub format_type: String,
}

/// Tool definition (function calling)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tool {
    /// Typ: "function"
    #[serde(rename = "type")]
    pub tool_type: String,

    /// Definicja funkcji
    pub function: FunctionDefinition,
}

/// Definicja funkcji dla tool calling
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionDefinition {
    /// Nazwa funkcji
    pub name: String,

    /// Opis funkcji
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// JSON Schema parametrow
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parameters: Option<serde_json::Value>,
}

/// Tool choice specification
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ToolChoice {
    /// "auto", "none"
    String(String),
    /// Konkretna funkcja: {"type": "function", "function": {"name": "..."}}
    Object(ToolChoiceObject),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolChoiceObject {
    #[serde(rename = "type")]
    pub tool_type: String,
    pub function: ToolChoiceFunction,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolChoiceFunction {
    pub name: String,
}

/// Tool call w odpowiedzi (function calling)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    /// ID wywolania
    pub id: String,

    /// Typ: "function"
    #[serde(rename = "type")]
    pub tool_type: String,

    /// Funkcja do wywolania
    pub function: FunctionCall,
}

/// Function call details
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCall {
    /// Nazwa funkcji
    pub name: String,

    /// Argumenty funkcji (JSON string)
    pub arguments: String,
}

/// Response z /v1/chat/completions (non-streaming)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatCompletionResponse {
    /// ID requestu
    pub id: String,

    /// Object type: "chat.completion"
    pub object: String,

    /// Unix timestamp utworzenia
    pub created: u64,

    /// Model uzyty do generowania
    pub model: String,

    /// Lista choices (zazwyczaj 1)
    pub choices: Vec<Choice>,

    /// Token usage statistics
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,

    /// System fingerprint (dla reproducibility)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_fingerprint: Option<String>,

    // === VOICE CONVERSATION FIELDS (z audio_input) ===
    /// Transkrybowany tekst z audio_input (jesli podano audio_input)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transcribed_text: Option<String>,

    /// ID rozpoznanego mowcy (z speaker identification)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speaker_id: Option<String>,

    /// Nazwa rozpoznanego mowcy
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speaker_name: Option<String>,

    /// Poziom pewnosci rozpoznania mowcy (0.0-1.0)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speaker_confidence: Option<f32>,

    // === INTENT ANALYZER FIELDS ===
    /// Wykryte intencje (z Intent Analyzer / Bielik 1.5B)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detected_intent: Option<String>,

    /// Wykryte wywolania narzedzi z wynikami wykonania
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detected_tools: Option<Vec<DetectedToolCall>>,
}

/// Wykryte wywolanie narzedzia z wynikiem wykonania
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DetectedToolCall {
    /// ID wywolania
    pub call_id: String,

    /// Nazwa narzedzia (calendar_add, email_send, web_search, etc.)
    pub tool_name: String,

    /// Parametry narzedzia (JSON)
    pub parameters: serde_json::Value,

    /// Czy wywolanie bylo kompletne (wszystkie wymagane parametry)
    pub is_complete: bool,

    /// Brakujace parametry (jesli niekompletne)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub missing_params: Option<Vec<String>>,

    /// Wynik wykonania (jesli is_complete=true)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution_result: Option<ToolExecutionResult>,

    /// Pytanie uzupelniajace (jesli brakuje parametrow)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub follow_up_question: Option<String>,
}

/// Wynik wykonania narzedzia
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolExecutionResult {
    /// Czy wykonanie sie powiodlo
    pub success: bool,

    /// Wiadomosc zwrotna
    pub message: String,

    /// Dane zwrotne (opcjonalne)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,

    /// Blad (jesli success=false)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Pojedynczy choice w odpowiedzi
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Choice {
    /// Index choice (0, 1, 2...)
    pub index: u32,

    /// Wiadomosc wygenerowana przez model
    pub message: Message,

    /// Finish reason: "stop", "length", "tool_calls", "content_filter"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<String>,

    /// Logprobs (jesli requested)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logprobs: Option<serde_json::Value>,
}

/// Token usage statistics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Usage {
    /// Tokeny w prompt
    pub prompt_tokens: u32,

    /// Tokeny w completion
    pub completion_tokens: u32,

    /// Suma tokenow
    pub total_tokens: u32,
}

/// Chunk w streaming response (SSE)
///
/// Format SSE: `data: {json}\n\n`
/// Ostatni chunk: `data: [DONE]\n\n`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatCompletionChunk {
    pub id: String,
    pub object: String, // "chat.completion.chunk"
    pub created: u64,
    pub model: String,
    pub choices: Vec<ChunkChoice>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_fingerprint: Option<String>,

    /// Opcjonalny audio chunk (TTS) - base64 encoded
    /// Rozszerzenie OpenAI API dla audio streaming
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio: Option<String>,

    // === INTENT ANALYZER FIELDS (tylko pierwszy chunk) ===
    /// Wykryta intencja glowna (z Intent Analyzer / Bielik 1.5B)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detected_intent: Option<String>,

    /// Wykryte wywolania narzedzi z wynikami wykonania
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detected_tools: Option<Vec<DetectedToolCall>>,

    /// Transkrybowany tekst z audio input
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transcribed_text: Option<String>,

    /// ID rozpoznanego mowcy
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speaker_id: Option<String>,

    /// Nazwa rozpoznanego mowcy
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speaker_name: Option<String>,
}

/// Choice w streaming chunk
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkChoice {
    pub index: u32,

    /// Delta (przyrostowe fragmenty wiadomosci)
    pub delta: Delta,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub logprobs: Option<serde_json::Value>,
}

/// Delta w streaming chunk (przyrostowe fragmenty)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Delta {
    /// Rola (tylko w pierwszym chunk)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,

    /// Fragment tekstu
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,

    /// Reasoning content (dla modeli z rozumowaniem, np. o1)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,

    /// Tool calls (przyrostowe)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
}

// =============================================================================
// IMAGE GENERATION (DALL-E)
// =============================================================================

/// Request do /v1/images/generations
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageGenerationRequest {
    /// Nazwa modelu (np. "dall-e-3", "dall-e-2")
    pub model: String,

    /// Prompt opisujacy obraz
    pub prompt: String,

    /// Liczba obrazow do wygenerowania (1-10, dall-e-3 = 1)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub n: Option<u32>,

    /// Rozmiar: "256x256", "512x512", "1024x1024", "1792x1024", "1024x1792"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<String>,

    /// Quality: "standard", "hd" (tylko dall-e-3)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quality: Option<String>,

    /// Format odpowiedzi: "url" lub "b64_json"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_format: Option<String>,

    /// Style: "vivid", "natural" (tylko dall-e-3)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub style: Option<String>,

    /// User identifier
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
}

/// Response z /v1/images/generations
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageGenerationResponse {
    pub created: u64,
    pub data: Vec<ImageData>,
}

/// Pojedynczy wygenerowany obraz
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageData {
    /// URL do obrazu (jesli response_format = "url")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,

    /// Base64 encoded obraz (jesli response_format = "b64_json")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub b64_json: Option<String>,

    /// Revised prompt (dall-e-3 moze zmodyfikowac prompt)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revised_prompt: Option<String>,
}

// =============================================================================
// AUDIO TTS (TEXT-TO-SPEECH)
// =============================================================================

/// Request do /v1/audio/speech
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TTSRequest {
    /// Model: "tts-1", "tts-1-hd"
    pub model: String,

    /// Tekst do zamiany na mowe
    pub input: String,

    /// Glos: "alloy", "echo", "fable", "onyx", "nova", "shimmer"
    pub voice: String,

    /// Format audio: "mp3", "opus", "aac", "flac", "wav", "pcm"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_format: Option<String>,

    /// Predkosc (0.25-4.0)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speed: Option<f32>,

    /// Jezyk syntezy (ISO-639-1: "en", "pl", "fr", "es", "de"). Gdy klient
    /// nie poda, handler dolozy preferencje uzytkownika lub default "en".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
}

// Response dla TTS to raw audio bytes (Content-Type: audio/mpeg, etc.)
// Wiec nie ma osobnej struktury - bezposrednio zwracamy Vec<u8>

// =============================================================================
// AUDIO STT (SPEECH-TO-TEXT, WHISPER)
// =============================================================================

/// R2d (D.3): pierwszorzedne opcje STT — speaker identification, diarization,
/// per-segment timestamps, format wyjscia. Ten typ jest source of truth dla
/// `SttRuntime`; `TranscriptionRequest.options` przenosi je razem z klasycznym
/// requestem OpenAI-compatible.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SttRequestOptions {
    /// Czy probowac dopasowac mowcow do bazy (Memory.persons / `voice_samples`).
    /// Domyslnie false zeby zachowac OpenAI-compatible zachowanie dla klientow
    /// nieswiadomych speaker logiki.
    #[serde(default)]
    pub speaker_identification: bool,

    /// Czy uruchomic diarization (oddzielenie wielu mowcow w jednym audio).
    /// Wynik trafia do `TranscriptionResponse.speakers` jako lista
    /// `SpeakerSegment` z timestampami i etykietami.
    #[serde(default)]
    pub diarization: bool,

    /// Granularnosc timestampow: "segment" (default) lub "word" — odpowiada
    /// OpenAI `timestamp_granularities[]` w multiparcie.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamps: Option<String>,

    /// Format wyjsciowy: "json", "text", "srt", "verbose_json", "vtt".
    /// `verbose_json` jest jedyny ktory ma prawo zwracac `segments` i
    /// `speakers`; pozostale formaty pomijaja oba pola.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_format: Option<String>,
}

/// Request do /v1/audio/transcriptions
///
/// To jest multipart/form-data request, wiec serializacja bedzie
/// inna niz JSON. Ta struktura reprezentuje pola formularza.
#[derive(Debug, Clone)]
pub struct TranscriptionRequest {
    /// Plik audio (multipart field: "file"). Arc<[u8]> zeby caly pipeline
    /// STT (route -> dispatch closure -> diarization fork -> local_stt)
    /// dzielil ten sam blok PCM przez refcount zamiast klonowac kazda kopie.
    pub file: std::sync::Arc<[u8]>,

    /// Nazwa pliku
    pub filename: String,

    /// Model: "whisper-1"
    pub model: String,

    /// Language code (ISO-639-1, np. "en", "pl")
    pub language: Option<String>,

    /// Prompt (kontekst dla modelu)
    pub prompt: Option<String>,

    /// Response format: "json", "text", "srt", "verbose_json", "vtt"
    pub response_format: Option<String>,

    /// Temperature (0.0-1.0)
    pub temperature: Option<f32>,

    /// Granularnosc timestampow (tylko dla verbose_json):
    /// - "segment" - timestamps per segment (domyslny)
    /// - "word" - timestamps per word
    pub timestamp_granularities: Option<Vec<String>>,

    // === OPCJE FILTROWANIA ===
    /// Prog no_speech_prob do filtrowania halucynacji
    /// Segmenty z no_speech_prob >= threshold zostana odfiltrowane
    pub no_speech_threshold: Option<f32>,

    /// Minimalny avg_logprob dla segmentu
    /// Segmenty z avg_logprob < threshold zostana odfiltrowane
    pub avg_logprob_threshold: Option<f32>,

    /// Maksymalny compression_ratio dla segmentu
    /// Segmenty z compression_ratio > threshold zostana odfiltrowane
    pub compression_ratio_threshold: Option<f32>,

    /// R2d: opcje pierwszorzedne (D.3). Domyslne wartosci zachowuja
    /// OpenAI-compatible zachowanie (brak speaker ID, brak diarization).
    /// Klasyczne `response_format` / `timestamp_granularities` zostaja dla
    /// kompatybilnosci wstecznej; `options.response_format` /
    /// `options.timestamps` maja pierwszenstwo gdy ustawione.
    pub options: SttRequestOptions,
}

/// Response z /v1/audio/transcriptions
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptionResponse {
    /// Transkrybowany tekst
    pub text: String,

    /// Dodatkowe pola dla verbose_json
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration: Option<f32>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub segments: Option<Vec<TranscriptionSegment>>,

    /// R2d (D.3): pierwszorzedne segmenty mowcow z diarization. Wypelniane
    /// gdy request mial `options.diarization=true`. Niezalezne od `segments`
    /// bo agregacja odbywa sie na granicach mowcow, nie na granicach
    /// segmentow whisper.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speakers: Option<Vec<SpeakerSegment>>,
}

/// R2d (D.3): segment przypisany jednemu mowcy. Zwracany w polu
/// `TranscriptionResponse.speakers` gdy `options.diarization=true`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpeakerSegment {
    /// Czas startu segmentu w sekundach.
    pub start: f32,

    /// Czas konca segmentu w sekundach.
    pub end: f32,

    /// Tekst wypowiedziany przez tego mowce w tym segmencie.
    pub text: String,

    /// Etykieta mowcy. Anonimowy mowca: "SPEAKER_00", "SPEAKER_01", ...
    /// Rozpoznany z bazy: nazwa osoby (np. "Jan Kowalski") gdy
    /// `options.speaker_identification=true` i similarity >= threshold.
    pub speaker_label: String,

    /// ID rozpoznanego mowcy z bazy persons (None gdy anonimowy).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speaker_id: Option<String>,

    /// Similarity score 0.0-1.0 z bazy mowcow (None gdy nie probowalismy
    /// dopasowac albo brak dopasowania).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub similarity: Option<f32>,
}

/// Segment w transkrypcji (verbose_json)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptionSegment {
    pub id: u32,
    pub seek: u32,
    pub start: f32,
    pub end: f32,
    pub text: String,
    pub tokens: Vec<u32>,
    pub temperature: f32,
    pub avg_logprob: f32,
    pub compression_ratio: f32,
    pub no_speech_prob: f32,
    /// Etykieta mowcy z diarization (np. "SPEAKER_00", "Jan Kowalski")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speaker_label: Option<String>,

    /// Similarity score z bazy mowcow (0.0-1.0, cosine similarity)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speaker_similarity: Option<f32>,

    /// Czy mowca zostal rozpoznany z bazy (true) czy to anonimowy speaker (false)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_known_speaker: Option<bool>,
}

// =============================================================================
// EMBEDDINGS
// =============================================================================

/// Request do /v1/embeddings
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingRequest {
    /// Model: "text-embedding-ada-002", "text-embedding-3-small", etc.
    pub model: String,

    /// Input text (string lub array of strings)
    pub input: EmbeddingInput,

    /// Encoding format: "float" lub "base64"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub encoding_format: Option<String>,

    /// Dimensions (dla embedding-3 models)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dimensions: Option<u32>,

    /// User identifier
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
}

/// Input dla embeddings (string lub array)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum EmbeddingInput {
    Single(String),
    Multiple(Vec<String>),
}

/// Response z /v1/embeddings
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingResponse {
    pub object: String, // "list"
    pub data: Vec<EmbeddingData>,
    pub model: String,
    pub usage: EmbeddingUsage,
}

/// Pojedynczy embedding
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingData {
    pub object: String, // "embedding"
    pub index: u32,
    pub embedding: Vec<f32>,
}

/// Token usage dla embeddings
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingUsage {
    pub prompt_tokens: u32,
    pub total_tokens: u32,
}

// =============================================================================
// ERROR RESPONSE
// =============================================================================

/// Error response zgodny z OpenAI API
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorResponse {
    pub error: ErrorDetail,
}

/// Szczegoly bledu
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorDetail {
    /// Typ bledu: "invalid_request_error", "authentication_error", etc.
    #[serde(rename = "type")]
    pub error_type: String,

    /// Human-readable message
    pub message: String,

    /// Konkretny parametr ktory spowodowal blad
    #[serde(skip_serializing_if = "Option::is_none")]
    pub param: Option<String>,

    /// Kod bledu (dla szczegolowych przypadkow)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
}
