// ============================================================================
// TENTAFLOW CLIENT - Klient .NET dla TentaFlow Router
// ============================================================================
//
// CEL:
// Wysokopoziomowy klient .NET do komunikacji z TentaFlow.Router przez QUIC.
// Oferuje API do embeddings, chat completion, TTS, STT.
//
// JAK DZIAŁA:
// 1. Używa P/Invoke do wywołań natywnej biblioteki Rust (tentaflow_client_native)
// 2. Biblioteka Rust obsługuje połączenie QUIC z one-way TLS (klient nie wysyła certyfikatu)
// 3. Dane są serializowane przez rkyv (zero-copy) dla niskiej latencji
// 4. Wszystkie operacje są thread-safe dzięki wewnętrznej synchronizacji
//
// PRZYKŁAD UŻYCIA:
// ```csharp
// var config = new ClientConfig
// {
//     RouterUrl = "quic://localhost:3000",
//     CaPath = "certs/ca.pem"  // opcjonalne - jeśli nie podane, używa systemowych CA
// };
//
// using var client = new TentaFlowClient(config);
// var embeddings = client.Embeddings("embeddings-gemma", ["text"]);
// ```
//
// KLUCZOWE KONCEPCJE:
// - P/Invoke: Platform Invocation Services dla wywołań natywnych
// - IDisposable: Automatyczne zwalnianie zasobów natywnych
// - GCHandle: Pinowanie pamięci managed dla przekazania do native
// - Marshal: Konwersja typów między managed i native
//
// WYDAJNOŚĆ:
// - Natywna biblioteka Rust z optymalizacją LTO
// - QUIC multiplexing do 1000 równoległych strumieni
// - Zero-copy serialization z rkyv
//
// ============================================================================

using System.Runtime.InteropServices;
using TentaFlow.Client.Models;
using TentaFlow.Client.Native;

namespace TentaFlow.Client;

/// <summary>
/// Wysokowydajny klient TentaFlow Router wykorzystujący protokół QUIC.
/// Zapewnia dostęp do embeddings, chat completion, TTS, STT.
/// </summary>
/// <remarks>
/// Klient używa P/Invoke do wywołań natywnej biblioteki Rust dla optymalnej wydajności.
/// Wszystkie operacje są thread-safe dzięki wewnętrznej synchronizacji.
/// </remarks>
public sealed class TentaFlowClient : IDisposable
{
    private nint _clientHandle;
    private bool _disposed;

    /// <summary>
    /// Tworzy nowego klienta i łączy się z TentaFlow Router.
    /// </summary>
    /// <param name="config">Konfiguracja klienta z danymi połączenia.</param>
    /// <exception cref="TentaFlowException">Rzucany gdy połączenie się nie powiedzie.</exception>
    public TentaFlowClient(ClientConfig config)
    {
        ArgumentNullException.ThrowIfNull(config);

        // Initialize native library
        NativeMethods.tentaflow_init();

        // Marshal config to native
        var nativeConfig = new ClientConfigNative();
        var handles = new List<GCHandle>();

        try
        {
            nativeConfig.RouterUrl = MarshalString(config.RouterUrl, handles);
            // CA path jest opcjonalny - może być null (wtedy używa systemowych CA)
            nativeConfig.CaPath = config.CaPath != null
                ? MarshalString(config.CaPath, handles)
                : nint.Zero;
            nativeConfig.TimeoutMs = config.TimeoutMs;

            _clientHandle = NativeMethods.tentaflow_client_new(ref nativeConfig);

            if (_clientHandle == nint.Zero)
            {
                throw new TentaFlowException("Failed to connect to Router. Check connection parameters.");
            }
        }
        finally
        {
            foreach (var handle in handles)
            {
                handle.Free();
            }
        }
    }

    /// <summary>
    /// Zwraca czy klient jest aktualnie połączony.
    /// </summary>
    public bool IsConnected
    {
        get
        {
            ThrowIfDisposed();
            return NativeMethods.tentaflow_client_is_connected(_clientHandle);
        }
    }

    // =========================================================================
    // EMBEDDINGS
    // =========================================================================

    /// <summary>
    /// Generuje embeddings dla podanych tekstów.
    /// </summary>
    /// <param name="model">Nazwa modelu (np. "embeddings-gemma").</param>
    /// <param name="texts">Teksty do przetworzenia.</param>
    /// <returns>Wynik z wektorami embeddings i metrykami.</returns>
    public EmbeddingsResult Embeddings(string model, IEnumerable<string> texts)
    {
        ThrowIfDisposed();
        ArgumentException.ThrowIfNullOrEmpty(model);
        ArgumentNullException.ThrowIfNull(texts);

        var textList = texts.ToList();
        if (textList.Count == 0)
        {
            return new EmbeddingsResult { Embeddings = [], Dimensions = 0, LatencyMs = 0 };
        }

        var handles = new List<GCHandle>();
        try
        {
            var textPtrs = new nint[textList.Count];
            for (int i = 0; i < textList.Count; i++)
            {
                textPtrs[i] = MarshalString(textList[i], handles);
            }

            var textPtrsHandle = GCHandle.Alloc(textPtrs, GCHandleType.Pinned);
            handles.Add(textPtrsHandle);

            var result = NativeMethods.tentaflow_embeddings(
                _clientHandle,
                model,
                textPtrsHandle.AddrOfPinnedObject(),
                (nuint)textList.Count);

            try
            {
                if (result.Error != nint.Zero)
                {
                    var error = Marshal.PtrToStringUTF8(result.Error);
                    throw new TentaFlowException($"Embeddings error: {error}");
                }

                var embeddings = new List<float[]>((int)result.EmbeddingsCount);
                var dimensions = (int)result.Dimensions;

                for (int i = 0; i < (int)result.EmbeddingsCount; i++)
                {
                    var embedding = new float[dimensions];
                    Marshal.Copy(result.Embeddings + i * dimensions * sizeof(float), embedding, 0, dimensions);
                    embeddings.Add(embedding);
                }

                return new EmbeddingsResult
                {
                    Embeddings = embeddings,
                    Dimensions = dimensions,
                    LatencyMs = result.LatencyMs
                };
            }
            finally
            {
                NativeMethods.tentaflow_free_embeddings(result);
            }
        }
        finally
        {
            foreach (var handle in handles)
            {
                handle.Free();
            }
        }
    }

