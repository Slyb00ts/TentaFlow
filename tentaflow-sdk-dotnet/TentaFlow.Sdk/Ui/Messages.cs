// ===== File: Ui/Messages.cs — UI-channel wire messages sent by addons =====
// Mirrors spec protocol/ui/{panel,slot,slot_msg,state,ui_payload}.rs for the
// addon→host direction of `ui_render_cbor`: PanelShell, SlotContent/Clear/
// Show/Hide, StateSnapshot/StatePatch/StateReset. Wire form of a payload is
// the CBOR array [tag: u16, body].

#nullable enable

using System;
using System.Collections.Generic;
using TentaFlow.Sdk.Cbor;

namespace TentaFlow.Sdk.Components;

/// <summary>Wire tags for UI-channel payloads (§6.1 table).</summary>
public enum UiTag : ushort
{
    PanelOpen = 0x0101,
    PanelShell = 0x0102,
    PanelReady = 0x0103,
    PanelError = 0x0104,
    PanelClose = 0x0105,
    PanelReset = 0x0106,
    SlotContent = 0x0110,
    SlotClear = 0x0111,
    SlotShow = 0x0112,
    SlotHide = 0x0113,
    StateSnapshot = 0x0120,
    StatePatch = 0x0121,
    StateReset = 0x0122,
    PatchRejected = 0x0123,
    Action = 0x0130,
    ActionAck = 0x0131,
    Command = 0x0140,
    Event = 0x0150,
    Batch = 0x0160,
}

/// <summary>A UI-channel message an addon can emit via ui_render_cbor.</summary>
public interface IUiPayload
{
    UiTag Tag { get; }

    Value BodyToValue();
}

public static class UiPayloadEncoder
{
    /// <summary>Encodes the payload as the canonical [tag, body] CBOR tuple.</summary>
    public static byte[] Encode(IUiPayload payload)
    {
        var w = new CborWriter(1024);
        w.WriteArrayHeader(2);
        w.WriteUInt((ushort)payload.Tag);
        payload.BodyToValue().Encode(w);
        return w.ToArray();
    }
}

// -----------------------------------------------------------------------------
// Slot declaration primitives (spec protocol/ui/slot.rs)
// -----------------------------------------------------------------------------

public enum SlotSemantics
{
    MainContent,
    Modal,
    Drawer,
    Toast,
    SidePanel,
    TabPane,
    Popover,
    Custom,
}

public static class SlotSemanticsWireExtensions
{
    public static string WireName(this SlotSemantics s) => s switch
    {
        SlotSemantics.MainContent => "main_content",
        SlotSemantics.Modal => "modal",
        SlotSemantics.Drawer => "drawer",
        SlotSemantics.Toast => "toast",
        SlotSemantics.SidePanel => "side_panel",
        SlotSemantics.TabPane => "tab_pane",
        SlotSemantics.Popover => "popover",
        SlotSemantics.Custom => "custom",
        _ => throw new ArgumentOutOfRangeException(nameof(s)),
    };
}

/// <summary>Initial slot content before the first SlotContent arrives.</summary>
public abstract class SlotDefault
{
    public static SlotDefault Empty { get; } = new SlotDefaultSimple("empty");
    public static SlotDefault Loading { get; } = new SlotDefaultSimple("loading");

    public static SlotDefault Static(Component fragment) => new SlotDefaultStatic(fragment);

    public abstract Value ToValue();

    private sealed class SlotDefaultSimple : SlotDefault
    {
        private readonly string _kind;

        internal SlotDefaultSimple(string kind)
        {
            _kind = kind;
        }

        public override Value ToValue() => Value.MapOf(("kind", Value.Text(_kind)));
    }

    private sealed class SlotDefaultStatic : SlotDefault
    {
        private readonly Component _fragment;

        internal SlotDefaultStatic(Component fragment)
        {
            _fragment = fragment;
        }

        public override Value ToValue() => Value.MapOf(
            ("kind", Value.Text("static")),
            ("fragment", _fragment.ToValue()));
    }
}

/// <summary>Cache lifecycle policy for slot content.</summary>
public abstract class CachePolicy
{
    public static CachePolicy None { get; } = new Simple("none");
    public static CachePolicy OnNavigateBack { get; } = new Simple("on_navigate_back");

    public static CachePolicy TtlSeconds(uint seconds) => new Ttl(seconds);

    public abstract Value ToValue();

    private sealed class Simple : CachePolicy
    {
        private readonly string _kind;

        internal Simple(string kind)
        {
            _kind = kind;
        }

        public override Value ToValue() => Value.MapOf(("kind", Value.Text(_kind)));
    }

    private sealed class Ttl : CachePolicy
    {
        private readonly uint _seconds;

