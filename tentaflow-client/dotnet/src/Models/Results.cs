// ============================================================================
// RESULTS - Struktury wyników operacji API
// ============================================================================
//
// CEL:
// Definiuje klasy wyników zwracanych przez metody TentaFlowClient.
// Każda klasa reprezentuje odpowiedź na konkretny typ operacji.
//
// KLUCZOWE KONCEPCJE:
// - record-like classes: Immutable data containers z init-only properties
// - required modifier: Wymusza ustawienie wartości podczas inicjalizacji
// - Nullable reference types: Opcjonalne właściwości oznaczone jako T?
//
// METRYKI WYDAJNOŚCI (ChatCompletionResult):
// - TimeToFirstTokenMs: Czas do pierwszego tokena (streaming only)
// - LatencyMs: Całkowity czas od requestu do ostatniego tokena
// - TokensPerSec: Przepustowość generacji (tokeny/s)
//
// ============================================================================

namespace TentaFlow.Client.Models;

/// <summary>
/// Wynik requestu embeddings.
/// </summary>
public sealed class EmbeddingsResult
{
    /// <summary>
    /// Wektory embeddings.
    /// </summary>
    public required IReadOnlyList<float[]> Embeddings { get; init; }

    /// <summary>
    /// Wymiary wektorów (np. 768).
    /// </summary>
    public int Dimensions { get; init; }

    /// <summary>
    /// Całkowita latencja w milisekundach.
    /// </summary>
    public ulong LatencyMs { get; init; }
}

// ============================================================================
// DETECTED TOOL CALLS - Wyniki z Intent Analyzer
// ============================================================================

/// <summary>
/// Wynik wykonania narzędzia wykrytego przez Intent Analyzer.
/// </summary>
public sealed class DetectedToolExecutionResult
{
    /// <summary>
    /// Czy wykonanie się powiodło.
    /// </summary>
    public bool Success { get; init; }

    /// <summary>
    /// Komunikat wyniku.
    /// </summary>
    public required string Message { get; init; }

    /// <summary>
    /// Dane wynikowe (opcjonalnie jako JSON string).
    /// </summary>
    public string? Data { get; init; }

    /// <summary>
    /// Komunikat błędu (jeśli Success = false).
    /// </summary>
    public string? Error { get; init; }
}

/// <summary>
/// Wykryte wywołanie narzędzia z Intent Analyzer.
/// </summary>
public sealed class DetectedToolCall
{
    /// <summary>
    /// Unikalny identyfikator wywołania.
    /// </summary>
    public required string CallId { get; init; }

    /// <summary>
    /// Nazwa narzędzia (np. "calendar", "email", "web_search").
    /// </summary>
    public required string ToolName { get; init; }

    /// <summary>
    /// Parametry narzędzia jako JSON string.
    /// </summary>
    public required string Parameters { get; init; }

    /// <summary>
    /// Czy wywołanie jest kompletne (wszystkie wymagane parametry).
    /// </summary>
    public bool IsComplete { get; init; }

    /// <summary>
    /// Lista brakujących parametrów (jeśli IsComplete = false).
    /// </summary>
    public IReadOnlyList<string>? MissingParams { get; init; }

    /// <summary>
    /// Wynik wykonania narzędzia (jeśli już wykonano).
    /// </summary>
    public DetectedToolExecutionResult? ExecutionResult { get; init; }

    /// <summary>
    /// Pytanie uzupełniające do użytkownika (jeśli brakuje parametrów).
    /// </summary>
    public string? FollowUpQuestion { get; init; }
}

/// <summary>
/// Wynik requestu chat completion.
/// Zawiera metryki wydajności dla streamingu i non-streamingu.
/// </summary>
public sealed class ChatCompletionResult
{
    /// <summary>
    /// Wygenerowana treść tekstowa.
    /// </summary>
    public required string Content { get; init; }

    /// <summary>
    /// Chain-of-thought reasoning (dla modeli reasoning jak DeepSeek R1, OpenAI o1).
    /// NULL jeśli model nie zwraca reasoning.
    /// </summary>
    public string? ReasoningContent { get; init; }