    // =========================================================================
    // CHAT COMPLETION (Unified API)
    // =========================================================================

    /// <summary>
    /// Wysyła request chat completion z pełną kontrolą przez ChatCompletionOptions.
    /// Obsługuje streaming, TTS, Memory i różne chat templates.
    /// </summary>
    /// <param name="model">Nazwa modelu (np. "gpt-oss-20b").</param>
    /// <param name="messages">Lista wiadomości (role, content).</param>
    /// <param name="options">Opcje chat completion (temperatura, streaming, TTS, Memory, template).</param>
    /// <param name="onReasoningStart">Callback wywoływany przy rozpoczęciu fazy reasoning (opcjonalny).</param>
    /// <param name="onReasoning">Callback wywoływany dla każdego tokena reasoning (opcjonalny).</param>
    /// <param name="onReasoningEnd">Callback wywoływany przy zakończeniu fazy reasoning (opcjonalny).</param>
    /// <param name="onContentStart">Callback wywoływany przy rozpoczęciu fazy content (opcjonalny).</param>
    /// <param name="onContent">Callback wywoływany dla każdego tokena content (opcjonalny).</param>
    /// <param name="onContentEnd">Callback wywoływany przy zakończeniu fazy content (opcjonalny).</param>
    /// <param name="onAudio">Callback wywoływany dla każdego audio chunk z TTS (opcjonalny).</param>
    /// <returns>Wynik chat completion z opcjonalnymi audio chunks.</returns>
    public ChatCompletionWithAudioResult ChatCompletion(
        string model,
        IEnumerable<ChatMessage> messages,
        ChatCompletionOptions? options = null,
        Action? onReasoningStart = null,
        Action<string>? onReasoning = null,
        Action? onReasoningEnd = null,
        Action? onContentStart = null,
        Action<string>? onContent = null,
        Action? onContentEnd = null,
        Action<byte[]>? onAudio = null)
    {
        ThrowIfDisposed();
        ArgumentException.ThrowIfNullOrEmpty(model);
        ArgumentNullException.ThrowIfNull(messages);

        var messageList = messages.ToList();
        if (messageList.Count == 0)
        {
            throw new ArgumentException("At least one message is required.", nameof(messages));
        }

        options ??= new ChatCompletionOptions();
        var handles = new List<GCHandle>();
        var audioChunks = new List<byte[]>();

        try
        {
            // Marshal messages
            var nativeMessages = new ChatMessageNative[messageList.Count];
            for (int i = 0; i < messageList.Count; i++)
            {
                nativeMessages[i] = new ChatMessageNative
                {
                    Role = MarshalString(messageList[i].Role, handles),
                    Content = MarshalString(messageList[i].Content, handles)
                };
            }

            var messagesHandle = GCHandle.Alloc(nativeMessages, GCHandleType.Pinned);
            handles.Add(messagesHandle);

            // Marshal TTS options
            nint ttsOptionsPtr = nint.Zero;
            if (options.Tts != null)
            {
                var ttsOptionsNative = new TtsOptionsNative
                {
                    Model = MarshalString(options.Tts.Model, handles),
                    Voice = MarshalString(options.Tts.Voice, handles),
                    Format = MarshalString(options.Tts.Format, handles),
                    Speed = options.Tts.Speed ?? 0.0f
                };
                var ttsHandle = GCHandle.Alloc(ttsOptionsNative, GCHandleType.Pinned);
                handles.Add(ttsHandle);
                ttsOptionsPtr = ttsHandle.AddrOfPinnedObject();
            }

            // Marshal Memory options
            nint memoryOptionsPtr = nint.Zero;
            if (options.Memory != null)
            {
                var memoryOptionsNative = new MemoryOptionsNative
                {
                    Enabled = (sbyte)(options.Memory.Enabled ? 1 : 0),
                    SessionId = MarshalString(options.Memory.SessionId, handles),
                    PersonId = MarshalString(options.Memory.PersonId, handles),
                    SpeakerConfidence = options.Memory.SpeakerConfidence ?? -1.0f,
                    StoreEnabled = (sbyte)(options.Memory.StoreEnabled ? 1 : 0),
                    QueryEnabled = (sbyte)(options.Memory.QueryEnabled ? 1 : 0)
                };
                var memoryHandle = GCHandle.Alloc(memoryOptionsNative, GCHandleType.Pinned);
                handles.Add(memoryHandle);
                memoryOptionsPtr = memoryHandle.AddrOfPinnedObject();
            }

            // Marshal audio input if present
            nint audioInputPtr = nint.Zero;
            nuint audioInputLen = 0;
            if (options.AudioInput != null && options.AudioInput.Length > 0)
            {
                var audioHandle = GCHandle.Alloc(options.AudioInput, GCHandleType.Pinned);
                handles.Add(audioHandle);
                audioInputPtr = audioHandle.AddrOfPinnedObject();
                audioInputLen = (nuint)options.AudioInput.Length;
            }

            // Marshal main options struct
            var optionsNative = new ChatCompletionOptionsNative
            {
                Temperature = options.Temperature ?? -1.0f,
                MaxTokens = options.MaxTokens ?? -1,
                TemplateType = (int)options.Template,
                Stream = (byte)(options.Stream ? 1 : 0),
                TtsOptions = ttsOptionsPtr,
                MemoryOptions = memoryOptionsPtr,
                SessionId = MarshalString(options.SessionId, handles),
                AudioInput = audioInputPtr,
                AudioInputLen = audioInputLen
            };
            var optionsHandle = GCHandle.Alloc(optionsNative, GCHandleType.Pinned);
            handles.Add(optionsHandle);

            // Create native callbacks
            NativeMethods.StreamingEventDelegate? nativeOnReasoningStart = null;
            NativeMethods.StreamingTokenDelegate? nativeOnReasoning = null;
            NativeMethods.StreamingEventDelegate? nativeOnReasoningEnd = null;
            NativeMethods.StreamingEventDelegate? nativeOnContentStart = null;
            NativeMethods.StreamingTokenDelegate? nativeOnContent = null;
            NativeMethods.StreamingEventDelegate? nativeOnContentEnd = null;
            NativeMethods.StreamingAudioDelegate? nativeOnAudio = null;

            if (onReasoningStart != null)
            {
                nativeOnReasoningStart = () => onReasoningStart();
                handles.Add(GCHandle.Alloc(nativeOnReasoningStart));
            }

            if (onReasoning != null)
            {
                nativeOnReasoning = (nint tokenPtr) =>
                {
                    if (tokenPtr != nint.Zero)
                    {
                        var token = Marshal.PtrToStringUTF8(tokenPtr);
                        if (!string.IsNullOrEmpty(token))
                        {
                            onReasoning(token);
                        }
                    }
                };
                handles.Add(GCHandle.Alloc(nativeOnReasoning));
            }

            if (onReasoningEnd != null)
            {
                nativeOnReasoningEnd = () => onReasoningEnd();
                handles.Add(GCHandle.Alloc(nativeOnReasoningEnd));
            }

            if (onContentStart != null)
            {
                nativeOnContentStart = () => onContentStart();
                handles.Add(GCHandle.Alloc(nativeOnContentStart));
            }

            if (onContent != null)
            {
                nativeOnContent = (nint tokenPtr) =>
                {
                    if (tokenPtr != nint.Zero)
                    {
                        var token = Marshal.PtrToStringUTF8(tokenPtr);
                        if (!string.IsNullOrEmpty(token))
                        {
                            onContent(token);
                        }
                    }
                };
                handles.Add(GCHandle.Alloc(nativeOnContent));
            }

            if (onContentEnd != null)
            {
                nativeOnContentEnd = () => onContentEnd();
                handles.Add(GCHandle.Alloc(nativeOnContentEnd));
            }

            // Audio callback - capture chunks and optionally call user callback
            if (options.Tts != null || onAudio != null)
            {
                nativeOnAudio = (nint audioDataPtr, nuint audioLen) =>
                {
                    if (audioDataPtr != nint.Zero && audioLen > 0)
                    {
                        var audioData = new byte[(int)audioLen];
                        Marshal.Copy(audioDataPtr, audioData, 0, (int)audioLen);
                        audioChunks.Add(audioData);
                        onAudio?.Invoke(audioData);
                    }
                };
                handles.Add(GCHandle.Alloc(nativeOnAudio));
            }

            var result = NativeMethods.tentaflow_chat_completion(
                _clientHandle,
                model,
                messagesHandle.AddrOfPinnedObject(),
                (nuint)messageList.Count,
                optionsHandle.AddrOfPinnedObject(),
                nativeOnReasoningStart,
                nativeOnReasoning,
                nativeOnReasoningEnd,
                nativeOnContentStart,
                nativeOnContent,
                nativeOnContentEnd,
                nativeOnAudio,
                nint.Zero); // request_id_out - not used for now

            try
            {
                if (result.Error != nint.Zero)
                {
                    var error = Marshal.PtrToStringUTF8(result.Error);
                    throw new TentaFlowException($"Chat completion error: {error}");
                }

                return new ChatCompletionWithAudioResult
                {
                    Completion = new ChatCompletionResult
                    {
                        Content = Marshal.PtrToStringUTF8(result.Content) ?? string.Empty,
                        ReasoningContent = result.ReasoningContent != nint.Zero
                            ? Marshal.PtrToStringUTF8(result.ReasoningContent)
                            : null,
                        Model = Marshal.PtrToStringUTF8(result.Model) ?? model,
                        FinishReason = Marshal.PtrToStringUTF8(result.FinishReason),
                        PromptTokens = result.PromptTokens,
                        CompletionTokens = result.CompletionTokens,
                        TotalTokens = result.TotalTokens,
                        TimeToFirstTokenMs = result.TimeToFirstTokenMs,
                        LatencyMs = result.LatencyMs,
                        TokensPerSec = result.TokensPerSec
                    },
                    AudioChunks = audioChunks.Count > 0 ? audioChunks : null,
                    TranscribedText = result.TranscribedText != nint.Zero
                        ? Marshal.PtrToStringUTF8(result.TranscribedText)
                        : null,
                    SpeakerId = result.SpeakerId != nint.Zero
                        ? Marshal.PtrToStringUTF8(result.SpeakerId)
                        : null,
                    SpeakerName = result.SpeakerName != nint.Zero
                        ? Marshal.PtrToStringUTF8(result.SpeakerName)
                        : null,
                    DetectedIntent = result.DetectedIntent != nint.Zero
                        ? Marshal.PtrToStringUTF8(result.DetectedIntent)
                        : null,
                    DetectedTools = MarshalDetectedTools(result.DetectedTools, result.DetectedToolsCount)
                };
            }
            finally
            {
                NativeMethods.tentaflow_free_chat_completion(result);
            }
        }
        finally
        {
            foreach (var handle in handles)
            {
                handle.Free();
            }
        }
    }

