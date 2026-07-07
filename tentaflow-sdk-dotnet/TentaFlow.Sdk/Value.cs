// ===== File: Value.cs — generic CBOR Value model (mirror of spec protocol/value.rs) =====
// Typed representation of arbitrary CBOR data used everywhere the protocol
// declares `Value` / `map<u8, Value>` / `map<tstr, Value>`. Encodes with
// canonical (bytewise-sorted) map keys and rejects duplicate keys, matching
// the Rust encoder byte-for-byte.

#nullable enable

using System;
using System.Collections.Generic;
using TentaFlow.Sdk.Cbor;

namespace TentaFlow.Sdk.Components;

/// <summary>Marker attribute carrying the wire string of an enum variant.</summary>
[AttributeUsage(AttributeTargets.Field)]
public sealed class WireAttribute : Attribute
{
    public string Name { get; }

    public WireAttribute(string name)
    {
        Name = name;
    }
}

public sealed class Value
{
    public enum ValueKind
    {
        Null,
        Bool,
        UInt,
        NegInt,
        Float,
        Bytes,
        Text,
        Array,
        Map,
    }

    public ValueKind Kind { get; }

    private readonly ulong _u;
    private readonly long _i;
    private readonly double _f;
    private readonly bool _b;
    private readonly string? _s;
    private readonly byte[]? _bytes;
    private readonly List<Value>? _array;
    private readonly List<KeyValuePair<Value, Value>>? _map;

    private Value(ValueKind kind, ulong u = 0, long i = 0, double f = 0, bool b = false,
        string? s = null, byte[]? bytes = null, List<Value>? array = null,
        List<KeyValuePair<Value, Value>>? map = null)
    {
        Kind = kind;
        _u = u;
        _i = i;
        _f = f;
        _b = b;
        _s = s;
        _bytes = bytes;
        _array = array;
        _map = map;
    }

    private static readonly Value NullValue = new(ValueKind.Null);
    private static readonly Value TrueValue = new(ValueKind.Bool, b: true);
    private static readonly Value FalseValue = new(ValueKind.Bool, b: false);

    public static Value Null() => NullValue;

    public static Value Bool(bool b) => b ? TrueValue : FalseValue;

    public static Value UInt(ulong v) => new(ValueKind.UInt, u: v);

    /// <summary>Non-negative values encode as major-0 (like Rust's Value::U64 on decode).</summary>
    public static Value Int(long v) =>
        v >= 0 ? new(ValueKind.UInt, u: (ulong)v) : new(ValueKind.NegInt, i: v);

    public static Value Float(double v) => new(ValueKind.Float, f: v);

    public static Value Text(string s) => new(ValueKind.Text, s: s);

    public static Value Bytes(byte[] b) => new(ValueKind.Bytes, bytes: b);

    public static Value Array(List<Value> items) => new(ValueKind.Array, array: items);

    public static Value Array(params Value[] items) =>
        new(ValueKind.Array, array: new List<Value>(items));

    public static Value Map(List<KeyValuePair<Value, Value>> entries) =>
        new(ValueKind.Map, map: entries);

    /// <summary>Empty tstr-keyed map (e.g. an empty Handler params CborMap).</summary>
    public static Value EmptyMap() => Map(new List<KeyValuePair<Value, Value>>());

    /// <summary>Builds a tstr-keyed map from alternating key/value pairs.</summary>
    public static Value MapOf(params (string Key, Value Val)[] entries)
    {
        var list = new List<KeyValuePair<Value, Value>>(entries.Length);
        foreach (var (k, v) in entries)
        {
            list.Add(new KeyValuePair<Value, Value>(Text(k), v));
        }
        return Map(list);
    }

    // -- Accessors (used by decode paths and tests) -------------------------

    public bool AsBool => Kind == ValueKind.Bool
        ? _b
        : throw new InvalidOperationException($"Value is {Kind}, not Bool");

    public ulong AsUInt => Kind == ValueKind.UInt
        ? _u
        : throw new InvalidOperationException($"Value is {Kind}, not UInt");

    public long AsInt => Kind switch
    {
        ValueKind.UInt => checked((long)_u),
        ValueKind.NegInt => _i,
        _ => throw new InvalidOperationException($"Value is {Kind}, not integer"),
    };

    public double AsFloat => Kind == ValueKind.Float
        ? _f
        : throw new InvalidOperationException($"Value is {Kind}, not Float");

    public string AsText => Kind == ValueKind.Text
        ? _s!
        : throw new InvalidOperationException($"Value is {Kind}, not Text");

    public byte[] AsBytes => Kind == ValueKind.Bytes
        ? _bytes!
        : throw new InvalidOperationException($"Value is {Kind}, not Bytes");

