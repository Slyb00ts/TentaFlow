// ===== File: Cbor/CborReader.cs — minimal CBOR reader for host-function outputs =====
// Reads definite-length items only (the host encodes canonically); rejects
// indefinite lengths and semantic tags, mirroring the deterministic profile.

#nullable enable

using System;
using System.Buffers.Binary;
using System.Text;

namespace TentaFlow.Sdk.Cbor;

public sealed class CborReader
{
    // CBOR text strings MUST be valid UTF-8 (RFC 8949 §3.1); reject invalid
    // bytes instead of substituting the replacement character.
    private static readonly Encoding StrictUtf8 =
        new UTF8Encoding(encoderShouldEmitUTF8Identifier: false, throwOnInvalidBytes: true);

    private readonly byte[] _data;
    private int _pos;

    public CborReader(byte[] data)
    {
        _data = data;
        _pos = 0;
    }

    public int Position => _pos;
    public bool AtEnd => _pos >= _data.Length;

    /// <summary>Bytes left to read — an upper bound on remaining CBOR items
    /// (each item is at least one byte), used to cap speculative allocations.</summary>
    public int RemainingBytes => _data.Length - _pos;

    public byte PeekInitialByte()
    {
        if (_pos >= _data.Length)
        {
            throw new FormatException("CBOR: unexpected end of input");
        }
        return _data[_pos];
    }

    public int PeekMajorType() => PeekInitialByte() >> 5;

    private byte NextByte()
    {
        byte b = PeekInitialByte();
        _pos++;
        return b;
    }

    private ulong ReadLength(int additional)
    {
        switch (additional)
        {
            case < 24:
                return (ulong)additional;
            case 24:
                return NextByte();
            case 25:
                {
                    var v = BinaryPrimitives.ReadUInt16BigEndian(Slice(2));
                    return v;
                }
            case 26:
                {
                    var v = BinaryPrimitives.ReadUInt32BigEndian(Slice(4));
                    return v;
                }
            case 27:
                {
                    var v = BinaryPrimitives.ReadUInt64BigEndian(Slice(8));
                    return v;
                }
            default:
                throw new FormatException("CBOR: indefinite lengths are not allowed");
        }
    }

    private ReadOnlySpan<byte> Slice(int count)
    {
        if (_pos + count > _data.Length)
        {
            throw new FormatException("CBOR: unexpected end of input");
        }
        var span = new ReadOnlySpan<byte>(_data, _pos, count);
        _pos += count;
        return span;
    }

    public ulong ReadUInt()
    {
        byte ib = NextByte();
        if (ib >> 5 != 0)
        {
            throw new FormatException($"CBOR: expected unsigned integer, got major {ib >> 5}");
        }
        return ReadLength(ib & 0x1f);
    }

    public long ReadInt()
    {
        byte ib = NextByte();
        int major = ib >> 5;
        ulong raw = ReadLength(ib & 0x1f);
        return major switch
        {
            0 => checked((long)raw),
            1 => checked(-1 - (long)raw),
            _ => throw new FormatException($"CBOR: expected integer, got major {major}"),
        };
    }

    public bool ReadBool()
    {
        byte ib = NextByte();
        return ib switch
        {
            0xf4 => false,
            0xf5 => true,
            _ => throw new FormatException("CBOR: expected bool"),
        };
    }

    public void ReadNull()
    {
        if (NextByte() != 0xf6)
        {
            throw new FormatException("CBOR: expected null");
        }
    }

    /// <summary>
    /// Consumes a CBOR null (0xf6) when the next item is null and returns true;
    /// otherwise leaves the position untouched and returns false. Used to decode
    /// `Option&lt;T&gt;` fields that a Rust encoder may emit as an explicit null.
    /// </summary>
    public bool TryReadNull()
    {
        if (_pos < _data.Length && _data[_pos] == 0xf6)
        {
            _pos++;
            return true;
        }
        return false;
    }

    public double ReadFloat()
    {
        byte ib = NextByte();
        return ib switch
        {
            0xf9 => ReadHalf(),
            0xfa => BitConverter.Int32BitsToSingle(
                BinaryPrimitives.ReadInt32BigEndian(Slice(4))),
            0xfb => BitConverter.Int64BitsToDouble(
                BinaryPrimitives.ReadInt64BigEndian(Slice(8))),
            _ => throw new FormatException("CBOR: expected float"),
        };
    }

    private double ReadHalf()
    {
        ushort bits = BinaryPrimitives.ReadUInt16BigEndian(Slice(2));
        return (double)BitConverter.UInt16BitsToHalf(bits);
    }

    public string ReadText()
    {
        byte ib = NextByte();
        if (ib >> 5 != 3)
        {
            throw new FormatException($"CBOR: expected text string, got major {ib >> 5}");
        }
        ulong len = ReadLength(ib & 0x1f);
        var span = Slice(checked((int)len));
        try
        {
            return StrictUtf8.GetString(span);
        }
        catch (DecoderFallbackException e)
        {
            throw new FormatException("CBOR: text string is not valid UTF-8", e);
        }
    }

    public byte[] ReadBytes()
    {
        byte ib = NextByte();
        if (ib >> 5 != 2)
        {
            throw new FormatException($"CBOR: expected byte string, got major {ib >> 5}");
        }
        ulong len = ReadLength(ib & 0x1f);
        return Slice(checked((int)len)).ToArray();
    }

    public int ReadArrayHeader()
    {
        byte ib = NextByte();
        if (ib >> 5 != 4)
        {
            throw new FormatException($"CBOR: expected array, got major {ib >> 5}");
        }
        // An array needs at least one byte per element; a declared length past
        // the remaining input is malformed. Reject it (as a decode error, not
        // an overflow) so a bogus huge count never drives a large allocation.
        return CheckedCount(ReadLength(ib & 0x1f), "array");
    }

    public int ReadMapHeader()
    {
        byte ib = NextByte();
        if (ib >> 5 != 5)
        {
            throw new FormatException($"CBOR: expected map, got major {ib >> 5}");
        }
        return CheckedCount(ReadLength(ib & 0x1f), "map");
    }

    private int CheckedCount(ulong len, string what)
    {
        if (len > (ulong)RemainingBytes)
        {
            throw new FormatException(
                $"CBOR: {what} length {len} exceeds {RemainingBytes} remaining bytes");
        }
        return (int)len;
    }
}