    /// <summary>
    /// Nazwa modelu użytego do generacji.
    /// </summary>
    public required string Model { get; init; }

    /// <summary>
    /// Przyczyna zakończenia ("stop", "length", itd.).
    /// </summary>
    public string? FinishReason { get; init; }

    /// <summary>
    /// Liczba tokenów w prompcie.
    /// </summary>
    public uint PromptTokens { get; init; }

    /// <summary>
    /// Liczba tokenów w wygenerowanej odpowiedzi.
    /// </summary>
    public uint CompletionTokens { get; init; }

    /// <summary>
    /// Całkowita liczba użytych tokenów.
    /// </summary>
    public uint TotalTokens { get; init; }

    /// <summary>
    /// Czas do pierwszego tokena w milisekundach (tylko streaming, 0 dla non-streaming).
    /// </summary>
    public ulong TimeToFirstTokenMs { get; init; }

    /// <summary>
    /// Całkowita latencja w milisekundach (czas od requestu do ostatniego tokena).
    /// </summary>
    public ulong LatencyMs { get; init; }

    /// <summary>
    /// Tokeny na sekundę (przepustowość generacji, 0 jeśli niedostępne).
    /// </summary>
    public float TokensPerSec { get; init; }

    /// <summary>
    /// Wykryty intent z Intent Analyzer (np. "self_introduction", "tool_call").
    /// NULL jeśli Intent Analyzer nie jest włączony lub nie wykrył intencji.
    /// </summary>
    public string? DetectedIntent { get; init; }

    /// <summary>
    /// Wykryte wywołania narzędzi z Intent Analyzer.
    /// NULL jeśli brak wykrytych tool calls.
    /// </summary>
    public IReadOnlyList<DetectedToolCall>? DetectedTools { get; init; }
}

/// <summary>
/// Wynik requestu TTS (Text-to-Speech).
/// </summary>
public sealed class TtsResult
{
    /// <summary>
    /// Dane audio w żądanym formacie.
    /// </summary>
    public required byte[] AudioData { get; init; }

    /// <summary>
    /// Format audio (np. "mp3", "opus", "wav").
    /// </summary>
    public required string Format { get; init; }

    /// <summary>
    /// Całkowita latencja w milisekundach.
    /// </summary>
    public ulong LatencyMs { get; init; }

    /// <summary>
    /// Czas trwania audio w sekundach.
    /// </summary>
    public float AudioDurationSec { get; init; }
}

/// <summary>
/// Wynik requestu STT (Speech-to-Text).
/// </summary>
public sealed class SttResult
{
    /// <summary>
    /// Transkrybowany tekst.
    /// </summary>
    public required string Text { get; init; }

    /// <summary>
    /// Wykryty język (kod ISO-639-1).
    /// </summary>
    public string? Language { get; init; }

    /// <summary>
    /// Czas trwania audio w sekundach.
    /// </summary>
    public float DurationSeconds { get; init; }
}

/// <summary>
/// Opcje STT z filtrowaniem halucynacji i formatem verbose_json.
/// </summary>
public sealed class SttOptions
{
    /// <summary>
    /// Język audio (kod ISO-639-1, np. "pl", "en"). Opcjonalne - automatyczne wykrywanie.
    /// </summary>
    public string? Language { get; init; }

    /// <summary>
    /// Prompt kontekstowy do poprawy jakości transkrypcji.
    /// </summary>
    public string? Prompt { get; init; }

    /// <summary>
    /// Format odpowiedzi: "json", "text", "verbose_json", "srt", "vtt".
    /// Użyj "verbose_json" żeby otrzymać segmenty z metrykami.
    /// </summary>
    public string? ResponseFormat { get; init; }

    /// <summary>
    /// Temperatura (0.0-1.0). Wyższa = więcej kreatywności.
    /// </summary>
    public float? Temperature { get; init; }

    /// <summary>
    /// Granularność timestampów: "segment" lub "word" (tylko dla verbose_json).
    /// </summary>
    public string? TimestampGranularities { get; init; }

