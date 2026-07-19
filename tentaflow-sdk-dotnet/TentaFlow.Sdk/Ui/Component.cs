// ===== File: Ui/Component.cs — Component envelope + FieldMap + HandlerMap =====
// Mirrors spec protocol/ui/component.rs: every UI tree node is a map
// {0: tag, 1: id, 2: fields, 3?: handlers, 4?: bind, 5?: a11y,
//  6?: visibility, 7?: test_id} with u8 keys, Option fields omitted.

#nullable enable

using System;
using System.Collections.Generic;
using TentaFlow.Sdk.Cbor;

namespace TentaFlow.Sdk.Components;

/// <summary>Opaque per-component field bag: map&lt;u8, Value&gt; on the wire.</summary>
public sealed class FieldMap
{
    private readonly List<KeyValuePair<byte, Value>> _entries = new();

    public IReadOnlyList<KeyValuePair<byte, Value>> Entries => _entries;

    public void Set(byte key, Value value)
    {
        _entries.Add(new KeyValuePair<byte, Value>(key, value));
    }

    public Value ToValue()
    {
        var entries = new List<KeyValuePair<Value, Value>>(_entries.Count);
        foreach (var (k, v) in AsTuples())
        {
            entries.Add(new KeyValuePair<Value, Value>(Value.UInt(k), v));
        }
        return Value.Map(entries);
    }

    private IEnumerable<(byte, Value)> AsTuples()
    {
        foreach (var e in _entries)
        {
            yield return (e.Key, e.Value);
        }
    }
}

/// <summary>Ordered (EventKind, Handler) list; wire form is a tstr-keyed map.</summary>
public sealed class HandlerMap
{
    private readonly List<KeyValuePair<EventKind, Handler>> _entries = new();

    public HandlerMap()
    {
    }

    public HandlerMap(EventKind kind, Handler handler)
    {
        Add(kind, handler);
    }

    public HandlerMap Add(EventKind kind, Handler handler)
    {
        _entries.Add(new KeyValuePair<EventKind, Handler>(kind, handler));
        return this;
    }

    public Value ToValue()
    {
        var entries = new List<KeyValuePair<Value, Value>>(_entries.Count);
        foreach (var e in _entries)
        {
            entries.Add(new KeyValuePair<Value, Value>(
                Value.Text(e.Key.WireName()), e.Value.ToValue()));
        }
        return Value.Map(entries);
    }
}

/// <summary>Base envelope for every typed UI component (catalog §1.6).</summary>
public abstract class Component
{
    /// <summary>Unique id within the panel.</summary>
    public string Id { get; set; } = "";

    public HandlerMap? Handlers { get; set; }

    public BindSpec? Bind { get; set; }

    public Accessibility? A11y { get; set; }

    public Visibility? Visibility { get; set; }

    /// <summary>Stable E2E identifier, validated [a-z0-9_-]{1,64} by the host.</summary>
    public string? TestId { get; set; }

    /// <summary>Stable wire discriminant (catalog tag tables).</summary>
    public abstract ushort ComponentTag { get; }

    /// <summary>Lowers the typed per-tag fields into the opaque FieldMap.</summary>
    public abstract FieldMap ToFieldMap();

    /// <summary>Fluent id setter so trees read like the Rust `into_component(id)`.</summary>
    public Component WithId(string id)
    {
        Id = id;
        return this;
    }

    public Value ToValue()
    {
        var entries = new List<KeyValuePair<Value, Value>>(8)
        {
            new(Value.UInt(0), Value.UInt(ComponentTag)),
            new(Value.UInt(1), Value.Text(Id)),
            new(Value.UInt(2), ToFieldMap().ToValue()),
        };
        if (Handlers != null)
        {
            entries.Add(new(Value.UInt(3), Handlers.ToValue()));
        }
        if (Bind != null)
        {
            entries.Add(new(Value.UInt(4), Bind.ToValue()));
        }
        if (A11y != null)
        {
            entries.Add(new(Value.UInt(5), A11y.ToValue()));
        }
        if (Visibility != null)
        {
            entries.Add(new(Value.UInt(6), Visibility.ToValue()));
        }
        if (TestId != null)
        {
            entries.Add(new(Value.UInt(7), Value.Text(TestId)));
        }
        return Value.Map(entries);
    }

    public void Encode(CborWriter w) => ToValue().Encode(w);
}

/// <summary>
/// Escape hatch for components not covered by the generated catalog classes
/// (or for replaying a decoded component verbatim).
/// </summary>
public sealed class RawComponent : Component
{
    public ushort Tag { get; set; }

    public FieldMap Fields { get; set; } = new();

    public override ushort ComponentTag => Tag;

    public override FieldMap ToFieldMap() => Fields;
}
