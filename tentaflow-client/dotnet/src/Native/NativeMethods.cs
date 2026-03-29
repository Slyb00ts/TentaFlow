// ============================================================================
// P/INVOKE BINDINGS - Deklaracje natywnych metod z tentaflow_client_native
// ============================================================================
//
// CEL:
// Definiuje sygnatury P/Invoke dla wywołań natywnej biblioteki Rust.
// Każda metoda odpowiada funkcji extern "C" w bibliotece.
//
// JAK DZIAŁA:
// 1. LibraryImport generuje kod marshallingu w czasie kompilacji (source generator)
// 2. Runtime ładuje tentaflow_client_native.so/.dll/.dylib
// 3. Wywołania .NET są tłumaczone na wywołania C ABI
// 4. Wyniki są konwertowane z powrotem na typy managed
//
// KLUCZOWE KONCEPCJE:
// - LibraryImport: Source generator dla P/Invoke (nowsze niż DllImport)
// - StructLayout.Sequential: Gwarantuje layout pamięci zgodny z #[repr(C)]
// - StringMarshalling.Utf8: Automatyczna konwersja string ↔ null-terminated UTF-8
// - CallingConvention.Cdecl: Domyślna konwencja wywołań dla Rust
//
// STRUKTURY NATIVE:
// - *Native: Struktury C-compatible dla marshallingu
// - Layout musi dokładnie odpowiadać strukturom Rust w types.rs
// - Kolejność pól jest krytyczna!
//
// BEZPIECZEŃSTWO PAMIĘCI:
// - Pamięć alokowana przez Rust musi być zwolniona przez tentaflow_free_*
// - Nigdy nie używaj Marshal.FreeHGlobal na pamięci z Rust!
//
// ============================================================================

using System.Runtime.InteropServices;

namespace TentaFlow.Client.Native;

/// <summary>
/// Deklaracje P/Invoke dla natywnej biblioteki Rust.
/// Wszystkie metody są thread-safe dzięki wewnętrznej synchronizacji w Rust.
/// </summary>
internal static partial class NativeMethods
{
    private const string LibraryName = "tentaflow_client_native";

    // =========================================================================
    // INICJALIZACJA
    // =========================================================================

    /// <summary>
    /// Inicjalizuje natywną bibliotekę (wywoływane automatycznie przy pierwszym użyciu).
    /// </summary>
    [LibraryImport(LibraryName)]
    internal static partial void tentaflow_init();

    // =========================================================================
    // ZARZĄDZANIE KLIENTEM
    // =========================================================================

    /// <summary>
    /// Tworzy nowego klienta i łączy się z Router.
    /// </summary>
    [LibraryImport(LibraryName)]
    internal static partial nint tentaflow_client_new(ref ClientConfigNative config);

    /// <summary>
    /// Zamyka klienta i zwalnia pamięć.
    /// </summary>
    [LibraryImport(LibraryName)]
    internal static partial void tentaflow_client_free(nint client);

    /// <summary>
    /// Sprawdza czy klient jest połączony.
    /// </summary>
    [LibraryImport(LibraryName)]
    [return: MarshalAs(UnmanagedType.Bool)]
    internal static partial bool tentaflow_client_is_connected(nint client);

    // =========================================================================
    // EMBEDDINGS
    // =========================================================================

    /// <summary>
    /// Generuje embeddings dla podanych tekstów.
    /// </summary>
    [LibraryImport(LibraryName, StringMarshalling = StringMarshalling.Utf8)]
    internal static partial EmbeddingsResultNative tentaflow_embeddings(
        nint client,
        string model,
        nint texts,
        nuint textsCount);

    /// <summary>
    /// Zwalnia pamięć wyniku embeddings.
    /// </summary>
    [LibraryImport(LibraryName)]
    internal static partial void tentaflow_free_embeddings(EmbeddingsResultNative result);

    // =========================================================================
    // CHAT COMPLETION (unified API)
    // =========================================================================

    /// <summary>
    /// Delegat callback dla streaming tokenów.
    /// Wywoływany dla każdego tokena podczas streaming completion.
    /// </summary>
    /// <param name="token">Wskaźnik do null-terminated UTF-8 string tokena (ważny tylko podczas callback).</param>
    [UnmanagedFunctionPointer(CallingConvention.Cdecl)]
    internal delegate void StreamingTokenDelegate(nint token);

    /// <summary>
    /// Delegat callback dla zdarzeń streaming (start/end).
    /// Wywoływany przy rozpoczęciu/zakończeniu fazy reasoning lub content.
    /// </summary>
    [UnmanagedFunctionPointer(CallingConvention.Cdecl)]
    internal delegate void StreamingEventDelegate();

    /// <summary>
    /// Delegat callback dla audio chunks.
    /// Wywoływany dla każdego audio chunk podczas streaming TTS.
    /// </summary>
    /// <param name="audioData">Wskaźnik do danych audio.</param>
    /// <param name="audioLen">Długość danych audio w bajtach.</param>
    [UnmanagedFunctionPointer(CallingConvention.Cdecl)]
    internal delegate void StreamingAudioDelegate(nint audioData, nuint audioLen);

