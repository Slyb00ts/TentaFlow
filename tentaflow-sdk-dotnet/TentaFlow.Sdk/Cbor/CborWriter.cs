// ===== File: Cbor/CborWriter.cs — canonical (RFC 8949 §4.2.1) CBOR writer =====
// Definite lengths only, minimum-width integers, f64 floats (matching the Rust
// `tentaflow-sdk-spec` Value encoder, which always emits 8-byte floats).

#nullable enable

using System;
using System.Buffers.Binary;
using System.Text;

namespace TentaFlow.Sdk.Cbor;

public sealed class CborWriter
{
    private byte[] _buffer;
    private int _length;

    public CborWriter(int capacity = 256)
    {
        _buffer = new byte[Math.Max(capacity, 16)];
        _length = 0;
    }

    public int Length => _length;

    public byte[] ToArray()
    {
        var result = new byte[_length];
        System.Array.Copy(_buffer, result, _length);
        return result;
    }

    public ReadOnlySpan<byte> WrittenSpan => new(_buffer, 0, _length);

    private void Ensure(int extra)
    {
        if (_length + extra <= _buffer.Length)
        {
            return;
        }
        int newSize = _buffer.Length * 2;
        while (newSize < _length + extra)
        {
            newSize *= 2;
        }
        System.Array.Resize(ref _buffer, newSize);
    }

    private void WriteByte(byte b)
    {
        Ensure(1);
        _buffer[_length++] = b;
    }

    private void WriteRaw(ReadOnlySpan<byte> bytes)
    {
        Ensure(bytes.Length);
        bytes.CopyTo(new Span<byte>(_buffer, _length, bytes.Length));
        _length += bytes.Length;
    }

    /// <summary>Writes a major type header with preferred (minimal) width.</summary>
    public void WriteTypeAndValue(int major, ulong value)
    {
        byte mt = (byte)(major << 5);
        if (value < 24)
        {
            WriteByte((byte)(mt | (byte)value));
        }
        else if (value <= byte.MaxValue)
        {
            WriteByte((byte)(mt | 24));
            WriteByte((byte)value);
        }
        else if (value <= ushort.MaxValue)
        {
            WriteByte((byte)(mt | 25));
            Ensure(2);
            BinaryPrimitives.WriteUInt16BigEndian(new Span<byte>(_buffer, _length, 2), (ushort)value);
            _length += 2;
        }
        else if (value <= uint.MaxValue)
        {
            WriteByte((byte)(mt | 26));
            Ensure(4);
            BinaryPrimitives.WriteUInt32BigEndian(new Span<byte>(_buffer, _length, 4), (uint)value);
            _length += 4;
        }
        else
        {
            WriteByte((byte)(mt | 27));
            Ensure(8);
            BinaryPrimitives.WriteUInt64BigEndian(new Span<byte>(_buffer, _length, 8), value);
            _length += 8;
        }
    }

    public void WriteUInt(ulong value) => WriteTypeAndValue(0, value);

    public void WriteInt(long value)
    {
        if (value >= 0)
        {
            WriteTypeAndValue(0, (ulong)value);
        }
        else
        {
            WriteTypeAndValue(1, (ulong)(-1 - value));
        }
    }

    public void WriteBool(bool value) => WriteByte(value ? (byte)0xf5 : (byte)0xf4);

    public void WriteNull() => WriteByte(0xf6);

    public void WriteFloat64(double value)
    {
        WriteByte(0xfb);
        Ensure(8);
        BinaryPrimitives.WriteUInt64BigEndian(
            new Span<byte>(_buffer, _length, 8),
            (ulong)BitConverter.DoubleToInt64Bits(value));
        _length += 8;
    }

    public void WriteText(string value)
    {
        int byteCount = Encoding.UTF8.GetByteCount(value);
        WriteTypeAndValue(3, (ulong)byteCount);
        Ensure(byteCount);
        Encoding.UTF8.GetBytes(value, new Span<byte>(_buffer, _length, byteCount));
        _length += byteCount;
    }

    public void WriteBytes(ReadOnlySpan<byte> value)
    {
        WriteTypeAndValue(2, (ulong)value.Length);
        WriteRaw(value);
    }

    public void WriteArrayHeader(int count) => WriteTypeAndValue(4, (ulong)count);

    public void WriteMapHeader(int count) => WriteTypeAndValue(5, (ulong)count);

    /// <summary>Appends pre-encoded CBOR bytes verbatim.</summary>
    public void WriteEncoded(ReadOnlySpan<byte> encoded) => WriteRaw(encoded);
}
