// ============================================================================
// FFI-SAFE TYPES - Typy bezpieczne do przekazywania przez granicę FFI
// ============================================================================
//
// CEL:
// Definicje struktur C-compatible dla komunikacji między Rust a .NET przez P/Invoke.
// Każda struktura musi mieć identyczny layout pamięci w obu językach.
//
// JAK DZIAŁA:
// 1. Wszystkie typy używają #[repr(C)] dla przewidywalnego layoutu pamięci
// 2. Stringi są przekazywane jako *mut c_char (null-terminated)
// 3. Tablice jako (*mut T, len) - wskaźnik + długość
// 4. Pamięć alokowana przez Rust musi być zwolniona przez tentaflow_free_* funkcje
//
// PRZYKŁAD UŻYCIA (C#):
// ```csharp
// [StructLayout(LayoutKind.Sequential)]
// public struct EmbeddingsResult {
//     public IntPtr embeddings;      // float* - płaska tablica
//     public UIntPtr embeddingsCount;
//     public UIntPtr dimensions;
//     public IntPtr error;           // null jeśli sukces
// }
// ```
//
// KLUCZOWE KONCEPCJE:
// - #[repr(C)]: Gwarantuje layout pamięci zgodny z C ABI
// - CString: Rust string konwertowany na null-terminated C string
// - Box::into_raw/from_raw: Transfer własności pamięci przez FFI
// - std::mem::forget: Zapobiega zwolnieniu pamięci przed przekazaniem do .NET
//
// BEZPIECZEŃSTWO PAMIĘCI:
// - Każdy Result ma odpowiadającą funkcję tentaflow_free_* w ffi.rs
// - .NET MUSI wywołać free po zakończeniu używania danych
// - Brak wywołania free = memory leak
//
// ============================================================================

use std::ffi::{c_char, CString};
use std::ptr;

// ============================================================================
// RESULT TYPES
// ============================================================================

/// Wynik operacji embeddings.
///
/// Layout pamięci (C-compatible):
/// - embeddings: wskaźnik do płaskiej tablicy float32
/// - embeddings_count: liczba wektorów
/// - dimensions: wymiary każdego wektora
/// - latency_ms: całkowita latencja w ms
/// - error: wskaźnik do stringa błędu (null jeśli sukces)
#[repr(C)]
pub struct EmbeddingsResult {
    /// Płaska tablica embeddings: [vec1_dim1, vec1_dim2, ..., vec2_dim1, ...]
    pub embeddings: *mut f32,
    /// Liczba wektorów embeddings
    pub embeddings_count: usize,
    /// Wymiary każdego wektora (np. 768)
    pub dimensions: usize,
    /// Całkowita latencja w ms
    pub latency_ms: u64,
    /// Komunikat błędu (null jeśli sukces)
    pub error: *mut c_char,
}

impl EmbeddingsResult {
    pub fn success(embeddings: Vec<Vec<f32>>, latency_ms: u64) -> Self {
        if embeddings.is_empty() {
            return Self {
                embeddings: ptr::null_mut(),
                embeddings_count: 0,
                dimensions: 0,
                latency_ms,
                error: ptr::null_mut(),
            };
        }

        let dimensions = embeddings[0].len();
        let embeddings_count = embeddings.len();

        // Flatten to single Vec
        let flat: Vec<f32> = embeddings.into_iter().flatten().collect();
        let mut boxed = flat.into_boxed_slice();
        let ptr = boxed.as_mut_ptr();
        std::mem::forget(boxed);

        Self {
            embeddings: ptr,
            embeddings_count,
            dimensions,
            latency_ms,
            error: ptr::null_mut(),
        }
    }

    pub fn error(msg: &str) -> Self {
        let c_str = CString::new(msg).unwrap_or_else(|_| CString::new("Unknown error").unwrap());
        Self {
            embeddings: ptr::null_mut(),
            embeddings_count: 0,
            dimensions: 0,
            latency_ms: 0,
            error: c_str.into_raw(),
        }
    }
}

// ============================================================================
// DETECTED TOOL CALL TYPES (Intent Analyzer)
// ============================================================================

/// Wynik wykonania narzędzia wykrytego przez Intent Analyzer.
#[repr(C)]
pub struct DetectedToolExecutionResult {
    /// Czy wykonanie się powiodło (1 = true, 0 = false)
    pub success: u8,
    /// Komunikat wyniku
    pub message: *mut c_char,
    /// Dane wynikowe (JSON string, null jeśli brak)
    pub data: *mut c_char,
    /// Komunikat błędu (null jeśli sukces)
    pub error: *mut c_char,
}

impl DetectedToolExecutionResult {
    pub fn new(success: bool, message: String, data: Option<String>, error: Option<String>) -> Self {
        Self {
            success: if success { 1 } else { 0 },
            message: CString::new(message).unwrap_or_default().into_raw(),
            data: data
                .map(|s| CString::new(s).unwrap_or_default().into_raw())
                .unwrap_or(ptr::null_mut()),
            error: error
                .map(|s| CString::new(s).unwrap_or_default().into_raw())
                .unwrap_or(ptr::null_mut()),
        }
    }
}

/// Wykryte wywołanie narzędzia z Intent Analyzer.
#[repr(C)]
pub struct DetectedToolCall {
    /// Unikalny identyfikator wywołania
    pub call_id: *mut c_char,
    /// Nazwa narzędzia (np. "calendar", "email", "web_search")
    pub tool_name: *mut c_char,
    /// Parametry jako JSON string
    pub parameters: *mut c_char,
    /// Czy wywołanie jest kompletne (1 = true, 0 = false)
    pub is_complete: u8,
    /// Brakujące parametry (tablica stringów)
    pub missing_params: *mut *mut c_char,
    /// Liczba brakujących parametrów
    pub missing_params_count: u32,
    /// Wynik wykonania (null jeśli nie wykonano)
    pub execution_result: *mut DetectedToolExecutionResult,
    /// Pytanie uzupełniające (null jeśli brak)
    pub follow_up_question: *mut c_char,
}