    /// <summary>
    /// Uniwersalna funkcja chat completion - obsługuje streaming, TTS i Memory.
    /// </summary>
    [LibraryImport(LibraryName, StringMarshalling = StringMarshalling.Utf8)]
    internal static partial ChatCompletionResultNative tentaflow_chat_completion(
        nint client,
        string model,
        nint messages,
        nuint messagesCount,
        nint options,
        StreamingEventDelegate? onReasoningStart,
        StreamingTokenDelegate? onReasoning,
        StreamingEventDelegate? onReasoningEnd,
        StreamingEventDelegate? onContentStart,
        StreamingTokenDelegate? onContent,
        StreamingEventDelegate? onContentEnd,
        StreamingAudioDelegate? onAudio,
        nint requestIdOut);

    /// <summary>
    /// Zwalnia pamięć wyniku chat completion.
    /// </summary>
    [LibraryImport(LibraryName)]
    internal static partial void tentaflow_free_chat_completion(ChatCompletionResultNative result);

    // =========================================================================
    // REQUEST CANCELLATION
    // =========================================================================

    /// <summary>
    /// Anuluje trwający request.
    /// </summary>
    [LibraryImport(LibraryName, StringMarshalling = StringMarshalling.Utf8)]
    internal static partial CancelResultNative tentaflow_cancel_request(
        nint client,
        string requestId,
        string? reason);

    /// <summary>
    /// Zwalnia pamięć wyniku cancellation.
    /// </summary>
    [LibraryImport(LibraryName)]
    internal static partial void tentaflow_free_cancel(CancelResultNative result);

    // =========================================================================
    // TTS (Text-to-Speech)
    // =========================================================================

    /// <summary>
    /// Generuje audio z tekstu.
    /// </summary>
    [LibraryImport(LibraryName, StringMarshalling = StringMarshalling.Utf8)]
    internal static partial TtsResultNative tentaflow_tts(
        nint client,
        string model,
        string text,
        string voice,
        string? format);

    /// <summary>
    /// Zwalnia pamięć wyniku TTS.
    /// </summary>
    [LibraryImport(LibraryName)]
    internal static partial void tentaflow_free_tts(TtsResultNative result);

    // =========================================================================
    // STT (Speech-to-Text)
    // =========================================================================

    /// <summary>
    /// Transkrybuje audio na tekst.
    /// </summary>
    [LibraryImport(LibraryName, StringMarshalling = StringMarshalling.Utf8)]
    internal static partial SttResultNative tentaflow_stt(
        nint client,
        string model,
        nint audioData,
        nuint audioLen,
        string? language);

    /// <summary>
    /// Zwalnia pamięć wyniku STT.
    /// </summary>
    [LibraryImport(LibraryName)]
    internal static partial void tentaflow_free_stt(SttResultNative result);

    /// <summary>
    /// Transkrybuje audio na tekst z pełnymi opcjami.
    /// </summary>
    [LibraryImport(LibraryName, StringMarshalling = StringMarshalling.Utf8)]
    internal static partial SttDetailedResultNative tentaflow_stt_with_options(
        nint client,
        string model,
        nint audioData,
        nuint audioLen,
        SttOptionsNative options);

    /// <summary>
    /// Zwalnia pamięć szczegółowego wyniku STT.
    /// </summary>
    [LibraryImport(LibraryName)]
    internal static partial void tentaflow_free_stt_detailed(SttDetailedResultNative result);

    // =========================================================================
    // SPEAKER IDENTIFICATION
    // =========================================================================

    /// <summary>
    /// Rejestruje nowego mówcę lub dodaje próbki do istniejącego.
    /// </summary>
    [LibraryImport(LibraryName, StringMarshalling = StringMarshalling.Utf8)]
    internal static partial SpeakerEnrollResultNative tentaflow_speaker_enroll(
        nint client,
        string speakerId,
        string speakerName,
        nint audioSamples,
        nint sampleLengths,
        nuint samplesCount);

    /// <summary>
    /// Zwalnia pamięć wyniku SpeakerEnroll.
    /// </summary>
    [LibraryImport(LibraryName)]
    internal static partial void tentaflow_free_speaker_enroll(SpeakerEnrollResultNative result);

    /// <summary>
    /// Dodaje próbki audio do istniejącego mówcy.
    /// </summary>
    [LibraryImport(LibraryName, StringMarshalling = StringMarshalling.Utf8)]
    internal static partial SpeakerEnrollResultNative tentaflow_speaker_add_samples(
        nint client,
        string speakerId,
        nint audioSamples,
        nint sampleLengths,
        nuint samplesCount);

    /// <summary>
    /// Usuwa mówcę z bazy głosów.
    /// </summary>
    [LibraryImport(LibraryName, StringMarshalling = StringMarshalling.Utf8)]
    internal static partial SpeakerRemoveResultNative tentaflow_speaker_remove(
        nint client,
        string speakerId);

    /// <summary>
    /// Zwalnia pamięć wyniku SpeakerRemove.
    /// </summary>
    [LibraryImport(LibraryName)]
    internal static partial void tentaflow_free_speaker_remove(SpeakerRemoveResultNative result);

    /// <summary>
    /// Pobiera listę wszystkich mówców.
    /// </summary>
    [LibraryImport(LibraryName)]
    internal static partial SpeakerListResultNative tentaflow_speaker_list(nint client);

