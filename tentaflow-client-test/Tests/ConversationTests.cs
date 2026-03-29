using TentaFlow.Client;
using TentaFlow.Client.Models;

namespace TentaFlow.Client.Test.Tests;

/// <summary>
/// Testy Conversation Sessions - tryby pracy asystenta głosowego.
///
/// Tryby sesji:
/// - AlwaysOn (0): Zawsze aktywny, nie wymaga wake word
/// - WakeWordTimeout (1): Aktywacja przez wake word, deaktywacja po timeout
/// - WakeWordExplicitStop (2): Aktywacja przez wake word, deaktywacja przez stop phrase
/// </summary>
public static class ConversationTests
{
    /// <summary>
    /// Test podstawowy - start i stop sesji konwersacji.
    /// </summary>
    public static void RunBasicSession(TentaFlowClient client)
    {
        Console.WriteLine("\n─── TEST: Basic Conversation Session ───");

        var config = new ConversationSessionConfig
        {
            Mode = SessionMode.AlwaysOn,
            Language = "pl",
            SttModel = "whisper",
            SilenceTimeoutMs = 30000
        };

        Console.WriteLine($"  Mode: {config.Mode}");
        Console.WriteLine($"  Language: {config.Language}");

        // === START SESJI ===
        Console.WriteLine("\n  [1] Startuję sesję...");

        try
        {
            var startResult = client.ConversationStart(config);

            Console.WriteLine($"  ✓ Sesja utworzona");
            Console.WriteLine($"    Session ID: {startResult.SessionId}");
            Console.WriteLine($"    State: {startResult.State}");

            // === STATUS SESJI ===
            Console.WriteLine("\n  [2] Sprawdzam status sesji...");

            var status = client.ConversationStatus(startResult.SessionId);

            Console.WriteLine($"    Exists: {status.Exists}");
            Console.WriteLine($"    State: {status.State}");
            Console.WriteLine($"    Mode: {status.Mode}");
            Console.WriteLine($"    Duration: {status.DurationMs} ms");

            // === KONIEC SESJI ===
            Console.WriteLine("\n  [3] Kończę sesję...");

            var endResult = client.ConversationEnd(startResult.SessionId, "Test completed");

            Console.WriteLine($"  ✓ Sesja zakończona");
            Console.WriteLine($"    Session ID: {endResult.SessionId}");
            Console.WriteLine($"    Stats:");
            Console.WriteLine($"      Total duration: {endResult.Stats.TotalDurationMs} ms");
            Console.WriteLine($"      Active speech: {endResult.Stats.ActiveSpeechMs} ms");
            Console.WriteLine($"      Transcriptions: {endResult.Stats.TranscriptionsCount}");

            Console.WriteLine("\n  ✓ Test podstawowy zakończony pomyślnie!");
        }
        catch (TentaFlowException ex)
        {
            Console.WriteLine($"  ✗ Błąd TentaFlow: {ex.Message}");
            Console.WriteLine("    (To może oznaczać że serwer nie obsługuje jeszcze Conversation API)");
        }
        catch (Exception ex)
        {
            Console.WriteLine($"  ✗ Błąd: {ex.Message}");
        }
    }