impl DetectedToolCall {
    pub fn new(
        call_id: String,
        tool_name: String,
        parameters: String,
        is_complete: bool,
        missing_params: Option<Vec<String>>,
        execution_result: Option<DetectedToolExecutionResult>,
        follow_up_question: Option<String>,
    ) -> Self {
        let (missing_ptr, missing_count) = match missing_params {
            Some(params) if !params.is_empty() => {
                let count = params.len() as u32;
                let ptrs: Vec<*mut c_char> = params
                    .into_iter()
                    .map(|s| CString::new(s).unwrap_or_default().into_raw())
                    .collect();
                let mut boxed = ptrs.into_boxed_slice();
                let ptr = boxed.as_mut_ptr();
                std::mem::forget(boxed);
                (ptr, count)
            }
            _ => (ptr::null_mut(), 0),
        };

        let exec_ptr = execution_result
            .map(|er| Box::into_raw(Box::new(er)))
            .unwrap_or(ptr::null_mut());

        Self {
            call_id: CString::new(call_id).unwrap_or_default().into_raw(),
            tool_name: CString::new(tool_name).unwrap_or_default().into_raw(),
            parameters: CString::new(parameters).unwrap_or_default().into_raw(),
            is_complete: if is_complete { 1 } else { 0 },
            missing_params: missing_ptr,
            missing_params_count: missing_count,
            execution_result: exec_ptr,
            follow_up_question: follow_up_question
                .map(|s| CString::new(s).unwrap_or_default().into_raw())
                .unwrap_or(ptr::null_mut()),
        }
    }
}

/// Wynik operacji chat completion (non-streaming i streaming).
#[repr(C)]
pub struct ChatCompletionResult {
    /// Wygenerowany tekst (content)
    pub content: *mut c_char,
    /// Chain-of-thought reasoning (dla modeli reasoning jak DeepSeek R1, OpenAI o1)
    /// NULL jeśli model nie zwraca reasoning
    pub reasoning_content: *mut c_char,
    /// Nazwa modelu
    pub model: *mut c_char,
    /// Finish reason ("stop", "length", etc.)
    pub finish_reason: *mut c_char,
    /// Prompt tokens
    pub prompt_tokens: u32,
    /// Completion tokens
    pub completion_tokens: u32,
    /// Total tokens
    pub total_tokens: u32,
    /// Time To First Token w ms (tylko dla streaming, 0 dla non-streaming)
    pub time_to_first_token_ms: u64,
    /// Całkowita latencja w ms
    pub latency_ms: u64,
    /// Tokeny na sekundę (0.0 jeśli niedostępne)
    pub tokens_per_sec: f32,
    /// Transkrybowany tekst z audio input (null jeśli brak audio input)
    pub transcribed_text: *mut c_char,
    /// ID rozpoznanego mówcy (null jeśli nie rozpoznano)
    pub speaker_id: *mut c_char,
    /// Nazwa rozpoznanego mówcy (null jeśli nie rozpoznano)
    pub speaker_name: *mut c_char,
    /// Wykryty intent z Intent Analyzer (null jeśli brak)
    pub detected_intent: *mut c_char,
    /// Wykryte wywołania narzędzi z Intent Analyzer
    pub detected_tools: *mut DetectedToolCall,
    /// Liczba wykrytych narzędzi
    pub detected_tools_count: u32,
    /// Komunikat błędu (null jeśli sukces)
    pub error: *mut c_char,
}

impl ChatCompletionResult {
    pub fn success(
        content: String,
        reasoning_content: Option<String>,
        model: String,
        finish_reason: Option<String>,
        prompt_tokens: u32,
        completion_tokens: u32,
    ) -> Self {
        Self::success_with_metrics(content, reasoning_content, model, finish_reason, prompt_tokens, completion_tokens, 0, 0, 0.0)
    }

    pub fn success_with_metrics(
        content: String,
        reasoning_content: Option<String>,
        model: String,
        finish_reason: Option<String>,
        prompt_tokens: u32,
        completion_tokens: u32,
        time_to_first_token_ms: u64,
        latency_ms: u64,
        tokens_per_sec: f32,
    ) -> Self {
        Self::success_with_speaker(
            content, reasoning_content, model, finish_reason,
            prompt_tokens, completion_tokens,
            time_to_first_token_ms, latency_ms, tokens_per_sec,
            None, None, None,
        )
    }

    pub fn success_with_speaker(
        content: String,
        reasoning_content: Option<String>,
        model: String,
        finish_reason: Option<String>,
        prompt_tokens: u32,
        completion_tokens: u32,
        time_to_first_token_ms: u64,
        latency_ms: u64,
        tokens_per_sec: f32,
        transcribed_text: Option<String>,
        speaker_id: Option<String>,
        speaker_name: Option<String>,
    ) -> Self {
        Self::success_with_intent(
            content, reasoning_content, model, finish_reason,
            prompt_tokens, completion_tokens,
            time_to_first_token_ms, latency_ms, tokens_per_sec,
            transcribed_text, speaker_id, speaker_name,
            None, None,
        )
    }

