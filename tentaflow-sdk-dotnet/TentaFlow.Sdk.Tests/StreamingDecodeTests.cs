// ===== File: StreamingDecodeTests.cs — Option-field decode regression tests =====
// LlmStreamNextOutput.finish_reason/error and SttTranscribeOutput.detected_
// language/duration_ms are CBOR `Option` fields. minicbor omits `None` map
// entries, and a re-encoder may emit an explicit CBOR null — the decoder must
// accept BOTH (an intermediate streaming batch has neither field, so a decode
// that treated them as mandatory crashed the whole stream at runtime).

using TentaFlow.Sdk;
using TentaFlow.Sdk.Cbor;
using Xunit;

namespace TentaFlow.Sdk.Tests;

public class StreamingDecodeTests
{
    // --- LlmStreamNextOutput ---

    private static byte[] IntermediateBatch()
    {
        // {0: ["Hel", "lo"], 1: false} — no finish_reason, no error (keys absent).
        var w = new CborWriter(32);
        w.WriteMapHeader(2);
        w.WriteUInt(0);
        w.WriteArrayHeader(2);
        w.WriteText("Hel");
        w.WriteText("lo");
        w.WriteUInt(1);
        w.WriteBool(false);
        return w.ToArray();
    }

    private static byte[] FinalBatchWithNulls()
    {
        // {0: [], 1: true, 2: null, 3: null} — Option fields present as null.
        var w = new CborWriter(32);
        w.WriteMapHeader(4);
        w.WriteUInt(0);
        w.WriteArrayHeader(0);
        w.WriteUInt(1);
        w.WriteBool(true);
        w.WriteUInt(2);
        w.WriteNull();
        w.WriteUInt(3);
        w.WriteNull();
        return w.ToArray();
    }

    private static byte[] FinalBatchWithReason()
    {
        // {0: [], 1: true, 2: "stop"} — finish_reason set, error absent.
        var w = new CborWriter(32);
        w.WriteMapHeader(3);
        w.WriteUInt(0);
        w.WriteArrayHeader(0);
        w.WriteUInt(1);
        w.WriteBool(true);
        w.WriteUInt(2);
        w.WriteText("stop");
        return w.ToArray();
    }

    private static byte[] ErrorBatch()
    {
        // {0: [], 1: true, 2: "error", 3: "backend unavailable"}.
        var w = new CborWriter(64);
        w.WriteMapHeader(4);
        w.WriteUInt(0);
        w.WriteArrayHeader(0);
        w.WriteUInt(1);
        w.WriteBool(true);
        w.WriteUInt(2);
        w.WriteText("error");
        w.WriteUInt(3);
        w.WriteText("backend unavailable");
        return w.ToArray();
    }

    [Fact]
    public void IntermediateBatch_MissingOptionFields_DoesNotThrow()
    {
        var batch = LlmStream.DecodeBatch(IntermediateBatch());
        Assert.Equal(new[] { "Hel", "lo" }, batch.Chunks);
        Assert.False(batch.Finished);
        Assert.Null(batch.FinishReason);
        Assert.Null(batch.Error);
    }

    [Fact]
    public void FinalBatch_ExplicitNullOptionFields_DecodeAsNull()
    {
        var batch = LlmStream.DecodeBatch(FinalBatchWithNulls());
        Assert.Empty(batch.Chunks);
        Assert.True(batch.Finished);
        Assert.Null(batch.FinishReason);
        Assert.Null(batch.Error);
    }

    [Fact]
    public void FinalBatch_WithFinishReason_DecodesReason()
    {
        var batch = LlmStream.DecodeBatch(FinalBatchWithReason());
        Assert.True(batch.Finished);
        Assert.Equal("stop", batch.FinishReason);
        Assert.Null(batch.Error);
    }

    [Fact]
    public void ErrorBatch_DecodesReasonAndError()
    {
        var batch = LlmStream.DecodeBatch(ErrorBatch());
        Assert.True(batch.Finished);
        Assert.Equal("error", batch.FinishReason);
        Assert.Equal("backend unavailable", batch.Error);
    }

    // --- SttTranscribeOutput ---

    [Fact]
    public void Stt_MissingOptionFields_DoesNotThrow()
    {
        // {0: "dzień dobry"} — no detected_language, no duration_ms.
        var w = new CborWriter(32);
        w.WriteMapHeader(1);
        w.WriteUInt(0);
        w.WriteText("dzień dobry");
        var result = Stt.DecodeOutput(w.ToArray());
        Assert.Equal("dzień dobry", result.Text);
        Assert.Null(result.DetectedLanguage);
        Assert.Null(result.DurationMs);
    }

    [Fact]
    public void Stt_ExplicitNullOptionFields_DecodeAsNull()
    {
        // {0: "hi", 1: null, 2: null}.
        var w = new CborWriter(32);
        w.WriteMapHeader(3);
        w.WriteUInt(0);
        w.WriteText("hi");
        w.WriteUInt(1);
        w.WriteNull();
        w.WriteUInt(2);
        w.WriteNull();
        var result = Stt.DecodeOutput(w.ToArray());
        Assert.Equal("hi", result.Text);
        Assert.Null(result.DetectedLanguage);
        Assert.Null(result.DurationMs);
    }

    [Fact]
    public void Stt_WithAllFields_Decodes()
    {
        // {0: "cześć", 1: "pl", 2: 1200}.
        var w = new CborWriter(32);
        w.WriteMapHeader(3);
        w.WriteUInt(0);
        w.WriteText("cześć");
        w.WriteUInt(1);
        w.WriteText("pl");
        w.WriteUInt(2);
        w.WriteUInt(1200);
        var result = Stt.DecodeOutput(w.ToArray());
        Assert.Equal("cześć", result.Text);
        Assert.Equal("pl", result.DetectedLanguage);
        Assert.Equal(1200UL, result.DurationMs);
    }
}
