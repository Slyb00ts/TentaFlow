// ===== File: Host.cs — high-level wrappers over the "tentaflow" host functions =====
// Public addon-facing API: logging, storage, shared state, secrets, config,
// HTTP, LLM, events, users, SQL, flows, services and tool registration.
// JSON uses Utf8JsonWriter / JsonDocument only (no reflection — NativeAOT safe).

#nullable enable

using System;
using System.Collections.Generic;
using System.IO;
using System.Text.Json;
using TentaFlow.Sdk.Cbor;
using TentaFlow.Sdk.Components;

namespace TentaFlow.Sdk;

// -----------------------------------------------------------------------------
// Log
// -----------------------------------------------------------------------------

public static class Log
{
    public static void Info(string message) => Send(HostImports.LogInfo, message);

    public static void Warn(string message) => Send(HostImports.LogWarn, message);

    public static void Error(string message) => Send(HostImports.LogError, message);

    private static unsafe void Send(Func<int, int, int> fn, string message)
    {
        var bytes = HostCalls.Utf8(message);
        fixed (byte* p = bytes)
        {
            fn(bytes.Length == 0 ? 0 : (int)p, bytes.Length);
        }
    }
}

// -----------------------------------------------------------------------------
// Storage (sandboxed key/value)
// -----------------------------------------------------------------------------

public static class Storage
{
    /// <summary>Reads a value; returns null when the key does not exist.</summary>
    public static string? Get(string key)
    {
        var result = HostCalls.CallInOut(HostImports.StorageGet, HostCalls.Utf8(key));
        if (result.Code == LegacyAbi.ErrNotFound)
        {
            return null;
        }
        if (result.Code != 0)
        {
            throw new HostCallException("storage_get", result.Code);
        }
        return HostCalls.Utf8(result.Data);
    }

    public static unsafe void Set(string key, string value)
    {
        var k = HostCalls.Utf8(key);
        var v = HostCalls.Utf8(value);
        int rc;
        fixed (byte* kp = k)
        fixed (byte* vp = v)
        {
            rc = HostImports.StorageSet(
                (int)kp, k.Length, v.Length == 0 ? 0 : (int)vp, v.Length);
        }
        if (rc != 0)
        {
            throw new HostCallException("storage_set", rc);
        }
    }

    /// <summary>Deletes a key; returns false when it was absent.</summary>
    public static unsafe bool Delete(string key)
    {
        var k = HostCalls.Utf8(key);
        int rc;
        fixed (byte* kp = k)
        {
            rc = HostImports.StorageDelete((int)kp, k.Length);
        }
        if (rc == LegacyAbi.ErrNotFound)
        {
            return false;
        }
        if (rc != 0)
        {
            throw new HostCallException("storage_delete", rc);
        }
        return true;
    }

    /// <summary>Lists keys under a prefix (null = all keys).</summary>
    public static List<string> List(string? prefix = null)
    {
        var input = prefix == null ? System.Array.Empty<byte>() : HostCalls.Utf8(prefix);
        var result = HostCalls.CallInOut(HostImports.StorageList, input);
        if (result.Code != 0)
        {
            throw new HostCallException("storage_list", result.Code);
        }
        var keys = new List<string>();
        using var doc = JsonDocument.Parse(result.Data);
        foreach (var item in doc.RootElement.EnumerateArray())
        {
            keys.Add(item.GetString() ?? "");
        }
        return keys;
    }
}

// -----------------------------------------------------------------------------
// Shared state (host-side AddonStateStore, per-addon scope)
// -----------------------------------------------------------------------------

public enum StateTier : byte
{
    /// <summary>RAM-only, never persisted.</summary>
    Ephemeral = 0,

    /// <summary>RAM-served and flushed to the backing store.</summary>
    Durable = 1,
}

public sealed class StateEntryMeta
{
    public string Key { get; init; } = "";
    public ulong Size { get; init; }
    public StateTier Tier { get; init; }
}

public sealed class StateListResult
{
    public List<StateEntryMeta> Entries { get; init; } = new();

    /// <summary>Host clipped the result — narrow the prefix to page further.</summary>
    public bool Truncated { get; init; }
}

public static class SharedState
{
    /// <summary>Reads a value; returns null when the key is absent.</summary>
    public static byte[]? Get(string key)
    {
        var result = HostCalls.CallInOut(HostImports.StateGetV1, HostCalls.Utf8(key));
        if (result.Code == (int)AbiError.NotFound)
        {
            return null;
        }
        if (result.Code != 0)
        {
            throw new HostCallException("state_get_v1", result.Code);
        }
        return result.Data;
    }

    /// <summary>
    /// Wire: CBOR map {0: key tstr, 1: value array&lt;u8&gt;, 2: tier u8}
    /// (minicbor derive encodes Vec&lt;u8&gt; as an array of small ints).
    /// </summary>
    internal static byte[] EncodeSetInput(string key, byte[] value, StateTier tier)
    {
        var w = new CborWriter(value.Length + key.Length + 16);
        w.WriteMapHeader(3);
        w.WriteUInt(0);
        w.WriteText(key);
        w.WriteUInt(1);
        w.WriteArrayHeader(value.Length);
        foreach (byte b in value)
        {
            w.WriteUInt(b);
        }
        w.WriteUInt(2);
        w.WriteUInt((byte)tier);
        return w.ToArray();
    }

    public static unsafe void Set(string key, byte[] value, StateTier tier)
    {
        var payload = EncodeSetInput(key, value, tier);
        int rc;
        fixed (byte* p = payload)
        {
            rc = HostImports.StateSetV1((int)p, payload.Length);
        }
        if (rc != 0)
        {
            throw new HostCallException("state_set_v1", rc);
        }
    }

    /// <summary>Removes a key; returns true when it existed.</summary>
    public static unsafe bool Delete(string key)
    {
        var k = HostCalls.Utf8(key);
        int rc;
        fixed (byte* kp = k)
        {
            rc = HostImports.StateDeleteV1((int)kp, k.Length);
        }
        return rc switch
        {
            1 => true,
            0 => false,
            _ => throw new HostCallException("state_delete_v1", rc),
        };
    }

    public static StateListResult List(string? prefix = null)
    {
        var input = prefix == null ? System.Array.Empty<byte>() : HostCalls.Utf8(prefix);
        var result = HostCalls.CallInOut(HostImports.StateListV1, input);
        if (result.Code != 0)
        {
            throw new HostCallException("state_list_v1", result.Code);
        }
        // Wire: CBOR map {0: entries array<{0: key, 1: size, 2: tier}>, 1: truncated}
        var r = new CborReader(result.Data);
        var list = new StateListResult();
        int mapLen = r.ReadMapHeader();
        var entries = new List<StateEntryMeta>();
        bool truncated = false;
        for (int i = 0; i < mapLen; i++)
        {
            ulong k = r.ReadUInt();
            switch (k)
            {
                case 0:
                    {
                        int n = r.ReadArrayHeader();
                        for (int j = 0; j < n; j++)
                        {
                            int em = r.ReadMapHeader();
                            string key = "";
                            ulong size = 0;
                            byte tier = 0;
                            for (int f = 0; f < em; f++)
                            {
                                ulong fk = r.ReadUInt();
                                switch (fk)
                                {
                                    case 0:
                                        key = r.ReadText();
                                        break;
                                    case 1:
                                        size = r.ReadUInt();
                                        break;
                                    case 2:
                                        tier = (byte)r.ReadUInt();
                                        break;
                                    default:
                                        Value.Decode(r);
                                        break;
                                }
                            }
                            entries.Add(new StateEntryMeta
                            {
                                Key = key,
                                Size = size,
                                Tier = tier == 1 ? StateTier.Durable : StateTier.Ephemeral,
                            });
                        }
                        break;
                    }
                case 1:
                    truncated = r.ReadBool();
                    break;
                default:
                    Value.Decode(r);
                    break;
            }
        }
        return new StateListResult { Entries = entries, Truncated = truncated };
    }
}