    pub fn success_with_intent(
        content: String,
        reasoning_content: Option<String>,
        model: String,
        finish_reason: Option<String>,
        prompt_tokens: u32,
        completion_tokens: u32,
        time_to_first_token_ms: u64,
        latency_ms: u64,
        tokens_per_sec: f32,
        transcribed_text: Option<String>,
        speaker_id: Option<String>,
        speaker_name: Option<String>,
        detected_intent: Option<String>,
        detected_tools: Option<Vec<DetectedToolCall>>,
    ) -> Self {
        let content_cstr = CString::new(content).unwrap_or_default();
        let reasoning_cstr = reasoning_content
            .map(|s| CString::new(s).unwrap_or_default().into_raw())
            .unwrap_or(ptr::null_mut());
        let model_cstr = CString::new(model).unwrap_or_default();
        let finish_cstr = CString::new(finish_reason.unwrap_or_default()).unwrap_or_default();
        let transcribed_cstr = transcribed_text
            .map(|s| CString::new(s).unwrap_or_default().into_raw())
            .unwrap_or(ptr::null_mut());
        let speaker_id_cstr = speaker_id
            .map(|s| CString::new(s).unwrap_or_default().into_raw())
            .unwrap_or(ptr::null_mut());
        let speaker_name_cstr = speaker_name
            .map(|s| CString::new(s).unwrap_or_default().into_raw())
            .unwrap_or(ptr::null_mut());
        let detected_intent_cstr = detected_intent
            .map(|s| CString::new(s).unwrap_or_default().into_raw())
            .unwrap_or(ptr::null_mut());

        let (detected_tools_ptr, detected_tools_count) = match detected_tools {
            Some(tools) if !tools.is_empty() => {
                let count = tools.len() as u32;
                let mut boxed = tools.into_boxed_slice();
                let ptr = boxed.as_mut_ptr();
                std::mem::forget(boxed);
                (ptr, count)
            }
            _ => (ptr::null_mut(), 0),
        };

        Self {
            content: content_cstr.into_raw(),
            reasoning_content: reasoning_cstr,
            model: model_cstr.into_raw(),
            finish_reason: finish_cstr.into_raw(),
            prompt_tokens,
            completion_tokens,
            total_tokens: prompt_tokens + completion_tokens,
            time_to_first_token_ms,
            latency_ms,
            tokens_per_sec,
            transcribed_text: transcribed_cstr,
            speaker_id: speaker_id_cstr,
            speaker_name: speaker_name_cstr,
            detected_intent: detected_intent_cstr,
            detected_tools: detected_tools_ptr,
            detected_tools_count,
            error: ptr::null_mut(),
        }
    }

    pub fn error(msg: &str) -> Self {
        let c_str = CString::new(msg).unwrap_or_else(|_| CString::new("Unknown error").unwrap());
        Self {
            content: ptr::null_mut(),
            reasoning_content: ptr::null_mut(),
            model: ptr::null_mut(),
            finish_reason: ptr::null_mut(),
            prompt_tokens: 0,
            completion_tokens: 0,
            total_tokens: 0,
            time_to_first_token_ms: 0,
            latency_ms: 0,
            tokens_per_sec: 0.0,
            transcribed_text: ptr::null_mut(),
            speaker_id: ptr::null_mut(),
            speaker_name: ptr::null_mut(),
            detected_intent: ptr::null_mut(),
            detected_tools: ptr::null_mut(),
            detected_tools_count: 0,
            error: c_str.into_raw(),
        }
    }
}

/// Wynik operacji TTS (Text-to-Speech).
#[repr(C)]
pub struct TtsResult {
    /// Audio data (raw bytes - format zależy od requestu)
    pub audio_data: *mut u8,
    /// Długość audio data w bajtach
    pub audio_len: usize,
    /// Format audio ("mp3", "opus", "wav", etc.)
    pub format: *mut c_char,
    /// Całkowita latencja w ms
    pub latency_ms: u64,
    /// Czas trwania audio w sekundach
    pub audio_duration_sec: f32,
    /// Komunikat błędu (null jeśli sukces)
    pub error: *mut c_char,
}

impl TtsResult {
    pub fn success(audio: Vec<u8>, format: &str, latency_ms: u64, audio_duration_sec: f32) -> Self {
        let mut boxed = audio.into_boxed_slice();
        let ptr = boxed.as_mut_ptr();
        let len = boxed.len();
        std::mem::forget(boxed);

        let format_cstr = CString::new(format).unwrap_or_default();

        Self {
            audio_data: ptr,
            audio_len: len,
            format: format_cstr.into_raw(),
            latency_ms,
            audio_duration_sec,
            error: ptr::null_mut(),
        }
    }

    pub fn error(msg: &str) -> Self {
        let c_str = CString::new(msg).unwrap_or_else(|_| CString::new("Unknown error").unwrap());
        Self {
            audio_data: ptr::null_mut(),
            audio_len: 0,
            format: ptr::null_mut(),
            latency_ms: 0,
            audio_duration_sec: 0.0,
            error: c_str.into_raw(),
        }
    }
}

/// Wynik operacji STT (Speech-to-Text).
#[repr(C)]
pub struct SttResult {
    /// Transkrypcja tekstu
    pub text: *mut c_char,
    /// Wykryty język (ISO-639-1)
    pub language: *mut c_char,
    /// Czas trwania audio w sekundach
    pub duration_seconds: f32,
    /// Komunikat błędu (null jeśli sukces)
    pub error: *mut c_char,
}

impl SttResult {
    pub fn success(text: String, language: Option<String>, duration: f32) -> Self {
        let text_cstr = CString::new(text).unwrap_or_default();
        let lang_cstr = CString::new(language.unwrap_or_default()).unwrap_or_default();

        Self {
            text: text_cstr.into_raw(),
            language: lang_cstr.into_raw(),
            duration_seconds: duration,
            error: ptr::null_mut(),
        }
    }

    pub fn error(msg: &str) -> Self {
        let c_str = CString::new(msg).unwrap_or_else(|_| CString::new("Unknown error").unwrap());
        Self {
            text: ptr::null_mut(),
            language: ptr::null_mut(),
            duration_seconds: 0.0,
            error: c_str.into_raw(),
        }
    }
}

/// Para klucz-wartość dla metadanych.
#[repr(C)]
pub struct KeyValuePair {
    pub key: *mut c_char,
    pub value: *mut c_char,
}

impl KeyValuePair {
    pub fn new(key: String, value: String) -> Self {
        Self {
            key: CString::new(key).unwrap_or_default().into_raw(),
            value: CString::new(value).unwrap_or_default().into_raw(),
        }
    }
}

/// Dokument zawierający chunk.
#[repr(C)]
pub struct ChunkDocument {
    /// Identyfikator dokumentu
    pub doc_id: *mut c_char,
    /// Metadane dokumentu
    pub metadata: *mut KeyValuePair,
    /// Liczba metadanych
    pub metadata_count: u32,
}

