// ===== File: LlmOptionsTests.cs — Llm.MergeSystem options merging =====
// A system prompt is delivered to the host via the options `"system"` field so
// the host builds a proper [system, user] message pair. These tests pin the
// merge behaviour (preserve existing options, override a prior system, no-op
// when there is no system prompt).

#nullable enable

using System.Text.Json;
using TentaFlow.Sdk;
using Xunit;

namespace TentaFlow.Sdk.Tests;

public class LlmOptionsTests
{
    [Fact]
    public void NullSystemReturnsOptionsUnchanged()
    {
        Assert.Equal("{\"temperature\":0.2}", Llm.MergeSystem("{\"temperature\":0.2}", null));
        Assert.Null(Llm.MergeSystem(null, null));
    }

    [Fact]
    public void MergesSystemPreservingExistingOptions()
    {
        var merged = Llm.MergeSystem("{\"temperature\":0.2,\"max_tokens\":2048}", "You translate.");
        using var doc = JsonDocument.Parse(merged!);
        var root = doc.RootElement;
        Assert.Equal(0.2, root.GetProperty("temperature").GetDouble(), 3);
        Assert.Equal(2048, root.GetProperty("max_tokens").GetInt32());
        Assert.Equal("You translate.", root.GetProperty("system").GetString());
    }

    [Fact]
    public void SystemWithoutOptionsYieldsSingleField()
    {
        var merged = Llm.MergeSystem(null, "sys");
        using var doc = JsonDocument.Parse(merged!);
        Assert.Equal("sys", doc.RootElement.GetProperty("system").GetString());
        Assert.False(doc.RootElement.TryGetProperty("temperature", out _));
    }

    [Fact]
    public void SystemOverridesPriorSystem()
    {
        var merged = Llm.MergeSystem("{\"system\":\"old\",\"top_p\":0.9}", "new");
        using var doc = JsonDocument.Parse(merged!);
        Assert.Equal("new", doc.RootElement.GetProperty("system").GetString());
        Assert.Equal(0.9, doc.RootElement.GetProperty("top_p").GetDouble(), 3);
    }

    [Fact]
    public void MalformedOptionsDegradeToSystemOnly()
    {
        // Must not throw — malformed options are dropped, like the host's
        // serde_json(...).ok() tolerance, leaving a clean {"system": ...}.
        var merged = Llm.MergeSystem("{not valid json", "sys");
        using var doc = JsonDocument.Parse(merged!);
        Assert.Equal("sys", doc.RootElement.GetProperty("system").GetString());
        var count = 0;
        foreach (var _ in doc.RootElement.EnumerateObject())
        {
            count++;
        }
        Assert.Equal(1, count);
    }
}
