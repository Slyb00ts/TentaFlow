// ===== File: Ui/UiClient.cs — ui_render_cbor / ui_notify wrappers =====
// Sends canonical-CBOR UI payloads (PanelShell, SlotContent, StatePatch, ...)
// to the host, which validates and forwards them to connected sessions.

#nullable enable

using TentaFlow.Sdk.Components;

namespace TentaFlow.Sdk;

public static class Ui
{
    /// <summary>Sends one UI payload; throws on host rejection.</summary>
    public static unsafe void Render(IUiPayload payload)
    {
        var bytes = UiPayloadEncoder.Encode(payload);
        int rc;
        fixed (byte* p = bytes)
        {
            rc = HostImports.UiRenderCbor((int)p, bytes.Length);
        }
        if (rc != 0)
        {
            throw new HostCallException("ui_render_cbor", rc);
        }
    }

    /// <summary>Shows a toast notification (level: info, warn, error, success).</summary>
    public static unsafe void Notify(string title, string body, string level = "info")
    {
        var t = HostCalls.Utf8(title);
        var b = HostCalls.Utf8(body);
        var l = HostCalls.Utf8(level);
        int rc;
        fixed (byte* tp = t)
        fixed (byte* bp = b)
        fixed (byte* lp = l)
        {
            rc = HostImports.UiNotify(
                (int)tp, t.Length, (int)bp, b.Length, (int)lp, l.Length);
        }
        if (rc != 0)
        {
            throw new HostCallException("ui_notify", rc);
        }
    }
}