impl ChunkDocument {
    pub fn new(doc_id: String, metadata: Vec<(String, String)>) -> Self {
        let metadata_count = metadata.len() as u32;
        let metadata_vec: Vec<KeyValuePair> = metadata
            .into_iter()
            .map(|(k, v)| KeyValuePair::new(k, v))
            .collect();

        let metadata_ptr = if metadata_vec.is_empty() {
            ptr::null_mut()
        } else {
            let mut boxed = metadata_vec.into_boxed_slice();
            let ptr = boxed.as_mut_ptr();
            std::mem::forget(boxed);
            ptr
        };

        Self {
            doc_id: CString::new(doc_id).unwrap_or_default().into_raw(),
            metadata: metadata_ptr,
            metadata_count,
        }
    }
}

/// Informacje o pojedynczym chunka RAG.
#[repr(C)]
pub struct RagChunkInfo {
    /// Identyfikator chunka
    pub chunk_id: *mut c_char,
    /// Treść chunka
    pub chunk_text: *mut c_char,
    /// Nazwa pliku źródłowego
    pub source_file: *mut c_char,
    /// Typ źródła (pdf, docx, txt, etc.)
    pub source_type: *mut c_char,
    /// Score podobieństwa (0.0-1.0)
    pub similarity_score: f32,
    /// Pozycja w rankingu (1 = najlepszy)
    pub rank: u32,
    /// Indeks chunka w dokumencie
    pub chunk_index: u32,
    /// Lista dokumentów zawierających ten chunk
    pub documents: *mut ChunkDocument,
    /// Liczba dokumentów
    pub documents_count: u32,
}

impl RagChunkInfo {
    pub fn new(
        chunk_id: String,
        chunk_text: String,
        source_file: String,
        source_type: String,
        similarity_score: f32,
        rank: u32,
        chunk_index: u32,
        documents: Vec<ChunkDocument>,
    ) -> Self {
        let documents_count = documents.len() as u32;
        let documents_ptr = if documents.is_empty() {
            ptr::null_mut()
        } else {
            let mut boxed = documents.into_boxed_slice();
            let ptr = boxed.as_mut_ptr();
            std::mem::forget(boxed);
            ptr
        };

        Self {
            chunk_id: CString::new(chunk_id).unwrap_or_default().into_raw(),
            chunk_text: CString::new(chunk_text).unwrap_or_default().into_raw(),
            source_file: CString::new(source_file).unwrap_or_default().into_raw(),
            source_type: CString::new(source_type).unwrap_or_default().into_raw(),
            similarity_score,
            rank,
            chunk_index,
            documents: documents_ptr,
            documents_count,
        }
    }
}

/// Wynik operacji RAG (Retrieval Augmented Generation).
#[repr(C)]
pub struct RagResult {
    /// Odpowiedź (tekst lub context)
    pub response: *mut c_char,
    /// Liczba znalezionych chunków
    pub chunks_found: u32,
    /// Czy wymaga dalszego przetwarzania LLM (0 = false, 1 = true)
    pub requires_llm: u8,
    /// Tablica szczegółowych informacji o chunkach
    pub chunks: *mut RagChunkInfo,
    /// Liczba elementów w tablicy chunks
    pub chunks_count: u32,
    /// Komunikat błędu (null jeśli sukces)
    pub error: *mut c_char,
}

impl RagResult {
    pub fn success(
        response: String,
        chunks_found: u32,
        requires_llm: bool,
        chunks: Vec<RagChunkInfo>,
    ) -> Self {
        let response_cstr = CString::new(response).unwrap_or_default();
        let chunks_count = chunks.len() as u32;

        // Konwertuj Vec na surowy wskaźnik
        let chunks_ptr = if chunks.is_empty() {
            ptr::null_mut()
        } else {
            let mut boxed = chunks.into_boxed_slice();
            let ptr = boxed.as_mut_ptr();
            std::mem::forget(boxed);
            ptr
        };

        Self {
            response: response_cstr.into_raw(),
            chunks_found,
            requires_llm: if requires_llm { 1 } else { 0 },
            chunks: chunks_ptr,
            chunks_count,
            error: ptr::null_mut(),
        }
    }

    pub fn error(msg: &str) -> Self {
        let c_str = CString::new(msg).unwrap_or_else(|_| CString::new("Unknown error").unwrap());
        Self {
            response: ptr::null_mut(),
            chunks_found: 0,
            requires_llm: 0,
            chunks: ptr::null_mut(),
            chunks_count: 0,
            error: c_str.into_raw(),
        }
    }
}

// ============================================================================
// INPUT TYPES
// ============================================================================

/// Wiadomość w konwersacji chat.
#[repr(C)]
pub struct ChatMessage {
    /// Rola: "system", "user", "assistant"
    pub role: *const c_char,
    /// Treść wiadomości
    pub content: *const c_char,
}

/// Konfiguracja klienta (one-way TLS - klient NIE wysyła certyfikatu).
#[repr(C)]
pub struct ClientConfig {
    /// URL routera (np. "quic://localhost:4000")
    pub router_url: *const c_char,
    /// Ścieżka do CA certificate (opcjonalne - może być null, wtedy używa systemowych CA)
    pub ca_path: *const c_char,
    /// Timeout w ms (0 = default 30000)
    pub timeout_ms: u32,
}

/// Wynik operacji ingest (dodawanie dokumentu do RAG).
#[repr(C)]
pub struct IngestResult {
    /// ID dokumentu
    pub document_id: *mut c_char,
    /// Status operacji (0 = Success, 1 = Duplicate, 2 = Error, 3 = Updated)
    pub status: u32,
    /// Liczba chunków utworzonych
    pub chunk_count: u32,
    /// Liczba wektorów utworzonych
    pub vector_count: u32,
    /// Całkowity czas przetwarzania w ms
    pub total_ms: u32,
    /// Komunikat błędu (null jeśli sukces)
    pub error: *mut c_char,
}

impl IngestResult {
    pub fn success(
        document_id: String,
        status: u32,
        chunk_count: u32,
        vector_count: u32,
        total_ms: u32,
    ) -> Self {
        let doc_id_cstr = CString::new(document_id).unwrap_or_default();
        Self {
            document_id: doc_id_cstr.into_raw(),
            status,
            chunk_count,
            vector_count,
            total_ms,
            error: ptr::null_mut(),
        }
    }

