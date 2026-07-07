// ===== File: AddonRuntime.cs — lifecycle dispatch + wasm exports =====
// The host (tentaflow-core DotnetAdapter) calls `_initialize` first (runs all
// module initializers, including the addon's registration), then the
// `tentaflow_*` exports below. Buffer ownership follows the Rust addon ABI:
// the host allocates guest memory through the exported `alloc`/`dealloc`.

#nullable enable

using System;
using System.Runtime.InteropServices;
using System.Text;

namespace TentaFlow.Sdk;

/// <summary>Addon lifecycle contract. Override only what the addon needs.</summary>
public abstract class AddonBase
{
    /// <summary>Called once after instance start. Throw to fail startup.</summary>
    public virtual void OnStart()
    {
    }

    public virtual void OnStop()
    {
    }

    /// <summary>
    /// Tool / flow-block dispatch. `requestJson` is
    /// {"tool": name, "params": {...}, ...}; flow blocks arrive with
    /// tool = "block.&lt;type&gt;". Returns the response JSON.
    /// </summary>
    public virtual string OnRequest(string requestJson) =>
        "{\"ok\":false,\"error\":\"addon does not handle requests\"}";

    /// <summary>Event bus delivery for subscribed event types (JSON payload).</summary>
    public virtual void OnEvent(string eventJson)
    {
    }

    /// <summary>Service-mode tick with the host timestamp (ms since epoch).</summary>
    public virtual void OnTick(long timestampMs)
    {
    }

    /// <summary>
    /// Panel open with the host-assigned epoch. Render the PanelShell here —
    /// never in OnStart — so the shell carries the epoch the host expects.
    /// </summary>
    public virtual void OnPanelOpen(string panelId, ulong epoch)
    {
    }
}

/// <summary>Registration point wiring a user addon into the wasm exports.</summary>
public static class AddonRuntime
{
    private static AddonBase? _addon;

    /// <summary>
    /// Registers the addon instance. Call from a [ModuleInitializer] in the
    /// addon assembly; NativeAOT runs module initializers during `_initialize`,
    /// before any lifecycle export fires.
    /// </summary>
    public static void Register(AddonBase addon)
    {
        _addon = addon;
    }

    internal static AddonBase Current =>
        _addon ?? throw new InvalidOperationException(
            "no addon registered — call AddonRuntime.Register from a module initializer");

    internal static bool HasAddon => _addon != null;
}

internal static unsafe class Exports
{
    private static string ReadUtf8(int ptr, int len)
    {
        if (ptr == 0 || len <= 0)
        {
            return "";
        }
        return Encoding.UTF8.GetString((byte*)ptr, len);
    }

    /// <summary>
    /// Writes `payload` into the host-provided output buffer and its length
    /// into out_len_ptr (i32 LE). Returns 0, or a nonzero code when the
    /// response does not fit.
    ///
    /// The host calls `tentaflow_on_request` with a FIXED 64 KiB output buffer
    /// and bails on any nonzero return — there is no "buffer too small → retry
    /// with required size" protocol for this guest export (verified against
    /// tentaflow-core/src/addon/mod.rs call_tool). So a tool / flow-block
    /// response is hard-capped at the host's 64 KiB, exactly like Rust addons.
    /// Rather than silently truncating, we log and fail loudly (matching the
    /// Rust addon write_response). Large UI never travels this path — panels go
    /// through `ui_render_cbor` (Ui.Render), which the host bounds at 2 MiB with
    /// its own buffer-too-small retry.
    /// </summary>
    private static int WriteResponse(string payload, int outPtr, int outCap, int outLenPtr)
    {
        var bytes = Encoding.UTF8.GetBytes(payload);
        if (bytes.Length > outCap)
        {
            SafeLogError(
                $"on_request response is {bytes.Length} bytes but the host output " +
                $"buffer is only {outCap} bytes — response dropped. Split large " +
                "payloads or push data through ui_render_cbor / state instead.");
            // Nonzero → the host bails loudly; the request fails visibly rather
            // than the caller receiving a truncated (invalid) JSON body.
            return LegacyAbi.ErrBufferTooSmall;
        }
        new ReadOnlySpan<byte>(bytes).CopyTo(new Span<byte>((void*)outPtr, outCap));
        *(int*)outLenPtr = bytes.Length;
        return 0;
    }