    /// <summary>
    /// Próg no_speech_prob do filtrowania halucynacji (0.0-1.0).
    /// Segmenty z no_speech_prob >= threshold zostaną odfiltrowane.
    /// Typowe wartości: 0.5 - 0.8.
    /// </summary>
    public float? NoSpeechThreshold { get; init; }

    /// <summary>
    /// Minimalny avg_logprob dla segmentu (typowo ujemne, np. -1.0).
    /// Segmenty z avg_logprob &lt; threshold zostaną odfiltrowane.
    /// </summary>
    public float? AvgLogprobThreshold { get; init; }

    /// <summary>
    /// Maksymalny compression_ratio dla segmentu (typowo: 2.4).
    /// Segmenty z compression_ratio &gt; threshold zostaną odfiltrowane.
    /// </summary>
    public float? CompressionRatioThreshold { get; init; }
}

/// <summary>
/// Segment transkrypcji z metrykami jakości (dla verbose_json).
/// </summary>
public sealed class SttSegment
{
    /// <summary>
    /// ID segmentu.
    /// </summary>
    public uint Id { get; init; }

    /// <summary>
    /// Czas rozpoczęcia w sekundach.
    /// </summary>
    public float Start { get; init; }

    /// <summary>
    /// Czas zakończenia w sekundach.
    /// </summary>
    public float End { get; init; }

    /// <summary>
    /// Tekst segmentu.
    /// </summary>
    public required string Text { get; init; }

    /// <summary>
    /// Średnia log probability (typowo ujemne, np. -0.5).
    /// </summary>
    public float AvgLogprob { get; init; }

    /// <summary>
    /// Prawdopodobieństwo ciszy (0.0-1.0). Wysokie wartości = prawdopodobna halucynacja.
    /// </summary>
    public float NoSpeechProb { get; init; }

    /// <summary>
    /// Współczynnik kompresji. Wysokie wartości (>2.4) mogą oznaczać halucynacje.
    /// </summary>
    public float CompressionRatio { get; init; }

    /// <summary>
    /// Temperatura użyta do generacji tego segmentu.
    /// </summary>
    public float Temperature { get; init; }

    /// <summary>
    /// Etykieta mówcy z diarization (np. "SPEAKER_00", "Jan Kowalski").
    /// NULL jeśli diarization wyłączona lub nie wykryto mówcy.
    /// </summary>
    public string? SpeakerLabel { get; init; }

    /// <summary>
    /// Similarity score z bazy mówców (0.0-1.0, cosine similarity).
    /// NULL jeśli diarization wyłączona lub brak bazy mówców.
    /// </summary>
    public float? SpeakerSimilarity { get; init; }

    /// <summary>
    /// Czy mówca został rozpoznany z bazy (true) czy to anonimowy speaker (false).
    /// NULL jeśli diarization wyłączona.
    /// </summary>
    public bool? IsKnownSpeaker { get; init; }
}

/// <summary>
/// Szczegółowy wynik STT z segmentami i metrykami filtrowania.
/// </summary>
public sealed class SttDetailedResult
{
    /// <summary>
    /// Transkrybowany tekst (po filtrowaniu jeśli włączone).
    /// </summary>
    public required string Text { get; init; }

    /// <summary>
    /// Wykryty język (kod ISO-639-1).
    /// </summary>
    public string? Language { get; init; }

    /// <summary>
    /// Czas trwania audio w sekundach.
    /// </summary>
    public float DurationSeconds { get; init; }

    /// <summary>
    /// Segmenty transkrypcji z metrykami (tylko dla verbose_json).
    /// </summary>
    public IReadOnlyList<SttSegment> Segments { get; init; } = [];

    /// <summary>
    /// Liczba segmentów odfiltrowanych (jeśli włączone filtrowanie).
    /// </summary>
    public uint FilteredSegmentsCount { get; init; }

    /// <summary>
    /// Całkowita latencja w milisekundach.
    /// </summary>
    public ulong LatencyMs { get; init; }
}

/// <summary>
/// Opcje TTS dla chat completion z audio.
/// </summary>
public sealed class TtsOptions
{
    /// <summary>
    /// Model TTS do użycia (np. "jarvis").
    /// </summary>
    public required string Model { get; init; }