    pub fn error(msg: &str) -> Self {
        let c_str = CString::new(msg).unwrap_or_else(|_| CString::new("Unknown error").unwrap());
        Self {
            document_id: ptr::null_mut(),
            status: 2, // Error
            chunk_count: 0,
            vector_count: 0,
            total_ms: 0,
            error: c_str.into_raw(),
        }
    }
}

/// Para klucz-wartość dla metadata.
#[repr(C)]
pub struct MetadataEntry {
    pub key: *const c_char,
    pub value: *const c_char,
}

// ============================================================================
// STT EXTENDED TYPES
// ============================================================================

/// Opcje dla STT (Speech-to-Text) z filtrowaniem halucynacji.
#[repr(C)]
pub struct SttOptions {
    /// Język (ISO-639-1) - może być null
    pub language: *const c_char,
    /// Prompt kontekstowy - może być null
    pub prompt: *const c_char,
    /// Format odpowiedzi: "json", "text", "verbose_json", "srt", "vtt"
    /// Użyj "verbose_json" żeby otrzymać segmenty
    pub response_format: *const c_char,
    /// Temperatura (0.0-1.0), -1.0 = default
    pub temperature: f32,
    /// Granularność timestampów: "segment" lub "word" (tylko dla verbose_json)
    /// może być null
    pub timestamp_granularities: *const c_char,
    /// Próg no_speech_prob do filtrowania halucynacji
    /// Segmenty z no_speech_prob >= threshold zostaną odfiltrowane
    /// -1.0 = wyłączone
    pub no_speech_threshold: f32,
    /// Minimalny avg_logprob dla segmentu
    /// Segmenty z avg_logprob < threshold zostaną odfiltrowane
    pub avg_logprob_threshold: f32,
    /// Maksymalny compression_ratio dla segmentu
    pub compression_ratio_threshold: f32,
}

/// Segment transkrypcji (dla verbose_json).
#[repr(C)]
pub struct SttSegment {
    /// ID segmentu
    pub id: u32,
    /// Czas rozpoczęcia w sekundach
    pub start: f32,
    /// Czas zakończenia w sekundach
    pub end: f32,
    /// Tekst segmentu
    pub text: *mut c_char,
    /// Średnia log probability
    pub avg_logprob: f32,
    /// Prawdopodobieństwo ciszy (no_speech)
    pub no_speech_prob: f32,
    /// Współczynnik kompresji
    pub compression_ratio: f32,
    /// Temperatura użyta
    pub temperature: f32,
    /// Etykieta mówcy z diarization (może być null)
    pub speaker_label: *mut c_char,
    /// Similarity score z bazy mówców (0.0-1.0), -1.0 jeśli niedostępne
    pub speaker_similarity: f32,
    /// Czy mówca został rozpoznany z bazy: 1=tak, 0=nie, -1=niedostępne
    pub is_known_speaker: i8,
}

impl SttSegment {
    pub fn new(
        id: u32,
        start: f32,
        end: f32,
        text: String,
        avg_logprob: f32,
        no_speech_prob: f32,
        compression_ratio: f32,
        temperature: f32,
        speaker_label: Option<String>,
        speaker_similarity: Option<f32>,
        is_known_speaker: Option<bool>,
    ) -> Self {
        Self {
            id,
            start,
            end,
            text: CString::new(text).unwrap_or_default().into_raw(),
            avg_logprob,
            no_speech_prob,
            compression_ratio,
            temperature,
            speaker_label: speaker_label
                .map(|s| CString::new(s).unwrap_or_default().into_raw())
                .unwrap_or(std::ptr::null_mut()),
            speaker_similarity: speaker_similarity.unwrap_or(-1.0),
            is_known_speaker: is_known_speaker.map(|b| if b { 1 } else { 0 }).unwrap_or(-1),
        }
    }
}

/// Wynik operacji STT z segmentami (dla verbose_json i filtrowania).
#[repr(C)]
pub struct SttDetailedResult {
    /// Transkrypcja tekstu (pełna lub przefiltrowana)
    pub text: *mut c_char,
    /// Wykryty język (ISO-639-1)
    pub language: *mut c_char,
    /// Czas trwania audio w sekundach
    pub duration_seconds: f32,
    /// Segmenty transkrypcji (tylko dla verbose_json)
    pub segments: *mut SttSegment,
    /// Liczba segmentów
    pub segments_count: u32,
    /// Liczba segmentów odfiltrowanych (jeśli włączone filtrowanie)
    pub filtered_segments_count: u32,
    /// Całkowita latencja w ms
    pub latency_ms: u64,
    /// Komunikat błędu (null jeśli sukces)
    pub error: *mut c_char,
}

impl SttDetailedResult {
    pub fn success(
        text: String,
        language: Option<String>,
        duration: f32,
        segments: Vec<SttSegment>,
        filtered_count: u32,
        latency_ms: u64,
    ) -> Self {
        let text_cstr = CString::new(text).unwrap_or_default();
        let lang_cstr = CString::new(language.unwrap_or_default()).unwrap_or_default();

        let segments_count = segments.len() as u32;
        let segments_ptr = if segments.is_empty() {
            ptr::null_mut()
        } else {
            let mut boxed = segments.into_boxed_slice();
            let ptr = boxed.as_mut_ptr();
            std::mem::forget(boxed);
            ptr
        };

        Self {
            text: text_cstr.into_raw(),
            language: lang_cstr.into_raw(),
            duration_seconds: duration,
            segments: segments_ptr,
            segments_count,
            filtered_segments_count: filtered_count,
            latency_ms,
            error: ptr::null_mut(),
        }
    }

    pub fn error(msg: &str) -> Self {
        let c_str = CString::new(msg).unwrap_or_else(|_| CString::new("Unknown error").unwrap());
        Self {
            text: ptr::null_mut(),
            language: ptr::null_mut(),
            duration_seconds: 0.0,
            segments: ptr::null_mut(),
            segments_count: 0,
            filtered_segments_count: 0,
            latency_ms: 0,
            error: c_str.into_raw(),
        }
    }
}

// ============================================================================
// SPEAKER IDENTIFICATION TYPES
// ============================================================================

