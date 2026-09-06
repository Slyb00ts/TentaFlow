// ===== File: BusDecodeTests.cs — M3b bus_* decode regression tests =====
// BusRecordOut.key is a CBOR `Option` field. minicbor omits `None` map
// entries, and a re-encoder may emit an explicit CBOR null — the decoder
// must accept BOTH, same discipline `StreamingDecodeTests.cs` established
// for LlmStreamNextOutput/SttTranscribeOutput.

using TentaFlow.Sdk;
using TentaFlow.Sdk.Cbor;
using Xunit;

namespace TentaFlow.Sdk.Tests;

public class BusDecodeTests
{
    // --- BusPublishOutput ---

    [Fact]
    public void PublishOutput_Decodes()
    {
        // A payload without key 1 (`schema_rejected`) — the pre-F3 wire
        // shape — must decode with `SchemaRejected` defaulting to 0.
        var w = new CborWriter(16);
        w.WriteMapHeader(1);
        w.WriteUInt(0);
        w.WriteUInt(42);
        var result = Bus.DecodePublishOutput(w.ToArray());
        Assert.Equal(42u, result.Published);
        Assert.Equal(0u, result.SchemaRejected);
    }

    [Fact]
    public void PublishOutput_DecodesSchemaRejected()
    {
        var w = new CborWriter(16);
        w.WriteMapHeader(2);
        w.WriteUInt(0);
        w.WriteUInt(5);
        w.WriteUInt(1);
        w.WriteUInt(2);
        var result = Bus.DecodePublishOutput(w.ToArray());
        Assert.Equal(5u, result.Published);
        Assert.Equal(2u, result.SchemaRejected);
    }

    [Fact]
    public void PublishOutput_SkipsAnUnknownFutureKey()
    {
        // A NEWER host sending a field this SDK version does not know
        // about yet must not break decoding of the fields it does know.
        var w = new CborWriter(24);
        w.WriteMapHeader(3);
        w.WriteUInt(0);
        w.WriteUInt(9);
        w.WriteUInt(1);
        w.WriteUInt(1);
        w.WriteUInt(2);
        w.WriteText("future-field");
        var result = Bus.DecodePublishOutput(w.ToArray());
        Assert.Equal(9u, result.Published);
        Assert.Equal(1u, result.SchemaRejected);
    }

    // --- BusConsumeOpenOutput ---

    [Fact]
    public void ConsumeOpenOutput_Decodes()
    {
        var w = new CborWriter(64);
        w.WriteMapHeader(1);
        w.WriteUInt(0);
        w.WriteText("busc_00000000-0000-0000-0000-000000000000");
        Assert.Equal(
            "busc_00000000-0000-0000-0000-000000000000",
            Bus.DecodeConsumeOpenOutput(w.ToArray()));
    }

    // --- BusConsumeNextOutput / BusRecordOut ---

    private static byte[] EmptyBatch()
    {
        // {0: "empty", 1: []}
        var w = new CborWriter(32);
        w.WriteMapHeader(2);
        w.WriteUInt(0);
        w.WriteText("empty");
        w.WriteUInt(1);
        w.WriteArrayHeader(0);
        return w.ToArray();
    }

    private static void WriteRecordWithKeyOmitted(CborWriter w)
    {
        // {0: topic, 1: partition, 2: offset, 3: timestamp_ms, 5: headers, 6: payload}
        // — key (field 4) entirely absent, mirroring minicbor's Option::None omission.
        w.WriteMapHeader(6);
        w.WriteUInt(0);
        w.WriteText("orders.created");
        w.WriteUInt(1);
        w.WriteUInt(0);
        w.WriteUInt(2);
        w.WriteUInt(42);
        w.WriteUInt(3);
        w.WriteInt(1_700_000_000_000);
        w.WriteUInt(5);
        w.WriteArrayHeader(1);
        w.WriteMapHeader(2);
        w.WriteUInt(0);
        w.WriteText("content-type");
        w.WriteUInt(1);
        w.WriteBytes(new byte[] { (byte)'j', (byte)'s', (byte)'o', (byte)'n' });
        w.WriteUInt(6);
        w.WriteBytes(new byte[] { 1, 2, 3 });
    }