    /// <summary>
    /// Zwalnia pamięć wyniku SpeakerList.
    /// </summary>
    [LibraryImport(LibraryName)]
    internal static partial void tentaflow_free_speaker_list(SpeakerListResultNative result);

    /// <summary>
    /// Pobiera informacje o bazie głosów.
    /// </summary>
    [LibraryImport(LibraryName)]
    internal static partial SpeakerInfoResultNative tentaflow_speaker_info(nint client);

    /// <summary>
    /// Zwalnia pamięć wyniku SpeakerInfo.
    /// </summary>
    [LibraryImport(LibraryName)]
    internal static partial void tentaflow_free_speaker_info(SpeakerInfoResultNative result);

    /// <summary>
    /// Identyfikuje mówcę na podstawie próbki audio.
    /// threshold: próg similarity (-1.0 = domyślny)
    /// </summary>
    [LibraryImport(LibraryName)]
    internal static partial SpeakerIdentifyResultNative tentaflow_speaker_identify(
        nint client,
        nint audioData,
        nuint audioLen,
        float threshold);

    /// <summary>
    /// Zwalnia pamięć wyniku SpeakerIdentify.
    /// </summary>
    [LibraryImport(LibraryName)]
    internal static partial void tentaflow_free_speaker_identify(SpeakerIdentifyResultNative result);

    /// <summary>
    /// Weryfikuje czy próbka audio należy do konkretnego mówcy.
    /// threshold: próg similarity (-1.0 = domyślny)
    /// </summary>
    [LibraryImport(LibraryName, StringMarshalling = StringMarshalling.Utf8)]
    internal static partial SpeakerVerifyResultNative tentaflow_speaker_verify(
        nint client,
        string speakerId,
        nint audioData,
        nuint audioLen,
        float threshold);

    /// <summary>
    /// Zwalnia pamięć wyniku SpeakerVerify.
    /// </summary>
    [LibraryImport(LibraryName)]
    internal static partial void tentaflow_free_speaker_verify(SpeakerVerifyResultNative result);

    // =========================================================================
    // RAG
    // =========================================================================

    /// <summary>
    /// Wysyła zapytanie RAG z pełną kontrolą parametrów.
    /// searchModesFlags: 0x01=FTS, 0x02=Vector, 0x04=HiRAG, 0x08=GSW, 0=domyślnie Vector
    /// useReranking/requiresLlm/requiresAudio: -1=None, 0=false, 1=true
    /// </summary>
    [LibraryImport(LibraryName, StringMarshalling = StringMarshalling.Utf8)]
    internal static partial RagResultNative tentaflow_rag(
        nint client,
        string query,
        uint topK,
        float minSimilarity,
        uint searchModesFlags,
        int useReranking,
        int requiresLlm,
        int requiresAudio);

    /// <summary>
    /// Zwalnia pamięć wyniku RAG.
    /// </summary>
    [LibraryImport(LibraryName)]
    internal static partial void tentaflow_free_rag(RagResultNative result);

    // =========================================================================
    // INGEST (Indeksowanie dokumentów)
    // =========================================================================

    /// <summary>
    /// Dodaje dokument tekstowy do RAG.
    /// </summary>
    [LibraryImport(LibraryName, StringMarshalling = StringMarshalling.Utf8)]
    internal static partial IngestResultNative tentaflow_ingest_text(
        nint client,
        string documentId,
        string text,
        nint metadata,
        nuint metadataCount);

    /// <summary>
    /// Dodaje plik do RAG.
    /// </summary>
    [LibraryImport(LibraryName, StringMarshalling = StringMarshalling.Utf8)]
    internal static partial IngestResultNative tentaflow_ingest_file(
        nint client,
        string documentId,
        string filename,
        nint data,
        nuint dataLen,
        nint metadata,
        nuint metadataCount);

    /// <summary>
    /// Zwalnia pamięć wyniku ingest.
    /// </summary>
    [LibraryImport(LibraryName)]
    internal static partial void tentaflow_free_ingest(IngestResultNative result);

    // =========================================================================
    // CONVERSATION SESSIONS
    // =========================================================================

    /// <summary>
    /// Rozpoczyna nową sesję konwersacji głosowej.
    /// </summary>
    [LibraryImport(LibraryName, StringMarshalling = StringMarshalling.Utf8)]
    internal static partial ConversationStartResultNative tentaflow_conversation_start(
        nint client,
        ref ConversationSessionConfigNative config);

    /// <summary>
    /// Zwalnia pamięć wyniku ConversationStart.
    /// </summary>
    [LibraryImport(LibraryName)]
    internal static partial void tentaflow_free_conversation_start(ConversationStartResultNative result);

    /// <summary>
    /// Wysyła audio do aktywnej sesji konwersacji.
    /// </summary>
    [LibraryImport(LibraryName, StringMarshalling = StringMarshalling.Utf8)]
    internal static partial ConversationAudioResultNative tentaflow_conversation_audio(
        nint client,
        string sessionId,
        nint audioData,
        nuint audioLen,
        ulong timestampMs);

