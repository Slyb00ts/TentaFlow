// ===== File: Program.cs — hello-dotnet smoke addon =====
// End-to-end proof of the .NET addon toolchain: lifecycle exports, storage
// round-trip, logging, tool dispatch and a CBOR UI panel rendered through
// ui_render_cbor.

#nullable enable

using System;
using System.Collections.Generic;
using System.Runtime.CompilerServices;
using System.Text.Json;
using TentaFlow.Sdk;
using TentaFlow.Sdk.Components;

namespace HelloDotnet;

internal static class Boot
{
    [ModuleInitializer]
    internal static void Init() => AddonRuntime.Register(new HelloAddon());
}

internal sealed class HelloAddon : AddonBase
{
    private const string AddonId = "hello-dotnet";
    private const string PanelId = "main";
    private const string SlotId = "content";

    public override void OnStart()
    {
        Log.Info("hello-dotnet addon started (.NET NativeAOT-LLVM, wasm32-wasip1)");
    }

    public override void OnPanelOpen(string panelId, ulong epoch)
    {
        if (panelId != PanelId)
        {
            return;
        }
        SendShell(epoch);
        SendContent(epoch);
    }

    public override string OnRequest(string requestJson)
    {
        using var doc = JsonDocument.Parse(requestJson);
        var root = doc.RootElement;
        string tool = root.TryGetProperty("tool", out var t) ? t.GetString() ?? "" : "";
        var params_ = root.TryGetProperty("params", out var p) ? p : default;

        return tool switch
        {
            "echo" => HandleEcho(params_),
            "test_storage" => HandleStorage(params_),
            _ => "{\"ok\":false,\"error\":\"unknown tool\"}",
        };
    }

    private static string HandleEcho(JsonElement params_)
    {
        string text = params_.ValueKind == JsonValueKind.Object
            && params_.TryGetProperty("text", out var t)
            ? t.GetString() ?? ""
            : "";
        using var stream = new System.IO.MemoryStream();
        using (var jw = new Utf8JsonWriter(stream))
        {
            jw.WriteStartObject();
            jw.WriteBoolean("ok", true);
            jw.WriteStartObject("data");
            jw.WriteString("echo", text);
            jw.WriteEndObject();
            jw.WriteEndObject();
        }
        return System.Text.Encoding.UTF8.GetString(stream.ToArray());
    }

    private static string HandleStorage(JsonElement params_)
    {
        string key = params_.ValueKind == JsonValueKind.Object
            && params_.TryGetProperty("key", out var k)
            ? k.GetString() ?? "k"
            : "k";
        string value = params_.ValueKind == JsonValueKind.Object
            && params_.TryGetProperty("value", out var v)
            ? v.GetString() ?? ""
            : "";
        Storage.Set(key, value);
        string? readBack = Storage.Get(key);
        using var stream = new System.IO.MemoryStream();
        using (var jw = new Utf8JsonWriter(stream))
        {
            jw.WriteStartObject();
            jw.WriteBoolean("ok", true);
            jw.WriteStartObject("data");
            jw.WriteString("written", value);
            jw.WriteString("read", readBack);
            jw.WriteBoolean("match", readBack == value);
            jw.WriteEndObject();
            jw.WriteEndObject();
        }
        return System.Text.Encoding.UTF8.GetString(stream.ToArray());
    }

    private void SendShell(ulong epoch)
    {
        var layout = new Stack
        {
            Id = "root",
            Gap = Spacing.Md,
            Align = FlexAlign.Stretch,
            Children = new List<Component>
            {
                new Heading
                {
                    Id = "title",
                    Content = Bind.Lit("Hello from .NET"),
                    Level = 2,
                },
                new Inspector
                {
                    Id = "content-host",
                    Title = Bind.Lit("Hello .NET"),
                    ContentSlot = SlotId,
                },
            },
        };
        Ui.Render(new PanelShell
        {
            AddonId = AddonId,
            PanelId = PanelId,
            PanelEpoch = epoch,
            Layout = layout,
            Slots = new List<SlotDecl>
            {
                new()
                {
                    Id = SlotId,
                    Semantics = SlotSemantics.MainContent,
                    DefaultState = SlotDefault.Loading,
                    CachePolicy = CachePolicy.None,
                    Visibility = SlotVisibility.Always,
                },
            },
        });
    }

    private void SendContent(ulong epoch)
    {
        var fragment = new Stack
        {
            Id = "content-root",
            Gap = Spacing.Sm,
            Align = FlexAlign.Stretch,
            Children = new List<Component>
            {
                new Text
                {
                    Id = "greeting",
                    Content = Bind.Lit("This panel was rendered by a C# addon."),
                    Style = TextStyle.Body,
                },
                new Text
                {
                    Id = "runtime-info",
                    Content = Bind.Lit("Runtime: .NET NativeAOT-LLVM → wasm32-wasip1"),
                    Style = TextStyle.Caption,
                    Tone = Tone.Muted,
                },
            },
        };
        Ui.Render(new SlotContent
        {
            AddonId = AddonId,
            PanelId = PanelId,
            PanelEpoch = epoch,
            SlotId = SlotId,
            Fragment = fragment,
        });
    }
}
