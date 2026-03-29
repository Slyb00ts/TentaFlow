using System.Diagnostics;
using TentaFlow.Client;
using TentaFlow.Client.Models;

namespace TentaFlow.Client.Test.Tests;

public static class RagTests
{
    /// <summary>
    /// Test podstawowy RAG Query.
    /// </summary>
    public static void RunBasic(TentaFlowClient client)
    {
        Console.WriteLine("\n─── TEST: RAG Query ───");

        var result = client.Rag("Testowe zapytanie RAG o dokumenty", topK: 5, minSimilarity: 0.5f);

        Console.WriteLine($"✓ RAG query wykonane");
        Console.WriteLine($"  Znalezione chunki: {result.ChunksFound}");
        Console.WriteLine($"  Wymaga LLM: {result.RequiresLlm}");
        Console.WriteLine($"  Response: {result.Response[..Math.Min(100, result.Response.Length)]}...");
    }

    /// <summary>
    /// Test RAG + TTS - odpowiedź z dźwiękiem.
    /// </summary>
    public static void RunWithTts(TentaFlowClient client)
    {
        Console.WriteLine("\n─── TEST: RAG + TTS (odpowiedź z dźwiękiem) ───");

        var query = "Jaka jest struktura projektu NextApp?";
        Console.WriteLine($"  Query: {query}");

        var result = client.Rag(query, topK: 3, minSimilarity: 0.5f);

        Console.WriteLine($"  RAG znalazł: {result.ChunksFound} chunków");
        Console.WriteLine($"  Response: {result.Response[..Math.Min(150, result.Response.Length)]}...");

        // Jeśli RAG zwrócił odpowiedź, generuj audio
        if (!string.IsNullOrWhiteSpace(result.Response) && result.Response.Length > 10)
        {
            // Skróć odpowiedź RAG do max 500 znaków dla TTS
            var textForTts = result.Response.Length > 500
                ? result.Response[..500] + "..."
                : result.Response;

            var ttsResult = client.TextToSpeech(
                model: "sherpa-tts",
                text: textForTts,
                voice: "jarvis",
                format: "wav");

            Console.WriteLine($"✓ RAG + TTS wygenerowane");
            Console.WriteLine($"  Audio: {ttsResult.AudioData.Length} bytes");
            Console.WriteLine($"  TTS Latency: {ttsResult.LatencyMs} ms");
            Console.WriteLine($"  Czas trwania: {ttsResult.AudioDurationSec:F2}s");

            var outputPath = Path.Combine(Path.GetTempPath(), "dotnet_rag_tts_test.wav");
            File.WriteAllBytes(outputPath, ttsResult.AudioData);
            Console.WriteLine($"  Zapisano: {outputPath}");
        }
        else
        {
            Console.WriteLine("  (RAG nie zwrócił wystarczającej odpowiedzi dla TTS)");
        }
    }

    /// <summary>
    /// Test RAG Search po Ingestion - wyszukiwanie w zindeksowanych dokumentach.
    /// </summary>
    public static void RunSearchAfterIngestion(TentaFlowClient client)
    {
        Console.WriteLine("\n─── TEST: RAG Search po Ingestion ───");

        var queries = new[]
        {
            "TentaFlow.AI platforma sztucznej inteligencji",
            "Umowa strop budowa domu",
            "wymagania dokumenty projekt"
        };

        foreach (var query in queries)
        {
            Console.WriteLine($"\n  Query: \"{query}\"");

            var result = client.Rag(query, topK: 3, minSimilarity: 0.3f);

            Console.WriteLine($"  Znaleziono: {result.ChunksFound} chunków");
            Console.WriteLine($"  RequiresLlm: {result.RequiresLlm}");

            if (!string.IsNullOrEmpty(result.Response))
            {
                var preview = result.Response.Length > 200
                    ? result.Response[..200] + "..."
                    : result.Response;
                Console.WriteLine($"  Response: {preview}");
            }
        }

        Console.WriteLine("\n✓ Testy RAG Search zakończone");
    }

    /// <summary>
    /// Test porównania silników wyszukiwania RAG.
    /// </summary>
    public static void RunEngineComparison(TentaFlowClient client)
    {
        Console.WriteLine("\n─── TEST: Porównanie silników wyszukiwania RAG ───");

        var query = "TentaFlow.AI platforma sztucznej inteligencji";
        Console.WriteLine($"  Query: \"{query}\"");
        Console.WriteLine($"  Top K: 5, Min similarity: 0.6");
        Console.WriteLine();

        var results = new List<(string Engine, long TimeMs, uint Chunks, string FullResponse, RagResult RagData)>();

        // Test 1: Full Text Search
        {
            var sw = Stopwatch.StartNew();
            var ftsResult = client.Rag(query, topK: 5, minSimilarity: 0.6f, searchModes: SearchMode.FullTextSearch);
            sw.Stop();
            results.Add(("FTS (Full Text Search)", sw.ElapsedMilliseconds, ftsResult.ChunksFound, ftsResult.Response, ftsResult));
        }

        // Test 2: Vector Search
        {
            var sw = Stopwatch.StartNew();
            var vecResult = client.Rag(query, topK: 5, minSimilarity: 0.6f, searchModes: SearchMode.VectorSearch);
            sw.Stop();
            results.Add(("Vector Search (HNSW)", sw.ElapsedMilliseconds, vecResult.ChunksFound, vecResult.Response, vecResult));
        }

        // Test 3: HiRAG
        {
            var sw = Stopwatch.StartNew();
            var hiragResult = client.Rag(query, topK: 5, minSimilarity: 0.6f, searchModes: SearchMode.HiRAG);
            sw.Stop();
            results.Add(("HiRAG (Knowledge Graph)", sw.ElapsedMilliseconds, hiragResult.ChunksFound, hiragResult.Response, hiragResult));
        }

        // Test 4: Kombinacja Vector + HiRAG
        {
            var sw = Stopwatch.StartNew();
            var combinedResult = client.Rag(query, topK: 5, minSimilarity: 0.6f,
                searchModes: SearchMode.VectorSearch | SearchMode.HiRAG);
            sw.Stop();
            results.Add(("Vector + HiRAG", sw.ElapsedMilliseconds, combinedResult.ChunksFound, combinedResult.Response, combinedResult));
        }

        // Test 5: Wszystkie silniki z rerankingiem
        {
            var sw = Stopwatch.StartNew();
            var allResult = client.Rag(query, topK: 5, minSimilarity: 0.6f,
                searchModes: SearchMode.FullTextSearch | SearchMode.VectorSearch | SearchMode.HiRAG,
                useReranking: true);
            sw.Stop();
            results.Add(("All Engines + Reranking", sw.ElapsedMilliseconds, allResult.ChunksFound, allResult.Response, allResult));
        }

        // Wyświetl szczegółowe wyniki
        PrintDetailedResults(results);

        // Podsumowanie w tabeli
        PrintSummaryTable(results);

        Console.WriteLine("\n✓ Porównanie silników zakończone");
    }

