// ============================================================================
// TENTAFLOW CLIENT.TEST - Test aplikacji klienta .NET
// ============================================================================
//
// CEL:
// Program testowy do weryfikacji całej ścieżki komunikacji:
// .NET → P/Invoke → tentaflow_client_native.so → QUIC+TLS → Router
//
// URUCHOMIENIE:
// cd TentaFlow.Client.Test
// dotnet run
//
// KONFIGURACJA TESTÓW:
// Ustaw flagi poniżej na true/false aby włączyć/wyłączyć poszczególne testy.
//
// ============================================================================

using TentaFlow.Client;
using TentaFlow.Client.Models;
using TentaFlow.Client.Test;
using TentaFlow.Client.Test.Tests;

// ============================================================================
// KONFIGURACJA TESTÓW - ustaw true/false aby włączyć/wyłączyć testy
// ============================================================================

bool RUN_EMBEDDINGS = true;          // Test embeddingów
bool RUN_COMPLETION = true;          // Test chat completion (basic + streaming)
bool RUN_TTS = true;                 // Test Text-to-Speech (basic + LLM + streaming)
bool RUN_STT = true;                 // Test Speech-to-Text (basic + with options)
bool RUN_TOOLS = true;               // Test tools/function calling (Bielik 1.5B)
bool RUN_MEMORY = true;              // Test Memory (session-based conversation)
bool RUN_CONVERSATION = true;        // Test Conversation Sessions (voice assistant modes)

// ============================================================================
// KONFIGURACJA POŁĄCZENIA
// ============================================================================

var config = new ClientConfig
{
    RouterUrl = "quic://pj.nextapp.pl:3001",
    TimeoutMs = 30000
};

// ============================================================================
// GŁÓWNA LOGIKA TESTÓW
// ============================================================================

Console.WriteLine("╔══════════════════════════════════════════════════════════════╗");
Console.WriteLine("║           TEST KLIENTA .NET TENTAFLOW.AI                       ║");
Console.WriteLine("╚══════════════════════════════════════════════════════════════╝\n");

Console.WriteLine($"Router URL: {config.RouterUrl}");
Console.WriteLine($"CA: {config.CaPath ?? "(systemowe certyfikaty)"}");
Console.WriteLine();

// Wyświetl które testy są włączone
Console.WriteLine("Aktywne testy:");
Console.WriteLine($"  Embeddings:  {(RUN_EMBEDDINGS ? "✓" : "✗")}");
Console.WriteLine($"  Completion:  {(RUN_COMPLETION ? "✓" : "✗")}");
Console.WriteLine($"  TTS:         {(RUN_TTS ? "✓" : "✗")}");
Console.WriteLine($"  STT:         {(RUN_STT ? "✓" : "✗")}");
Console.WriteLine($"  Tools:       {(RUN_TOOLS ? "✓" : "✗")}");
Console.WriteLine($"  Memory:      {(RUN_MEMORY ? "✓" : "✗")}");
Console.WriteLine($"  Conversation:{(RUN_CONVERSATION ? "✓" : "✗")}");
Console.WriteLine();