        internal Ttl(uint seconds)
        {
            _seconds = seconds;
        }

        public override Value ToValue() => Value.MapOf(
            ("kind", Value.Text("ttl_seconds")),
            ("value", Value.UInt(_seconds)));
    }
}

/// <summary>Visibility policy of a slot.</summary>
public abstract class SlotVisibility
{
    public static SlotVisibility Always { get; } = new Simple("always");
    public static SlotVisibility Hidden { get; } = new Simple("hidden");

    public static SlotVisibility Conditional(StatePath path) => new Cond(path);

    public abstract Value ToValue();

    private sealed class Simple : SlotVisibility
    {
        private readonly string _kind;

        internal Simple(string kind)
        {
            _kind = kind;
        }

        public override Value ToValue() => Value.MapOf(("kind", Value.Text(_kind)));
    }

    private sealed class Cond : SlotVisibility
    {
        private readonly StatePath _path;

        internal Cond(StatePath path)
        {
            _path = path;
        }

        public override Value ToValue() => Value.MapOf(
            ("kind", Value.Text("conditional")),
            ("path", _path.ToValue()));
    }
}

/// <summary>Per-slot declaration inside a PanelShell.</summary>
public sealed class SlotDecl
{
    public string Id { get; set; } = "";
    public SlotSemantics Semantics { get; set; } = SlotSemantics.MainContent;
    public SlotDefault DefaultState { get; set; } = SlotDefault.Empty;
    public CachePolicy CachePolicy { get; set; } = CachePolicy.None;
    public SlotVisibility Visibility { get; set; } = SlotVisibility.Always;
    public uint? MaxPayloadBytes { get; set; }

    public Value ToValue()
    {
        var entries = new List<KeyValuePair<Value, Value>>
        {
            new(Value.UInt(0), Value.Text(Id)),
            new(Value.UInt(1), Value.Text(Semantics.WireName())),
            new(Value.UInt(2), DefaultState.ToValue()),
            new(Value.UInt(3), CachePolicy.ToValue()),
            new(Value.UInt(4), Visibility.ToValue()),
        };
        if (MaxPayloadBytes != null)
        {
            entries.Add(new(Value.UInt(5), Value.UInt(MaxPayloadBytes.Value)));
        }
        return Value.Map(entries);
    }
}

/// <summary>(path, value) tuple used by initial_state / state_overlay / snapshots.</summary>
public sealed class StateEntry
{
    public StatePath Path { get; set; } = new();
    public Value Value { get; set; } = Value.Null();

    public StateEntry()
    {
    }

    public StateEntry(StatePath path, Value value)
    {
        Path = path;
        Value = value;
    }

    public Value ToValue() => Components.Value.Map(new List<KeyValuePair<Value, Value>>
    {
        new(Components.Value.UInt(0), Path.ToValue()),
        new(Components.Value.UInt(1), Value),
    });
}

// -----------------------------------------------------------------------------
// Panel / slot / state messages (addon → host)
// -----------------------------------------------------------------------------

/// <summary>`PanelShell` (0x0102) — layout + slot declarations for a panel.</summary>
public sealed class PanelShell : IUiPayload
{
    public string AddonId { get; set; } = "";
    public string PanelId { get; set; } = "";
    public ulong PanelEpoch { get; set; }
    public Component Layout { get; set; } = null!;
    public List<SlotDecl> Slots { get; set; } = new();
    public List<StateEntry> InitialState { get; set; } = new();

    /// <summary>
    /// Raw CBOR command values (spec Command union, §6.5) executed by the
    /// renderer right after first paint. Values are encoded verbatim.
    /// </summary>
    public List<Value> InitialCommands { get; set; } = new();

    public UiTag Tag => UiTag.PanelShell;

    public Value BodyToValue() => Value.Map(new List<KeyValuePair<Value, Value>>
    {
        new(Value.UInt(0), Value.Text(AddonId)),
        new(Value.UInt(1), Value.Text(PanelId)),
        new(Value.UInt(2), Value.UInt(PanelEpoch)),
        new(Value.UInt(3), Layout.ToValue()),
        new(Value.UInt(4), Value.Array(Slots.ConvertAll(s => s.ToValue()))),
        new(Value.UInt(5), Value.Array(InitialState.ConvertAll(s => s.ToValue()))),
        new(Value.UInt(6), Value.Array(InitialCommands)),
    });
}

/// <summary>`SlotContent` (0x0110) — replaces a slot's fragment.</summary>
public sealed class SlotContent : IUiPayload
{
    public string AddonId { get; set; } = "";
    public string PanelId { get; set; } = "";
    public ulong PanelEpoch { get; set; }
    public string SlotId { get; set; } = "";
    public Component Fragment { get; set; } = null!;
    public List<StateEntry>? StateOverlay { get; set; }