    /// <summary>
    /// Konwertuje tablicę native DetectedToolCall na managed List.
    /// </summary>
    private static IReadOnlyList<DetectedToolCall>? MarshalDetectedTools(nint toolsPtr, uint count)
    {
        if (toolsPtr == nint.Zero || count == 0)
            return null;

        var tools = new List<DetectedToolCall>((int)count);
        var structSize = Marshal.SizeOf<DetectedToolCallNative>();

        for (int i = 0; i < count; i++)
        {
            var toolPtr = toolsPtr + (i * structSize);
            var native = Marshal.PtrToStructure<DetectedToolCallNative>(toolPtr);

            tools.Add(new DetectedToolCall
            {
                CallId = Marshal.PtrToStringUTF8(native.CallId) ?? string.Empty,
                ToolName = Marshal.PtrToStringUTF8(native.ToolName) ?? string.Empty,
                Parameters = Marshal.PtrToStringUTF8(native.Parameters) ?? "{}",
                IsComplete = native.IsComplete != 0,
                MissingParams = MarshalStringArray(native.MissingParams, native.MissingParamsCount),
                ExecutionResult = MarshalToolExecutionResult(native.ExecutionResult),
                FollowUpQuestion = native.FollowUpQuestion != nint.Zero
                    ? Marshal.PtrToStringUTF8(native.FollowUpQuestion)
                    : null
            });
        }

        return tools;
    }