try
{
    // === POŁĄCZENIE ===
    Console.WriteLine("─── Połączenie z Router ───");

    using var client = new TentaFlowClient(config);

    Console.WriteLine($"✓ Połączenie nawiązane");
    Console.WriteLine($"  IsConnected: {client.IsConnected}");

    // Zmienna do przechowywania audio z TTS (używane w testach STT)
    byte[]? ttsAudioData = null;
    string ttsOriginalText = "Cześć, to jest test serwera Text-to-Speech. Działa poprawnie przez Router.";

    // === EMBEDDINGS ===
    if (RUN_EMBEDDINGS)
    {
        EmbeddingsTests.Run(client);
    }

    // === COMPLETION ===
    if (RUN_COMPLETION)
    {
        CompletionTests.RunBasic(client);
        CompletionTests.RunStreaming(client);
    }

    // === TTS ===
    if (RUN_TTS)
    {
        var ttsResult = TtsTests.RunBasic(client);
        ttsAudioData = ttsResult.AudioData;

        TtsTests.RunLlmPlusTts(client);
        TtsTests.RunStreamingLlmPlusTts(client);
        TtsTests.RunStreamingTts(client);
    }

    // === STT ===
    if (RUN_STT)
    {
        // Jeśli mamy audio z TTS, użyj go do testu STT
        if (ttsAudioData != null)
        {
            SttTests.RunBasic(client, ttsAudioData, ttsOriginalText);
            SttTests.RunWithOptions(client, ttsAudioData);
        }
        else
        {
            // Użyj pliku pit.wav jako fallback gdy TTS nie działa
            var pitWavPath = Path.Combine(Directory.GetCurrentDirectory(), "..", "TentaFlow.TTS", "pit.wav");
            if (File.Exists(pitWavPath))
            {
                SttTests.RunWithFile(client, pitWavPath);
            }
            else
            {
                // Spróbuj wygenerować audio przez TTS
                Console.WriteLine("\n─── Generowanie audio dla testu STT ───");
                try
                {
                    var ttsResult = client.TextToSpeech(
                        model: "sherpa-tts",
                        text: ttsOriginalText,
                        voice: "jarvis",
                        format: "wav");
                    ttsAudioData = ttsResult.AudioData;
                    Console.WriteLine($"✓ Audio wygenerowane: {ttsAudioData.Length} bytes");

                    SttTests.RunBasic(client, ttsAudioData, ttsOriginalText);
                    SttTests.RunWithOptions(client, ttsAudioData);
                }
                catch (Exception ex)
                {
                    Console.WriteLine($"  (STT pominięto - TTS niedostępny: {ex.Message})");
                }
            }
        }
    }

    // === TOOLS ===
    if (RUN_TOOLS)
    {
        ToolsTests.RunFunctionCalling(client);
        ToolsTests.RunJsonOutput(client);
        ToolsTests.PrintSummary();
    }

    // === MEMORY ===
    if (RUN_MEMORY)
    {
        MemoryTests.RunBasicMemory(client);
        MemoryTests.RunMultiSessionMemory(client);
        if (RUN_TTS)
        {
            MemoryTests.RunMemoryWithTts(client);
        }
    }

    // === CONVERSATION SESSIONS ===
    if (RUN_CONVERSATION)
    {
        ConversationTests.PrintModesSummary();
        ConversationTests.RunBasicSession(client);
        ConversationTests.RunWakeWordSession(client);
        ConversationTests.RunStopPhraseSession(client);
        if (ttsAudioData != null)
        {
            ConversationTests.RunAudioSession(client, ttsAudioData);
        }
    }

    // === PODSUMOWANIE ===
    var tempDir = Path.GetTempPath();
    Console.WriteLine("\n╔══════════════════════════════════════════════════════════════╗");
    Console.WriteLine("║         WSZYSTKIE TESTY ZAKOŃCZONE POMYŚLNIE                 ║");
    Console.WriteLine("╠══════════════════════════════════════════════════════════════╣");
    Console.WriteLine($"║  Pliki audio zapisane w: {tempDir,-35}║");
    if (RUN_TTS)
    {
        Console.WriteLine("║    dotnet_tts_test.wav            - Basic TTS                ║");
        Console.WriteLine("║    dotnet_llm_tts_test.wav        - LLM + TTS                ║");
        Console.WriteLine("║    dotnet_stream_tts_test.wav     - Streaming + TTS          ║");
        Console.WriteLine("║    dotnet_streaming_tts_chunks.wav - Streaming chunks        ║");
    }
    if (RUN_MEMORY && RUN_TTS)
    {
        Console.WriteLine("║    dotnet_memory_tts_test.wav     - Memory + TTS             ║");
    }
    Console.WriteLine("╚══════════════════════════════════════════════════════════════╝");
}
catch (TentaFlowException ex)
{
    Console.WriteLine($"\n✗ BŁĄD TENTAFLOW: {ex.Message}");
}
catch (Exception ex)
{
    Console.WriteLine($"\n✗ BŁĄD: {ex.Message}");
    Console.WriteLine($"  Typ: {ex.GetType().Name}");
    Console.WriteLine($"  Stack: {ex.StackTrace}");
}