    /// <summary>
    /// Zwalnia pamięć wyniku ConversationAudio.
    /// </summary>
    [LibraryImport(LibraryName)]
    internal static partial void tentaflow_free_conversation_audio(ConversationAudioResultNative result);

    /// <summary>
    /// Kończy sesję konwersacji.
    /// </summary>
    [LibraryImport(LibraryName, StringMarshalling = StringMarshalling.Utf8)]
    internal static partial ConversationEndResultNative tentaflow_conversation_end(
        nint client,
        string sessionId,
        string? reason);

    /// <summary>
    /// Zwalnia pamięć wyniku ConversationEnd.
    /// </summary>
    [LibraryImport(LibraryName)]
    internal static partial void tentaflow_free_conversation_end(ConversationEndResultNative result);

    /// <summary>
    /// Pobiera status sesji konwersacji.
    /// </summary>
    [LibraryImport(LibraryName, StringMarshalling = StringMarshalling.Utf8)]
    internal static partial ConversationStatusResultNative tentaflow_conversation_status(
        nint client,
        string sessionId);

    /// <summary>
    /// Zwalnia pamięć wyniku ConversationStatus.
    /// </summary>
    [LibraryImport(LibraryName)]
    internal static partial void tentaflow_free_conversation_status(ConversationStatusResultNative result);

    // =========================================================================
    // NARZĘDZIA
    // =========================================================================

    /// <summary>
    /// Zwalnia string alokowany przez bibliotekę.
    /// </summary>
    [LibraryImport(LibraryName)]
    internal static partial void tentaflow_free_string(nint str);

    /// <summary>
    /// Zwraca wersję biblioteki.
    /// </summary>
    [LibraryImport(LibraryName)]
    internal static partial nint tentaflow_version();
}

// ============================================================================
// STRUKTURY NATIVE
// ============================================================================

/// <summary>
/// Konfiguracja klienta (layout C-compatible, one-way TLS).
/// </summary>
[StructLayout(LayoutKind.Sequential)]
internal struct ClientConfigNative
{
    public nint RouterUrl;
    public nint CaPath; // opcjonalne - może być IntPtr.Zero
    public uint TimeoutMs;
}

/// <summary>
/// Wiadomość chat dla completion API.
/// </summary>
[StructLayout(LayoutKind.Sequential)]
internal struct ChatMessageNative
{
    public nint Role;
    public nint Content;
}

/// <summary>
/// Wpis metadanych dla indeksowania dokumentów.
/// </summary>
[StructLayout(LayoutKind.Sequential)]
internal struct MetadataEntryNative
{
    public nint Key;
    public nint Value;
}

/// <summary>
/// Wynik embeddings (layout C-compatible).
/// </summary>
[StructLayout(LayoutKind.Sequential)]
internal struct EmbeddingsResultNative
{
    public nint Embeddings;
    public nuint EmbeddingsCount;
    public nuint Dimensions;
    /// <summary>Całkowita latencja w ms</summary>
    public ulong LatencyMs;
    public nint Error;
}

/// <summary>
/// Wynik chat completion (layout C-compatible).
/// Zawiera metryki streamingu (TTFT, latency, tokens/sec).
/// </summary>
[StructLayout(LayoutKind.Sequential)]
internal struct ChatCompletionResultNative
{
    public nint Content;
    /// <summary>Chain-of-thought reasoning (dla modeli jak DeepSeek R1, OpenAI o1). NULL jeśli niedostępny.</summary>
    public nint ReasoningContent;
    public nint Model;
    public nint FinishReason;
    public uint PromptTokens;
    public uint CompletionTokens;
    public uint TotalTokens;
    /// <summary>Czas do pierwszego tokena w ms (tylko streaming, 0 dla non-streaming)</summary>
    public ulong TimeToFirstTokenMs;
    /// <summary>Całkowita latencja w ms</summary>
    public ulong LatencyMs;
    /// <summary>Tokeny na sekundę (0 jeśli niedostępne)</summary>
    public float TokensPerSec;
    /// <summary>Transkrybowany tekst z audio input (null jeśli brak audio input)</summary>
    public nint TranscribedText;
    /// <summary>ID rozpoznanego mówcy (null jeśli nie rozpoznano)</summary>
    public nint SpeakerId;
    /// <summary>Nazwa rozpoznanego mówcy (null jeśli nie rozpoznano)</summary>
    public nint SpeakerName;
    /// <summary>Wykryty intent z Intent Analyzer (null jeśli brak)</summary>
    public nint DetectedIntent;
    /// <summary>Wykryte wywołania narzędzi z Intent Analyzer (tablica)</summary>
    public nint DetectedTools;
    /// <summary>Liczba wykrytych narzędzi</summary>
    public uint DetectedToolsCount;
    public nint Error;
}

/// <summary>
/// Wynik wykonania narzędzia (layout C-compatible).
/// </summary>
[StructLayout(LayoutKind.Sequential)]
internal struct DetectedToolExecutionResultNative
{
    /// <summary>Czy wykonanie się powiodło (1 = true, 0 = false)</summary>
    public byte Success;
    public nint Message;
    public nint Data;
    public nint Error;
}

