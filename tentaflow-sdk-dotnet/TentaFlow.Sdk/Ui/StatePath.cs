// ===== File: Ui/StatePath.cs — typed state path + BindRef helpers =====
// StatePath is a CBOR array of PathSegment maps (spec protocol/ui/bind.rs).
// PathSegment / BindRef themselves are generated tagged unions in
// Components.g.cs; this file adds the array wrapper and ergonomics.

#nullable enable

using System.Collections.Generic;

namespace TentaFlow.Sdk.Components;

public sealed class StatePath
{
    public List<PathSegment> Segments { get; } = new();

    public StatePath()
    {
    }

    public StatePath(IEnumerable<PathSegment> segments)
    {
        Segments.AddRange(segments);
    }

    /// <summary>Builds a path of map-key segments: Keys("a", "b") → a.b</summary>
    public static StatePath Keys(params string[] keys)
    {
        var path = new StatePath();
        foreach (var k in keys)
        {
            path.Segments.Add(new PathSegmentKey { Value = k });
        }
        return path;
    }

    public StatePath Key(string key)
    {
        Segments.Add(new PathSegmentKey { Value = key });
        return this;
    }

    public StatePath Index(uint index)
    {
        Segments.Add(new PathSegmentIndex { Value = index });
        return this;
    }

    public Value ToValue()
    {
        var items = new List<Value>(Segments.Count);
        foreach (var seg in Segments)
        {
            items.Add(seg.ToValue());
        }
        return Value.Array(items);
    }
}

/// <summary>Ergonomic constructors for BindRef (generated tagged union).</summary>
public static class Bind
{
    /// <summary>Literal text value.</summary>
    public static BindRef Lit(string text) =>
        new BindRefLiteral { Value = Value.Text(text) };

    /// <summary>Literal arbitrary value.</summary>
    public static BindRef Lit(Value value) =>
        new BindRefLiteral { Value = value };

    /// <summary>Bound to a state path of map keys.</summary>
    public static BindRef State(params string[] keys) =>
        new BindRefBound { Path = StatePath.Keys(keys) };

    public static BindRef State(StatePath path) =>
        new BindRefBound { Path = path };
}