    /// <summary>
    /// Konwertuje tablicę native stringów na managed List.
    /// </summary>
    private static IReadOnlyList<string>? MarshalStringArray(nint arrayPtr, uint count)
    {
        if (arrayPtr == nint.Zero || count == 0)
            return null;

        var result = new List<string>((int)count);
        for (int i = 0; i < count; i++)
        {
            var strPtr = Marshal.ReadIntPtr(arrayPtr, i * nint.Size);
            if (strPtr != nint.Zero)
            {
                var str = Marshal.PtrToStringUTF8(strPtr);
                if (str != null)
                    result.Add(str);
            }
        }
        return result.Count > 0 ? result : null;
    }

    /// <summary>
    /// Konwertuje native DetectedToolExecutionResult na managed.
    /// </summary>
    private static DetectedToolExecutionResult? MarshalToolExecutionResult(nint resultPtr)
    {
        if (resultPtr == nint.Zero)
            return null;

        var native = Marshal.PtrToStructure<DetectedToolExecutionResultNative>(resultPtr);
        return new DetectedToolExecutionResult
        {
            Success = native.Success != 0,
            Message = Marshal.PtrToStringUTF8(native.Message) ?? string.Empty,
            Data = native.Data != nint.Zero ? Marshal.PtrToStringUTF8(native.Data) : null,
            Error = native.Error != nint.Zero ? Marshal.PtrToStringUTF8(native.Error) : null
        };
    }

    // =========================================================================
    // REQUEST CANCELLATION
    // =========================================================================

    /// <summary>
    /// Anuluje trwający request.
    /// </summary>
    /// <param name="requestId">ID requestu do anulowania.</param>
    /// <param name="reason">Opcjonalny powód anulowania.</param>
    /// <returns>True jeśli request został anulowany, false jeśli nie znaleziono.</returns>
    public bool CancelRequest(string requestId, string? reason = null)
    {
        ThrowIfDisposed();
        ArgumentException.ThrowIfNullOrEmpty(requestId);

        var result = NativeMethods.tentaflow_cancel_request(_clientHandle, requestId, reason);

        try
        {
            if (result.Error != nint.Zero)
            {
                var error = Marshal.PtrToStringUTF8(result.Error);
                throw new TentaFlowException($"Cancel request error: {error}");
            }

            return result.Success;
        }
        finally
        {
            NativeMethods.tentaflow_free_cancel(result);
        }
    }

    // =========================================================================
    // TTS (Text-to-Speech)
    // =========================================================================

    /// <summary>
    /// Generuje audio z tekstu (Text-to-Speech).
    /// </summary>
    /// <param name="model">Nazwa modelu TTS.</param>
    /// <param name="text">Tekst do zamiany na mowę.</param>
    /// <param name="voice">Nazwa głosu.</param>
    /// <param name="format">Format audio (np. "mp3", "opus").</param>
    /// <returns>Wynik TTS z danymi audio.</returns>
    public TtsResult TextToSpeech(string model, string text, string voice, string? format = null)
    {
        ThrowIfDisposed();
        ArgumentException.ThrowIfNullOrEmpty(model);
        ArgumentException.ThrowIfNullOrEmpty(text);
        ArgumentException.ThrowIfNullOrEmpty(voice);

        var result = NativeMethods.tentaflow_tts(_clientHandle, model, text, voice, format);

        try
        {
            if (result.Error != nint.Zero)
            {
                var error = Marshal.PtrToStringUTF8(result.Error);
                throw new TentaFlowException($"TTS error: {error}");
            }

            var audioData = new byte[(int)result.AudioLen];
            Marshal.Copy(result.AudioData, audioData, 0, audioData.Length);

            return new TtsResult
            {
                AudioData = audioData,
                Format = Marshal.PtrToStringUTF8(result.Format) ?? format ?? "wav",
                LatencyMs = result.LatencyMs,
                AudioDurationSec = result.AudioDurationSec
            };
        }
        finally
        {
            NativeMethods.tentaflow_free_tts(result);
        }
    }

    // =========================================================================
    // STT (Speech-to-Text)
    // =========================================================================

    /// <summary>
    /// Transkrybuje audio na tekst (Speech-to-Text).
    /// </summary>
    /// <param name="model">Nazwa modelu STT.</param>
    /// <param name="audioData">Surowe dane audio.</param>
    /// <param name="language">Kod języka (opcjonalny).</param>
    /// <returns>Wynik STT z transkrypcją.</returns>
    public SttResult SpeechToText(string model, byte[] audioData, string? language = null)
    {
        ThrowIfDisposed();
        ArgumentException.ThrowIfNullOrEmpty(model);
        ArgumentNullException.ThrowIfNull(audioData);

        if (audioData.Length == 0)
        {
            throw new ArgumentException("Audio data cannot be empty.", nameof(audioData));
        }

        var dataHandle = GCHandle.Alloc(audioData, GCHandleType.Pinned);
        try
        {
            var result = NativeMethods.tentaflow_stt(
                _clientHandle,
                model,
                dataHandle.AddrOfPinnedObject(),
                (nuint)audioData.Length,
                language);

            try
            {
                if (result.Error != nint.Zero)
                {
                    var error = Marshal.PtrToStringUTF8(result.Error);
                    throw new TentaFlowException($"STT error: {error}");
                }

                return new SttResult
                {
                    Text = Marshal.PtrToStringUTF8(result.Text) ?? string.Empty,
                    Language = Marshal.PtrToStringUTF8(result.Language),
                    DurationSeconds = result.DurationSeconds
                };
            }
            finally
            {
                NativeMethods.tentaflow_free_stt(result);
            }
        }
        finally
        {
            dataHandle.Free();
        }
    }

