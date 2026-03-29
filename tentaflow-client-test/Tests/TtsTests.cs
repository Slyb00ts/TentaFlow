using System.Collections.Concurrent;
using TentaFlow.Client;
using TentaFlow.Client.Models;

namespace TentaFlow.Client.Test.Tests;

public static class TtsTests
{
    private static string voice = "jarvis";
    /// <summary>
    /// Test podstawowy TTS - generowanie audio z tekstu.
    /// Zwraca wygenerowane audio do użycia w innych testach.
    /// </summary>
    public static TtsResult RunBasic(TentaFlowClient client)
    {
        Console.WriteLine("\n─── TEST: Basic TTS (Text-to-Speech) ───");

        var text = "Cześć, to jest test serwera Text-to-Speech. Działa poprawnie przez Router.";
        Console.WriteLine($"  Tekst: \"{text}\"");

        var result = client.TextToSpeech(
            model: "sherpa-tts",
            text: text,
            voice: voice,
            format: "wav");

        Console.WriteLine($"✓ TTS wygenerowane");
        Console.WriteLine($"  Audio: {result.AudioData.Length} bytes");
        Console.WriteLine($"  Format: {result.Format}");
        Console.WriteLine($"  Latency: {result.LatencyMs} ms");
        Console.WriteLine($"  Czas trwania: {result.AudioDurationSec:F2}s");

        // Zapisz do pliku
        var outputPath = Path.Combine(Path.GetTempPath(), "dotnet_tts_test.wav");
        File.WriteAllBytes(outputPath, result.AudioData);
        Console.WriteLine($"  Zapisano: {outputPath}");

        return result;
    }

    /// <summary>
    /// Test LLM + TTS (Non-streaming) - generowanie odpowiedzi i audio.
    /// </summary>
    public static void RunLlmPlusTts(TentaFlowClient client)
    {
        Console.WriteLine("\n─── TEST: LLM + TTS (Non-streaming) ───");

        var messages = new[]
        {
            new ChatMessage { Role = "system", Content = "Jesteś pomocnym asystentem. Odpowiadaj krótko po polsku." },
            new ChatMessage { Role = "user", Content = "Podaj jednym zdaniem czym jest sztuczna inteligencja." }
        };

        Console.WriteLine("  Pytanie: Podaj jednym zdaniem czym jest sztuczna inteligencja.");

        var options = new ChatCompletionOptions
        {
            Temperature = 0.3f,
            MaxTokens = 2048,
            Stream = false
        };

        var llmResponse = client.ChatCompletion("bielik-11b", messages, options);
        Console.WriteLine($"  LLM odpowiedź: {llmResponse.Completion.Content.Trim()}");
        Console.WriteLine($"  LLM latency: {llmResponse.Completion.LatencyMs} ms");

        // Generuj audio z odpowiedzi LLM
        if (!string.IsNullOrWhiteSpace(llmResponse.Completion.Content))
        {
            var ttsResult = client.TextToSpeech(
                model: "sherpa-tts",
                text: llmResponse.Completion.Content.Trim(),
                voice: voice,
                format: "wav");

            Console.WriteLine($"✓ LLM + TTS wygenerowane");
            Console.WriteLine($"  Audio: {ttsResult.AudioData.Length} bytes");
            Console.WriteLine($"  TTS Latency: {ttsResult.LatencyMs} ms");
            Console.WriteLine($"  Czas trwania: {ttsResult.AudioDurationSec:F2}s");

            var outputPath = Path.Combine(Path.GetTempPath(), "dotnet_llm_tts_test.wav");
            File.WriteAllBytes(outputPath, ttsResult.AudioData);
            Console.WriteLine($"  Zapisano: {outputPath}");
        }
        else
        {
            Console.WriteLine("  (LLM zwrócił pustą odpowiedź - pomijam TTS)");
        }
    }