    public UiTag Tag => UiTag.SlotContent;

    public Value BodyToValue()
    {
        var entries = new List<KeyValuePair<Value, Value>>
        {
            new(Value.UInt(0), Value.Text(AddonId)),
            new(Value.UInt(1), Value.Text(PanelId)),
            new(Value.UInt(2), Value.UInt(PanelEpoch)),
            new(Value.UInt(3), Value.Text(SlotId)),
            new(Value.UInt(4), Fragment.ToValue()),
        };
        if (StateOverlay != null)
        {
            entries.Add(new(Value.UInt(5),
                Value.Array(StateOverlay.ConvertAll(s => s.ToValue()))));
        }
        return Value.Map(entries);
    }
}

/// <summary>Shared shape of SlotClear/SlotShow/SlotHide (0x0111–0x0113).</summary>
public abstract class SlotRef : IUiPayload
{
    public string AddonId { get; set; } = "";
    public string PanelId { get; set; } = "";
    public ulong PanelEpoch { get; set; }
    public string SlotId { get; set; } = "";

    public abstract UiTag Tag { get; }

    public Value BodyToValue() => Value.Map(new List<KeyValuePair<Value, Value>>
    {
        new(Value.UInt(0), Value.Text(AddonId)),
        new(Value.UInt(1), Value.Text(PanelId)),
        new(Value.UInt(2), Value.UInt(PanelEpoch)),
        new(Value.UInt(3), Value.Text(SlotId)),
    });
}

public sealed class SlotClear : SlotRef
{
    public override UiTag Tag => UiTag.SlotClear;
}

public sealed class SlotShow : SlotRef
{
    public override UiTag Tag => UiTag.SlotShow;
}

public sealed class SlotHide : SlotRef
{
    public override UiTag Tag => UiTag.SlotHide;
}

/// <summary>`StateSnapshot` (0x0120) — full state for a panel.</summary>
public sealed class StateSnapshot : IUiPayload
{
    public string AddonId { get; set; } = "";
    public string PanelId { get; set; } = "";
    public ulong PanelEpoch { get; set; }
    public ulong StateRevision { get; set; }

    /// <summary>MUST be sorted by canonical encoded StatePath bytes (§6.4).</summary>
    public List<StateEntry> Entries { get; set; } = new();

    public bool Truncated { get; set; }

    public UiTag Tag => UiTag.StateSnapshot;

    public Value BodyToValue() => Value.Map(new List<KeyValuePair<Value, Value>>
    {
        new(Value.UInt(0), Value.Text(AddonId)),
        new(Value.UInt(1), Value.Text(PanelId)),
        new(Value.UInt(2), Value.UInt(PanelEpoch)),
        new(Value.UInt(3), Value.UInt(StateRevision)),
        new(Value.UInt(4), Value.Array(Entries.ConvertAll(e => e.ToValue()))),
        new(Value.UInt(5), Value.Bool(Truncated)),
    });
}

/// <summary>`StatePatch` (0x0121) — incremental state mutations.</summary>
public sealed class StatePatch : IUiPayload
{
    public string AddonId { get; set; } = "";
    public string PanelId { get; set; } = "";
    public ulong PanelEpoch { get; set; }
    public ulong BaseRevision { get; set; }
    public ulong NewRevision { get; set; }
    public List<PatchOp> Ops { get; set; } = new();

    public UiTag Tag => UiTag.StatePatch;

    public Value BodyToValue() => Value.Map(new List<KeyValuePair<Value, Value>>
    {
        new(Value.UInt(0), Value.Text(AddonId)),
        new(Value.UInt(1), Value.Text(PanelId)),
        new(Value.UInt(2), Value.UInt(PanelEpoch)),
        new(Value.UInt(3), Value.UInt(BaseRevision)),
        new(Value.UInt(4), Value.UInt(NewRevision)),
        new(Value.UInt(5), Value.Array(Ops.ConvertAll(o => o.ToValue()))),
    });
}

/// <summary>`StateReset` (0x0122) — drop all state, restart at new_revision.</summary>
public sealed class StateReset : IUiPayload
{
    public string AddonId { get; set; } = "";
    public string PanelId { get; set; } = "";
    public ulong PanelEpoch { get; set; }
    public ulong NewRevision { get; set; }

    public UiTag Tag => UiTag.StateReset;

    public Value BodyToValue() => Value.Map(new List<KeyValuePair<Value, Value>>
    {
        new(Value.UInt(0), Value.Text(AddonId)),
        new(Value.UInt(1), Value.Text(PanelId)),
        new(Value.UInt(2), Value.UInt(PanelEpoch)),
        new(Value.UInt(3), Value.UInt(NewRevision)),
    });
}