    /// <summary>
    /// Transkrybuje audio na tekst z zaawansowanymi opcjami (Speech-to-Text).
    /// Obsługuje filtrowanie halucynacji i format verbose_json z segmentami.
    /// </summary>
    /// <param name="model">Nazwa modelu STT.</param>
    /// <param name="audioData">Surowe dane audio.</param>
    /// <param name="options">Opcje STT z parametrami filtrowania.</param>
    /// <returns>Szczegółowy wynik STT z segmentami i metrykami filtrowania.</returns>
    public SttDetailedResult SpeechToText(string model, byte[] audioData, SttOptions options)
    {
        ThrowIfDisposed();
        ArgumentException.ThrowIfNullOrEmpty(model);
        ArgumentNullException.ThrowIfNull(audioData);
        ArgumentNullException.ThrowIfNull(options);

        if (audioData.Length == 0)
        {
            throw new ArgumentException("Audio data cannot be empty.", nameof(audioData));
        }

        var handles = new List<GCHandle>();
        var dataHandle = GCHandle.Alloc(audioData, GCHandleType.Pinned);
        handles.Add(dataHandle);

        try
        {
            // Przygotuj native options
            var nativeOptions = new SttOptionsNative
            {
                Language = MarshalString(options.Language, handles),
                Prompt = MarshalString(options.Prompt, handles),
                ResponseFormat = MarshalString(options.ResponseFormat, handles),
                Temperature = options.Temperature ?? -1.0f,
                TimestampGranularities = MarshalString(options.TimestampGranularities, handles),
                NoSpeechThreshold = options.NoSpeechThreshold ?? -1.0f,
                AvgLogprobThreshold = options.AvgLogprobThreshold ?? -100.0f,
                CompressionRatioThreshold = options.CompressionRatioThreshold ?? -1.0f
            };

            var result = NativeMethods.tentaflow_stt_with_options(
                _clientHandle,
                model,
                dataHandle.AddrOfPinnedObject(),
                (nuint)audioData.Length,
                nativeOptions);

            try
            {
                if (result.Error != nint.Zero)
                {
                    var error = Marshal.PtrToStringUTF8(result.Error);
                    throw new TentaFlowException($"STT error: {error}");
                }

                // Marshal segments
                var segments = new List<SttSegment>();
                if (result.Segments != nint.Zero && result.SegmentsCount > 0)
                {
                    var segmentSize = Marshal.SizeOf<SttSegmentNative>();
                    for (int i = 0; i < (int)result.SegmentsCount; i++)
                    {
                        var segmentPtr = result.Segments + (i * segmentSize);
                        var segmentNative = Marshal.PtrToStructure<SttSegmentNative>(segmentPtr);

                        segments.Add(new SttSegment
                        {
                            Id = segmentNative.Id,
                            Start = segmentNative.Start,
                            End = segmentNative.End,
                            Text = Marshal.PtrToStringUTF8(segmentNative.Text) ?? string.Empty,
                            AvgLogprob = segmentNative.AvgLogprob,
                            NoSpeechProb = segmentNative.NoSpeechProb,
                            CompressionRatio = segmentNative.CompressionRatio,
                            Temperature = segmentNative.Temperature,
                            SpeakerLabel = Marshal.PtrToStringUTF8(segmentNative.SpeakerLabel),
                            SpeakerSimilarity = segmentNative.SpeakerSimilarity >= 0 ? segmentNative.SpeakerSimilarity : null,
                            IsKnownSpeaker = segmentNative.IsKnownSpeaker >= 0 ? segmentNative.IsKnownSpeaker == 1 : null
                        });
                    }
                }

                return new SttDetailedResult
                {
                    Text = Marshal.PtrToStringUTF8(result.Text) ?? string.Empty,
                    Language = Marshal.PtrToStringUTF8(result.Language),
                    DurationSeconds = result.DurationSeconds,
                    Segments = segments,
                    FilteredSegmentsCount = result.FilteredSegmentsCount,
                    LatencyMs = result.LatencyMs
                };
            }
            finally
            {
                NativeMethods.tentaflow_free_stt_detailed(result);
            }
        }
        finally
        {
            foreach (var handle in handles)
            {
                handle.Free();
            }
        }
    }

    // =========================================================================
    // SPEAKER ENROLLMENT
    // =========================================================================

    /// <summary>
    /// Rejestruje nowego mówcę lub aktualizuje istniejącego z próbkami audio.
    /// </summary>
    /// <param name="speakerId">Unikalny identyfikator mówcy.</param>
    /// <param name="speakerName">Nazwa mówcy.</param>
    /// <param name="audioSamples">Lista próbek audio (każda próbka to tablica bajtów).</param>
    /// <param name="metadata">Opcjonalne metadane mówcy.</param>
    /// <returns>Wynik rejestracji mówcy.</returns>
    public SpeakerEnrollResult SpeakerEnroll(
        string speakerId,
        string speakerName,
        IEnumerable<byte[]> audioSamples,
        IDictionary<string, string>? metadata = null)
    {
        ThrowIfDisposed();
        ArgumentException.ThrowIfNullOrEmpty(speakerId);
        ArgumentException.ThrowIfNullOrEmpty(speakerName);
        ArgumentNullException.ThrowIfNull(audioSamples);

        var samplesList = audioSamples.ToList();
        if (samplesList.Count == 0)
        {
            throw new ArgumentException("At least one audio sample is required.", nameof(audioSamples));
        }

        var handles = new List<GCHandle>();
        try
        {
            // Pin each audio sample and create arrays of pointers and lengths
            var samplePtrs = new nint[samplesList.Count];
            var sampleLengths = new nuint[samplesList.Count];

            for (int i = 0; i < samplesList.Count; i++)
            {
                var sampleHandle = GCHandle.Alloc(samplesList[i], GCHandleType.Pinned);
                handles.Add(sampleHandle);
                samplePtrs[i] = sampleHandle.AddrOfPinnedObject();
                sampleLengths[i] = (nuint)samplesList[i].Length;
            }

            var ptrHandle = GCHandle.Alloc(samplePtrs, GCHandleType.Pinned);
            handles.Add(ptrHandle);
            var lengthHandle = GCHandle.Alloc(sampleLengths, GCHandleType.Pinned);
            handles.Add(lengthHandle);

            var result = NativeMethods.tentaflow_speaker_enroll(
                _clientHandle,
                speakerId,
                speakerName,
                ptrHandle.AddrOfPinnedObject(),
                lengthHandle.AddrOfPinnedObject(),
                (nuint)samplesList.Count);

            try
            {
                if (result.Error != nint.Zero)
                {
                    var error = Marshal.PtrToStringUTF8(result.Error);
                    throw new TentaFlowException($"Speaker enroll error: {error}");
                }

                return new SpeakerEnrollResult
                {
                    SpeakerId = Marshal.PtrToStringUTF8(result.SpeakerId) ?? speakerId,
                    SpeakerName = Marshal.PtrToStringUTF8(result.SpeakerName) ?? speakerName,
                    SamplesProcessed = result.SamplesProcessed,
                    EmbeddingsAdded = result.EmbeddingsAdded,
                    IsNew = result.IsNew != 0,
                    LatencyMs = result.LatencyMs
                };
            }
            finally
            {
                NativeMethods.tentaflow_free_speaker_enroll(result);
            }
        }
        finally
        {
            foreach (var handle in handles)
            {
                handle.Free();
            }
        }
    }