    /// <summary>
    /// Głos do użycia (np. "jarvis").
    /// </summary>
    public required string Voice { get; init; }

    /// <summary>
    /// Format audio (np. "wav"). Opcjonalne.
    /// </summary>
    public string? Format { get; init; }

    /// <summary>
    /// Prędkość mowy (1.0 = normalna). Opcjonalne.
    /// </summary>
    public float? Speed { get; init; }
}

/// <summary>
/// Opcje Memory dla chat completion z kontekstem osobowym.
/// </summary>
public sealed class MemoryOptions
{
    /// <summary>
    /// Czy pamięć jest włączona. Domyślnie true jeśli podano session_id.
    /// </summary>
    public bool Enabled { get; init; } = true;

    /// <summary>
    /// Identyfikator sesji rozmowy dla grupowania kontekstu.
    /// </summary>
    public string? SessionId { get; init; }

    /// <summary>
    /// ID rozpoznanej osoby (z speaker identification).
    /// </summary>
    public string? PersonId { get; init; }

    /// <summary>
    /// Poziom pewności rozpoznania głosu (0.0-1.0).
    /// </summary>
    public float? SpeakerConfidence { get; init; }

    /// <summary>
    /// Czy zapisywać wypowiedzi i odpowiedzi do Memory. Domyślnie true.
    /// </summary>
    public bool StoreEnabled { get; init; } = true;

    /// <summary>
    /// Czy odpytywać Memory o kontekst. Domyślnie true.
    /// </summary>
    public bool QueryEnabled { get; init; } = true;
}

/// <summary>
/// Opcje dla chat completion - wszystkie parametry w jednej strukturze.
/// </summary>
public sealed class ChatCompletionOptions
{
    /// <summary>
    /// Temperatura (0.0-2.0). Wyższa = więcej kreatywności.
    /// </summary>
    public float? Temperature { get; init; }

    /// <summary>
    /// Maksymalna liczba tokenów do wygenerowania.
    /// </summary>
    public int? MaxTokens { get; init; }

    /// <summary>
    /// Chat template dla modeli lokalnych. Domyślnie Auto.
    /// </summary>
    public ChatTemplate Template { get; init; } = ChatTemplate.Auto;

    /// <summary>
    /// Czy włączyć streaming. Domyślnie true dla interaktywnych odpowiedzi.
    /// </summary>
    public bool Stream { get; init; } = true;

    /// <summary>
    /// Opcje TTS do generowania audio z odpowiedzi.
    /// </summary>
    public TtsOptions? Tts { get; init; }

    /// <summary>
    /// Opcje Memory dla kontekstu osobowego i historii rozmowy.
    /// </summary>
    public MemoryOptions? Memory { get; init; }

    /// <summary>
    /// ID sesji (używane przez Memory, opcjonalne).
    /// </summary>
    public string? SessionId { get; init; }

    /// <summary>
    /// Dane audio wejściowe (dla voice conversation).
    /// Router przetworzy przez STT i speaker identification.
    /// </summary>
    public byte[]? AudioInput { get; init; }
}

// Zachowujemy alias dla kompatybilności wstecznej
/// <summary>
/// Alias dla TtsOptions (kompatybilność wsteczna).
/// </summary>
[Obsolete("Użyj TtsOptions zamiast TtsStreamingOptions")]
public sealed class TtsStreamingOptions
{
    /// <summary>
    /// Model TTS do użycia (np. "jarvis").
    /// </summary>
    public required string Model { get; init; }

    /// <summary>
    /// Głos do użycia (np. "jarvis").
    /// </summary>
    public required string Voice { get; init; }

    /// <summary>
    /// Format audio (np. "wav"). Opcjonalne.
    /// </summary>
    public string? Format { get; init; }

    /// <summary>
    /// Prędkość mowy (1.0 = normalna). Opcjonalne.
    /// </summary>
    public float? Speed { get; init; }
}