    private static void PrintDetailedResults(List<(string Engine, long TimeMs, uint Chunks, string FullResponse, RagResult RagData)> results)
    {
        Console.WriteLine("╔══════════════════════════════════════════════════════════════════════════════════════════════════════════════════╗");
        Console.WriteLine("║                              SZCZEGÓŁOWE PORÓWNANIE SILNIKÓW WYSZUKIWANIA                                        ║");
        Console.WriteLine("╚══════════════════════════════════════════════════════════════════════════════════════════════════════════════════╝");

        foreach (var (engine, timeMs, chunks, fullResponse, ragData) in results)
        {
            Console.WriteLine();
            Console.WriteLine($"┌────────────────────────────────────────────────────────────────────────────────────────────────────────────────┐");
            Console.WriteLine($"│  {engine,-105} │");
            Console.WriteLine($"├────────────────────────────────────────────────────────────────────────────────────────────────────────────────┤");
            Console.WriteLine($"│  Czas: {timeMs,4} ms  │  Wyników: {chunks}  │  RequiresLLM: {ragData.RequiresLlm}                                                       │");
            Console.WriteLine($"├────────────────────────────────────────────────────────────────────────────────────────────────────────────────┤");

            // Wyświetl szczegóły każdego chunka
            if (ragData.Chunks.Count > 0)
            {
                Console.WriteLine($"│  ZNALEZIONE CHUNKI (z similarity score):                                                                      │");
                Console.WriteLine($"├────────────────────────────────────────────────────────────────────────────────────────────────────────────────┤");

                foreach (var chunkInfo in ragData.Chunks.OrderByDescending(c => c.SimilarityScore))
                {
                    var scoreBar = new string('█', (int)(chunkInfo.SimilarityScore * 20));
                    var emptyBar = new string('░', 20 - (int)(chunkInfo.SimilarityScore * 20));
                    Console.WriteLine($"│  [{chunkInfo.Rank,2}] Score: {chunkInfo.SimilarityScore:F4} {scoreBar}{emptyBar}  Source: {chunkInfo.SourceFile,-30}   │");

                    var chunkPreview = chunkInfo.ChunkText.Replace("\n", " ").Replace("\r", "");
                    if (chunkPreview.Length > 100)
                        chunkPreview = chunkPreview[..97] + "...";
                    Console.WriteLine($"│       {chunkPreview,-105} │");
                }
            }
            else
            {
                Console.WriteLine($"│  (Brak szczegółowych danych o chunkach - może używasz trybu bez metadanych)                                   │");
            }

            Console.WriteLine($"├────────────────────────────────────────────────────────────────────────────────────────────────────────────────┤");
            Console.WriteLine($"│  PEŁNA ODPOWIEDŹ:                                                                                              │");
            Console.WriteLine($"├────────────────────────────────────────────────────────────────────────────────────────────────────────────────┤");

            var lines = fullResponse.Split('\n');
            foreach (var rawLine in lines)
            {
                var line = rawLine.Replace("\r", "").Trim();
                if (string.IsNullOrEmpty(line)) continue;

                var maxLineLen = 107;
                var remaining = line;
                while (remaining.Length > 0)
                {
                    var lineChunk = remaining.Length > maxLineLen ? remaining[..maxLineLen] : remaining;
                    remaining = remaining.Length > maxLineLen ? remaining[maxLineLen..] : "";
                    Console.WriteLine($"│  {lineChunk,-107} │");
                }
            }

            Console.WriteLine($"└────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘");
        }
    }

    private static void PrintSummaryTable(List<(string Engine, long TimeMs, uint Chunks, string FullResponse, RagResult RagData)> results)
    {
        Console.WriteLine();
        Console.WriteLine("╔═══════════════════════════╦═══════════╦══════════╦═══════════════╗");
        Console.WriteLine("║ Silnik                    ║ Czas [ms] ║ Wyników  ║ RequiresLLM   ║");
        Console.WriteLine("╠═══════════════════════════╬═══════════╬══════════╬═══════════════╣");
        foreach (var (engine, timeMs, chunks, _, ragData) in results)
        {
            Console.WriteLine($"║ {engine,-25} ║ {timeMs,9} ║ {chunks,8} ║ {ragData.RequiresLlm,-13} ║");
        }
        Console.WriteLine("╚═══════════════════════════╩═══════════╩══════════╩═══════════════╝");
    }
}