    /// <summary>
    /// Dodaje próbki audio do istniejącego mówcy.
    /// </summary>
    /// <param name="speakerId">Identyfikator mówcy.</param>
    /// <param name="audioSamples">Lista próbek audio do dodania.</param>
    /// <returns>Wynik dodania próbek.</returns>
    public SpeakerEnrollResult SpeakerAddSamples(string speakerId, IEnumerable<byte[]> audioSamples)
    {
        ThrowIfDisposed();
        ArgumentException.ThrowIfNullOrEmpty(speakerId);
        ArgumentNullException.ThrowIfNull(audioSamples);

        var samplesList = audioSamples.ToList();
        if (samplesList.Count == 0)
        {
            throw new ArgumentException("At least one audio sample is required.", nameof(audioSamples));
        }

        var handles = new List<GCHandle>();
        try
        {
            var samplePtrs = new nint[samplesList.Count];
            var sampleLengths = new nuint[samplesList.Count];

            for (int i = 0; i < samplesList.Count; i++)
            {
                var sampleHandle = GCHandle.Alloc(samplesList[i], GCHandleType.Pinned);
                handles.Add(sampleHandle);
                samplePtrs[i] = sampleHandle.AddrOfPinnedObject();
                sampleLengths[i] = (nuint)samplesList[i].Length;
            }

            var ptrHandle = GCHandle.Alloc(samplePtrs, GCHandleType.Pinned);
            handles.Add(ptrHandle);
            var lengthHandle = GCHandle.Alloc(sampleLengths, GCHandleType.Pinned);
            handles.Add(lengthHandle);

            var result = NativeMethods.tentaflow_speaker_add_samples(
                _clientHandle,
                speakerId,
                ptrHandle.AddrOfPinnedObject(),
                lengthHandle.AddrOfPinnedObject(),
                (nuint)samplesList.Count);

            try
            {
                if (result.Error != nint.Zero)
                {
                    var error = Marshal.PtrToStringUTF8(result.Error);
                    throw new TentaFlowException($"Speaker add samples error: {error}");
                }

                return new SpeakerEnrollResult
                {
                    SpeakerId = Marshal.PtrToStringUTF8(result.SpeakerId) ?? speakerId,
                    SpeakerName = Marshal.PtrToStringUTF8(result.SpeakerName) ?? string.Empty,
                    SamplesProcessed = result.SamplesProcessed,
                    EmbeddingsAdded = result.EmbeddingsAdded,
                    IsNew = result.IsNew != 0,
                    LatencyMs = result.LatencyMs
                };
            }
            finally
            {
                NativeMethods.tentaflow_free_speaker_enroll(result);
            }
        }
        finally
        {
            foreach (var handle in handles)
            {
                handle.Free();
            }
        }
    }

    /// <summary>
    /// Usuwa mówcę z bazy danych.
    /// </summary>
    /// <param name="speakerId">Identyfikator mówcy do usunięcia.</param>
    /// <returns>Wynik usunięcia.</returns>
    public SpeakerRemoveResult SpeakerRemove(string speakerId)
    {
        ThrowIfDisposed();
        ArgumentException.ThrowIfNullOrEmpty(speakerId);

        var result = NativeMethods.tentaflow_speaker_remove(_clientHandle, speakerId);

        try
        {
            if (result.Error != nint.Zero)
            {
                var error = Marshal.PtrToStringUTF8(result.Error);
                throw new TentaFlowException($"Speaker remove error: {error}");
            }

            return new SpeakerRemoveResult
            {
                Success = result.Success != 0,
                SpeakerId = Marshal.PtrToStringUTF8(result.SpeakerId) ?? speakerId,
                LatencyMs = result.LatencyMs
            };
        }
        finally
        {
            NativeMethods.tentaflow_free_speaker_remove(result);
        }
    }

    /// <summary>
    /// Zwraca listę wszystkich zarejestrowanych mówców.
    /// </summary>
    /// <returns>Lista mówców.</returns>
    public SpeakerListResult SpeakerList()
    {
        ThrowIfDisposed();

        var result = NativeMethods.tentaflow_speaker_list(_clientHandle);

        try
        {
            if (result.Error != nint.Zero)
            {
                var error = Marshal.PtrToStringUTF8(result.Error);
                throw new TentaFlowException($"Speaker list error: {error}");
            }

            var speakers = new List<SpeakerEntry>();
            if (result.Speakers != nint.Zero && result.SpeakersCount > 0)
            {
                var entrySize = Marshal.SizeOf<SpeakerEntryNative>();
                for (int i = 0; i < (int)result.SpeakersCount; i++)
                {
                    var entryPtr = result.Speakers + (i * entrySize);
                    var entryNative = Marshal.PtrToStructure<SpeakerEntryNative>(entryPtr);

                    speakers.Add(new SpeakerEntry
                    {
                        SpeakerId = Marshal.PtrToStringUTF8(entryNative.SpeakerId) ?? string.Empty,
                        SpeakerName = Marshal.PtrToStringUTF8(entryNative.SpeakerName) ?? string.Empty
                    });
                }
            }

            return new SpeakerListResult
            {
                Speakers = speakers,
                TotalCount = result.TotalCount,
                LatencyMs = result.LatencyMs
            };
        }
        finally
        {
            NativeMethods.tentaflow_free_speaker_list(result);
        }
    }

    /// <summary>
    /// Zwraca informacje o bazie głosów.
    /// </summary>
    /// <returns>Informacje o bazie głosów.</returns>
    public SpeakerInfoResult SpeakerInfo()
    {
        ThrowIfDisposed();

        var result = NativeMethods.tentaflow_speaker_info(_clientHandle);

        try
        {
            if (result.Error != nint.Zero)
            {
                var error = Marshal.PtrToStringUTF8(result.Error);
                throw new TentaFlowException($"Speaker info error: {error}");
            }

            return new SpeakerInfoResult
            {
                SpeakerCount = result.SpeakerCount,
                EmbeddingDim = result.EmbeddingDim,
                SimilarityThreshold = result.SimilarityThreshold,
                LatencyMs = result.LatencyMs
            };
        }
        finally
        {
            NativeMethods.tentaflow_free_speaker_info(result);
        }
    }