/// <summary>
/// Wykryte wywołanie narzędzia z Intent Analyzer (layout C-compatible).
/// </summary>
[StructLayout(LayoutKind.Sequential)]
internal struct DetectedToolCallNative
{
    public nint CallId;
    public nint ToolName;
    public nint Parameters;
    /// <summary>Czy wywołanie jest kompletne (1 = true, 0 = false)</summary>
    public byte IsComplete;
    public nint MissingParams;
    public uint MissingParamsCount;
    public nint ExecutionResult;
    public nint FollowUpQuestion;
}

/// <summary>
/// Wynik TTS (layout C-compatible).
/// </summary>
[StructLayout(LayoutKind.Sequential)]
internal struct TtsResultNative
{
    public nint AudioData;
    public nuint AudioLen;
    public nint Format;
    /// <summary>Całkowita latencja w ms</summary>
    public ulong LatencyMs;
    /// <summary>Czas trwania audio w sekundach</summary>
    public float AudioDurationSec;
    public nint Error;
}

/// <summary>
/// Wynik STT (layout C-compatible).
/// </summary>
[StructLayout(LayoutKind.Sequential)]
internal struct SttResultNative
{
    public nint Text;
    public nint Language;
    public float DurationSeconds;
    public nint Error;
}

/// <summary>
/// Dokument zawierający chunk (layout C-compatible).
/// </summary>
[StructLayout(LayoutKind.Sequential)]
internal struct ChunkDocumentNative
{
    public nint DocId;
    public nint Metadata;       // Wskaźnik do tablicy KeyValuePairNative
    public uint MetadataCount;
}

/// <summary>
/// Para klucz-wartość dla metadanych (layout C-compatible).
/// </summary>
[StructLayout(LayoutKind.Sequential)]
internal struct KeyValuePairNative
{
    public nint Key;
    public nint Value;
}

/// <summary>
/// Informacje o pojedynczym chunka RAG (layout C-compatible).
/// </summary>
[StructLayout(LayoutKind.Sequential)]
internal struct RagChunkInfoNative
{
    public nint ChunkId;
    public nint ChunkText;
    public nint SourceFile;
    public nint SourceType;
    public float SimilarityScore;
    public uint Rank;
    public uint ChunkIndex;
    public nint Documents;      // Wskaźnik do tablicy ChunkDocumentNative
    public uint DocumentsCount;
}

/// <summary>
/// Wynik RAG (layout C-compatible).
/// Uwaga: RequiresLlm przechowywany jako byte (0 = false, 1 = true) dla uniknięcia problemów z marshallingiem.
/// </summary>
[StructLayout(LayoutKind.Sequential)]
internal struct RagResultNative
{
    public nint Response;
    public uint ChunksFound;
    public byte RequiresLlmByte;
    public nint Chunks;        // Wskaźnik do tablicy RagChunkInfoNative
    public uint ChunksCount;   // Liczba elementów w tablicy
    public nint Error;

    public bool RequiresLlm => RequiresLlmByte != 0;
}

/// <summary>
/// Wynik ingest (layout C-compatible).
/// </summary>
[StructLayout(LayoutKind.Sequential)]
internal struct IngestResultNative
{
    public nint DocumentId;
    public uint Status;
    public uint ChunkCount;
    public uint VectorCount;
    public uint TotalMs;
    public nint Error;
}

/// <summary>
/// Opcje TTS (layout C-compatible).
/// </summary>
[StructLayout(LayoutKind.Sequential)]
internal struct TtsOptionsNative
{
    public nint Model;
    public nint Voice;
    public nint Format;
    public float Speed;
}

/// <summary>
/// Opcje Memory (layout C-compatible).
/// </summary>
[StructLayout(LayoutKind.Sequential)]
internal struct MemoryOptionsNative
{
    /// <summary>Czy włączone (1=true, 0=false, -1=default)</summary>
    public sbyte Enabled;
    /// <summary>ID sesji (może być null)</summary>
    public nint SessionId;
    /// <summary>ID osoby (może być null)</summary>
    public nint PersonId;
    /// <summary>Pewność rozpoznania głosu (0.0-1.0, &lt;0=default)</summary>
    public float SpeakerConfidence;
    /// <summary>Czy zapisywać do Memory (1=true, 0=false, -1=default)</summary>
    public sbyte StoreEnabled;
    /// <summary>Czy odpytywać Memory (1=true, 0=false, -1=default)</summary>
    public sbyte QueryEnabled;
}

/// <summary>
/// Opcje chat completion (layout C-compatible).
/// </summary>
[StructLayout(LayoutKind.Sequential)]
internal struct ChatCompletionOptionsNative
{
    /// <summary>Temperatura (0.0-2.0, &lt;0 dla default)</summary>
    public float Temperature;
    /// <summary>Max tokenów (&lt;0 dla default)</summary>
    public int MaxTokens;
    /// <summary>Typ template (0=Auto, 1=Llama3, etc.)</summary>
    public int TemplateType;
    /// <summary>Czy streamować (1=true, 0=false)</summary>
    public byte Stream;
    /// <summary>Opcje TTS (może być null)</summary>
    public nint TtsOptions;
    /// <summary>Opcje Memory (może być null)</summary>
    public nint MemoryOptions;
    /// <summary>ID sesji (może być null)</summary>
    public nint SessionId;
    /// <summary>Wskaźnik do danych audio wejściowych (może być null)</summary>
    public nint AudioInput;
    /// <summary>Długość danych audio wejściowych w bajtach</summary>
    public nuint AudioInputLen;
}

