using TentaFlow.Client;
using TentaFlow.Client.Models;

namespace TentaFlow.Client.Test.Tests;

public static class SttTests
{
    /// <summary>
    /// Test podstawowy STT - transkrypcja audio.
    /// </summary>
    public static void RunBasic(TentaFlowClient client, byte[] audioData, string originalText)
    {
        Console.WriteLine("\n─── TEST: STT (Speech-to-Text) ───");

        Console.WriteLine($"  Używam audio ({audioData.Length} bytes)");
        Console.WriteLine($"  Oryginalny tekst: \"{originalText}\"");

        var result = client.SpeechToText(
            model: "whisper-notes",
            audioData: audioData,
            language: "pl");

        Console.WriteLine($"✓ STT zakończone");
        Console.WriteLine($"  Transkrypcja: \"{result.Text}\"");
        Console.WriteLine($"  Wykryty język: {result.Language ?? "brak"}");
        Console.WriteLine($"  Czas audio: {result.DurationSeconds:F2}s");
    }

    /// <summary>
    /// Test STT z opcjami (verbose_json + filtrowanie halucynacji + diarization).
    /// </summary>
    public static void RunWithOptions(TentaFlowClient client, byte[] audioData)
    {
        Console.WriteLine("\n─── TEST: STT z opcjami (verbose_json + diarization) ───");

        Console.WriteLine($"  Używam audio ({audioData.Length} bytes)");

        var options = new SttOptions
        {
            Language = "pl",
            ResponseFormat = "verbose_json",
            TimestampGranularities = "segment",
            NoSpeechThreshold = 0.6f,           // Filtruj segmenty z no_speech_prob >= 0.6
            AvgLogprobThreshold = -1.0f,        // Filtruj segmenty z avg_logprob < -1.0
            CompressionRatioThreshold = 2.4f    // Filtruj segmenty z compression_ratio > 2.4
        };

        Console.WriteLine($"  Opcje: verbose_json, no_speech >= 0.6, avg_logprob < -1.0, compression > 2.4");

        var result = client.SpeechToText(
            model: "whisper-notes",
            audioData: audioData,
            options: options);

        Console.WriteLine($"✓ STT z opcjami zakończone");
        Console.WriteLine($"  Transkrypcja: \"{result.Text}\"");
        Console.WriteLine($"  Wykryty język: {result.Language ?? "brak"}");
        Console.WriteLine($"  Czas audio: {result.DurationSeconds:F2}s");
        Console.WriteLine($"  Latency: {result.LatencyMs} ms");
        Console.WriteLine($"  Segmentów: {result.Segments.Count}");
        Console.WriteLine($"  Odfiltrowanych halucynacji: {result.FilteredSegmentsCount}");

        // Wyświetl szczegóły segmentów z diarization
        if (result.Segments.Count > 0)
        {
            Console.WriteLine("  Segmenty:");
            foreach (var seg in result.Segments)
            {
                var speakerInfo = "";
                if (seg.SpeakerLabel != null)
                {
                    speakerInfo = $" [{seg.SpeakerLabel}";
                    if (seg.SpeakerSimilarity.HasValue)
                    {
                        var knownStatus = seg.IsKnownSpeaker == true ? "✓" : "?";
                        speakerInfo += $" ({seg.SpeakerSimilarity:P0}{knownStatus})";
                    }
                    speakerInfo += "]";
                }
                Console.WriteLine($"    [{seg.Id}] {seg.Start:F2}s - {seg.End:F2}s:{speakerInfo} \"{seg.Text.Trim()}\"");
                Console.WriteLine($"        no_speech: {seg.NoSpeechProb:F3}, avg_logprob: {seg.AvgLogprob:F3}, compression: {seg.CompressionRatio:F2}");
            }
        }
    }

    /// <summary>
    /// Test STT z plikiem audio.
    /// </summary>
    public static void RunWithFile(TentaFlowClient client, string audioFilePath)
    {
        Console.WriteLine("\n─── TEST: STT z pliku audio ───");

        if (!File.Exists(audioFilePath))
        {
            Console.WriteLine($"  (Pominięto - brak pliku: {audioFilePath})");
            return;
        }

        var audioData = File.ReadAllBytes(audioFilePath);
        Console.WriteLine($"  Plik: {audioFilePath}");
        Console.WriteLine($"  Rozmiar: {audioData.Length} bytes");

        var options = new SttOptions
        {
            Language = "pl",
            ResponseFormat = "verbose_json",
            TimestampGranularities = "segment"
        };

        var result = client.SpeechToText(
            model: "whisper-notes",
            audioData: audioData,
            options: options);

        Console.WriteLine($"✓ STT zakończone");
        Console.WriteLine($"  Transkrypcja: \"{result.Text}\"");
        Console.WriteLine($"  Wykryty język: {result.Language ?? "brak"}");
        Console.WriteLine($"  Czas audio: {result.DurationSeconds:F2}s");
        Console.WriteLine($"  Latency: {result.LatencyMs} ms");
        Console.WriteLine($"  Segmentów: {result.Segments.Count}");

        // Wyświetl segmenty z mówcami
        if (result.Segments.Count > 0)
        {
            Console.WriteLine("  Segmenty z rozpoznaniem mówców:");
            foreach (var seg in result.Segments)
            {
                var speakerInfo = seg.SpeakerLabel ?? "UNKNOWN";
                var similarity = seg.SpeakerSimilarity.HasValue ? $" ({seg.SpeakerSimilarity:P0})" : "";
                var known = seg.IsKnownSpeaker == true ? " ✓" : "";

                Console.WriteLine($"    [{seg.Start:F2}s - {seg.End:F2}s] {speakerInfo}{similarity}{known}: \"{seg.Text.Trim()}\"");
            }
        }
    }
}