    /// <summary>
    /// Identyfikuje mówcę na podstawie próbki audio.
    /// </summary>
    /// <param name="audioData">Dane audio do identyfikacji.</param>
    /// <param name="threshold">Opcjonalny próg pewności (0.0-1.0).</param>
    /// <returns>Wynik identyfikacji.</returns>
    public SpeakerIdentifyResult SpeakerIdentify(byte[] audioData, float? threshold = null)
    {
        ThrowIfDisposed();
        ArgumentNullException.ThrowIfNull(audioData);

        if (audioData.Length == 0)
        {
            throw new ArgumentException("Audio data cannot be empty.", nameof(audioData));
        }

        var dataHandle = GCHandle.Alloc(audioData, GCHandleType.Pinned);
        try
        {
            var result = NativeMethods.tentaflow_speaker_identify(
                _clientHandle,
                dataHandle.AddrOfPinnedObject(),
                (nuint)audioData.Length,
                threshold ?? -1.0f);

            try
            {
                if (result.Error != nint.Zero)
                {
                    var error = Marshal.PtrToStringUTF8(result.Error);
                    throw new TentaFlowException($"Speaker identify error: {error}");
                }

                return new SpeakerIdentifyResult
                {
                    IsMatch = result.IsMatch != 0,
                    SpeakerId = Marshal.PtrToStringUTF8(result.SpeakerId),
                    SpeakerName = Marshal.PtrToStringUTF8(result.SpeakerName),
                    Similarity = result.Similarity,
                    Threshold = result.Threshold,
                    LatencyMs = result.LatencyMs
                };
            }
            finally
            {
                NativeMethods.tentaflow_free_speaker_identify(result);
            }
        }
        finally
        {
            dataHandle.Free();
        }
    }

    /// <summary>
    /// Weryfikuje czy próbka audio należy do określonego mówcy.
    /// </summary>
    /// <param name="speakerId">Identyfikator mówcy do weryfikacji.</param>
    /// <param name="audioData">Dane audio do weryfikacji.</param>
    /// <param name="threshold">Opcjonalny próg pewności (0.0-1.0).</param>
    /// <returns>Wynik weryfikacji.</returns>
    public SpeakerVerifyResult SpeakerVerify(string speakerId, byte[] audioData, float? threshold = null)
    {
        ThrowIfDisposed();
        ArgumentException.ThrowIfNullOrEmpty(speakerId);
        ArgumentNullException.ThrowIfNull(audioData);

        if (audioData.Length == 0)
        {
            throw new ArgumentException("Audio data cannot be empty.", nameof(audioData));
        }

        var dataHandle = GCHandle.Alloc(audioData, GCHandleType.Pinned);
        try
        {
            var result = NativeMethods.tentaflow_speaker_verify(
                _clientHandle,
                speakerId,
                dataHandle.AddrOfPinnedObject(),
                (nuint)audioData.Length,
                threshold ?? -1.0f);

            try
            {
                if (result.Error != nint.Zero)
                {
                    var error = Marshal.PtrToStringUTF8(result.Error);
                    throw new TentaFlowException($"Speaker verify error: {error}");
                }

                return new SpeakerVerifyResult
                {
                    SpeakerId = Marshal.PtrToStringUTF8(result.SpeakerId) ?? speakerId,
                    IsVerified = result.IsVerified != 0,
                    Similarity = result.Similarity,
                    Threshold = result.Threshold,
                    DetectedSpeakerId = Marshal.PtrToStringUTF8(result.DetectedSpeakerId),
                    LatencyMs = result.LatencyMs
                };
            }
            finally
            {
                NativeMethods.tentaflow_free_speaker_verify(result);
            }
        }
        finally
        {
            dataHandle.Free();
        }
    }

    // =========================================================================
    // CONVERSATION SESSIONS
    // =========================================================================

    /// <summary>
    /// Rozpoczyna nową sesję konwersacji głosowej.
    /// </summary>
    /// <param name="config">Konfiguracja sesji.</param>
    /// <returns>Wynik z ID sesji i stanem początkowym.</returns>
    public ConversationStartResult ConversationStart(ConversationSessionConfig config)
    {
        ThrowIfDisposed();
        ArgumentNullException.ThrowIfNull(config);

        var handles = new List<GCHandle>();
        try
        {
            var nativeConfig = new ConversationSessionConfigNative
            {
                Mode = (byte)config.Mode,
                UserId = MarshalString(config.UserId, handles),
                Language = MarshalString(config.Language, handles),
                SttModel = MarshalString(config.SttModel, handles),
                WakeWords = config.WakeWords != null
                    ? MarshalString(string.Join(",", config.WakeWords), handles)
                    : nint.Zero,
                StopPhrases = config.StopPhrases != null
                    ? MarshalString(string.Join(",", config.StopPhrases), handles)
                    : nint.Zero,
                SilenceTimeoutMs = config.SilenceTimeoutMs,
                PreWakeBufferMs = config.PreWakeBufferMs
            };

            var result = NativeMethods.tentaflow_conversation_start(_clientHandle, ref nativeConfig);

            try
            {
                if (result.Error != nint.Zero)
                {
                    var error = Marshal.PtrToStringUTF8(result.Error);
                    throw new TentaFlowException($"Conversation start error: {error}");
                }

                return new ConversationStartResult
                {
                    SessionId = Marshal.PtrToStringUTF8(result.SessionId) ?? string.Empty,
                    State = (SessionState)result.State
                };
            }
            finally
            {
                NativeMethods.tentaflow_free_conversation_start(result);
            }
        }
        finally
        {
            foreach (var handle in handles)
            {
                handle.Free();
            }
        }
    }