    private static void WriteRecordWithKeyNull(CborWriter w)
    {
        // Same as above but field 4 present as an explicit CBOR null.
        w.WriteMapHeader(7);
        w.WriteUInt(0);
        w.WriteText("orders.created");
        w.WriteUInt(1);
        w.WriteUInt(0);
        w.WriteUInt(2);
        w.WriteUInt(43);
        w.WriteUInt(3);
        w.WriteInt(1_700_000_000_001);
        w.WriteUInt(4);
        w.WriteNull();
        w.WriteUInt(5);
        w.WriteArrayHeader(0);
        w.WriteUInt(6);
        w.WriteBytes(new byte[] { 9 });
    }

    private static void WriteRecordWithKeyPresent(CborWriter w)
    {
        w.WriteMapHeader(7);
        w.WriteUInt(0);
        w.WriteText("orders.created");
        w.WriteUInt(1);
        w.WriteUInt(0);
        w.WriteUInt(2);
        w.WriteUInt(44);
        w.WriteUInt(3);
        w.WriteInt(1_700_000_000_002);
        w.WriteUInt(4);
        w.WriteBytes(new byte[] { (byte)'k' });
        w.WriteUInt(5);
        w.WriteArrayHeader(0);
        w.WriteUInt(6);
        w.WriteBytes(new byte[] { 7, 7 });
    }

    [Fact]
    public void EmptyBatch_DecodesToNoRecords()
    {
        var batch = BusConsumer.DecodeNextBatch(EmptyBatch());
        Assert.Empty(batch.Records);
    }

    [Fact]
    public void Batch_RecordWithKeyOmitted_DecodesKeyAsNull()
    {
        var w = new CborWriter(64);
        w.WriteMapHeader(2);
        w.WriteUInt(0);
        w.WriteText("batch");
        w.WriteUInt(1);
        w.WriteArrayHeader(1);
        WriteRecordWithKeyOmitted(w);

        var batch = BusConsumer.DecodeNextBatch(w.ToArray());
        Assert.Single(batch.Records);
        var rec = batch.Records[0];
        Assert.Equal("orders.created", rec.Topic);
        Assert.Equal(0u, rec.Partition);
        Assert.Equal(42UL, rec.Offset);
        Assert.Equal(1_700_000_000_000L, rec.TimestampMs);
        Assert.Null(rec.Key);
        Assert.Single(rec.Headers);
        Assert.Equal("content-type", rec.Headers[0].Name);
        Assert.Equal(new byte[] { 1, 2, 3 }, rec.Payload);
    }

    [Fact]
    public void Batch_RecordWithExplicitNullKey_DecodesKeyAsNull()
    {
        var w = new CborWriter(64);
        w.WriteMapHeader(2);
        w.WriteUInt(0);
        w.WriteText("batch");
        w.WriteUInt(1);
        w.WriteArrayHeader(1);
        WriteRecordWithKeyNull(w);

        var batch = BusConsumer.DecodeNextBatch(w.ToArray());
        Assert.Single(batch.Records);
        Assert.Null(batch.Records[0].Key);
        Assert.Equal(43UL, batch.Records[0].Offset);
    }

    [Fact]
    public void Batch_RecordWithKeyPresent_DecodesKeyBytes()
    {
        var w = new CborWriter(64);
        w.WriteMapHeader(2);
        w.WriteUInt(0);
        w.WriteText("batch");
        w.WriteUInt(1);
        w.WriteArrayHeader(1);
        WriteRecordWithKeyPresent(w);

        var batch = BusConsumer.DecodeNextBatch(w.ToArray());
        Assert.Single(batch.Records);
        Assert.Equal(new byte[] { (byte)'k' }, batch.Records[0].Key);
        Assert.Equal(44UL, batch.Records[0].Offset);
    }
}