/// <summary>
/// Wynik chat completion z opcjonalnymi audio chunks.
/// </summary>
public sealed class ChatCompletionWithAudioResult
{
    /// <summary>
    /// Wynik chat completion z tekstem i metrykami.
    /// </summary>
    public required ChatCompletionResult Completion { get; init; }

    /// <summary>
    /// Audio chunks wygenerowane przez TTS (opcjonalne).
    /// </summary>
    public IReadOnlyList<byte[]>? AudioChunks { get; init; }

    /// <summary>
    /// Request ID do anulowania (jeśli streaming ciągle trwa).
    /// </summary>
    public string? RequestId { get; init; }

    /// <summary>
    /// Transkrybowany tekst z audio input (jeśli podano AudioInput).
    /// Wypełniane przez Router po przetworzeniu STT.
    /// </summary>
    public string? TranscribedText { get; init; }

    /// <summary>
    /// ID rozpoznanego mówcy (z speaker identification).
    /// NULL jeśli nie rozpoznano lub nie podano AudioInput.
    /// </summary>
    public string? SpeakerId { get; init; }

    /// <summary>
    /// Nazwa rozpoznanego mówcy.
    /// NULL jeśli nie rozpoznano lub nie podano AudioInput.
    /// </summary>
    public string? SpeakerName { get; init; }

    /// <summary>
    /// Wykryty intent z Intent Analyzer (np. "self_introduction", "tool_call").
    /// NULL jeśli Intent Analyzer nie jest włączony lub nie wykrył intencji.
    /// </summary>
    public string? DetectedIntent { get; init; }

    /// <summary>
    /// Wykryte wywołania narzędzi z Intent Analyzer.
    /// NULL jeśli brak wykrytych tool calls.
    /// </summary>
    public IReadOnlyList<DetectedToolCall>? DetectedTools { get; init; }
}

// ============================================================================
// SPEAKER IDENTIFICATION RESULTS
// ============================================================================

/// <summary>
/// Wynik operacji rejestracji mówcy (SpeakerEnroll / SpeakerAddSamples).
/// </summary>
public sealed class SpeakerEnrollResult
{
    /// <summary>
    /// Unikalny identyfikator mówcy.
    /// </summary>
    public required string SpeakerId { get; init; }

    /// <summary>
    /// Nazwa mówcy.
    /// </summary>
    public required string SpeakerName { get; init; }

    /// <summary>
    /// Liczba przetworzonych próbek audio.
    /// </summary>
    public uint SamplesProcessed { get; init; }

    /// <summary>
    /// Liczba pomyślnie wyekstrahowanych embeddingów głosowych.
    /// </summary>
    public uint EmbeddingsAdded { get; init; }

    /// <summary>
    /// Czy to była nowa rejestracja (true) czy aktualizacja istniejącego mówcy (false).
    /// </summary>
    public bool IsNew { get; init; }

    /// <summary>
    /// Całkowita latencja w milisekundach.
    /// </summary>
    public ulong LatencyMs { get; init; }
}

/// <summary>
/// Wynik operacji usunięcia mówcy (SpeakerRemove).
/// </summary>
public sealed class SpeakerRemoveResult
{
    /// <summary>
    /// Identyfikator usuniętego mówcy.
    /// </summary>
    public required string SpeakerId { get; init; }

    /// <summary>
    /// Czy operacja usunięcia powiodła się.
    /// </summary>
    public bool Success { get; init; }

    /// <summary>
    /// Całkowita latencja w milisekundach.
    /// </summary>
    public ulong LatencyMs { get; init; }
}

/// <summary>
/// Wpis mówcy na liście.
/// </summary>
public sealed class SpeakerEntry
{
    /// <summary>
    /// Unikalny identyfikator mówcy.
    /// </summary>
    public required string SpeakerId { get; init; }

    /// <summary>
    /// Nazwa mówcy.
    /// </summary>
    public required string SpeakerName { get; init; }
}

/// <summary>
/// Wynik operacji pobrania listy mówców (SpeakerList).
/// </summary>
public sealed class SpeakerListResult
{
    /// <summary>
    /// Lista zarejestrowanych mówców (id, nazwa).
    /// </summary>
    public required IReadOnlyList<SpeakerEntry> Speakers { get; init; }

