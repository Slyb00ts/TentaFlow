// ===== File: Ui/A11y.cs — Accessibility + Visibility metadata (spec protocol/ui/a11y.rs) =====
// Integer-keyed CBOR maps attached to the Component envelope; Option fields
// are omitted when null, `HiddenForAssistive` is always present.

#nullable enable

using System.Collections.Generic;

namespace TentaFlow.Sdk.Components;

public sealed class Accessibility
{
    public string? Role { get; set; }
    public BindRef? Label { get; set; }
    public string? LabelFor { get; set; }
    public string? DescribedBy { get; set; }
    public LiveRegion? Live { get; set; }
    public BindRef? Expanded { get; set; }
    public BindRef? Disabled { get; set; }
    public BindRef? Required { get; set; }
    public BindRef? Invalid { get; set; }
    public BindRef? Pressed { get; set; }
    public BindRef? Selected { get; set; }

    public Value ToValue()
    {
        var entries = new List<KeyValuePair<Value, Value>>();
        if (Role != null) entries.Add(new(Value.UInt(0), Value.Text(Role)));
        if (Label != null) entries.Add(new(Value.UInt(1), Label.ToValue()));
        if (LabelFor != null) entries.Add(new(Value.UInt(2), Value.Text(LabelFor)));
        if (DescribedBy != null) entries.Add(new(Value.UInt(3), Value.Text(DescribedBy)));
        if (Live != null) entries.Add(new(Value.UInt(4), Live.Value.ToWire()));
        if (Expanded != null) entries.Add(new(Value.UInt(5), Expanded.ToValue()));
        if (Disabled != null) entries.Add(new(Value.UInt(6), Disabled.ToValue()));
        if (Required != null) entries.Add(new(Value.UInt(7), Required.ToValue()));
        if (Invalid != null) entries.Add(new(Value.UInt(8), Invalid.ToValue()));
        if (Pressed != null) entries.Add(new(Value.UInt(9), Pressed.ToValue()));
        if (Selected != null) entries.Add(new(Value.UInt(10), Selected.ToValue()));
        return Value.Map(entries);
    }
}

public sealed class Visibility
{
    public BindRef? Visible { get; set; }
    public Breakpoint? DisplayAboveBreakpoint { get; set; }
    public Breakpoint? DisplayBelowBreakpoint { get; set; }
    public bool HiddenForAssistive { get; set; }

    public Value ToValue()
    {
        var entries = new List<KeyValuePair<Value, Value>>();
        if (Visible != null) entries.Add(new(Value.UInt(0), Visible.ToValue()));
        if (DisplayAboveBreakpoint != null)
        {
            entries.Add(new(Value.UInt(1), DisplayAboveBreakpoint.Value.ToWire()));
        }
        if (DisplayBelowBreakpoint != null)
        {
            entries.Add(new(Value.UInt(2), DisplayBelowBreakpoint.Value.ToWire()));
        }
        entries.Add(new(Value.UInt(3), Value.Bool(HiddenForAssistive)));
        return Value.Map(entries);
    }
}