/// Wynik operacji SpeakerEnroll / SpeakerAddSamples.
#[repr(C)]
pub struct SpeakerEnrollResult {
    /// ID mówcy
    pub speaker_id: *mut c_char,
    /// Nazwa mówcy
    pub speaker_name: *mut c_char,
    /// Liczba przetworzonych próbek audio
    pub samples_processed: u32,
    /// Liczba pomyślnie wyekstrahowanych embeddingów
    pub embeddings_added: u32,
    /// Czy to była nowa rejestracja (1) czy aktualizacja (0)
    pub is_new: u8,
    /// Całkowita latencja w ms
    pub latency_ms: u64,
    /// Komunikat błędu (null jeśli sukces)
    pub error: *mut c_char,
}

impl SpeakerEnrollResult {
    pub fn success(
        speaker_id: String,
        speaker_name: String,
        samples_processed: u32,
        embeddings_added: u32,
        is_new: bool,
        latency_ms: u64,
    ) -> Self {
        Self {
            speaker_id: CString::new(speaker_id).unwrap_or_default().into_raw(),
            speaker_name: CString::new(speaker_name).unwrap_or_default().into_raw(),
            samples_processed,
            embeddings_added,
            is_new: if is_new { 1 } else { 0 },
            latency_ms,
            error: ptr::null_mut(),
        }
    }

    pub fn error(msg: &str) -> Self {
        let c_str = CString::new(msg).unwrap_or_else(|_| CString::new("Unknown error").unwrap());
        Self {
            speaker_id: ptr::null_mut(),
            speaker_name: ptr::null_mut(),
            samples_processed: 0,
            embeddings_added: 0,
            is_new: 0,
            latency_ms: 0,
            error: c_str.into_raw(),
        }
    }
}

/// Wynik operacji SpeakerRemove.
#[repr(C)]
pub struct SpeakerRemoveResult {
    /// ID usuniętego mówcy
    pub speaker_id: *mut c_char,
    /// Czy usunięcie się powiodło (1 = true, 0 = false)
    pub success: u8,
    /// Całkowita latencja w ms
    pub latency_ms: u64,
    /// Komunikat błędu (null jeśli sukces)
    pub error: *mut c_char,
}

impl SpeakerRemoveResult {
    pub fn success(speaker_id: String, removed: bool, latency_ms: u64) -> Self {
        Self {
            speaker_id: CString::new(speaker_id).unwrap_or_default().into_raw(),
            success: if removed { 1 } else { 0 },
            latency_ms,
            error: ptr::null_mut(),
        }
    }

    pub fn error(msg: &str) -> Self {
        let c_str = CString::new(msg).unwrap_or_else(|_| CString::new("Unknown error").unwrap());
        Self {
            speaker_id: ptr::null_mut(),
            success: 0,
            latency_ms: 0,
            error: c_str.into_raw(),
        }
    }
}

/// Wpis na liście mówców.
#[repr(C)]
pub struct SpeakerEntry {
    /// ID mówcy
    pub speaker_id: *mut c_char,
    /// Nazwa mówcy
    pub speaker_name: *mut c_char,
}

impl SpeakerEntry {
    pub fn new(id: String, name: String) -> Self {
        Self {
            speaker_id: CString::new(id).unwrap_or_default().into_raw(),
            speaker_name: CString::new(name).unwrap_or_default().into_raw(),
        }
    }
}

/// Wynik operacji SpeakerList.
#[repr(C)]
pub struct SpeakerListResult {
    /// Tablica mówców
    pub speakers: *mut SpeakerEntry,
    /// Liczba mówców
    pub speakers_count: u32,
    /// Całkowita liczba mówców w bazie
    pub total_count: u32,
    /// Całkowita latencja w ms
    pub latency_ms: u64,
    /// Komunikat błędu (null jeśli sukces)
    pub error: *mut c_char,
}

impl SpeakerListResult {
    pub fn success(speakers: Vec<(String, String)>, total_count: u32, latency_ms: u64) -> Self {
        let speakers_count = speakers.len() as u32;
        let entries: Vec<SpeakerEntry> = speakers
            .into_iter()
            .map(|(id, name)| SpeakerEntry::new(id, name))
            .collect();

        let speakers_ptr = if entries.is_empty() {
            ptr::null_mut()
        } else {
            let mut boxed = entries.into_boxed_slice();
            let ptr = boxed.as_mut_ptr();
            std::mem::forget(boxed);
            ptr
        };

        Self {
            speakers: speakers_ptr,
            speakers_count,
            total_count,
            latency_ms,
            error: ptr::null_mut(),
        }
    }

    pub fn error(msg: &str) -> Self {
        let c_str = CString::new(msg).unwrap_or_else(|_| CString::new("Unknown error").unwrap());
        Self {
            speakers: ptr::null_mut(),
            speakers_count: 0,
            total_count: 0,
            latency_ms: 0,
            error: c_str.into_raw(),
        }
    }
}

/// Wynik operacji SpeakerInfo (informacje o bazie głosów).
#[repr(C)]
pub struct SpeakerInfoResult {
    /// Liczba zarejestrowanych mówców
    pub speaker_count: u32,
    /// Wymiar embeddingów (192 dla ECAPA-TDNN)
    pub embedding_dim: u32,
    /// Próg similarity używany w bazie
    pub similarity_threshold: f32,
    /// Całkowita latencja w ms
    pub latency_ms: u64,
    /// Komunikat błędu (null jeśli sukces)
    pub error: *mut c_char,
}

impl SpeakerInfoResult {
    pub fn success(
        speaker_count: u32,
        embedding_dim: u32,
        similarity_threshold: f32,
        latency_ms: u64,
    ) -> Self {
        Self {
            speaker_count,
            embedding_dim,
            similarity_threshold,
            latency_ms,
            error: ptr::null_mut(),
        }
    }

    pub fn error(msg: &str) -> Self {
        let c_str = CString::new(msg).unwrap_or_else(|_| CString::new("Unknown error").unwrap());
        Self {
            speaker_count: 0,
            embedding_dim: 0,
            similarity_threshold: 0.0,
            latency_ms: 0,
            error: c_str.into_raw(),
        }
    }
}