    /// <summary>
    /// Całkowita liczba mówców w bazie.
    /// </summary>
    public uint TotalCount { get; init; }

    /// <summary>
    /// Całkowita latencja w milisekundach.
    /// </summary>
    public ulong LatencyMs { get; init; }
}

/// <summary>
/// Wynik operacji pobrania informacji o bazie głosów (SpeakerInfo).
/// </summary>
public sealed class SpeakerInfoResult
{
    /// <summary>
    /// Liczba zarejestrowanych mówców.
    /// </summary>
    public uint SpeakerCount { get; init; }

    /// <summary>
    /// Wymiar embeddingów głosowych (np. 192 dla ECAPA-TDNN).
    /// </summary>
    public uint EmbeddingDim { get; init; }

    /// <summary>
    /// Próg similarity używany do identyfikacji/weryfikacji.
    /// </summary>
    public float SimilarityThreshold { get; init; }

    /// <summary>
    /// Całkowita latencja w milisekundach.
    /// </summary>
    public ulong LatencyMs { get; init; }
}

/// <summary>
/// Wynik operacji identyfikacji mówcy (SpeakerIdentify).
/// </summary>
public sealed class SpeakerIdentifyResult
{
    /// <summary>
    /// Czy rozpoznano mówcę (similarity >= threshold).
    /// </summary>
    public bool IsMatch { get; init; }

    /// <summary>
    /// Identyfikator rozpoznanego mówcy (null jeśli IsMatch = false).
    /// </summary>
    public string? SpeakerId { get; init; }

    /// <summary>
    /// Nazwa rozpoznanego mówcy (null jeśli IsMatch = false).
    /// </summary>
    public string? SpeakerName { get; init; }

    /// <summary>
    /// Similarity score (0.0-1.0, cosine similarity).
    /// </summary>
    public float Similarity { get; init; }

    /// <summary>
    /// Użyty próg similarity.
    /// </summary>
    public float Threshold { get; init; }

    /// <summary>
    /// Całkowita latencja w milisekundach.
    /// </summary>
    public ulong LatencyMs { get; init; }
}

/// <summary>
/// Wynik operacji weryfikacji mówcy (SpeakerVerify).
/// </summary>
public sealed class SpeakerVerifyResult
{
    /// <summary>
    /// Identyfikator weryfikowanego mówcy.
    /// </summary>
    public required string SpeakerId { get; init; }

    /// <summary>
    /// Czy weryfikacja pozytywna (similarity >= threshold).
    /// </summary>
    public bool IsVerified { get; init; }

    /// <summary>
    /// Similarity score z docelowym mówcą (0.0-1.0).
    /// </summary>
    public float Similarity { get; init; }

    /// <summary>
    /// Użyty próg similarity.
    /// </summary>
    public float Threshold { get; init; }

    /// <summary>
    /// Identyfikator wykrytego mówcy, jeśli jest inny niż weryfikowany (null jeśli zgodny lub nierozpoznany).
    /// </summary>
    public string? DetectedSpeakerId { get; init; }

    /// <summary>
    /// Całkowita latencja w milisekundach.
    /// </summary>
    public ulong LatencyMs { get; init; }
}

// ============================================================================
// CONVERSATION SESSION - Sesje konwersacyjne "Jarvis"
// ============================================================================

/// <summary>
/// Tryb pracy sesji konwersacyjnej.
/// </summary>
public enum SessionMode
{
    /// <summary>
    /// Zawsze aktywny - nie wymaga wake word.
    /// Idealne dla dedykowanych asystentów, smart speakerów.
    /// </summary>
    AlwaysOn = 0,

    /// <summary>
    /// Aktywacja przez wake word, deaktywacja przez timeout ciszy.
    /// Po wykryciu "Jarvis" sesja jest aktywna przez określony czas.
    /// Jeśli przez SilenceTimeoutMs nie ma tekstu, sesja kończy się.
    /// </summary>
    WakeWordTimeout = 1,