// -----------------------------------------------------------------------------
// Secrets + Config
// -----------------------------------------------------------------------------

public static class Secrets
{
    public static string? Get(string key)
    {
        var result = HostCalls.CallInOut(HostImports.SecretGet, HostCalls.Utf8(key));
        if (result.Code == LegacyAbi.ErrNotFound)
        {
            return null;
        }
        if (result.Code != 0)
        {
            throw new HostCallException("secret_get", result.Code);
        }
        return result.Data.Length == 0 ? null : HostCalls.Utf8(result.Data);
    }

    public static unsafe void Set(string key, string value)
    {
        var k = HostCalls.Utf8(key);
        var v = HostCalls.Utf8(value);
        int rc;
        fixed (byte* kp = k)
        fixed (byte* vp = v)
        {
            rc = HostImports.SecretSet(
                (int)kp, k.Length, v.Length == 0 ? 0 : (int)vp, v.Length);
        }
        if (rc != 0)
        {
            throw new HostCallException("secret_set", rc);
        }
    }
}

public static class Config
{
    /// <summary>Reads an install-time connection parameter; null when absent.</summary>
    public static string? Get(string key)
    {
        var result = HostCalls.CallInOut(HostImports.ConfigGetV1, HostCalls.Utf8(key));
        if (result.Code == (int)AbiError.NotFound
            || result.Code == LegacyAbi.ErrNotFound)
        {
            return null;
        }
        if (result.Code != 0)
        {
            throw new HostCallException("config_get_v1", result.Code);
        }
        return HostCalls.Utf8(result.Data);
    }
}

// -----------------------------------------------------------------------------
// HTTP
// -----------------------------------------------------------------------------

public sealed class HttpRequestSpec
{
    public string Method { get; set; } = "GET";
    public string Url { get; set; } = "";
    public Dictionary<string, string> Headers { get; } = new();
    public string? Body { get; set; }
}

public sealed class HttpResponseSpec
{
    public int Status { get; init; }
    public Dictionary<string, string> Headers { get; init; } = new();
    public string Body { get; init; } = "";
}

public static class Http
{
    public static HttpResponseSpec Get(string url) =>
        Send(new HttpRequestSpec { Method = "GET", Url = url });

    public static HttpResponseSpec Post(string url, string body, string contentType)
    {
        var req = new HttpRequestSpec { Method = "POST", Url = url, Body = body };
        req.Headers["Content-Type"] = contentType;
        return Send(req);
    }

    public static HttpResponseSpec Send(HttpRequestSpec request)
    {
        using var stream = new MemoryStream();
        using (var jw = new Utf8JsonWriter(stream))
        {
            jw.WriteStartObject();
            jw.WriteString("method", request.Method);
            jw.WriteString("url", request.Url);
            jw.WriteStartObject("headers");
            foreach (var (k, v) in request.Headers)
            {
                jw.WriteString(k, v);
            }
            jw.WriteEndObject();
            if (request.Body != null)
            {
                jw.WriteString("body", request.Body);
            }
            else
            {
                jw.WriteNull("body");
            }
            jw.WriteEndObject();
        }
        var result = HostCalls.CallInOut(HostImports.HttpRequest, stream.ToArray());
        if (result.Code != 0)
        {
            throw new HostCallException("http_request", result.Code);
        }
        using var doc = JsonDocument.Parse(result.Data);
        var root = doc.RootElement;
        var headers = new Dictionary<string, string>();
        if (root.TryGetProperty("headers", out var hs) && hs.ValueKind == JsonValueKind.Object)
        {
            foreach (var p in hs.EnumerateObject())
            {
                headers[p.Name] = p.Value.GetString() ?? "";
            }
        }
        return new HttpResponseSpec
        {
            Status = root.TryGetProperty("status", out var st) ? st.GetInt32() : 0,
            Headers = headers,
            Body = root.TryGetProperty("body", out var b)
                ? b.GetString() ?? ""
                : "",
        };
    }
}

// -----------------------------------------------------------------------------
// LLM
// -----------------------------------------------------------------------------

public static class Llm
{
    /// <summary>
    /// Blocking generation. model/optionsJson are optional (host defaults). When
    /// `system` is set it is merged into the options as `"system"`, so the host
    /// builds a proper [system, user] message pair (`prompt` stays the pure user
    /// turn) instead of a single stuffed user message.
    /// </summary>
    public static unsafe string Generate(
        string prompt, string? model = null, string? optionsJson = null, string? system = null)
    {
        optionsJson = MergeSystem(optionsJson, system);
        var promptBytes = HostCalls.Utf8(prompt);
        var modelBytes = model == null ? System.Array.Empty<byte>() : HostCalls.Utf8(model);
        var optBytes = optionsJson == null
            ? System.Array.Empty<byte>()
            : HostCalls.Utf8(optionsJson);
        fixed (byte* mp = modelBytes)
        fixed (byte* op = optBytes)
        {
            int modelPtr = modelBytes.Length == 0 ? 0 : (int)mp;
            int optPtr = optBytes.Length == 0 ? 0 : (int)op;
            var result = HostCalls.CallInOut(
                (inPtr, inLen, outPtr, outCap, outLenPtr) => HostImports.LlmGenerate(
                    inPtr, inLen, modelPtr, modelBytes.Length, optPtr, optBytes.Length,
                    outPtr, outCap, outLenPtr),
                promptBytes);
            if (result.Code != 0)
            {
                throw new HostCallException("llm_generate", result.Code);
            }
            return HostCalls.Utf8(result.Data);
        }
    }

    /// <summary>
    /// Starts a streamed generation; pull batches with <see cref="LlmStream.NextBatch"/>.
    /// `optionsJson` carries generation options ({temperature, max_tokens, top_p, ...}).
    /// When `system` is set it is merged into the options as `"system"`, so the
    /// host builds a proper [system, user] message pair (`prompt` is the pure user
    /// turn). At most 4 concurrent streams per addon; an idle stream is reaped
    /// after 60 s.
    /// </summary>
    public static unsafe LlmStream GenerateStream(
        string prompt, string? model = null, string? optionsJson = null, string? system = null)
    {
        optionsJson = MergeSystem(optionsJson, system);
        var promptBytes = HostCalls.Utf8(prompt);
        var modelBytes = model == null ? System.Array.Empty<byte>() : HostCalls.Utf8(model);
        var optBytes = optionsJson == null
            ? System.Array.Empty<byte>()
            : HostCalls.Utf8(optionsJson);
        int rc;
        fixed (byte* pp = promptBytes)
        fixed (byte* mp = modelBytes)
        fixed (byte* op = optBytes)
        {
            rc = HostImports.LlmGenerateStreamStart(
                (int)pp, promptBytes.Length,
                modelBytes.Length == 0 ? 0 : (int)mp, modelBytes.Length,
                optBytes.Length == 0 ? 0 : (int)op, optBytes.Length);
        }
        // stream_start returns callback_id (> 0) or a NEGATED error code
        // (-1 permission, -4/-11 quota) — never a positive AbiError.
        if (rc <= 0)
        {
            throw new HostCallException("llm_generate_stream_start", rc);
        }
        return new LlmStream(rc);
    }