    /// <summary>
    /// Wysyła audio do aktywnej sesji konwersacji i odbiera zdarzenia.
    /// </summary>
    /// <param name="sessionId">ID sesji.</param>
    /// <param name="audioData">Dane audio.</param>
    /// <param name="timestampMs">Timestamp audio w ms (opcjonalny, 0 = teraz).</param>
    /// <returns>Wynik z listą zdarzeń i ewentualną transkrypcją.</returns>
    public ConversationAudioResult ConversationSendAudio(string sessionId, byte[] audioData, ulong timestampMs = 0)
    {
        ThrowIfDisposed();
        ArgumentException.ThrowIfNullOrEmpty(sessionId);
        ArgumentNullException.ThrowIfNull(audioData);

        if (audioData.Length == 0)
        {
            throw new ArgumentException("Audio data cannot be empty.", nameof(audioData));
        }

        var dataHandle = GCHandle.Alloc(audioData, GCHandleType.Pinned);
        try
        {
            var result = NativeMethods.tentaflow_conversation_audio(
                _clientHandle,
                sessionId,
                dataHandle.AddrOfPinnedObject(),
                (nuint)audioData.Length,
                timestampMs);

            try
            {
                if (result.Error != nint.Zero)
                {
                    var error = Marshal.PtrToStringUTF8(result.Error);
                    throw new TentaFlowException($"Conversation audio error: {error}");
                }

                // Marshal events
                var events = new List<ConversationEvent>();
                if (result.Events != nint.Zero && result.EventsCount > 0)
                {
                    var eventSize = Marshal.SizeOf<ConversationEventNative>();
                    for (int i = 0; i < (int)result.EventsCount; i++)
                    {
                        var eventPtr = result.Events + (i * eventSize);
                        var eventNative = Marshal.PtrToStructure<ConversationEventNative>(eventPtr);

                        events.Add(new ConversationEvent
                        {
                            EventType = (ConversationEventType)eventNative.EventType,
                            TimestampMs = eventNative.TimestampMs,
                            Transcription = Marshal.PtrToStringUTF8(eventNative.Transcription),
                            Confidence = eventNative.Confidence,
                            WakeWord = Marshal.PtrToStringUTF8(eventNative.WakeWord),
                            StopPhrase = Marshal.PtrToStringUTF8(eventNative.StopPhrase),
                            UserId = Marshal.PtrToStringUTF8(eventNative.UserId)
                        });
                    }
                }

                return new ConversationAudioResult
                {
                    SessionId = Marshal.PtrToStringUTF8(result.SessionId) ?? sessionId,
                    State = (SessionState)result.State,
                    Events = events,
                    Transcription = Marshal.PtrToStringUTF8(result.Transcription),
                    Confidence = result.Confidence
                };
            }
            finally
            {
                NativeMethods.tentaflow_free_conversation_audio(result);
            }
        }
        finally
        {
            dataHandle.Free();
        }
    }

    /// <summary>
    /// Kończy sesję konwersacji i zwraca statystyki.
    /// </summary>
    /// <param name="sessionId">ID sesji do zakończenia.</param>
    /// <param name="reason">Opcjonalny powód zakończenia.</param>
    /// <returns>Wynik z końcową transkrypcją i statystykami.</returns>
    public ConversationEndResult ConversationEnd(string sessionId, string? reason = null)
    {
        ThrowIfDisposed();
        ArgumentException.ThrowIfNullOrEmpty(sessionId);

        var result = NativeMethods.tentaflow_conversation_end(_clientHandle, sessionId, reason);

        try
        {
            if (result.Error != nint.Zero)
            {
                var error = Marshal.PtrToStringUTF8(result.Error);
                throw new TentaFlowException($"Conversation end error: {error}");
            }

            return new ConversationEndResult
            {
                SessionId = Marshal.PtrToStringUTF8(result.SessionId) ?? sessionId,
                FinalTranscription = Marshal.PtrToStringUTF8(result.FinalTranscription),
                Stats = new ConversationSessionStats
                {
                    TotalDurationMs = result.Stats.TotalDurationMs,
                    ActiveSpeechMs = result.Stats.ActiveSpeechMs,
                    WakeWordsDetected = result.Stats.WakeWordsDetected,
                    TranscriptionsCount = result.Stats.TranscriptionsCount,
                    SpeakersDetected = result.Stats.SpeakersDetected
                }
            };
        }
        finally
        {
            NativeMethods.tentaflow_free_conversation_end(result);
        }
    }

    /// <summary>
    /// Pobiera status aktywnej sesji konwersacji.
    /// </summary>
    /// <param name="sessionId">ID sesji.</param>
    /// <returns>Informacje o stanie sesji.</returns>
    public ConversationStatusResult ConversationStatus(string sessionId)
    {
        ThrowIfDisposed();
        ArgumentException.ThrowIfNullOrEmpty(sessionId);

        var result = NativeMethods.tentaflow_conversation_status(_clientHandle, sessionId);

        try
        {
            if (result.Error != nint.Zero)
            {
                var error = Marshal.PtrToStringUTF8(result.Error);
                throw new TentaFlowException($"Conversation status error: {error}");
            }

            return new ConversationStatusResult
            {
                SessionId = Marshal.PtrToStringUTF8(result.SessionId) ?? sessionId,
                Exists = result.Exists != 0,
                State = (SessionState)result.State,
                Mode = (SessionMode)result.Mode,
                DurationMs = result.DurationMs,
                LastActivityMs = result.LastActivityMs
            };
        }
        finally
        {
            NativeMethods.tentaflow_free_conversation_status(result);
        }
    }

    // =========================================================================
    // HELPERS
    // =========================================================================

    private static nint MarshalString(string? str, List<GCHandle> handles)
    {
        if (string.IsNullOrEmpty(str))
        {
            return nint.Zero;
        }

        var bytes = System.Text.Encoding.UTF8.GetBytes(str + "\0");
        var handle = GCHandle.Alloc(bytes, GCHandleType.Pinned);
        handles.Add(handle);
        return handle.AddrOfPinnedObject();
    }

    private void ThrowIfDisposed()
    {
        ObjectDisposedException.ThrowIf(_disposed, this);
    }

    // =========================================================================
    // DISPOSE
    // =========================================================================

    /// <summary>
    /// Zamyka połączenie i zwalnia zasoby natywne.
    /// </summary>
    public void Dispose()
    {
        if (_disposed)
        {
            return;
        }

        if (_clientHandle != nint.Zero)
        {
            NativeMethods.tentaflow_client_free(_clientHandle);
            _clientHandle = nint.Zero;
        }

        _disposed = true;
        GC.SuppressFinalize(this);
    }

    /// <summary>
    /// Finalizator - zwalnia zasoby jeśli Dispose nie został wywołany.
    /// </summary>
    ~TentaFlowClient()
    {
        Dispose();
    }
}