    /// <summary>
    /// Aktywacja przez wake word, deaktywacja przez explicit stop phrase.
    /// Sesja trwa do powiedzenia frazy typu "dzięki Jarvis, to koniec".
    /// </summary>
    WakeWordExplicitStop = 2
}

/// <summary>
/// Stan sesji konwersacyjnej.
/// </summary>
public enum SessionState
{
    /// <summary>Nieaktywna - czeka na wake word</summary>
    Inactive = 0,

    /// <summary>Aktywna - słucha i przetwarza komendy</summary>
    Active = 1,

    /// <summary>Przetwarzanie - generuje odpowiedź</summary>
    Processing = 2,

    /// <summary>Mówi - odtwarza TTS</summary>
    Speaking = 3
}

/// <summary>
/// Konfiguracja sesji konwersacyjnej.
/// </summary>
public sealed class ConversationSessionConfig
{
    /// <summary>
    /// Tryb pracy sesji (AlwaysOn, WakeWordTimeout, WakeWordExplicitStop).
    /// </summary>
    public SessionMode Mode { get; init; } = SessionMode.WakeWordTimeout;

    /// <summary>
    /// ID użytkownika (dla personalizacji i speaker ID).
    /// </summary>
    public string? UserId { get; init; }

    /// <summary>
    /// Język rozpoznawania mowy (ISO-639-1). Domyślnie "pl".
    /// </summary>
    public string? Language { get; init; } = "pl";

    /// <summary>
    /// Model STT do użycia. Domyślnie "whisper".
    /// </summary>
    public string? SttModel { get; init; } = "whisper";

    /// <summary>
    /// Timeout ciszy w ms (dla trybu WakeWordTimeout). Domyślnie 30000 (30s).
    /// "Cisza" oznacza brak rozpoznanego tekstu, nie brak dźwięku.
    /// </summary>
    public uint SilenceTimeoutMs { get; init; } = 30_000;

    /// <summary>
    /// Bufor audio przed wake word w ms. Domyślnie 2000 (2s).
    /// </summary>
    public uint PreWakeBufferMs { get; init; } = 2_000;

    /// <summary>
    /// Wake words (dla trybów WakeWord*).
    /// Domyślnie: ["jarvis", "hej jarvis", "cześć jarvis", "ok jarvis"]
    /// </summary>
    public IReadOnlyList<string>? WakeWords { get; init; } = [
        "jarvis", "hej jarvis", "cześć jarvis", "ok jarvis"
    ];

    /// <summary>
    /// Stop phrases (dla trybu WakeWordExplicitStop).
    /// Domyślnie: ["dzięki jarvis to koniec", "ok jarvis wystarczy", ...]
    /// </summary>
    public IReadOnlyList<string>? StopPhrases { get; init; } = [
        "dzięki jarvis to koniec",
        "ok jarvis wystarczy",
        "jarvis koniec",
        "to wszystko jarvis",
        "jarvis dziękuję",
        "dziękuję jarvis"
    ];
}

/// <summary>
/// Wynik rozpoczęcia sesji konwersacyjnej.
/// </summary>
public sealed class ConversationStartResult
{
    /// <summary>
    /// ID sesji (używaj w ConversationSendAudio/End).
    /// </summary>
    public required string SessionId { get; init; }

    /// <summary>
    /// Początkowy stan sesji (Active dla AlwaysOn, Inactive dla WakeWord*).
    /// </summary>
    public SessionState State { get; init; }
}

/// <summary>
/// Typ zdarzenia z sesji konwersacyjnej.
/// </summary>
public enum ConversationEventType
{
    /// <summary>Sesja została rozpoczęta</summary>
    SessionStarted = 0,

    /// <summary>Wykryto wake word</summary>
    WakeWordDetected = 1,

    /// <summary>Dostępna transkrypcja</summary>
    TranscriptionAvailable = 2,

    /// <summary>Timeout ciszy - sesja deaktywowana</summary>
    SilenceTimeout = 3,

    /// <summary>Wykryto stop phrase - sesja deaktywowana</summary>
    StopPhraseDetected = 4,

    /// <summary>Sesja zakończona</summary>
    SessionEnded = 5,