    // Injects `system` as the options `"system"` field, preserving any existing
    // options and overriding a prior `"system"`. Returns the unchanged options
    // when there is no system prompt to add.
    internal static string? MergeSystem(string? optionsJson, string? system)
    {
        if (system == null)
        {
            return optionsJson;
        }
        // Parse first, tolerantly: malformed options degrade to a clean
        // {"system": ...} (matching the host, which does serde_json(...).ok()).
        JsonDocument? doc = null;
        if (!string.IsNullOrEmpty(optionsJson))
        {
            try
            {
                doc = JsonDocument.Parse(optionsJson);
            }
            catch (JsonException)
            {
                doc = null;
            }
        }
        using (doc)
        {
            using var stream = new MemoryStream();
            using (var jw = new Utf8JsonWriter(stream))
            {
                jw.WriteStartObject();
                if (doc != null && doc.RootElement.ValueKind == JsonValueKind.Object)
                {
                    foreach (var p in doc.RootElement.EnumerateObject())
                    {
                        if (p.Name != "system")
                        {
                            p.WriteTo(jw);
                        }
                    }
                }
                jw.WriteString("system", system);
                jw.WriteEndObject();
            }
            return System.Text.Encoding.UTF8.GetString(stream.ToArray());
        }
    }
}

/// <summary>One batch of stream deltas returned by <see cref="LlmStream.NextBatch"/>.</summary>
public sealed class LlmStreamBatch
{
    /// <summary>Text deltas in generation order. Empty with Finished == false = timeout.</summary>
    public List<string> Chunks { get; init; } = new();

    /// <summary>Stream ended; the handle is invalid afterwards.</summary>
    public bool Finished { get; init; }

    /// <summary>Reason on the final batch: "stop", "length", "error", ...</summary>
    public string? FinishReason { get; init; }

    /// <summary>Generation error text (set when FinishReason == "error").</summary>
    public string? Error { get; init; }
}

/// <summary>
/// Pull-based LLM token stream over the CBOR batch ABI
/// (LlmStreamNextInput → LlmStreamNextOutput). Dropping the handle without
/// <see cref="Cancel"/> leaves the host to reap it after 60 s idle; Cancel frees
/// it immediately.
/// </summary>
public sealed class LlmStream
{
    private readonly int _callbackId;
    private bool _finished;

    internal LlmStream(int callbackId)
    {
        _callbackId = callbackId;
    }

    /// <summary>
    /// Pulls the next batch of deltas. `timeoutMs` bounds the wait for the FIRST
    /// delta of the batch (host-clamped to 30 s); everything already queued comes
    /// back in one batch. After Finished == true further calls throw StreamClosed.
    /// </summary>
    public LlmStreamBatch NextBatch(ulong timeoutMs = 5000)
    {
        if (_finished)
        {
            throw new HostCallException("llm_generate_stream_next", (int)AbiError.StreamClosed);
        }
        var w = new Cbor.CborWriter(32);
        w.WriteMapHeader(2);
        w.WriteUInt(0);
        w.WriteInt(_callbackId);
        w.WriteUInt(1);
        w.WriteUInt(timeoutMs);
        var input = w.ToArray();

        var result = HostCalls.CallInOut(HostImports.LlmGenerateStreamNext, input);
        if (result.Code != 0)
        {
            throw new HostCallException("llm_generate_stream_next", result.Code);
        }
        var batch = DecodeBatch(result.Data);
        if (batch.Finished)
        {
            _finished = true;
        }
        return batch;
    }

    /// <summary>Cancels the stream and frees host resources immediately.</summary>
    public void Cancel()
    {
        if (_finished)
        {
            return;
        }
        _finished = true;
        int rc = HostImports.LlmGenerateStreamCancel(_callbackId);
        // StreamNotFound just means the host already reaped it — not an error.
        if (rc != 0 && rc != (int)AbiError.StreamNotFound)
        {
            throw new HostCallException("llm_generate_stream_cancel", rc);
        }
    }

    // Wire: CBOR LlmStreamNextOutput {0: chunks, 1: finished, 2: finish_reason?,
    // 3: error?}. minicbor omits `None` Option fields, but a re-encoder may emit
    // an explicit CBOR null — both mean "absent", so guard the Option fields.
    internal static LlmStreamBatch DecodeBatch(byte[] data)
    {
        var r = new Cbor.CborReader(data);
        int n = r.ReadMapHeader();
        var chunks = new List<string>();
        bool finished = false;
        string? finishReason = null;
        string? error = null;
        for (int i = 0; i < n; i++)
        {
            ulong k = r.ReadUInt();
            switch (k)
            {
                case 0:
                    {
                        int m = r.ReadArrayHeader();
                        for (int j = 0; j < m; j++)
                        {
                            chunks.Add(r.ReadText());
                        }
                        break;
                    }
                case 1:
                    finished = r.ReadBool();
                    break;
                case 2:
                    finishReason = r.TryReadNull() ? null : r.ReadText();
                    break;
                case 3:
                    error = r.TryReadNull() ? null : r.ReadText();
                    break;
                default:
                    Value.Decode(r);
                    break;
            }
        }
        return new LlmStreamBatch
        {
            Chunks = chunks,
            Finished = finished,
            FinishReason = finishReason,
            Error = error,
        };
    }
}

// -----------------------------------------------------------------------------
// Bus (M3b, PLAN §6.4) — batch publish + handle-based consume over TentaBus.
//
// "nigdy per komunikat": `Bus.Publish` always takes a batch of records (up to
// 1000 / 8 MiB per call — the host's `PayloadKind::BusBatch`); consuming is a
// handle+batch pattern (open once via `Bus.ConsumeOpen`, drain repeated
// `NextBatch` calls, `Commit`, `Close`), mirroring `LlmStream`'s shape.
//
// Requires the addon manifest to declare "bus.publish" (for `Bus.Publish`)
// and/or "bus.subscribe" (for the consume quartet) — both fail-closed,
// per-topic checks the host re-verifies on every `NextBatch` call, not just
// at `ConsumeOpen`.
// -----------------------------------------------------------------------------

/// <summary>One record to publish via <see cref="Bus.Publish"/>. Header values
/// are raw bytes — a caller that wants a text header value encodes it itself.</summary>
public sealed class BusRecord
{
    public byte[]? Key { get; init; }
    public List<(string Name, byte[] Value)> Headers { get; init; } = new();
    public byte[] Payload { get; init; } = System.Array.Empty<byte>();
}

/// <summary>One record returned by <see cref="BusConsumer.NextBatch"/> — carries
/// the delivery metadata (`Topic`/`Partition`/`Offset`) needed to build the
/// offsets later passed to <see cref="BusConsumer.Commit"/>.</summary>
public sealed class BusConsumedRecord
{
    public string Topic { get; init; } = "";
    public uint Partition { get; init; }
    public ulong Offset { get; init; }
    public long TimestampMs { get; init; }
    public byte[]? Key { get; init; }
    public List<(string Name, byte[] Value)> Headers { get; init; } = new();
    public byte[] Payload { get; init; } = System.Array.Empty<byte>();
}

/// <summary>Batch returned by <see cref="BusConsumer.NextBatch"/>. An empty
/// `Records` list means the long-poll window elapsed with nothing new — a
/// normal, expected outcome, never an error; call `NextBatch` again.</summary>
public sealed class BusConsumeBatch
{
    public List<BusConsumedRecord> Records { get; init; } = new();
}

/// <summary>Full outcome of <see cref="Bus.PublishEx"/> — the fields of the
/// wire `BusPublishOutput` (SUM/tentabus/PLAN-F3.md §4.5), as a struct
/// instead of the bare <c>uint</c> <see cref="Bus.Publish"/> returns, now
/// that a batch can partially divert to `__dlq.&lt;topic&gt;` rather than
/// simply accepting or failing whole.</summary>
public sealed class BusPublishResult
{
    /// <summary>Records actually appended (summed across every partition touched).</summary>
    public uint Published { get; init; }

    /// <summary>Records diverted to `__dlq.&lt;topic&gt;` for failing schema
    /// validation under `validation = dlq` — `0` for a topic with no bound
    /// schema, or `validation` other than `dlq`. `0` by default so an OLDER
    /// host that never sends key 1 decodes as "no schema enforcement ran".</summary>
    public uint SchemaRejected { get; init; }
}