/// Wynik operacji SpeakerIdentify.
#[repr(C)]
pub struct SpeakerIdentifyResult {
    /// Czy rozpoznano mówcę (1 = true, 0 = false)
    pub is_match: u8,
    /// ID rozpoznanego mówcy (null jeśli !is_match)
    pub speaker_id: *mut c_char,
    /// Nazwa rozpoznanego mówcy (null jeśli !is_match)
    pub speaker_name: *mut c_char,
    /// Similarity score (0.0-1.0)
    pub similarity: f32,
    /// Użyty próg similarity
    pub threshold: f32,
    /// Całkowita latencja w ms
    pub latency_ms: u64,
    /// Komunikat błędu (null jeśli sukces)
    pub error: *mut c_char,
}

impl SpeakerIdentifyResult {
    pub fn success(
        is_match: bool,
        speaker_id: Option<String>,
        speaker_name: Option<String>,
        similarity: f32,
        threshold: f32,
        latency_ms: u64,
    ) -> Self {
        Self {
            is_match: if is_match { 1 } else { 0 },
            speaker_id: speaker_id
                .map(|s| CString::new(s).unwrap_or_default().into_raw())
                .unwrap_or(ptr::null_mut()),
            speaker_name: speaker_name
                .map(|s| CString::new(s).unwrap_or_default().into_raw())
                .unwrap_or(ptr::null_mut()),
            similarity,
            threshold,
            latency_ms,
            error: ptr::null_mut(),
        }
    }

    pub fn error(msg: &str) -> Self {
        let c_str = CString::new(msg).unwrap_or_else(|_| CString::new("Unknown error").unwrap());
        Self {
            is_match: 0,
            speaker_id: ptr::null_mut(),
            speaker_name: ptr::null_mut(),
            similarity: 0.0,
            threshold: 0.0,
            latency_ms: 0,
            error: c_str.into_raw(),
        }
    }
}

/// Wynik operacji SpeakerVerify.
#[repr(C)]
pub struct SpeakerVerifyResult {
    /// ID weryfikowanego mówcy
    pub speaker_id: *mut c_char,
    /// Czy weryfikacja pozytywna (1 = true, 0 = false)
    pub is_verified: u8,
    /// Similarity score z docelowym mówcą
    pub similarity: f32,
    /// Użyty próg
    pub threshold: f32,
    /// ID wykrytego mówcy (jeśli inny niż weryfikowany)
    pub detected_speaker_id: *mut c_char,
    /// Całkowita latencja w ms
    pub latency_ms: u64,
    /// Komunikat błędu (null jeśli sukces)
    pub error: *mut c_char,
}

impl SpeakerVerifyResult {
    pub fn success(
        speaker_id: String,
        is_verified: bool,
        similarity: f32,
        threshold: f32,
        detected_speaker_id: Option<String>,
        latency_ms: u64,
    ) -> Self {
        Self {
            speaker_id: CString::new(speaker_id).unwrap_or_default().into_raw(),
            is_verified: if is_verified { 1 } else { 0 },
            similarity,
            threshold,
            detected_speaker_id: detected_speaker_id
                .map(|s| CString::new(s).unwrap_or_default().into_raw())
                .unwrap_or(ptr::null_mut()),
            latency_ms,
            error: ptr::null_mut(),
        }
    }

    pub fn error(msg: &str) -> Self {
        let c_str = CString::new(msg).unwrap_or_else(|_| CString::new("Unknown error").unwrap());
        Self {
            speaker_id: ptr::null_mut(),
            is_verified: 0,
            similarity: 0.0,
            threshold: 0.0,
            detected_speaker_id: ptr::null_mut(),
            latency_ms: 0,
            error: c_str.into_raw(),
        }
    }
}

// ============================================================================
// CONVERSATION SESSION TYPES
// ============================================================================

/// Konfiguracja sesji konwersacji (input).
#[repr(C)]
pub struct ConversationSessionConfig {
    /// Tryb sesji: 0=AlwaysOn, 1=WakeWordTimeout, 2=WakeWordExplicitStop
    pub mode: u8,
    /// ID użytkownika (dla personalizacji)
    pub user_id: *const c_char,
    /// Język rozpoznawania mowy (ISO-639-1)
    pub language: *const c_char,
    /// Model STT do użycia
    pub stt_model: *const c_char,
    /// Lista wake words (rozdzielona przecinkami)
    pub wake_words: *const c_char,
    /// Lista stop phrases (rozdzielona przecinkami)
    pub stop_phrases: *const c_char,
    /// Timeout ciszy w ms (dla WakeWordTimeout), 0=domyślny 30000
    pub silence_timeout_ms: u32,
    /// Bufor audio przed wake word w ms (0=domyślny 2000)
    pub pre_wake_buffer_ms: u32,
}

/// Wynik operacji ConversationStart.
#[repr(C)]
pub struct ConversationStartResult {
    /// ID utworzonej sesji
    pub session_id: *mut c_char,
    /// Aktualny stan: 0=Inactive, 1=Active, 2=Processing, 3=Speaking
    pub state: u8,
    /// Komunikat błędu (null jeśli sukces)
    pub error: *mut c_char,
}

impl ConversationStartResult {
    pub fn success(session_id: String, state: u8) -> Self {
        Self {
            session_id: CString::new(session_id).unwrap_or_default().into_raw(),
            state,
            error: ptr::null_mut(),
        }
    }

    pub fn error(msg: &str) -> Self {
        let c_str = CString::new(msg).unwrap_or_else(|_| CString::new("Unknown error").unwrap());
        Self {
            session_id: ptr::null_mut(),
            state: 0,
            error: c_str.into_raw(),
        }
    }
}

/// Zdarzenie konwersacji (output).
#[repr(C)]
pub struct ConversationEvent {
    /// Typ zdarzenia: 0=SessionStarted, 1=WakeWordDetected, 2=TranscriptionAvailable,
    /// 3=SilenceTimeout, 4=StopPhraseDetected, 5=SessionEnded, 6=UserChanged
    pub event_type: u8,
    /// Timestamp zdarzenia w ms
    pub timestamp_ms: u64,
    /// Transkrypcja (dla TranscriptionAvailable)
    pub transcription: *mut c_char,
    /// Pewność transkrypcji (0.0-1.0)
    pub confidence: f32,
    /// Wykryty wake word
    pub wake_word: *mut c_char,
    /// Wykryty stop phrase
    pub stop_phrase: *mut c_char,
    /// ID użytkownika (dla UserChanged)
    pub user_id: *mut c_char,
}