    [UnmanagedCallersOnly(EntryPoint = "alloc")]
    internal static int Alloc(int size)
    {
        if (size < 0)
        {
            return -1;
        }
        return (int)NativeMemory.Alloc((nuint)Math.Max(size, 1));
    }

    [UnmanagedCallersOnly(EntryPoint = "dealloc")]
    internal static void Dealloc(int ptr, int size)
    {
        _ = size;
        if (ptr != 0)
        {
            NativeMemory.Free((void*)ptr);
        }
    }

    [UnmanagedCallersOnly(EntryPoint = "tentaflow_on_start")]
    internal static int OnStart()
    {
        try
        {
            if (!AddonRuntime.HasAddon)
            {
                return 1;
            }
            AddonRuntime.Current.OnStart();
            return 0;
        }
        catch (Exception e)
        {
            SafeLogError($"on_start failed: {e}");
            return 1;
        }
    }

    [UnmanagedCallersOnly(EntryPoint = "tentaflow_on_stop")]
    internal static int OnStop()
    {
        try
        {
            AddonRuntime.Current.OnStop();
            return 0;
        }
        catch (Exception e)
        {
            SafeLogError($"on_stop failed: {e}");
            return 1;
        }
    }

    [UnmanagedCallersOnly(EntryPoint = "tentaflow_on_request")]
    internal static int OnRequest(int inPtr, int inLen, int outPtr, int outCap, int outLenPtr)
    {
        try
        {
            string request = ReadUtf8(inPtr, inLen);
            string response = AddonRuntime.Current.OnRequest(request);
            return WriteResponse(response, outPtr, outCap, outLenPtr);
        }
        catch (Exception e)
        {
            SafeLogError($"on_request failed: {e}");
            // Best effort: surface the failure as a JSON error envelope.
            try
            {
                return WriteResponse(
                    "{\"ok\":false,\"error\":\"unhandled addon exception\"}",
                    outPtr, outCap, outLenPtr);
            }
            catch
            {
                return LegacyAbi.ErrOperation;
            }
        }
    }

    [UnmanagedCallersOnly(EntryPoint = "tentaflow_on_event")]
    internal static int OnEvent(int ptr, int len)
    {
        try
        {
            AddonRuntime.Current.OnEvent(ReadUtf8(ptr, len));
            return 0;
        }
        catch (Exception e)
        {
            SafeLogError($"on_event failed: {e}");
            return 1;
        }
    }

    [UnmanagedCallersOnly(EntryPoint = "tentaflow_on_tick")]
    internal static int OnTick(long timestampMs)
    {
        try
        {
            AddonRuntime.Current.OnTick(timestampMs);
            return 0;
        }
        catch (Exception e)
        {
            SafeLogError($"on_tick failed: {e}");
            return 1;
        }
    }

    [UnmanagedCallersOnly(EntryPoint = "tentaflow_on_panel_open")]
    internal static int OnPanelOpen(int panelIdPtr, int panelIdLen, long epoch)
    {
        try
        {
            AddonRuntime.Current.OnPanelOpen(ReadUtf8(panelIdPtr, panelIdLen), (ulong)epoch);
            return 0;
        }
        catch (Exception e)
        {
            SafeLogError($"on_panel_open failed: {e}");
            return 1;
        }
    }

    private static void SafeLogError(string message)
    {
        try
        {
            Log.Error(message);
        }
        catch
        {
            // Logging must never mask the original failure path.
        }
    }
}