public static class Bus
{
    /// <summary>
    /// Publishes a batch of records to <paramref name="topic"/> in one call
    /// (up to 1000 records / 8 MiB total, enforced by the host).
    /// <paramref name="createIfMissing"/> mirrors the `bus_publish` flow
    /// node's own config field. Returns the number of records actually
    /// appended.
    ///
    /// Does not surface <see cref="BusPublishResult.SchemaRejected"/>
    /// (PLAN-F3 §4.5) — kept as a stable, narrow <c>uint</c> return for
    /// existing callers; use <see cref="PublishEx"/> for the full outcome
    /// including how many records a `dlq`-mode schema violation diverted.
    /// </summary>
    public static uint Publish(string topic, IReadOnlyList<BusRecord> records, bool createIfMissing = false)
    {
        return PublishEx(topic, records, createIfMissing).Published;
    }

    /// <summary>Same as <see cref="Publish"/>, but returns the full
    /// <see cref="BusPublishResult"/> (published count AND schema-rejected
    /// count) instead of just the published count.</summary>
    public static BusPublishResult PublishEx(string topic, IReadOnlyList<BusRecord> records, bool createIfMissing = false)
    {
        var w = new CborWriter(256 + records.Count * 64);
        w.WriteMapHeader(createIfMissing ? 3 : 2);
        w.WriteUInt(0);
        w.WriteText(topic);
        w.WriteUInt(1);
        w.WriteArrayHeader(records.Count);
        foreach (var r in records)
        {
            int fieldCount = 2 + (r.Key != null ? 1 : 0);
            w.WriteMapHeader(fieldCount);
            if (r.Key != null)
            {
                w.WriteUInt(0);
                w.WriteBytes(r.Key);
            }
            w.WriteUInt(1);
            w.WriteArrayHeader(r.Headers.Count);
            foreach (var (name, value) in r.Headers)
            {
                w.WriteMapHeader(2);
                w.WriteUInt(0);
                w.WriteText(name);
                w.WriteUInt(1);
                w.WriteBytes(value);
            }
            w.WriteUInt(2);
            w.WriteBytes(r.Payload);
        }
        if (createIfMissing)
        {
            w.WriteUInt(2);
            w.WriteBool(true);
        }

        var result = HostCalls.CallInOut(HostImports.BusPublishV1, w.WrittenSpan);
        if (result.Code != 0)
        {
            throw new HostCallException("bus_publish_v1", result.Code);
        }
        return DecodePublishOutput(result.Data);
    }

    // Wire: CBOR BusPublishOutput {0: published, 1: schema_rejected}.
    // `schema_rejected` (key 1) is absent from a pre-F3 host's payload —
    // defaults to 0 below, same as `#[cbor(default)]` on the Rust side.
    // An unrecognized key (future field) is skipped via `Value.Decode`,
    // same forward-compatible discipline as every other key already here.
    internal static BusPublishResult DecodePublishOutput(byte[] data)
    {
        var r = new CborReader(data);
        int n = r.ReadMapHeader();
        uint published = 0;
        uint schemaRejected = 0;
        for (int i = 0; i < n; i++)
        {
            ulong k = r.ReadUInt();
            if (k == 0)
            {
                published = (uint)r.ReadUInt();
            }
            else if (k == 1)
            {
                schemaRejected = (uint)r.ReadUInt();
            }
            else
            {
                Value.Decode(r);
            }
        }
        return new BusPublishResult { Published = published, SchemaRejected = schemaRejected };
    }

    /// <summary>
    /// Opens a consume handle for <paramref name="group"/> across
    /// <paramref name="topics"/>. <paramref name="commitMode"/>:
    /// `"auto_after_success"` (the default when null) | `"explicit"` |
    /// `"at_most_once"` — same values the `bus_consume` flow node accepts.
    /// </summary>
    public static BusConsumer ConsumeOpen(IReadOnlyList<string> topics, string group, string? commitMode = null)
    {
        var w = new CborWriter(128);
        w.WriteMapHeader(commitMode != null ? 3 : 2);
        w.WriteUInt(0);
        w.WriteArrayHeader(topics.Count);
        foreach (var t in topics)
        {
            w.WriteText(t);
        }
        w.WriteUInt(1);
        w.WriteText(group);
        if (commitMode != null)
        {
            w.WriteUInt(2);
            w.WriteText(commitMode);
        }

        var result = HostCalls.CallInOut(HostImports.BusConsumeOpenV1, w.WrittenSpan);
        if (result.Code != 0)
        {
            throw new HostCallException("bus_consume_open_v1", result.Code);
        }
        return new BusConsumer(DecodeConsumeOpenOutput(result.Data));
    }

    // Wire: CBOR BusConsumeOpenOutput {0: consumer_id}.
    internal static string DecodeConsumeOpenOutput(byte[] data)
    {
        var r = new CborReader(data);
        int n = r.ReadMapHeader();
        string consumerId = "";
        for (int i = 0; i < n; i++)
        {
            ulong k = r.ReadUInt();
            if (k == 0)
            {
                consumerId = r.ReadText();
            }
            else
            {
                Value.Decode(r);
            }
        }
        return consumerId;
    }
}

/// <summary>
/// Handle-based bus consumer over the CBOR batch ABI
/// (`bus_consume_next/commit/close_v1`). The host force-closes an idle
/// handle after 300 s of inactivity — call <see cref="Close"/> explicitly
/// when done rather than relying on the reaper, which exists only to catch
/// a crashed addon.
/// </summary>
public sealed class BusConsumer
{
    private readonly string _consumerId;
    private bool _closed;

    internal BusConsumer(string consumerId)
    {
        _consumerId = consumerId;
    }

    /// <summary>
    /// Bounded-await poll for the next batch. `maxRecords` and `timeoutMs`
    /// are clamped by the host (1000 records, 5000 ms). The underlying
    /// `fetch` is byte-bounded, not record-bounded — the returned count can
    /// run over or under `maxRecords`; never assume an exact match.
    /// </summary>
    public BusConsumeBatch NextBatch(uint maxRecords = 1000, uint timeoutMs = 1000)
    {
        var w = new CborWriter(64);
        w.WriteMapHeader(3);
        w.WriteUInt(0);
        w.WriteText(_consumerId);
        w.WriteUInt(1);
        w.WriteUInt(maxRecords);
        w.WriteUInt(2);
        w.WriteUInt(timeoutMs);

        var result = HostCalls.CallInOut(HostImports.BusConsumeNextV1, w.WrittenSpan);
        if (result.Code != 0)
        {
            throw new HostCallException("bus_consume_next_v1", result.Code);
        }
        return DecodeNextBatch(result.Data);
    }

    // Wire: CBOR BusConsumeNextOutput {0: kind, 1: records}.
    internal static BusConsumeBatch DecodeNextBatch(byte[] data)
    {
        var r = new CborReader(data);
        int n = r.ReadMapHeader();
        var records = new List<BusConsumedRecord>();
        for (int i = 0; i < n; i++)
        {
            ulong k = r.ReadUInt();
            switch (k)
            {
                case 0:
                    r.ReadText(); // "batch" | "empty" — records.Count already distinguishes them
                    break;
                case 1:
                    {
                        int m = r.ReadArrayHeader();
                        for (int j = 0; j < m; j++)
                        {
                            records.Add(DecodeRecord(r));
                        }
                        break;
                    }
                default:
                    Value.Decode(r);
                    break;
            }
        }
        return new BusConsumeBatch { Records = records };
    }

