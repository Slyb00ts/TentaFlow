using TentaFlow.Client;
using TentaFlow.Client.Models;

namespace TentaFlow.Client.Test.Tests;

public static class CompletionTests
{
    public static void RunBasic(TentaFlowClient client)
    {
        Console.WriteLine("\n─── TEST: Chat Completion ───");

        var messages = new[]
        {
            new ChatMessage { Role = "system", Content = "Jesteś pomocnym asystentem. Odpowiadaj krótko." },
            new ChatMessage { Role = "user", Content = "Powiedz 'test' jednym słowem." }
        };

        var options = new ChatCompletionOptions
        {
            Temperature = 0.1f,
            MaxTokens = 1024,
            Stream = false
        };

        var result = client.ChatCompletion("bielik-11b", messages, options);
        var completion = result.Completion;

        Console.WriteLine($"✓ Completion otrzymany");
        if (!string.IsNullOrEmpty(completion.ReasoningContent))
        {
            Console.WriteLine($"  Myślenie: {completion.ReasoningContent[..Math.Min(200, completion.ReasoningContent.Length)]}...");
        }
        Console.WriteLine($"  Odpowiedź: {completion.Content.Trim()}");
        Console.WriteLine($"  Model: {completion.Model}");
        Console.WriteLine($"  Finish reason: {completion.FinishReason}");
        Console.WriteLine($"  Tokeny: prompt={completion.PromptTokens}, completion={completion.CompletionTokens}, total={completion.TotalTokens}");
        Console.WriteLine($"  TTFT: {completion.TimeToFirstTokenMs} ms");
        Console.WriteLine($"  Latency: {completion.LatencyMs} ms");
        Console.WriteLine($"  Tokens/sec: {completion.TokensPerSec:F1}");
    }

    public static void RunStreaming(TentaFlowClient client)
    {
        Console.WriteLine("\n─── TEST: Chat Completion STREAMING ───");

        var messages = new[]
        {
            new ChatMessage { Role = "system", Content = "Jesteś pomocnym asystentem. Odpowiadaj krótko." },
            new ChatMessage { Role = "user", Content = "Napisz krótki wiersz o programowaniu to ma być dłuższy tekst." }
        };

        Console.WriteLine("Streaming response:");

        var options = new ChatCompletionOptions
        {
            Temperature = 0.7f,
            MaxTokens = 1024,
            Stream = true
        };

        var result = client.ChatCompletion(
            "bielik-11b",
            messages,
            options,
            onReasoningStart: () => Console.WriteLine("  [Myślenie]"),
            onReasoning: token => Console.Write(token),
            onReasoningEnd: () => Console.WriteLine("\n  [/Myślenie]"),
            onContentStart: () => Console.Write("  [Odpowiedź]\n "),
            onContent: token => Console.Write(token),
            onContentEnd: () => Console.WriteLine("\n  [/Odpowiedź]"));

        var completion = result.Completion;
        Console.WriteLine($"✓ Streaming zakończony");
        Console.WriteLine($"  Content length: {completion.Content.Length} znaków");
        Console.WriteLine($"  Model: {completion.Model}");
        Console.WriteLine($"  Finish reason: {completion.FinishReason}");
        Console.WriteLine($"  Tokeny: prompt={completion.PromptTokens}, completion={completion.CompletionTokens}, total={completion.TotalTokens}");
        Console.WriteLine($"  TTFT: {completion.TimeToFirstTokenMs} ms");
        Console.WriteLine($"  Latency: {completion.LatencyMs} ms");
        Console.WriteLine($"  Tokens/sec: {completion.TokensPerSec:F1}");
    }
}