    public List<Value> AsArray => Kind == ValueKind.Array
        ? _array!
        : throw new InvalidOperationException($"Value is {Kind}, not Array");

    public List<KeyValuePair<Value, Value>> AsMap => Kind == ValueKind.Map
        ? _map!
        : throw new InvalidOperationException($"Value is {Kind}, not Map");

    // -- Encoding ------------------------------------------------------------

    public void Encode(CborWriter w)
    {
        switch (Kind)
        {
            case ValueKind.Null:
                w.WriteNull();
                break;
            case ValueKind.Bool:
                w.WriteBool(_b);
                break;
            case ValueKind.UInt:
                w.WriteUInt(_u);
                break;
            case ValueKind.NegInt:
                w.WriteInt(_i);
                break;
            case ValueKind.Float:
                w.WriteFloat64(_f);
                break;
            case ValueKind.Bytes:
                w.WriteBytes(_bytes);
                break;
            case ValueKind.Text:
                w.WriteText(_s!);
                break;
            case ValueKind.Array:
                w.WriteArrayHeader(_array!.Count);
                foreach (var item in _array)
                {
                    item.Encode(w);
                }
                break;
            case ValueKind.Map:
                EncodeMap(w);
                break;
        }
    }

    private void EncodeMap(CborWriter w)
    {
        var entries = _map!;
        // Canonical key order: sort by the bytewise CBOR encoding of the key.
        var indexed = new List<(byte[] KeyBytes, KeyValuePair<Value, Value> Entry)>(entries.Count);
        foreach (var entry in entries)
        {
            var kw = new CborWriter(16);
            entry.Key.Encode(kw);
            indexed.Add((kw.ToArray(), entry));
        }
        indexed.Sort(static (a, b) => CompareBytes(a.KeyBytes, b.KeyBytes));
        for (int i = 1; i < indexed.Count; i++)
        {
            if (CompareBytes(indexed[i - 1].KeyBytes, indexed[i].KeyBytes) == 0)
            {
                throw new InvalidOperationException("Value.Map: duplicate map key");
            }
        }
        w.WriteMapHeader(indexed.Count);
        foreach (var (keyBytes, entry) in indexed)
        {
            w.WriteEncoded(keyBytes);
            entry.Value.Encode(w);
        }
    }

    private static int CompareBytes(byte[] a, byte[] b)
    {
        int min = Math.Min(a.Length, b.Length);
        for (int i = 0; i < min; i++)
        {
            int cmp = a[i].CompareTo(b[i]);
            if (cmp != 0)
            {
                return cmp;
            }
        }
        return a.Length.CompareTo(b.Length);
    }

    public byte[] ToCborBytes()
    {
        var w = new CborWriter();
        Encode(w);
        return w.ToArray();
    }

    // -- Decoding ------------------------------------------------------------

    public static Value Decode(CborReader r)
    {
        byte ib = r.PeekInitialByte();
        int major = ib >> 5;
        switch (major)
        {
            case 0:
                return UInt(r.ReadUInt());
            case 1:
                return Int(r.ReadInt());
            case 2:
                return Bytes(r.ReadBytes());
            case 3:
                return Text(r.ReadText());
            case 4:
                {
                    int n = r.ReadArrayHeader();
                    // Each element is at least one byte, so the declared count
                    // cannot legitimately exceed the bytes left. Cap the initial
                    // capacity accordingly; the List grows if the payload really
                    // holds that many items (a malformed huge N then simply runs
                    // out of input and throws, without a giant up-front alloc).
                    var items = new List<Value>(Math.Min(n, r.RemainingBytes));
                    for (int i = 0; i < n; i++)
                    {
                        items.Add(Decode(r));
                    }
                    return Array(items);
                }
            case 5:
                {
                    int n = r.ReadMapHeader();
                    // Each pair is at least two bytes; cap at RemainingBytes / 2.
                    var entries = new List<KeyValuePair<Value, Value>>(
                        Math.Min(n, r.RemainingBytes / 2));
                    for (int i = 0; i < n; i++)
                    {
                        var k = Decode(r);
                        var v = Decode(r);
                        entries.Add(new KeyValuePair<Value, Value>(k, v));
                    }
                    return Map(entries);
                }
            case 7:
                switch (ib)
                {
                    case 0xf4:
                    case 0xf5:
                        return Bool(r.ReadBool());
                    case 0xf6:
                        r.ReadNull();
                        return Null();
                    case 0xf9:
                    case 0xfa:
                    case 0xfb:
                        return Float(r.ReadFloat());
                    default:
                        throw new FormatException($"CBOR: unsupported simple value 0x{ib:x2}");
                }
            default:
                throw new FormatException($"CBOR: unsupported major type {major}");
        }
    }
}