    internal static BusConsumedRecord DecodeRecord(CborReader r)
    {
        int n = r.ReadMapHeader();
        string topic = "";
        uint partition = 0;
        ulong offset = 0;
        long timestampMs = 0;
        byte[]? key = null;
        var headers = new List<(string, byte[])>();
        byte[] payload = System.Array.Empty<byte>();
        for (int i = 0; i < n; i++)
        {
            ulong k = r.ReadUInt();
            switch (k)
            {
                case 0: topic = r.ReadText(); break;
                case 1: partition = (uint)r.ReadUInt(); break;
                case 2: offset = r.ReadUInt(); break;
                case 3: timestampMs = r.ReadInt(); break;
                case 4: key = r.TryReadNull() ? null : r.ReadBytes(); break;
                case 5:
                    {
                        int hm = r.ReadArrayHeader();
                        for (int j = 0; j < hm; j++)
                        {
                            int fn = r.ReadMapHeader();
                            string name = "";
                            byte[] value = System.Array.Empty<byte>();
                            for (int f = 0; f < fn; f++)
                            {
                                ulong fk = r.ReadUInt();
                                if (fk == 0) name = r.ReadText();
                                else if (fk == 1) value = r.ReadBytes();
                                else Value.Decode(r);
                            }
                            headers.Add((name, value));
                        }
                        break;
                    }
                case 6: payload = r.ReadBytes(); break;
                default: Value.Decode(r); break;
            }
        }
        return new BusConsumedRecord
        {
            Topic = topic,
            Partition = partition,
            Offset = offset,
            TimestampMs = timestampMs,
            Key = key,
            Headers = headers,
            Payload = payload,
        };
    }

    /// <summary>
    /// Durably advances the committed offset for each `(topic, partition,
    /// offset)` triple. Required before <see cref="Close"/> under
    /// `commit_mode = "explicit"` — an unfetched-past offset is silently
    /// ignored, never rewound.
    /// </summary>
    public void Commit(IReadOnlyList<(string Topic, uint Partition, ulong Offset)> offsets)
    {
        var w = new CborWriter(64 + offsets.Count * 32);
        w.WriteMapHeader(2);
        w.WriteUInt(0);
        w.WriteText(_consumerId);
        w.WriteUInt(1);
        w.WriteArrayHeader(offsets.Count);
        foreach (var (topic, partition, offset) in offsets)
        {
            w.WriteMapHeader(3);
            w.WriteUInt(0);
            w.WriteText(topic);
            w.WriteUInt(1);
            w.WriteUInt(partition);
            w.WriteUInt(2);
            w.WriteUInt(offset);
        }

        var result = HostCalls.CallInOut(HostImports.BusConsumeCommitV1, w.WrittenSpan);
        if (result.Code != 0)
        {
            throw new HostCallException("bus_consume_commit_v1", result.Code);
        }
    }

    /// <summary>Drops the consumer handle. Subsequent `NextBatch`/`Commit`
    /// calls throw `NotFound`. Idempotent — a second `Close` is a no-op.</summary>
    public void Close()
    {
        if (_closed)
        {
            return;
        }
        _closed = true;
        var w = new CborWriter(48);
        w.WriteMapHeader(1);
        w.WriteUInt(0);
        w.WriteText(_consumerId);

        var result = HostCalls.CallInOut(HostImports.BusConsumeCloseV1, w.WrittenSpan);
        if (result.Code != 0 && result.Code != (int)AbiError.NotFound)
        {
            throw new HostCallException("bus_consume_close_v1", result.Code);
        }
    }
}

// -----------------------------------------------------------------------------
// STT (speech-to-text)
// -----------------------------------------------------------------------------

/// <summary>Optional transcription parameters for <see cref="Stt.Transcribe"/>.</summary>
public sealed class SttOptions
{
    /// <summary>Sample rate hint (informational, e.g. 16000).</summary>
    public uint? SampleRate { get; set; }

    /// <summary>STT model name; null = default local engine (whisper).</summary>
    public string? Model { get; set; }

    /// <summary>ISO-639-1 language code (e.g. "pl") to skip auto-detection.</summary>
    public string? Language { get; set; }

    /// <summary>Context prompt for the model.</summary>
    public string? Prompt { get; set; }
}

/// <summary>Result of <see cref="Stt.Transcribe"/>.</summary>
public sealed class SttResult
{
    public string Text { get; init; } = "";
    public string? DetectedLanguage { get; init; }
    public ulong? DurationMs { get; init; }
}

public static class Stt
{
    /// <summary>25 MiB — mirrors the host PayloadKind::AudioInline ceiling.</summary>
    private const int MaxAudioBytes = 25 * 1024 * 1024;

    /// <summary>
    /// Transcribes encoded audio (WAV/Opus/MP3, ≤ 25 MiB) through Core's STT path
    /// (same route as the flow-engine stt node). Requires the "stt" permission.
    /// Wire: CBOR SttTranscribeInput → SttTranscribeOutput.
    /// </summary>
    public static SttResult Transcribe(byte[] audio, string mime, SttOptions? options = null)
    {
        if (audio.Length == 0)
        {
            throw new HostCallException("stt_transcribe_v1", (int)AbiError.Operation);
        }
        if (audio.Length > MaxAudioBytes)
        {
            throw new HostCallException("stt_transcribe_v1", (int)AbiError.PayloadTooLarge);
        }
        options ??= new SttOptions();

        int fieldCount = 2
            + (options.SampleRate != null ? 1 : 0)
            + (options.Model != null ? 1 : 0)
            + (options.Language != null ? 1 : 0)
            + (options.Prompt != null ? 1 : 0);
        var w = new Cbor.CborWriter(audio.Length + 64);
        w.WriteMapHeader(fieldCount);
        w.WriteUInt(0);
        w.WriteBytes(audio);
        w.WriteUInt(1);
        w.WriteText(mime);
        if (options.SampleRate != null)
        {
            w.WriteUInt(2);
            w.WriteUInt(options.SampleRate.Value);
        }
        if (options.Model != null)
        {
            w.WriteUInt(3);
            w.WriteText(options.Model);
        }
        if (options.Language != null)
        {
            w.WriteUInt(4);
            w.WriteText(options.Language);
        }
        if (options.Prompt != null)
        {
            w.WriteUInt(5);
            w.WriteText(options.Prompt);
        }

        var result = HostCalls.CallInOut(HostImports.SttTranscribeV1, w.ToArray());
        if (result.Code != 0)
        {
            throw new HostCallException("stt_transcribe_v1", result.Code);
        }
        return DecodeOutput(result.Data);
    }

    // Wire: CBOR SttTranscribeOutput {0: text, 1: detected_language?,
    // 2: duration_ms?}. The two Option fields may be absent or an explicit null.
    internal static SttResult DecodeOutput(byte[] data)
    {
        var r = new Cbor.CborReader(data);
        int n = r.ReadMapHeader();
        string text = "";
        string? detected = null;
        ulong? durationMs = null;
        for (int i = 0; i < n; i++)
        {
            ulong k = r.ReadUInt();
            switch (k)
            {
                case 0: text = r.ReadText(); break;
                case 1: detected = r.TryReadNull() ? null : r.ReadText(); break;
                case 2: durationMs = r.TryReadNull() ? null : r.ReadUInt(); break;
                default: Value.Decode(r); break;
            }
        }
        return new SttResult
        {
            Text = text,
            DetectedLanguage = detected,
            DurationMs = durationMs,
        };
    }
}

// -----------------------------------------------------------------------------
// Document / blob store (per-instance file store)
// -----------------------------------------------------------------------------

public static class Documents
{
    /// <summary>256 KiB — matches the host document-store chunk size.</summary>
    private const int ChunkBytes = 256 * 1024;

