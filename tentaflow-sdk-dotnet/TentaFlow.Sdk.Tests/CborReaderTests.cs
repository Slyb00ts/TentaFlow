// ===== File: CborReaderTests.cs — decoder hardening (strict UTF-8, bounded alloc) =====

#nullable enable

using System;
using System.IO;
using TentaFlow.Sdk.Cbor;
using TentaFlow.Sdk.Components;
using Xunit;

namespace TentaFlow.Sdk.Tests;

public class CborReaderTests
{
    [Fact]
    public void RejectsInvalidUtf8TextString()
    {
        // Major 3 (text), length 2, then a lone 0xFF 0xFE (invalid UTF-8).
        var bytes = new byte[] { 0x62, 0xff, 0xfe };
        var reader = new CborReader(bytes);
        var ex = Assert.Throws<FormatException>(() => reader.ReadText());
        Assert.Contains("UTF-8", ex.Message);
    }

    [Fact]
    public void AcceptsValidUtf8TextString()
    {
        // "café" as UTF-8 (5 bytes): 63 61 66 c3 a9, prefixed with major-3 len 5.
        var bytes = new byte[] { 0x65, 0x63, 0x61, 0x66, 0xc3, 0xa9 };
        var reader = new CborReader(bytes);
        Assert.Equal("café", reader.ReadText());
    }

    [Fact]
    public void MalformedHugeArrayLengthThrowsWithoutHugeAllocation()
    {
        // Array header claiming 0xFFFFFFFF elements but no element bytes follow.
        // Bounded preallocation must not OOM; decode runs out of input and
        // throws a FormatException instead.
        var bytes = new byte[] { 0x9a, 0xff, 0xff, 0xff, 0xff };
        var reader = new CborReader(bytes);
        Assert.Throws<FormatException>(() => Value.Decode(reader));
    }

    [Fact]
    public void MalformedHugeMapLengthThrowsWithoutHugeAllocation()
    {
        // Map header claiming 0xFFFFFFFF pairs but no entry bytes follow.
        var bytes = new byte[] { 0xba, 0xff, 0xff, 0xff, 0xff };
        var reader = new CborReader(bytes);
        Assert.Throws<FormatException>(() => Value.Decode(reader));
    }

    [Fact]
    public void WellFormedNestedValueRoundTrips()
    {
        var value = Value.Array(
            Value.UInt(1),
            Value.Map(new System.Collections.Generic.List<
                System.Collections.Generic.KeyValuePair<Value, Value>>
            {
                new(Value.Text("k"), Value.Bool(true)),
            }),
            Value.Text("café"));
        var bytes = value.ToCborBytes();
        var decoded = Value.Decode(new CborReader(bytes));
        Assert.Equal(Convert.ToHexStringLower(bytes),
            Convert.ToHexStringLower(decoded.ToCborBytes()));
    }
}
