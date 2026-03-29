using TentaFlow.Client;

namespace TentaFlow.Client.Test.Tests;

public static class EmbeddingsTests
{
    public static void Run(TentaFlowClient client)
    {
        Console.WriteLine("\n─── TEST: Embeddings ───");

        var texts = new[]
        {
            "To jest testowy tekst do embeddingu.",
            "Drugi tekst testowy w języku polskim."
        };

        var result = client.Embeddings("embeddings-gemma", texts);

        Console.WriteLine($"✓ Embeddings otrzymane: {result.Embeddings.Count} wektorów");
        Console.WriteLine($"  Wymiary: {result.Dimensions}");
        Console.WriteLine($"  Latency: {result.LatencyMs} ms");
        Console.WriteLine($"  Pierwsze 5 wartości: [{string.Join(", ", result.Embeddings[0].Take(5).Select(v => v.ToString("F6")))}]");
    }
}