    /// <summary>
    /// Reads a complete file from the per-instance document store by its
    /// `docRef` (as produced by an upload / AudioCapture emission), reassembling
    /// its chunks. Returns (bytes, mime). Requires the "document.read" permission.
    /// An absent doc surfaces as NotFound.
    /// </summary>
    public static unsafe (byte[] Bytes, string Mime) Get(string docId)
    {
        using var assembled = new MemoryStream();
        string mime = "";
        uint chunkIndex = 0;
        uint totalChunks = 1;
        int blobCap = ChunkBytes;
        do
        {
            var w = new Cbor.CborWriter(docId.Length + 16);
            w.WriteMapHeader(2);
            w.WriteUInt(0);
            w.WriteText(docId);
            w.WriteUInt(1);
            w.WriteUInt(chunkIndex);
            var input = w.ToArray();

            byte[] blobBuf = new byte[blobCap];
            byte[] metaBuf = new byte[HostCalls.DefaultOutCap];
            int metaLen = 0;
            int rc;
            fixed (byte* inP = input)
            fixed (byte* blobP = blobBuf)
            fixed (byte* metaP = metaBuf)
            {
                rc = HostImports.DocumentGetV1(
                    (int)inP, input.Length,
                    (int)blobP, blobBuf.Length,
                    (int)metaP, metaBuf.Length, (int)(&metaLen));
            }
            // Blob buffer too small: the host writes the required size to metaLen
            // and skips the copy; grow and retry the same chunk.
            if (rc == (int)AbiError.OutputBufferTooSmall && metaLen > blobCap)
            {
                blobCap = metaLen;
                continue;
            }
            if (rc != 0)
            {
                throw new HostCallException("document_get_v1", rc);
            }

            var meta = new Cbor.CborReader(new ReadOnlySpan<byte>(metaBuf, 0, metaLen).ToArray());
            int mn = meta.ReadMapHeader();
            uint chunkLen = 0;
            for (int i = 0; i < mn; i++)
            {
                ulong key = meta.ReadUInt();
                switch (key)
                {
                    case 0: totalChunks = (uint)meta.ReadUInt(); break;
                    case 1: chunkLen = (uint)meta.ReadUInt(); break;
                    case 2: mime = meta.ReadText(); break;
                    case 3: meta.ReadUInt(); break;
                    default: Value.Decode(meta); break;
                }
            }
            assembled.Write(blobBuf, 0, (int)chunkLen);
            chunkIndex++;
        }
        while (chunkIndex < totalChunks);

        return (assembled.ToArray(), mime);
    }
}

// -----------------------------------------------------------------------------
// Events
// -----------------------------------------------------------------------------

public static class Events
{
    public static unsafe void Publish(string eventType, string payloadJson)
    {
        var et = HostCalls.Utf8(eventType);
        var pl = HostCalls.Utf8(payloadJson);
        int rc;
        fixed (byte* ep = et)
        fixed (byte* pp = pl)
        {
            rc = HostImports.EventPublish(
                (int)ep, et.Length, pl.Length == 0 ? 0 : (int)pp, pl.Length);
        }
        if (rc < 0)
        {
            throw new HostCallException("event_publish", rc);
        }
    }

    /// <summary>
    /// Subscribes to an event type; the host later calls the addon's
    /// tentaflow_on_event export on delivery. Returns the subscription id.
    /// </summary>
    public static unsafe long Subscribe(string eventType, string? filterJson = null)
    {
        var et = HostCalls.Utf8(eventType);
        var fl = filterJson == null ? System.Array.Empty<byte>() : HostCalls.Utf8(filterJson);
        int rc;
        fixed (byte* ep = et)
        fixed (byte* fp = fl)
        {
            rc = HostImports.EventSubscribe(
                (int)ep, et.Length, fl.Length == 0 ? 0 : (int)fp, fl.Length);
        }
        if (rc < 0)
        {
            throw new HostCallException("event_subscribe", rc);
        }
        return rc;
    }
}

// -----------------------------------------------------------------------------
// Users
// -----------------------------------------------------------------------------

public static class Users
{
    /// <summary>Returns the current user's JSON document (host-defined shape).</summary>
    public static string GetCurrent()
    {
        var result = HostCalls.CallInOut(
            (inPtr, inLen, outPtr, outCap, outLenPtr) =>
                HostImports.UserGetCurrent(outPtr, outCap, outLenPtr),
            ReadOnlySpan<byte>.Empty);
        if (result.Code != 0)
        {
            throw new HostCallException("user_get_current", result.Code);
        }
        return HostCalls.Utf8(result.Data);
    }

    public static unsafe bool CheckPermission(
        string permissionType, string? resource = null, string? accessLevel = null)
    {
        var pt = HostCalls.Utf8(permissionType);
        var rs = resource == null ? System.Array.Empty<byte>() : HostCalls.Utf8(resource);
        var al = accessLevel == null ? System.Array.Empty<byte>() : HostCalls.Utf8(accessLevel);
        int rc;
        fixed (byte* pp = pt)
        fixed (byte* rp = rs)
        fixed (byte* ap = al)
        {
            rc = HostImports.UserCheckPermission(
                (int)pp, pt.Length,
                rs.Length == 0 ? 0 : (int)rp, rs.Length,
                al.Length == 0 ? 0 : (int)ap, al.Length);
        }
        return rc == 1;
    }
}

// -----------------------------------------------------------------------------
// Directory (org users / groups / roles)
// -----------------------------------------------------------------------------

/// <summary>One active user of the caller's organization (directory_users_v1).</summary>
public sealed class DirectoryUser
{
    public string Id { get; init; } = "";
    public string Username { get; init; } = "";
    public string DisplayName { get; init; } = "";
    public string? Email { get; init; }
    /// <summary>Group IDs (user_groups.id) the user belongs to.</summary>
    public List<string> Groups { get; init; } = new();
    public bool IsActive { get; init; }
    /// <summary>Organization RBAC role (user | power_user | admin).</summary>
    public string Role { get; init; } = "";
}

/// <summary>One user group; MemberCount counts only active users of the caller's org.</summary>
public sealed class DirectoryGroup
{
    public string Id { get; init; } = "";
    public string Name { get; init; } = "";
    public string Description { get; init; } = "";
    public ulong MemberCount { get; init; }
}

/// <summary>One RBAC role (directory_roles_v1). Permission lists stay host-side.</summary>
public sealed class DirectoryRole
{
    public string RoleId { get; init; } = "";
    public string Name { get; init; } = "";
}

/// <summary>The caller's organization (directory_org_v1).</summary>
public sealed class DirectoryOrg
{
    public string OrgId { get; init; } = "";
    public string Name { get; init; } = "";
    public string Slug { get; init; } = "";
}

/// <summary>
/// Read-only directory of the caller org's users, groups, RBAC roles and the
/// org itself. Backs sharing UIs (pick a person / group / org). All calls
/// require the "directory.read" permission.
/// Wire: CBOR Directory*Output shapes from tentaflow-sdk-spec (output-only ABI).
/// </summary>
public static class Directory
{
    /// <summary>Active users of the caller's organization.</summary>
    public static List<DirectoryUser> Users()
    {
        var data = CallOutOnly(HostImports.DirectoryUsersV1, "directory_users_v1");
        return DecodeUsers(data);
    }

    /// <summary>User groups with member counts scoped to the caller's org.</summary>
    public static List<DirectoryGroup> Groups()
    {
        var data = CallOutOnly(HostImports.DirectoryGroupsV1, "directory_groups_v1");
        return DecodeGroups(data);
    }

    /// <summary>RBAC roles (role_id + name).</summary>
    public static List<DirectoryRole> Roles()
    {
        var data = CallOutOnly(HostImports.DirectoryRolesV1, "directory_roles_v1");
        return DecodeRoles(data);
    }

    /// <summary>The caller's organization (org_id, name, slug).</summary>
    public static DirectoryOrg Org()
    {
        var data = CallOutOnly(HostImports.DirectoryOrgV1, "directory_org_v1");
        return DecodeOrg(data);
    }

    private delegate int OutOnlyFn(int outPtr, int outCap, int outLenPtr);