impl Default for ConversationEvent {
    fn default() -> Self {
        Self {
            event_type: 0,
            timestamp_ms: 0,
            transcription: ptr::null_mut(),
            confidence: 0.0,
            wake_word: ptr::null_mut(),
            stop_phrase: ptr::null_mut(),
            user_id: ptr::null_mut(),
        }
    }
}

/// Wynik operacji ConversationAudio.
#[repr(C)]
pub struct ConversationAudioResult {
    /// ID sesji
    pub session_id: *mut c_char,
    /// Aktualny stan sesji
    pub state: u8,
    /// Wskaźnik do tablicy zdarzeń
    pub events: *mut ConversationEvent,
    /// Liczba zdarzeń
    pub events_count: u32,
    /// Transkrypcja (jeśli dostępna)
    pub transcription: *mut c_char,
    /// Pewność transkrypcji
    pub confidence: f32,
    /// Komunikat błędu (null jeśli sukces)
    pub error: *mut c_char,
}

impl ConversationAudioResult {
    pub fn success(
        session_id: String,
        state: u8,
        events: Vec<ConversationEvent>,
        transcription: Option<String>,
        confidence: f32,
    ) -> Self {
        let events_count = events.len() as u32;
        let events_ptr = if events.is_empty() {
            ptr::null_mut()
        } else {
            let mut boxed = events.into_boxed_slice();
            let ptr = boxed.as_mut_ptr();
            std::mem::forget(boxed);
            ptr
        };

        Self {
            session_id: CString::new(session_id).unwrap_or_default().into_raw(),
            state,
            events: events_ptr,
            events_count,
            transcription: transcription
                .map(|s| CString::new(s).unwrap_or_default().into_raw())
                .unwrap_or(ptr::null_mut()),
            confidence,
            error: ptr::null_mut(),
        }
    }

    pub fn error(msg: &str) -> Self {
        let c_str = CString::new(msg).unwrap_or_else(|_| CString::new("Unknown error").unwrap());
        Self {
            session_id: ptr::null_mut(),
            state: 0,
            events: ptr::null_mut(),
            events_count: 0,
            transcription: ptr::null_mut(),
            confidence: 0.0,
            error: c_str.into_raw(),
        }
    }
}

/// Statystyki sesji konwersacji.
#[repr(C)]
pub struct ConversationSessionStats {
    /// Całkowity czas trwania sesji w ms
    pub total_duration_ms: u64,
    /// Czas aktywnego mówienia w ms
    pub active_speech_ms: u64,
    /// Liczba wykrytych wake words
    pub wake_words_detected: u32,
    /// Liczba transkrypcji
    pub transcriptions_count: u32,
    /// Liczba wykrytych mówców
    pub speakers_detected: u32,
}

impl Default for ConversationSessionStats {
    fn default() -> Self {
        Self {
            total_duration_ms: 0,
            active_speech_ms: 0,
            wake_words_detected: 0,
            transcriptions_count: 0,
            speakers_detected: 0,
        }
    }
}

/// Wynik operacji ConversationEnd.
#[repr(C)]
pub struct ConversationEndResult {
    /// ID zakończonej sesji
    pub session_id: *mut c_char,
    /// Pełna transkrypcja sesji
    pub final_transcription: *mut c_char,
    /// Statystyki sesji
    pub stats: ConversationSessionStats,
    /// Komunikat błędu (null jeśli sukces)
    pub error: *mut c_char,
}

impl ConversationEndResult {
    pub fn success(
        session_id: String,
        final_transcription: Option<String>,
        stats: ConversationSessionStats,
    ) -> Self {
        Self {
            session_id: CString::new(session_id).unwrap_or_default().into_raw(),
            final_transcription: final_transcription
                .map(|s| CString::new(s).unwrap_or_default().into_raw())
                .unwrap_or(ptr::null_mut()),
            stats,
            error: ptr::null_mut(),
        }
    }

    pub fn error(msg: &str) -> Self {
        let c_str = CString::new(msg).unwrap_or_else(|_| CString::new("Unknown error").unwrap());
        Self {
            session_id: ptr::null_mut(),
            final_transcription: ptr::null_mut(),
            stats: ConversationSessionStats::default(),
            error: c_str.into_raw(),
        }
    }
}

/// Wynik operacji ConversationStatus.
#[repr(C)]
pub struct ConversationStatusResult {
    /// ID sesji
    pub session_id: *mut c_char,
    /// Czy sesja istnieje (1 = true, 0 = false)
    pub exists: u8,
    /// Aktualny stan: 0=Inactive, 1=Active, 2=Processing, 3=Speaking
    pub state: u8,
    /// Tryb sesji: 0=AlwaysOn, 1=WakeWordTimeout, 2=WakeWordExplicitStop
    pub mode: u8,
    /// Czas trwania sesji w ms
    pub duration_ms: u64,
    /// Czas od ostatniej aktywności w ms
    pub last_activity_ms: u64,
    /// Komunikat błędu (null jeśli sukces)
    pub error: *mut c_char,
}

impl ConversationStatusResult {
    pub fn success(
        session_id: String,
        exists: bool,
        state: u8,
        mode: u8,
        duration_ms: u64,
        last_activity_ms: u64,
    ) -> Self {
        Self {
            session_id: CString::new(session_id).unwrap_or_default().into_raw(),
            exists: if exists { 1 } else { 0 },
            state,
            mode,
            duration_ms,
            last_activity_ms,
            error: ptr::null_mut(),
        }
    }

    pub fn error(msg: &str) -> Self {
        let c_str = CString::new(msg).unwrap_or_else(|_| CString::new("Unknown error").unwrap());
        Self {
            session_id: ptr::null_mut(),
            exists: 0,
            state: 0,
            mode: 0,
            duration_ms: 0,
            last_activity_ms: 0,
            error: c_str.into_raw(),
        }
    }
}