    /// <summary>
    /// Test sesji z wake word - symuluje wykrywanie "Jarvis".
    /// </summary>
    public static void RunWakeWordSession(TentaFlowClient client)
    {
        Console.WriteLine("\n─── TEST: Wake Word Session (Timeout Mode) ───");

        var config = new ConversationSessionConfig
        {
            Mode = SessionMode.WakeWordTimeout,
            WakeWords = new[] { "jarvis", "hej jarvis", "ok jarvis" },
            SilenceTimeoutMs = 10000, // 10s timeout dla testu
            Language = "pl"
        };

        Console.WriteLine($"  Mode: {config.Mode}");
        Console.WriteLine($"  Wake words: {string.Join(", ", config.WakeWords ?? Array.Empty<string>())}");
        Console.WriteLine($"  Silence timeout: {config.SilenceTimeoutMs} ms");

        try
        {
            var startResult = client.ConversationStart(config);

            Console.WriteLine($"\n  ✓ Sesja wake word utworzona");
            Console.WriteLine($"    Session ID: {startResult.SessionId}");
            Console.WriteLine($"    Initial state: {startResult.State} (0=Inactive, czeka na wake word)");

            // Symulacja: W prawdziwym scenariuszu wysyłalibyśmy audio
            // i czekali na event WakeWordDetected
            Console.WriteLine("\n  (W produkcji: wysyłasz audio przez ConversationSendAudio)");
            Console.WriteLine("  (Serwer wykrywa 'Jarvis' i aktywuje sesję)");

            // Sprawdź status
            var status = client.ConversationStatus(startResult.SessionId);
            Console.WriteLine($"\n  Status: State={status.State}, Mode={status.Mode}");

            // Zakończ
            var endResult = client.ConversationEnd(startResult.SessionId, "Test done");
            Console.WriteLine($"\n  ✓ Sesja zakończona, duration={endResult.Stats.TotalDurationMs}ms");
        }
        catch (TentaFlowException ex)
        {
            Console.WriteLine($"  ✗ Błąd: {ex.Message}");
        }
    }

    /// <summary>
    /// Test sesji z explicit stop phrase - "Dzięki Jarvis, to koniec".
    /// </summary>
    public static void RunStopPhraseSession(TentaFlowClient client)
    {
        Console.WriteLine("\n─── TEST: Stop Phrase Session ───");

        var config = new ConversationSessionConfig
        {
            Mode = SessionMode.WakeWordExplicitStop,
            WakeWords = new[] { "jarvis" },
            StopPhrases = new[] { "dzięki jarvis", "to koniec", "wystarczy" },
            Language = "pl"
        };

        Console.WriteLine($"  Mode: {config.Mode}");
        Console.WriteLine($"  Wake words: {string.Join(", ", config.WakeWords ?? Array.Empty<string>())}");
        Console.WriteLine($"  Stop phrases: {string.Join(", ", config.StopPhrases ?? Array.Empty<string>())}");

        try
        {
            var startResult = client.ConversationStart(config);

            Console.WriteLine($"\n  ✓ Sesja stop-phrase utworzona");
            Console.WriteLine($"    Session ID: {startResult.SessionId}");

            Console.WriteLine("\n  (W produkcji: sesja trwa aż użytkownik powie stop phrase)");
            Console.WriteLine("  (Np. 'Dzięki Jarvis, to koniec')");

            // Zakończ manualnie
            var endResult = client.ConversationEnd(startResult.SessionId, "Manual stop");
            Console.WriteLine($"\n  ✓ Sesja zakończona manualnie");
        }
        catch (TentaFlowException ex)
        {
            Console.WriteLine($"  ✗ Błąd: {ex.Message}");
        }
    }