    private static byte[] CallOutOnly(OutOnlyFn fn, string name)
    {
        var result = HostCalls.CallInOut(
            (inPtr, inLen, outPtr, outCap, outLenPtr) => fn(outPtr, outCap, outLenPtr),
            ReadOnlySpan<byte>.Empty);
        if (result.Code != 0)
        {
            throw new HostCallException(name, result.Code);
        }
        return result.Data;
    }

    // Wire: DirectoryUsersOutput {0: [DirectoryUserOut {0: id, 1: username,
    // 2: display_name, 3: email?, 4: [group_id], 5: is_active, 6: role}]}.
    internal static List<DirectoryUser> DecodeUsers(byte[] data)
    {
        var users = new List<DirectoryUser>();
        var r = new Cbor.CborReader(data);
        int outer = r.ReadMapHeader();
        for (int f = 0; f < outer; f++)
        {
            ulong key = r.ReadUInt();
            if (key != 0)
            {
                Value.Decode(r);
                continue;
            }
            int count = r.ReadArrayHeader();
            for (int i = 0; i < count; i++)
            {
                int n = r.ReadMapHeader();
                string id = "", username = "", displayName = "", role = "";
                string? email = null;
                var groups = new List<string>();
                bool isActive = false;
                for (int j = 0; j < n; j++)
                {
                    ulong k = r.ReadUInt();
                    switch (k)
                    {
                        case 0: id = r.ReadText(); break;
                        case 1: username = r.ReadText(); break;
                        case 2: displayName = r.ReadText(); break;
                        case 3: email = r.TryReadNull() ? null : r.ReadText(); break;
                        case 4:
                            int gc = r.ReadArrayHeader();
                            for (int g = 0; g < gc; g++)
                            {
                                groups.Add(r.ReadText());
                            }
                            break;
                        case 5: isActive = r.ReadBool(); break;
                        case 6: role = r.ReadText(); break;
                        default: Value.Decode(r); break;
                    }
                }
                users.Add(new DirectoryUser
                {
                    Id = id,
                    Username = username,
                    DisplayName = displayName,
                    Email = email,
                    Groups = groups,
                    IsActive = isActive,
                    Role = role,
                });
            }
        }
        return users;
    }

    // Wire: DirectoryGroupsOutput {0: [DirectoryGroupOut {0: id, 1: name,
    // 2: description, 3: member_count}]}.
    internal static List<DirectoryGroup> DecodeGroups(byte[] data)
    {
        var groups = new List<DirectoryGroup>();
        var r = new Cbor.CborReader(data);
        int outer = r.ReadMapHeader();
        for (int f = 0; f < outer; f++)
        {
            ulong key = r.ReadUInt();
            if (key != 0)
            {
                Value.Decode(r);
                continue;
            }
            int count = r.ReadArrayHeader();
            for (int i = 0; i < count; i++)
            {
                int n = r.ReadMapHeader();
                string id = "", name = "", description = "";
                ulong memberCount = 0;
                for (int j = 0; j < n; j++)
                {
                    ulong k = r.ReadUInt();
                    switch (k)
                    {
                        case 0: id = r.ReadText(); break;
                        case 1: name = r.ReadText(); break;
                        case 2: description = r.ReadText(); break;
                        case 3: memberCount = r.ReadUInt(); break;
                        default: Value.Decode(r); break;
                    }
                }
                groups.Add(new DirectoryGroup
                {
                    Id = id,
                    Name = name,
                    Description = description,
                    MemberCount = memberCount,
                });
            }
        }
        return groups;
    }

    // Wire: DirectoryRolesOutput {0: [DirectoryRoleOut {0: role_id, 1: name}]}.
    internal static List<DirectoryRole> DecodeRoles(byte[] data)
    {
        var roles = new List<DirectoryRole>();
        var r = new Cbor.CborReader(data);
        int outer = r.ReadMapHeader();
        for (int f = 0; f < outer; f++)
        {
            ulong key = r.ReadUInt();
            if (key != 0)
            {
                Value.Decode(r);
                continue;
            }
            int count = r.ReadArrayHeader();
            for (int i = 0; i < count; i++)
            {
                int n = r.ReadMapHeader();
                string roleId = "", name = "";
                for (int j = 0; j < n; j++)
                {
                    ulong k = r.ReadUInt();
                    switch (k)
                    {
                        case 0: roleId = r.ReadText(); break;
                        case 1: name = r.ReadText(); break;
                        default: Value.Decode(r); break;
                    }
                }
                roles.Add(new DirectoryRole { RoleId = roleId, Name = name });
            }
        }
        return roles;
    }

    // Wire: DirectoryOrgOutput {0: org_id, 1: name, 2: slug}.
    internal static DirectoryOrg DecodeOrg(byte[] data)
    {
        var r = new Cbor.CborReader(data);
        int n = r.ReadMapHeader();
        string orgId = "", name = "", slug = "";
        for (int i = 0; i < n; i++)
        {
            ulong k = r.ReadUInt();
            switch (k)
            {
                case 0: orgId = r.ReadText(); break;
                case 1: name = r.ReadText(); break;
                case 2: slug = r.ReadText(); break;
                default: Value.Decode(r); break;
            }
        }
        return new DirectoryOrg { OrgId = orgId, Name = name, Slug = slug };
    }
}

// -----------------------------------------------------------------------------
// Model aliases (readonly)
// -----------------------------------------------------------------------------

/// <summary>
/// One alias/model this addon MAY consume — the result of the access-grant
/// system (its <c>[[uses_alias]]</c> declarations joined with the concrete
/// target model, methods, strategy, visibility and grant status). Mirrors the
/// host <c>AvailableAlias</c>.
/// </summary>
public sealed class AvailableAlias
{
    /// <summary>Alias name the addon declared in <c>[[uses_alias]]</c>.</summary>
    public string AliasId { get; init; } = "";

    /// <summary>Concrete model the alias resolves to; null when it does not exist yet.</summary>
    public string? TargetModel { get; init; }

    /// <summary>Capabilities declared by the owner (chat/generate/transcribe/...).</summary>
    public List<string> Methods { get; init; } = new();

    /// <summary>Routing strategy of the alias (when it exists).</summary>
    public string? Strategy { get; init; }

    /// <summary>Grant status: granted / auto_granted / pending / denied.</summary>
    public string GrantStatus { get; init; } = "";

    /// <summary>Owner-set visibility: private / restricted / public.</summary>
    public string? Visibility { get; init; }

    /// <summary>Whether the resolved alias is active.</summary>
    public bool Active { get; init; }

    /// <summary>Whether the consumer declared the alias as required.</summary>
    public bool Required { get; init; }
}

public static class Aliases
{
    /// <summary>
    /// Lists the aliases/models this addon may consume (its <c>[[uses_alias]]</c>
    /// grants with their resolved target, methods and grant status). Includes all
    /// statuses so a UI can show an honest state. Requires the "alias.read"
    /// permission. Wire: JSON <c>{ "aliases": [AvailableAlias...] }</c>.
    /// </summary>
    public static List<AvailableAlias> ListAvailable()
    {
        var result = HostCalls.CallInOut(
            (inPtr, inLen, outPtr, outCap, outLenPtr) =>
                HostImports.AliasListAvailableV1(outPtr, outCap, outLenPtr),
            ReadOnlySpan<byte>.Empty);
        if (result.Code != 0)
        {
            throw new HostCallException("alias_list_available_v1", result.Code);
        }
        return ParseAvailable(result.Data);
    }