// Zachowujemy alias dla kompatybilności wstecznej
/// <summary>
/// Alias dla TtsOptionsNative (kompatybilność wsteczna).
/// </summary>
[StructLayout(LayoutKind.Sequential)]
internal struct TtsStreamingOptionsNative
{
    public nint Model;
    public nint Voice;
    public nint Format;
    public float Speed;
}

/// <summary>
/// Wynik anulowania requestu (layout C-compatible).
/// </summary>
[StructLayout(LayoutKind.Sequential)]
internal struct CancelResultNative
{
    public byte SuccessByte;
    public nint Error;

    public bool Success => SuccessByte != 0;
}

/// <summary>
/// Opcje STT z filtrowaniem halucynacji (layout C-compatible).
/// Wartości ujemne oznaczają "wyłączone" dla progów filtrowania.
/// </summary>
[StructLayout(LayoutKind.Sequential)]
internal struct SttOptionsNative
{
    /// <summary>Język (ISO-639-1) - może być IntPtr.Zero</summary>
    public nint Language;
    /// <summary>Prompt kontekstowy - może być IntPtr.Zero</summary>
    public nint Prompt;
    /// <summary>Format odpowiedzi: "json", "text", "verbose_json", "srt", "vtt" - może być IntPtr.Zero</summary>
    public nint ResponseFormat;
    /// <summary>Temperatura (0.0-1.0), -1.0 = default</summary>
    public float Temperature;
    /// <summary>Granularność timestampów: "segment" lub "word" - może być IntPtr.Zero</summary>
    public nint TimestampGranularities;
    /// <summary>Próg no_speech_prob do filtrowania halucynacji (-1.0 = wyłączone)</summary>
    public float NoSpeechThreshold;
    /// <summary>Minimalny avg_logprob dla segmentu (-1000.0 = wyłączone)</summary>
    public float AvgLogprobThreshold;
    /// <summary>Maksymalny compression_ratio dla segmentu (-1.0 = wyłączone)</summary>
    public float CompressionRatioThreshold;
}

/// <summary>
/// Segment transkrypcji z metrykami jakości (layout C-compatible).
/// </summary>
[StructLayout(LayoutKind.Sequential)]
internal struct SttSegmentNative
{
    /// <summary>ID segmentu</summary>
    public uint Id;
    /// <summary>Czas rozpoczęcia w sekundach</summary>
    public float Start;
    /// <summary>Czas zakończenia w sekundach</summary>
    public float End;
    /// <summary>Tekst segmentu</summary>
    public nint Text;
    /// <summary>Średnia log probability</summary>
    public float AvgLogprob;
    /// <summary>Prawdopodobieństwo ciszy (no_speech)</summary>
    public float NoSpeechProb;
    /// <summary>Współczynnik kompresji</summary>
    public float CompressionRatio;
    /// <summary>Temperatura użyta</summary>
    public float Temperature;
    /// <summary>Etykieta mówcy z diarization (może być IntPtr.Zero)</summary>
    public nint SpeakerLabel;
    /// <summary>Similarity score z bazy mówców (0.0-1.0), -1.0 jeśli niedostępne</summary>
    public float SpeakerSimilarity;
    /// <summary>Czy mówca został rozpoznany z bazy: 1=tak, 0=nie, -1=niedostępne</summary>
    public sbyte IsKnownSpeaker;
}

/// <summary>
/// Szczegółowy wynik STT z segmentami (layout C-compatible).
/// </summary>
[StructLayout(LayoutKind.Sequential)]
internal struct SttDetailedResultNative
{
    /// <summary>Transkrypcja tekstu (pełna lub przefiltrowana)</summary>
    public nint Text;
    /// <summary>Wykryty język (ISO-639-1)</summary>
    public nint Language;
    /// <summary>Czas trwania audio w sekundach</summary>
    public float DurationSeconds;
    /// <summary>Wskaźnik do tablicy segmentów</summary>
    public nint Segments;
    /// <summary>Liczba segmentów</summary>
    public uint SegmentsCount;
    /// <summary>Liczba segmentów odfiltrowanych</summary>
    public uint FilteredSegmentsCount;
    /// <summary>Całkowita latencja w ms</summary>
    public ulong LatencyMs;
    /// <summary>Komunikat błędu (null jeśli sukces)</summary>
    public nint Error;
}

// ============================================================================
// SPEAKER IDENTIFICATION NATIVE STRUCTS
// ============================================================================

/// <summary>
/// Wynik operacji SpeakerEnroll / SpeakerAddSamples (layout C-compatible).
/// </summary>
[StructLayout(LayoutKind.Sequential)]
internal struct SpeakerEnrollResultNative
{
    /// <summary>ID mówcy</summary>
    public nint SpeakerId;
    /// <summary>Nazwa mówcy</summary>
    public nint SpeakerName;
    /// <summary>Liczba przetworzonych próbek audio</summary>
    public uint SamplesProcessed;
    /// <summary>Liczba pomyślnie wyekstrahowanych embeddingów</summary>
    public uint EmbeddingsAdded;
    /// <summary>Czy to była nowa rejestracja (1) czy aktualizacja (0)</summary>
    public byte IsNew;
    /// <summary>Całkowita latencja w ms</summary>
    public ulong LatencyMs;
    /// <summary>Komunikat błędu (null jeśli sukces)</summary>
    public nint Error;
}