    /// <summary>
    /// Test LLM + TTS (Streaming) - streaming odpowiedzi + audio.
    /// </summary>
    public static void RunStreamingLlmPlusTts(TentaFlowClient client)
    {
        Console.WriteLine("\n─── TEST: LLM + TTS (Streaming) ───");

        var messages = new[]
        {
            new ChatMessage { Role = "system", Content = "Jesteś pomocnym asystentem. Odpowiadaj krótko po polsku." },
            new ChatMessage { Role = "user", Content = "Wymień 3 zastosowania AI w medycynie, krótko." }
        };

        Console.WriteLine("  Pytanie: Wymień 3 zastosowania AI w medycynie.");
        Console.Write("  Streaming: ");

        var options = new ChatCompletionOptions
        {
            Temperature = 0.3f,
            MaxTokens = 2048,
            Stream = true
        };

        var streamResponse = client.ChatCompletion(
            "bielik-11b",
            messages,
            options,
            onContent: token => Console.Write(token));

        Console.WriteLine();
        Console.WriteLine($"  Streaming latency: {streamResponse.Completion.LatencyMs} ms, TTFT: {streamResponse.Completion.TimeToFirstTokenMs} ms");

        // Generuj audio z odpowiedzi streamingowej
        if (!string.IsNullOrWhiteSpace(streamResponse.Completion.Content))
        {
            var ttsResult = client.TextToSpeech(
                model: "sherpa-tts",
                text: streamResponse.Completion.Content.Trim(),
                voice: voice,
                format: "wav");

            Console.WriteLine($"✓ Streaming LLM + TTS wygenerowane");
            Console.WriteLine($"  Audio: {ttsResult.AudioData.Length} bytes");
            Console.WriteLine($"  TTS Latency: {ttsResult.LatencyMs} ms");
            Console.WriteLine($"  Czas trwania: {ttsResult.AudioDurationSec:F2}s");

            var outputPath = Path.Combine(Path.GetTempPath(), "dotnet_stream_tts_test.wav");
            File.WriteAllBytes(outputPath, ttsResult.AudioData);
            Console.WriteLine($"  Zapisano: {outputPath}");
        }
        else
        {
            Console.WriteLine("  (Streaming zwrócił pustą odpowiedź - pomijam TTS)");
        }
    }

    /// <summary>
    /// Test Streaming TTS - audio chunks per sentence z odtwarzaniem na żywo.
    /// </summary>
    public static void RunStreamingTts(TentaFlowClient client)
    {
        Console.WriteLine("\n─── TEST: Streaming TTS (audio chunks per sentence) ───");

        var messages = new[]
        {
            new ChatMessage { Role = "system", Content = "Jesteś pomocnym asystentem. Odpowiadaj po polsku." },
            new ChatMessage { Role = "user", Content = "Opisz w trzech zdaniach co to jest sztuczna inteligencja." }
        };

        var ttsOptions = new TtsOptions
        {
            Model = "sherpa-tts",
            Voice = voice,
            Format = "wav",
            Speed = 1.25f
        };

        Console.WriteLine("  Pytanie: Opisz w trzech zdaniach co to jest sztuczna inteligencja.");
        Console.WriteLine("  TTS Model: sherpa-tts, Voice: jarvis");
        Console.WriteLine("  (Audio będzie odtwarzane na żywo podczas streamingu!)");
        Console.Write("  Streaming: ");

        int audioChunkCount = 0;
        long totalAudioBytes = 0;
        var allAudioChunks = new List<byte[]>();

        // Kolejka audio z osobnym wątkiem odtwarzania
        var audioQueue = new BlockingCollection<byte[]>();
        var playbackTask = Task.Run(() =>
        {
            foreach (var chunk in audioQueue.GetConsumingEnumerable())
            {
                AudioHelper.PlayAudio(chunk);
            }
        });

        var options = new ChatCompletionOptions
        {
            Temperature = 0.8f,
            MaxTokens = 4096,
            Stream = true,
            Tts = ttsOptions
        };

        var result = client.ChatCompletion(
            "bielik-11b",
            messages,
            options,
            onContentStart: () => Console.WriteLine(),
            onContent: token => Console.Write(token),
            onContentEnd: () => Console.WriteLine(),
            onAudio: audioChunk =>
            {
                audioChunkCount++;
                totalAudioBytes += audioChunk.Length;
                allAudioChunks.Add(audioChunk);
                audioQueue.Add(audioChunk);
                Console.WriteLine($"  [Audio chunk {audioChunkCount}: {audioChunk.Length} bytes - queued]");
            });

        Console.WriteLine($"✓ Streaming TTS zakończone (tekst)");
        Console.WriteLine($"  Text length: {result.Completion.Content.Length} chars");
        Console.WriteLine($"  Audio chunks: {audioChunkCount}");
        Console.WriteLine($"  Total audio: {totalAudioBytes} bytes");
        Console.WriteLine($"  Latency: {result.Completion.LatencyMs} ms");
        Console.WriteLine($"  TTFT: {result.Completion.TimeToFirstTokenMs} ms");

        // Poczekaj na zakończenie odtwarzania
        audioQueue.CompleteAdding();
        Console.WriteLine("  Czekam na zakończenie odtwarzania audio...");
        playbackTask.Wait();
        Console.WriteLine("  ✓ Audio odtworzone!");

        // Zapisz wszystkie audio chunki do jednego pliku
        if (allAudioChunks.Count > 0)
        {
            using var ms = new MemoryStream();
            foreach (var chunk in allAudioChunks)
            {
                ms.Write(chunk, 0, chunk.Length);
            }
            var combinedAudio = ms.ToArray();
            var outputPath = Path.Combine(Path.GetTempPath(), "dotnet_streaming_tts_chunks.wav");
            File.WriteAllBytes(outputPath, combinedAudio);
            Console.WriteLine($"  Zapisano: {outputPath}");
        }
    }
}