    internal static List<AvailableAlias> ParseAvailable(byte[] json)
    {
        var list = new List<AvailableAlias>();
        using var doc = JsonDocument.Parse(json);
        if (!doc.RootElement.TryGetProperty("aliases", out var arr)
            || arr.ValueKind != JsonValueKind.Array)
        {
            return list;
        }
        foreach (var e in arr.EnumerateArray())
        {
            var methods = new List<string>();
            if (e.TryGetProperty("methods", out var ms) && ms.ValueKind == JsonValueKind.Array)
            {
                foreach (var m in ms.EnumerateArray())
                {
                    methods.Add(m.GetString() ?? "");
                }
            }
            list.Add(new AvailableAlias
            {
                AliasId = Str(e, "alias_id") ?? "",
                TargetModel = Str(e, "target_model"),
                Methods = methods,
                Strategy = Str(e, "strategy"),
                GrantStatus = Str(e, "grant_status") ?? "",
                Visibility = Str(e, "visibility"),
                Active = Bool(e, "active"),
                Required = Bool(e, "required"),
            });
        }
        return list;
    }

    private static string? Str(JsonElement e, string name) =>
        e.TryGetProperty(name, out var v) && v.ValueKind == JsonValueKind.String
            ? v.GetString()
            : null;

    private static bool Bool(JsonElement e, string name) =>
        e.TryGetProperty(name, out var v) && v.ValueKind == JsonValueKind.True;
}

// -----------------------------------------------------------------------------
// SQL (per-addon SQLite)
// -----------------------------------------------------------------------------

public static class Sql
{
    /// <summary>Runs a DML/DDL statement; returns the host JSON result document.</summary>
    public static string Exec(string query, string paramsJson = "[]") =>
        Call(HostImports.SqlExecV1, "sql_exec_v1", query, paramsJson);

    /// <summary>Runs a SELECT; returns the host JSON rows document.</summary>
    public static string Query(string query, string paramsJson = "[]") =>
        Call(HostImports.SqlQueryV1, "sql_query_v1", query, paramsJson);

    /// <summary>Runs a SELECT expected to yield one row.</summary>
    public static string QueryOne(string query, string paramsJson = "[]") =>
        Call(HostImports.SqlQueryOneV1, "sql_query_one_v1", query, paramsJson);

    /// <summary>Runs a JSON array of statements atomically.</summary>
    public static string Transaction(string statementsJson)
    {
        var result = HostCalls.CallInOut(
            HostImports.SqlTransactionV1, HostCalls.Utf8(statementsJson));
        if (result.Code != 0)
        {
            throw new HostCallException("sql_transaction_v1", result.Code);
        }
        return HostCalls.Utf8(result.Data);
    }

    private static unsafe string Call(
        Func<int, int, int, int, int, int, int, int> fn,
        string name, string query, string paramsJson)
    {
        var p = HostCalls.Utf8(paramsJson);
        fixed (byte* pp = p)
        {
            int paramsPtr = p.Length == 0 ? 0 : (int)pp;
            int paramsLen = p.Length;
            var result = HostCalls.CallInOut(
                (inPtr, inLen, outPtr, outCap, outLenPtr) =>
                    fn(inPtr, inLen, paramsPtr, paramsLen, outPtr, outCap, outLenPtr),
                HostCalls.Utf8(query));
            if (result.Code != 0)
            {
                throw new HostCallException(name, result.Code);
            }
            return HostCalls.Utf8(result.Data);
        }
    }
}

// -----------------------------------------------------------------------------
// Flows
// -----------------------------------------------------------------------------

public sealed class FlowInvocation
{
    public string InvocationId { get; init; } = "";
    public string Status { get; init; } = "";
    public string StartedAt { get; init; } = "";
    public string? FinishedAt { get; init; }
    public long OperatorsCompleted { get; init; }
    public long OperatorsTotal { get; init; }
    public string? Error { get; init; }
    public string? ResultToml { get; init; }
}

public static class Flows
{
    /// <summary>Invokes an addon-declared flow template (requires flow.invoke).</summary>
    public static FlowInvocation Invoke(string flowId, string? inputToml = null, uint waitMs = 0)
    {
        var w = new CborWriter(128);
        w.WriteMapHeader(inputToml != null ? 3 : 2);
        w.WriteUInt(0);
        w.WriteText(flowId);
        if (inputToml != null)
        {
            w.WriteUInt(1);
            w.WriteText(inputToml);
        }
        w.WriteUInt(2);
        w.WriteUInt(waitMs);
        return CallInvocation(HostImports.FlowInvokeV1, "flow_invoke_v1", w.ToArray());
    }

    public static FlowInvocation Status(string invocationId) =>
        CallInvocation(HostImports.FlowStatusV1, "flow_status_v1", IdInput(invocationId));

    /// <summary>Requests cooperative cancellation; true when accepted.</summary>
    public static bool Cancel(string invocationId)
    {
        var result = HostCalls.CallInOut(HostImports.FlowCancelV1, IdInput(invocationId));
        if (result.Code != 0)
        {
            throw new HostCallException("flow_cancel_v1", result.Code);
        }
        var r = new CborReader(result.Data);
        int n = r.ReadMapHeader();
        bool cancelled = false;
        for (int i = 0; i < n; i++)
        {
            ulong k = r.ReadUInt();
            if (k == 0)
            {
                cancelled = r.ReadBool();
            }
            else
            {
                Value.Decode(r);
            }
        }
        return cancelled;
    }

    private static byte[] IdInput(string invocationId)
    {
        var w = new CborWriter(64);
        w.WriteMapHeader(1);
        w.WriteUInt(0);
        w.WriteText(invocationId);
        return w.ToArray();
    }

    private static FlowInvocation CallInvocation(
        HostCalls.InOutFn fn, string name, byte[] input)
    {
        var result = HostCalls.CallInOut(fn, input);
        if (result.Code != 0)
        {
            throw new HostCallException(name, result.Code);
        }
        var r = new CborReader(result.Data);
        int n = r.ReadMapHeader();
        string invocationId = "", status = "", startedAt = "";
        string? finishedAt = null, error = null, resultToml = null;
        long done = 0, total = 0;
        for (int i = 0; i < n; i++)
        {
            ulong k = r.ReadUInt();
            switch (k)
            {
                case 0: invocationId = r.ReadText(); break;
                case 1: status = r.ReadText(); break;
                case 2: startedAt = r.ReadText(); break;
                case 3: finishedAt = r.ReadText(); break;
                case 4: done = r.ReadInt(); break;
                case 5: total = r.ReadInt(); break;
                case 6: error = r.ReadText(); break;
                case 7: resultToml = r.ReadText(); break;
                default: Value.Decode(r); break;
            }
        }
        return new FlowInvocation
        {
            InvocationId = invocationId,
            Status = status,
            StartedAt = startedAt,
            FinishedAt = finishedAt,
            OperatorsCompleted = done,
            OperatorsTotal = total,
            Error = error,
            ResultToml = resultToml,
        };
    }
}

// -----------------------------------------------------------------------------
// Services + Tools
// -----------------------------------------------------------------------------

public static class Services
{
    /// <summary>Sends raw bytes to a registered service via the QUIC router.</summary>
    public static unsafe byte[] Call(string serviceName, byte[] request)
    {
        var svc = HostCalls.Utf8(serviceName);
        fixed (byte* sp = svc)
        {
            int svcPtr = (int)sp;
            int svcLen = svc.Length;
            var result = HostCalls.CallInOut(
                (inPtr, inLen, outPtr, outCap, outLenPtr) => HostImports.ServiceRequest(
                    svcPtr, svcLen, inPtr, inLen, outPtr, outCap, outLenPtr),
                request);
            if (result.Code != 0)
            {
                throw new HostCallException("service_request", result.Code);
            }
            return result.Data;
        }
    }
}

public static class Tools
{
    /// <summary>
    /// Registers an LLM tool. `definitionJson` carries
    /// {name, description, parameters, keywords?, return_schema?}.
    /// </summary>
    public static unsafe void Register(string definitionJson)
    {
        var def = HostCalls.Utf8(definitionJson);
        int rc;
        fixed (byte* dp = def)
        {
            rc = HostImports.ToolRegister((int)dp, def.Length);
        }
        if (rc != 0)
        {
            throw new HostCallException("tool_register", rc);
        }
    }
}