/// <summary>
/// Wynik operacji SpeakerRemove (layout C-compatible).
/// </summary>
[StructLayout(LayoutKind.Sequential)]
internal struct SpeakerRemoveResultNative
{
    /// <summary>ID usuniętego mówcy</summary>
    public nint SpeakerId;
    /// <summary>Czy usunięcie się powiodło (1 = true, 0 = false)</summary>
    public byte Success;
    /// <summary>Całkowita latencja w ms</summary>
    public ulong LatencyMs;
    /// <summary>Komunikat błędu (null jeśli sukces)</summary>
    public nint Error;
}

/// <summary>
/// Wpis na liście mówców (layout C-compatible).
/// </summary>
[StructLayout(LayoutKind.Sequential)]
internal struct SpeakerEntryNative
{
    /// <summary>ID mówcy</summary>
    public nint SpeakerId;
    /// <summary>Nazwa mówcy</summary>
    public nint SpeakerName;
}

/// <summary>
/// Wynik operacji SpeakerList (layout C-compatible).
/// </summary>
[StructLayout(LayoutKind.Sequential)]
internal struct SpeakerListResultNative
{
    /// <summary>Wskaźnik do tablicy mówców</summary>
    public nint Speakers;
    /// <summary>Liczba mówców</summary>
    public uint SpeakersCount;
    /// <summary>Całkowita liczba mówców w bazie</summary>
    public uint TotalCount;
    /// <summary>Całkowita latencja w ms</summary>
    public ulong LatencyMs;
    /// <summary>Komunikat błędu (null jeśli sukces)</summary>
    public nint Error;
}

/// <summary>
/// Wynik operacji SpeakerInfo (layout C-compatible).
/// </summary>
[StructLayout(LayoutKind.Sequential)]
internal struct SpeakerInfoResultNative
{
    /// <summary>Liczba zarejestrowanych mówców</summary>
    public uint SpeakerCount;
    /// <summary>Wymiar embeddingów (192 dla ECAPA-TDNN)</summary>
    public uint EmbeddingDim;
    /// <summary>Próg similarity używany w bazie</summary>
    public float SimilarityThreshold;
    /// <summary>Całkowita latencja w ms</summary>
    public ulong LatencyMs;
    /// <summary>Komunikat błędu (null jeśli sukces)</summary>
    public nint Error;
}

/// <summary>
/// Wynik operacji SpeakerIdentify (layout C-compatible).
/// </summary>
[StructLayout(LayoutKind.Sequential)]
internal struct SpeakerIdentifyResultNative
{
    /// <summary>Czy rozpoznano mówcę (1 = true, 0 = false)</summary>
    public byte IsMatch;
    /// <summary>ID rozpoznanego mówcy (null jeśli !is_match)</summary>
    public nint SpeakerId;
    /// <summary>Nazwa rozpoznanego mówcy (null jeśli !is_match)</summary>
    public nint SpeakerName;
    /// <summary>Similarity score (0.0-1.0)</summary>
    public float Similarity;
    /// <summary>Użyty próg similarity</summary>
    public float Threshold;
    /// <summary>Całkowita latencja w ms</summary>
    public ulong LatencyMs;
    /// <summary>Komunikat błędu (null jeśli sukces)</summary>
    public nint Error;
}

/// <summary>
/// Wynik operacji SpeakerVerify (layout C-compatible).
/// </summary>
[StructLayout(LayoutKind.Sequential)]
internal struct SpeakerVerifyResultNative
{
    /// <summary>ID weryfikowanego mówcy</summary>
    public nint SpeakerId;
    /// <summary>Czy weryfikacja pozytywna (1 = true, 0 = false)</summary>
    public byte IsVerified;
    /// <summary>Similarity score z docelowym mówcą</summary>
    public float Similarity;
    /// <summary>Użyty próg</summary>
    public float Threshold;
    /// <summary>ID wykrytego mówcy (jeśli inny niż weryfikowany)</summary>
    public nint DetectedSpeakerId;
    /// <summary>Całkowita latencja w ms</summary>
    public ulong LatencyMs;
    /// <summary>Komunikat błędu (null jeśli sukces)</summary>
    public nint Error;
}

// ============================================================================
// CONVERSATION SESSION NATIVE STRUCTS
// ============================================================================

