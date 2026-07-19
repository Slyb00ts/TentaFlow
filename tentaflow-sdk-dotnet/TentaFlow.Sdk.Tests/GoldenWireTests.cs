// ===== File: GoldenWireTests.cs — byte-for-byte compatibility with the Rust encoders =====
// golden/vectors.txt is produced by the Rust `tentaflow-sdk-spec` encoders
// (see tentaflow-sdk-dotnet/README.md for the regeneration snippet). Every
// test rebuilds the same value with the C# SDK and compares the hex output.

#nullable enable

using System;
using System.Collections.Generic;
using System.IO;
using TentaFlow.Sdk;
using TentaFlow.Sdk.Components;
using Xunit;

namespace TentaFlow.Sdk.Tests;

public class GoldenWireTests
{
    private static readonly Dictionary<string, string> Vectors = LoadVectors();

    private static Dictionary<string, string> LoadVectors()
    {
        var path = Path.Combine(AppContext.BaseDirectory, "golden", "vectors.txt");
        var result = new Dictionary<string, string>();
        foreach (var line in File.ReadAllLines(path))
        {
            if (string.IsNullOrWhiteSpace(line))
            {
                continue;
            }
            var parts = line.Split(' ', 2);
            result[parts[0]] = parts[1].Trim();
        }
        return result;
    }

    private static string Hex(byte[] bytes) => Convert.ToHexStringLower(bytes);

    private static void AssertGolden(string name, byte[] actual)
    {
        Assert.Equal(Vectors[name], Hex(actual));
    }

    private static Text SampleText() => new()
    {
        Id = "txt-1",
        Content = Bind.Lit("Hello"),
        Style = TextStyle.Body,
        Tone = Tone.Primary,
        MaxLines = 3,
    };

    private static Heading SampleHeading()
    {
        var heading = new Heading
        {
            Id = "hd-1",
            Content = Bind.State(new StatePath().Key("title").Index(2)),
            Level = 2,
            TestId = "hd-test",
        };
        heading.Handlers = new HandlerMap(EventKind.Click, new HandlerBackend
        {
            ActionId = "do-it",
            Params = Value.MapOf(
                ("zz", Value.UInt(9)),
                ("aa", Value.Text("x"))),
            OnFailure = new FailurePolicyToast(),
        });
        return heading;
    }

    [Fact]
    public void StateSetInputMatchesRust()
    {
        AssertGolden(
            "state_set_input",
            SharedState.EncodeSetInput("k1", new byte[] { 1, 2, 250 }, StateTier.Durable));
    }

    [Fact]
    public void TextComponentMatchesRust()
    {
        AssertGolden("text_component", SampleText().ToValue().ToCborBytes());
    }

    [Fact]
    public void HeadingWithHandlerMatchesRust()
    {
        AssertGolden("heading_with_handler", SampleHeading().ToValue().ToCborBytes());
    }

    [Fact]
    public void PanelShellPayloadMatchesRust()
    {
        var shell = new PanelShell
        {
            AddonId = "hello-dotnet",
            PanelId = "main",
            PanelEpoch = 7,
            Layout = new Stack
            {
                Id = "root",
                Gap = Spacing.Md,
                Align = FlexAlign.Stretch,
                Children = new List<Component> { SampleText(), SampleHeading() },
            },
            Slots = new List<SlotDecl>
            {
                new()
                {
                    Id = "content",
                    Semantics = SlotSemantics.MainContent,
                    DefaultState = SlotDefault.Loading,
                    CachePolicy = CachePolicy.TtlSeconds(60),
                    Visibility = SlotVisibility.Always,
                },
            },
            InitialState = new List<StateEntry>
            {
                new(StatePath.Keys("count"), Value.UInt(42)),
            },
        };
        AssertGolden("panel_shell_payload", UiPayloadEncoder.Encode(shell));
    }

    [Fact]
    public void SlotContentPayloadMatchesRust()
    {
        var slot = new SlotContent
        {
            AddonId = "hello-dotnet",
            PanelId = "main",
            PanelEpoch = 7,
            SlotId = "content",
            Fragment = SampleText(),
            StateOverlay = new List<StateEntry>
            {
                new(StatePath.Keys("ready"), Value.Bool(true)),
            },
        };
        AssertGolden("slot_content_payload", UiPayloadEncoder.Encode(slot));
    }

    [Fact]
    public void StatePatchPayloadMatchesRust()
    {
        var patch = new StatePatch
        {
            AddonId = "hello-dotnet",
            PanelId = "main",
            PanelEpoch = 7,
            BaseRevision = 3,
            NewRevision = 4,
            Ops = new List<PatchOp>
            {
                new()
                {
                    Path = StatePath.Keys("count"),
                    Op = new PatchOpKindIncrement { Delta = -2 },
                },
                new()
                {
                    Path = StatePath.Keys("items"),
                    Op = new PatchOpKindAppendArray { Value = Value.Text("new") },
                },
                new()
                {
                    Path = StatePath.Keys("tmp"),
                    Op = new PatchOpKindDelete(),
                },
            },
        };
        AssertGolden("state_patch_payload", UiPayloadEncoder.Encode(patch));
    }

    [Fact]
    public void MixedValueMatchesRust()
    {
        var value = Value.Map(new List<KeyValuePair<Value, Value>>
        {
            new(Value.Text("bbb"), Value.Float(1.5)),
            new(Value.Text("a"), Value.Int(-7)),
            new(Value.UInt(3), Value.Array(
                Value.Null(),
                Value.Bool(false),
                Value.Bytes(new byte[] { 0xde, 0xad }))),
        });
        AssertGolden("mixed_value", value.ToCborBytes());
    }

    [Fact]
    public void RawComponentMatchesRust()
    {
        var raw = new RawComponent { Tag = 0x0001, Id = "r" };
        raw.Fields.Set(5, Value.UInt(1));
        raw.Fields.Set(0, Value.Text("z"));
        AssertGolden("raw_component", raw.ToValue().ToCborBytes());
    }

    [Fact]
    public void ValueDecodeRoundTripsGoldenBytes()
    {
        foreach (var (name, hex) in Vectors)
        {
            var bytes = Convert.FromHexString(hex);
            var reader = new TentaFlow.Sdk.Cbor.CborReader(bytes);
            var value = Value.Decode(reader);
            // Skip [tag, body] payloads whose FieldMap ordering is
            // normalization-sensitive; scalar/map goldens must re-encode
            // to identical bytes through the canonical writer.
            if (name is "mixed_value" or "state_set_input" or "raw_component"
                or "text_component" or "heading_with_handler")
            {
                Assert.Equal(hex, Hex(value.ToCborBytes()));
            }
            Assert.True(reader.AtEnd, $"{name}: trailing bytes after decode");
        }
    }
}