    /// <summary>Wykryto zmianę użytkownika (speaker ID)</summary>
    UserChanged = 6
}

/// <summary>
/// Zdarzenie z sesji konwersacyjnej.
/// </summary>
public sealed class ConversationEvent
{
    /// <summary>
    /// Typ zdarzenia.
    /// </summary>
    public ConversationEventType EventType { get; init; }

    /// <summary>
    /// Timestamp zdarzenia (ms od początku sesji).
    /// </summary>
    public ulong TimestampMs { get; init; }

    /// <summary>
    /// Transkrypcja (dla TranscriptionAvailable).
    /// </summary>
    public string? Transcription { get; init; }

    /// <summary>
    /// Pewność transkrypcji (0.0-1.0).
    /// </summary>
    public float Confidence { get; init; }

    /// <summary>
    /// Wykryty wake word (dla WakeWordDetected).
    /// </summary>
    public string? WakeWord { get; init; }

    /// <summary>
    /// Wykryty stop phrase (dla StopPhraseDetected).
    /// </summary>
    public string? StopPhrase { get; init; }

    /// <summary>
    /// ID użytkownika (dla UserChanged).
    /// </summary>
    public string? UserId { get; init; }
}

/// <summary>
/// Wynik wysłania audio do sesji konwersacji.
/// </summary>
public sealed class ConversationAudioResult
{
    /// <summary>
    /// ID sesji.
    /// </summary>
    public required string SessionId { get; init; }

    /// <summary>
    /// Aktualny stan sesji.
    /// </summary>
    public SessionState State { get; init; }

    /// <summary>
    /// Lista zdarzeń wygenerowanych przez przetworzenie audio.
    /// </summary>
    public IReadOnlyList<ConversationEvent> Events { get; init; } = [];

    /// <summary>
    /// Transkrypcja (jeśli dostępna).
    /// </summary>
    public string? Transcription { get; init; }

    /// <summary>
    /// Pewność transkrypcji.
    /// </summary>
    public float Confidence { get; init; }
}

/// <summary>
/// Statystyki zakończonej sesji.
/// </summary>
public sealed class ConversationSessionStats
{
    /// <summary>
    /// Całkowity czas trwania sesji (ms).
    /// </summary>
    public ulong TotalDurationMs { get; init; }

    /// <summary>
    /// Czas aktywnej mowy (ms).
    /// </summary>
    public ulong ActiveSpeechMs { get; init; }

    /// <summary>
    /// Liczba wykrytych wake words.
    /// </summary>
    public uint WakeWordsDetected { get; init; }

    /// <summary>
    /// Liczba transkrypcji.
    /// </summary>
    public uint TranscriptionsCount { get; init; }

    /// <summary>
    /// Liczba wykrytych mówców.
    /// </summary>
    public uint SpeakersDetected { get; init; }
}

/// <summary>
/// Wynik zakończenia sesji.
/// </summary>
public sealed class ConversationEndResult
{
    /// <summary>
    /// ID sesji.
    /// </summary>
    public required string SessionId { get; init; }

    /// <summary>
    /// Pełna transkrypcja sesji.
    /// </summary>
    public string? FinalTranscription { get; init; }

    /// <summary>
    /// Statystyki sesji.
    /// </summary>
    public ConversationSessionStats? Stats { get; init; }
}

/// <summary>
/// Informacje o statusie sesji.
/// </summary>
public sealed class ConversationStatusResult
{
    /// <summary>
    /// ID sesji.
    /// </summary>
    public required string SessionId { get; init; }

    /// <summary>
    /// Czy sesja istnieje.
    /// </summary>
    public bool Exists { get; init; }

    /// <summary>
    /// Aktualny stan sesji.
    /// </summary>
    public SessionState State { get; init; }

    /// <summary>
    /// Tryb sesji.
    /// </summary>
    public SessionMode Mode { get; init; }

    /// <summary>
    /// Czas trwania sesji w ms.
    /// </summary>
    public ulong DurationMs { get; init; }

    /// <summary>
    /// Czas od ostatniej aktywności w ms.
    /// </summary>
    public ulong LastActivityMs { get; init; }
}