/// <summary>
/// Konfiguracja sesji konwersacji (layout C-compatible).
/// </summary>
[StructLayout(LayoutKind.Sequential)]
internal struct ConversationSessionConfigNative
{
    /// <summary>Tryb sesji: 0=AlwaysOn, 1=WakeWordTimeout, 2=WakeWordExplicitStop</summary>
    public byte Mode;
    /// <summary>ID użytkownika (dla personalizacji)</summary>
    public nint UserId;
    /// <summary>Język rozpoznawania mowy (ISO-639-1)</summary>
    public nint Language;
    /// <summary>Model STT do użycia</summary>
    public nint SttModel;
    /// <summary>Lista wake words (rozdzielona przecinkami)</summary>
    public nint WakeWords;
    /// <summary>Lista stop phrases (rozdzielona przecinkami)</summary>
    public nint StopPhrases;
    /// <summary>Timeout ciszy w ms (dla WakeWordTimeout), 0=domyślny 30000</summary>
    public uint SilenceTimeoutMs;
    /// <summary>Bufor audio przed wake word w ms (0=domyślny 2000)</summary>
    public uint PreWakeBufferMs;
}

/// <summary>
/// Wynik operacji ConversationStart (layout C-compatible).
/// </summary>
[StructLayout(LayoutKind.Sequential)]
internal struct ConversationStartResultNative
{
    /// <summary>ID utworzonej sesji</summary>
    public nint SessionId;
    /// <summary>Aktualny stan: 0=Inactive, 1=Active, 2=Processing, 3=Speaking</summary>
    public byte State;
    /// <summary>Komunikat błędu (null jeśli sukces)</summary>
    public nint Error;
}

/// <summary>
/// Zdarzenie konwersacji (layout C-compatible).
/// </summary>
[StructLayout(LayoutKind.Sequential)]
internal struct ConversationEventNative
{
    /// <summary>Typ zdarzenia: 0=SessionStarted, 1=WakeWordDetected, 2=TranscriptionAvailable,
    /// 3=SilenceTimeout, 4=StopPhraseDetected, 5=SessionEnded, 6=UserChanged</summary>
    public byte EventType;
    /// <summary>Timestamp zdarzenia w ms</summary>
    public ulong TimestampMs;
    /// <summary>Transkrypcja (dla TranscriptionAvailable)</summary>
    public nint Transcription;
    /// <summary>Pewność transkrypcji (0.0-1.0)</summary>
    public float Confidence;
    /// <summary>Wykryty wake word</summary>
    public nint WakeWord;
    /// <summary>Wykryty stop phrase</summary>
    public nint StopPhrase;
    /// <summary>ID użytkownika (dla UserChanged)</summary>
    public nint UserId;
}

/// <summary>
/// Wynik operacji ConversationAudio (layout C-compatible).
/// </summary>
[StructLayout(LayoutKind.Sequential)]
internal struct ConversationAudioResultNative
{
    /// <summary>ID sesji</summary>
    public nint SessionId;
    /// <summary>Aktualny stan sesji</summary>
    public byte State;
    /// <summary>Wskaźnik do tablicy zdarzeń</summary>
    public nint Events;
    /// <summary>Liczba zdarzeń</summary>
    public uint EventsCount;
    /// <summary>Transkrypcja (jeśli dostępna)</summary>
    public nint Transcription;
    /// <summary>Pewność transkrypcji</summary>
    public float Confidence;
    /// <summary>Komunikat błędu (null jeśli sukces)</summary>
    public nint Error;
}

/// <summary>
/// Statystyki sesji konwersacji (layout C-compatible).
/// </summary>
[StructLayout(LayoutKind.Sequential)]
internal struct ConversationSessionStatsNative
{
    /// <summary>Całkowity czas trwania sesji w ms</summary>
    public ulong TotalDurationMs;
    /// <summary>Czas aktywnego mówienia w ms</summary>
    public ulong ActiveSpeechMs;
    /// <summary>Liczba wykrytych wake words</summary>
    public uint WakeWordsDetected;
    /// <summary>Liczba transkrypcji</summary>
    public uint TranscriptionsCount;
    /// <summary>Liczba wykrytych mówców</summary>
    public uint SpeakersDetected;
}

/// <summary>
/// Wynik operacji ConversationEnd (layout C-compatible).
/// </summary>
[StructLayout(LayoutKind.Sequential)]
internal struct ConversationEndResultNative
{
    /// <summary>ID zakończonej sesji</summary>
    public nint SessionId;
    /// <summary>Pełna transkrypcja sesji</summary>
    public nint FinalTranscription;
    /// <summary>Statystyki sesji</summary>
    public ConversationSessionStatsNative Stats;
    /// <summary>Komunikat błędu (null jeśli sukces)</summary>
    public nint Error;
}

/// <summary>
/// Wynik operacji ConversationStatus (layout C-compatible).
/// </summary>
[StructLayout(LayoutKind.Sequential)]
internal struct ConversationStatusResultNative
{
    /// <summary>ID sesji</summary>
    public nint SessionId;
    /// <summary>Czy sesja istnieje (1 = true, 0 = false)</summary>
    public byte Exists;
    /// <summary>Aktualny stan: 0=Inactive, 1=Active, 2=Processing, 3=Speaking</summary>
    public byte State;
    /// <summary>Tryb sesji: 0=AlwaysOn, 1=WakeWordTimeout, 2=WakeWordExplicitStop</summary>
    public byte Mode;
    /// <summary>Czas trwania sesji w ms</summary>
    public ulong DurationMs;
    /// <summary>Czas od ostatniej aktywności w ms</summary>
    public ulong LastActivityMs;
    /// <summary>Komunikat błędu (null jeśli sukces)</summary>
    public nint Error;
}