    /// <summary>
    /// Test wysyłania audio do sesji (wymaga prawdziwego audio).
    /// </summary>
    public static void RunAudioSession(TentaFlowClient client, byte[]? audioData = null)
    {
        Console.WriteLine("\n─── TEST: Conversation with Audio ───");

        if (audioData == null || audioData.Length == 0)
        {
            Console.WriteLine("  (Brak danych audio - generuję testowe audio przez TTS)");

            try
            {
                var ttsResult = client.TextToSpeech(
                    model: "sherpa-tts",
                    text: "Hej Jarvis, jaka jest dzisiaj pogoda?",
                    voice: "jarvis",
                    format: "wav");

                audioData = ttsResult.AudioData;
                Console.WriteLine($"  Wygenerowano audio: {audioData.Length} bytes");
            }
            catch (Exception ex)
            {
                Console.WriteLine($"  ✗ Nie udało się wygenerować audio: {ex.Message}");
                return;
            }
        }

        var config = new ConversationSessionConfig
        {
            Mode = SessionMode.AlwaysOn,
            Language = "pl"
        };

        try
        {
            // Start sesji
            var startResult = client.ConversationStart(config);
            Console.WriteLine($"\n  ✓ Sesja utworzona: {startResult.SessionId}");

            // Wyślij audio w chunkach (symulacja real-time)
            Console.WriteLine("\n  Wysyłam audio w chunkach...");

            int chunkSize = 16000 * 2; // ~1 sekunda audio (16kHz, 16-bit)
            int chunks = 0;
            int totalEvents = 0;

            for (int offset = 0; offset < audioData.Length; offset += chunkSize)
            {
                int size = Math.Min(chunkSize, audioData.Length - offset);
                var chunk = new byte[size];
                Array.Copy(audioData, offset, chunk, 0, size);

                ulong timestampMs = (ulong)(chunks * 1000); // 1s per chunk

                var audioResult = client.ConversationSendAudio(
                    startResult.SessionId,
                    chunk,
                    timestampMs);

                chunks++;
                totalEvents += audioResult.Events?.Count ?? 0;

                Console.Write($"\r  Chunk {chunks}: state={audioResult.State}, events={audioResult.Events?.Count ?? 0}");

                if (!string.IsNullOrEmpty(audioResult.Transcription))
                {
                    Console.WriteLine($"\n    Transcription: {audioResult.Transcription} (conf: {audioResult.Confidence:F2})");
                }

                // Przetwarzaj eventy
                if (audioResult.Events != null)
                {
                    foreach (var evt in audioResult.Events)
                    {
                        Console.WriteLine($"\n    Event: {evt.EventType}");
                        if (!string.IsNullOrEmpty(evt.Transcription))
                        {
                            Console.WriteLine($"      Text: {evt.Transcription}");
                        }
                        if (!string.IsNullOrEmpty(evt.WakeWord))
                        {
                            Console.WriteLine($"      Wake word: {evt.WakeWord}");
                        }
                    }
                }

                Thread.Sleep(100); // Symulacja real-time
            }

            Console.WriteLine($"\n\n  Wysłano {chunks} chunków, otrzymano {totalEvents} eventów");

            // Zakończ sesję
            var endResult = client.ConversationEnd(startResult.SessionId, "Audio test done");

            Console.WriteLine($"\n  ✓ Test audio zakończony");
            Console.WriteLine($"    Total duration: {endResult.Stats.TotalDurationMs} ms");
            Console.WriteLine($"    Transcriptions: {endResult.Stats.TranscriptionsCount}");
        }
        catch (TentaFlowException ex)
        {
            Console.WriteLine($"\n  ✗ Błąd: {ex.Message}");
        }
    }

    /// <summary>
    /// Wyświetla podsumowanie dostępnych trybów sesji.
    /// </summary>
    public static void PrintModesSummary()
    {
        Console.WriteLine("\n─── CONVERSATION SESSION MODES SUMMARY ───");
        Console.WriteLine(@"
  ┌──────────────────────┬────────────────────────────────────────────┐
  │ Mode                 │ Opis                                       │
  ├──────────────────────┼────────────────────────────────────────────┤
  │ AlwaysOn (0)         │ Zawsze aktywny, bez wake word              │
  │                      │ Idealne: Smart speaker, dedykowany pokój   │
  ├──────────────────────┼────────────────────────────────────────────┤
  │ WakeWordTimeout (1)  │ Wake word → aktywny → timeout po ciszy     │
  │                      │ Idealne: Prywatność, oszczędność zasobów   │
  ├──────────────────────┼────────────────────────────────────────────┤
  │ WakeWordExplicit (2) │ Wake word → aktywny → stop phrase          │
  │                      │ Idealne: Długie rozmowy, sesje pracy       │
  └──────────────────────┴────────────────────────────────────────────┘

  Przykładowe wake words: 'Jarvis', 'Hej Jarvis', 'OK Jarvis'
  Przykładowe stop phrases: 'Dzięki Jarvis', 'To koniec', 'Wystarczy'
");
    }
}
