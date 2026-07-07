# TentaFlow .NET Addon SDK

C# SDK for building TentaFlow WASM addons (`runtime = "dotnet"`), compiled to
standalone `wasm32-wasip1` modules with **.NET 10 NativeAOT-LLVM**.

## Toolchain

| Piece | Version / source |
|-------|------------------|
| .NET SDK | 10.0.300+ |
| ILCompiler | `Microsoft.DotNet.ILCompiler.LLVM` **10.0.0-rc.1.26355.1** + `runtime.linux-x64.Microsoft.DotNet.ILCompiler.LLVM` (feed: `dotnet-experimental`, see the addon `nuget.config`) |
| WASI SDK | wasi-sdk **25.0+** (clang/wasm-ld used for linking); `WASI_SDK_PATH` must point at it. `tentaflow-core/build.rs` auto-detects `wasi-sdk-*` under `~/.cache/tentaflow-native-libs/` |
| RID | `wasi-wasm` |

The default NativeAOT-LLVM output for `wasi-wasm` is a **wasip2 component**,
which the wasmtime host cannot load. Two command-line properties force a bare
wasip1 core module (they must be command-line properties because the SDK
targets overwrite project-level values — each addon ships them in
`Directory.Build.rsp`):

```
-p:IlcLlvmTarget=wasm32-unknown-wasip1
-p:LinkerFlavor=lld
```

Build an addon:

```bash
cd tentaflow-core/addons/hello-dotnet
WASI_SDK_PATH=~/.cache/tentaflow-native-libs/wasi-sdk-25.0-x86_64-linux \
  dotnet publish -c Release -r wasi-wasm
# → bin/Release/net10.0/wasi-wasm/publish/<Assembly>.wasm
```

`tentaflow-core/build.rs` does this automatically for every bundled addon
directory containing a `.csproj` + `manifest.toml`, and skips with a warning
when the dotnet SDK or WASI SDK is missing (mirroring the Rust
`wasm32-wasip1`-target skip).

## Host contract (DotnetAdapter)

The wasm module is a WASI **reactor** (`-mexec-model=reactor`):

- exports `_initialize` — boots the NativeAOT runtime and runs all module
  initializers (this is where `AddonRuntime.Register` must happen);
- exports `memory`, `alloc(i32)->i32`, `dealloc(i32,i32)`;
- exports the prefixed lifecycle entry points `tentaflow_on_start`,
  `tentaflow_on_stop`, `tentaflow_on_request`, `tentaflow_on_event`,
  `tentaflow_on_tick`, `tentaflow_on_panel_open` (all provided by the SDK's
  `Exports` class — the addon only implements `AddonBase`);
- imports host functions from the wasm module `"tentaflow"`.

## Writing an addon

```csharp
using System.Runtime.CompilerServices;
using TentaFlow.Sdk;
using TentaFlow.Sdk.Components;

internal static class Boot
{
    [ModuleInitializer]
    internal static void Init() => AddonRuntime.Register(new MyAddon());
}

internal sealed class MyAddon : AddonBase
{
    public override void OnStart() => Log.Info("started");

    public override void OnPanelOpen(string panelId, ulong epoch)
    {
        Ui.Render(new PanelShell { /* ... */ PanelEpoch = epoch });
    }

    public override string OnRequest(string requestJson) => "{\"ok\":true}";
}
```

The addon `.csproj` needs (see `tentaflow-core/addons/hello-dotnet/` for the
complete template, including `Directory.Build.rsp` and `nuget.config`):

```xml
<NativeLib>shared</NativeLib>
<ItemGroup>
  <ProjectReference Include=".../TentaFlow.Sdk/TentaFlow.Sdk.csproj" />
  <UnmanagedEntryPointsAssembly Include="TentaFlow.Sdk" />
  <TrimmerRootAssembly Include="MyAddon" /> <!-- keeps the ModuleInitializer -->
</ItemGroup>
```

## SDK surface

- `AddonBase` / `AddonRuntime` — lifecycle dispatch behind the wasm exports.
- `Log`, `Storage`, `SharedState`, `Secrets`, `Config`, `Http`, `Llm`,
  `Events`, `Users`, `Sql`, `Flows`, `Services`, `Tools`, `Ui` — typed
  wrappers over the host functions (`TentaFlow.Sdk` namespace).
- `TentaFlow.Sdk.Components` — the full typed UI catalog. `Components.g.cs`
  is generated from `tentaflow-sdk-spec` by `scripts/gen-csharp.sh`
  (`tentaflow-sdk-gen`, bin `tentaflow-sdk-gen-csharp`); the support types
  (`Value`, `Component`, `FieldMap`, `HandlerMap`, `StatePath`, `PanelShell`,
  `SlotContent`, `StatePatch`, …) live in this project and produce canonical
  CBOR byte-identical to the Rust encoders.

Regenerate the component catalog after a spec change:

```bash
./scripts/gen-csharp.sh
```

## Wire-format tests

`TentaFlow.Sdk.Tests` compares the C# CBOR output byte-for-byte against golden
vectors produced by the Rust `tentaflow-sdk-spec` encoders
(`TentaFlow.Sdk.Tests/golden/vectors.txt`):

```bash
cd tentaflow-sdk-dotnet/TentaFlow.Sdk.Tests && dotnet test
```

End-to-end host tests (wasmtime loads the built hello-dotnet module and calls
the lifecycle exports + host functions):

```bash
cd tentaflow-core && cargo test --test addon_dotnet_e2e
```

## Known limitations

- **`on_request` responses are hard-capped at 64 KiB.** The host calls the
  guest `tentaflow_on_request` export with a fixed 64 KiB output buffer and
  bails on any nonzero return — there is no buffer-too-small→retry protocol
  for this export (verified in `tentaflow-core/src/addon/mod.rs`). This is a
  host-wide limit shared with Rust addons. `WriteResponse` logs and fails
  loudly rather than truncating. Large UI does NOT use this path — panels go
  through `ui_render_cbor` (`Ui.Render`), which the host bounds at 2 MiB with
  its own retry; push bulk data through UI state / `SharedState` instead of a
  giant tool response.
- `Llm.GenerateStream` pulls batches over the CBOR `LlmStreamNextInput` →
  `LlmStreamNextOutput` ABI (`LlmStream.NextBatch(timeoutMs)` returns the
  deltas queued so far; `Finished` marks the end, `Cancel()` frees the host
  slot early). At most 4 concurrent streams per addon, reaped after 60 s idle.
- `Stt.Transcribe(audio, mime, options)` transcribes inline audio (≤ 25 MiB)
  through Core's STT path; requires the `stt` permission. `Documents.Get(docRef)`
  reassembles a file from the per-instance document store (e.g. an AudioCapture
  upload) and requires `document.read`.
- `PanelShell.InitialCommands` carries raw CBOR `Value`s (spec `Command`
  union §6.5) rather than typed command classes.
- The wasi-sdk pin is 25.0; ILCompiler rc.1 expects 29.0 and prints a
  version warning — linking and runtime are verified working with 25.0.
