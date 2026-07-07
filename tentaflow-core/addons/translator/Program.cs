// ===== File: Program.cs — Translator (Tłumacz) addon =====
// Local translation addon with three modes:
//  - "text": two panels; the source textarea is translated token-by-token from a
//    local LLM via the streaming LLM host API (StatePatch per batch);
//  - "live": single-user live captioning. The device mic (AudioCapture VAD) hears
//    the speaker, stt_transcribe turns it into text, the LLM streams a translation,
//    and the translated line is rendered onto THIS user's own screen as large
//    subtitles (current line + a couple of fading prior lines). Fully local — no
//    session, no sharing, subtitles never leave the device;
//  - "settings": LLM/STT model names, default language pair, speech
//    auto-detection, subtitle delay and history toggle, persisted to storage.
//
// Backend actions carry only their static params + the DOM event detail (there
// is no automatic panel-state snapshot), so input values are mirrored into the
// addon storage on their change events and read back when an action fires.

#nullable enable

using System;
using System.Collections.Generic;
using System.Globalization;
using System.Runtime.CompilerServices;
using System.Text;
using System.Text.Json;
using TentaFlow.Sdk;
using TentaFlow.Sdk.Components;

using TfValue = TentaFlow.Sdk.Components.Value;

namespace Translator;

internal static class Boot
{
    [ModuleInitializer]
    internal static void Init() => AddonRuntime.Register(new TranslatorAddon());
}

internal sealed class TranslatorAddon : AddonBase
{
    private const string AddonId = "translator";
    private const string PanelId = "main";
    private const string ContentSlot = "content";

    // Host-validated panel epoch (adopted from on_panel_open / action params) and
    // the local mirror of the host state revision (reset to 0 on every shell,
    // advanced only when a StatePatch is accepted).
    private ulong _epoch;
    private ulong _revision;

    // -------------------------------------------------------------------------
    // Language tables
    // -------------------------------------------------------------------------

    private static readonly (string Code, string Display)[] SourceLanguages =
    {
        ("auto", "Wykryj automatycznie"),
        ("pl", "Polski"), ("en", "Angielski"), ("de", "Niemiecki"),
        ("fr", "Francuski"), ("es", "Hiszpański"), ("it", "Włoski"),
        ("uk", "Ukraiński"), ("cs", "Czeski"), ("nl", "Niderlandzki"),
        ("pt", "Portugalski"), ("sv", "Szwedzki"), ("ru", "Rosyjski"),
        ("ro", "Rumuński"), ("da", "Duński"), ("fi", "Fiński"),
        ("no", "Norweski"), ("sk", "Słowacki"), ("hu", "Węgierski"),
        ("el", "Grecki"),
    };

    private static readonly Dictionary<string, string> EnglishNames = new()
    {
        ["pl"] = "Polish", ["en"] = "English", ["de"] = "German",
        ["fr"] = "French", ["es"] = "Spanish", ["it"] = "Italian",
        ["uk"] = "Ukrainian", ["cs"] = "Czech", ["nl"] = "Dutch",
        ["pt"] = "Portuguese", ["sv"] = "Swedish", ["ru"] = "Russian",
        ["ro"] = "Romanian", ["da"] = "Danish", ["fi"] = "Finnish",
        ["no"] = "Norwegian", ["sk"] = "Slovak", ["hu"] = "Hungarian",
        ["el"] = "Greek",
    };

    private static string EnglishName(string code) =>
        EnglishNames.TryGetValue(code, out var n) ? n : "English";

    private static string DisplayName(string code)
    {
        foreach (var (c, d) in SourceLanguages)
        {
            if (c == code)
            {
                return d;
            }
        }
        return code;
    }

    // Target languages: same list without "auto".
    private static IEnumerable<(string Code, string Display)> TargetLanguages()
    {
        foreach (var lang in SourceLanguages)
        {
            if (lang.Code != "auto")
            {
                yield return lang;
            }
        }
    }

    // -------------------------------------------------------------------------
    // Lifecycle
    // -------------------------------------------------------------------------

    public override void OnStart()
    {
        Log.Info("translator addon started (.NET, wasm32-wasip1)");
    }

    public override void OnPanelOpen(string panelId, ulong epoch)
    {
        if (panelId != PanelId)
        {
            return;
        }
        _epoch = epoch;
        _revision = 0;
        SendShell(epoch);
        RenderMode("text");
    }

    public override string OnRequest(string requestJson)
    {
        using var doc = JsonDocument.Parse(requestJson);
        var root = doc.RootElement;
        string tool = root.TryGetProperty("tool", out var t) ? t.GetString() ?? "" : "";
        var pars = root.TryGetProperty("params", out var p) ? p : default;
        string userId = root.TryGetProperty("user_id", out var u)
            ? (u.ValueKind == JsonValueKind.String ? u.GetString() ?? "" : u.ToString())
            : "";

        AdoptEpoch(pars);

        // UI actions arrive as tool = "ui.<panel_id>.<action_id>".
        string actionId = tool.StartsWith("ui.", StringComparison.Ordinal)
            ? tool[(tool.LastIndexOf('.') + 1)..]
            : tool;

        try
        {
            return actionId switch
            {
                "set_mode" => HandleSetMode(pars, userId),
                "set_draft" => HandleSetDraft(pars, userId),
                "set_config" => HandleSetConfig(pars),
                "translate" => HandleTranslate(userId),
                "swap_langs" => HandleSwapLangs(userId),
                "clear_text" => HandleClearText(userId),
                "copy_result" => HandleCopyResult(),
                "speak_result" => HandleSpeakResult(),
                "save_settings" => HandleSaveSettings(),
                "audio_utterance" => HandleAudioUtterance(pars, userId),
                _ => Err($"unknown action: {actionId}"),
            };
        }
        catch (HostCallException e)
        {
            Log.Error($"action '{actionId}' host error {e.Code}");
            return Err($"host error {e.Code}");
        }
    }

    // -------------------------------------------------------------------------
    // Action handlers
    // -------------------------------------------------------------------------

    private string HandleSetMode(JsonElement pars, string userId)
    {
        string mode = GetString(pars, "value") ?? "text";
        // A mode change supersedes any in-flight translation / live stream: the
        // generation bump makes StreamTranslation abort + Cancel() its LLM slot
        // (calls may run on a different worker), and we clear its transient flags.
        BumpGeneration(userId);
        SendPatch(
            SetOp("translating", TfValue.Bool(false)),
            SetOp("pipeline", "Gotowy — mów, aby rozpocząć"));
        RenderMode(mode);
        return Ok();
    }

    // Mirrors an input's current value into per-user storage so a later action
    // (translate / utterance) can read it — backend actions never receive the
    // live panel state, only their own event detail.
    private string HandleSetDraft(JsonElement pars, string userId)
    {
        string key = GetString(pars, "key") ?? "";
        string value = GetValueAsString(pars);
        if (key.Length == 0)
        {
            return Err("missing draft key");
        }
        Storage.Set(DraftKey(userId, key), value);

        if (key == "source")
        {
            SendPatch(SetOp("char_count", value.Length.ToString(CultureInfo.InvariantCulture)));
            // "Translate automatically while typing" — the textarea commits its
            // value on change, which we treat as the debounce boundary.
            if (AutoTranslate() && value.Trim().Length > 0)
            {
                return HandleTranslate(userId);
            }
        }
        return Ok();
    }

    private string HandleSetConfig(JsonElement pars)
    {
        string key = GetString(pars, "key") ?? "";
        string value = GetValueAsString(pars);
        if (key.Length == 0)
        {
            return Err("missing config key");
        }
        Storage.Set(ConfigKey(key), value);
        return Ok();
    }

    private string HandleSwapLangs(string userId)
    {
        string src = DraftOr(userId, "src", DefaultSrc());
        string tgt = DraftOr(userId, "tgt", DefaultTgt());
        // "auto" cannot become a target — fall back to the current target's code
        // as the new source, keeping a valid pair.
        string newSrc = tgt;
        string newTgt = src == "auto" ? DefaultTgt() : src;
        Storage.Set(DraftKey(userId, "src"), newSrc);
        Storage.Set(DraftKey(userId, "tgt"), newTgt);
        SendPatch(
            SetOp("src_lang", newSrc),
            SetOp("tgt_lang", newTgt),
            SetOp("detected", ""));
        return Ok();
    }

    private string HandleClearText(string userId)
    {
        Storage.Set(DraftKey(userId, "source"), "");
        SendPatch(
            SetOp("source_text", ""),
            SetOp("target_text", ""),
            SetOp("char_count", "0"),
            SetOp("detected", ""),
            SetOp("status", "Gotowy"));
        return Ok();
    }

    // Clipboard / speech synthesis are client-side capabilities that the addon
    // wasm sandbox cannot reach directly (no host clipboard/TTS command is
    // exposed to guests yet), so these actions acknowledge the request with a
    // truthful transient status rather than claiming a copy/playback happened.
    private string HandleCopyResult()
    {
        SendPatch(SetOp("status", $"{ModelLabel()} · tłumaczenie gotowe do skopiowania"));
        return Ok();
    }

    private string HandleSpeakResult()
    {
        SendPatch(SetOp("status", $"{ModelLabel()} · odsłuch niedostępny w tej wersji"));
        return Ok();
    }

    // Settings already persist on each control's change event, so the explicit
    // Save button is a confirmation: it re-affirms the stored values and reports
    // that they take effect on the next session.
    private string HandleSaveSettings()
    {
        SendPatch(SetOp("settings_saved", "Zapisano — zmiany działają od następnej sesji"));
        return Ok();
    }

    private string HandleTranslate(string userId)
    {
        string source = Storage.Get(DraftKey(userId, "source")) ?? "";
        if (source.Trim().Length == 0)
        {
            SendPatch(SetOp("status", "Wpisz tekst do przetłumaczenia"));
            return Ok();
        }
        if (ConfigOrNull("llm_model") == null)
        {
            SendPatch(SetOp("status", "Wybierz model tłumaczenia w Ustawieniach"));
            return Ok();
        }
        string src = DraftOr(userId, "src", DefaultSrc());
        string tgt = DraftOr(userId, "tgt", DefaultTgt());

        long gen = BumpGeneration(userId);
        SendPatch(
            SetOp("translating", TfValue.Bool(true)),
            SetOp("target_text", ""),
            SetOp("status", $"{ModelLabel()} · tłumaczę…"));

        var (translation, finish, tokens) = StreamTranslation(
            source, BuildSystemPrompt(src, tgt), "target_text",
            () => IsStaleGeneration(userId, gen));

        // A newer translation / utterance or a mode switch took over — leave the
        // display to the newest generation instead of clobbering it.
        if (IsStaleGeneration(userId, gen))
        {
            return Ok();
        }

        string status = finish == "error"
            ? $"{ModelLabel()} · błąd tłumaczenia"
            : $"{ModelLabel()} · gotowe · {tokens} tok.";
        SendPatch(
            SetOp("translating", TfValue.Bool(false)),
            SetOp("status", status));

        if (finish != "error" && SaveHistory())
        {
            SaveTranslationHistory(src, tgt, source, translation);
        }
        return Ok();
    }

    /// <summary>Recent translated lines kept on-screen (current + fading prior).</summary>
    private const int LiveWindow = 3;

    // Single-user live captioning: the phone mic hears the speaker, we transcribe
    // and translate locally, and the translated line is streamed onto THIS user's
    // own screen. No session, no sharing — the subtitles never leave the device.
    private string HandleAudioUtterance(JsonElement pars, string userId)
    {
        string? docRef = GetString(pars, "doc_ref");
        string mime = GetString(pars, "mime") ?? "audio/wav";
        uint? sampleRate = GetUInt(pars, "sample_rate");
        if (string.IsNullOrEmpty(docRef))
        {
            return Err("missing doc_ref");
        }
        if (ConfigOrNull("llm_model") == null)
        {
            SendPatch(SetOp("pipeline", "Wybierz model tłumaczenia w Ustawieniach"));
            return Ok();
        }

        string speaker = DraftOr(userId, "speaker", "en");
        string subtitle = DraftOr(userId, "subtitle", DefaultTgt());

        // STT is authorized by the host on the plain "stt" permission only — it
        // does NOT resolve or re-check aliases. So we re-validate the selected STT
        // alias against the LIVE grants here and pass the resolved concrete model;
        // a revoked/deactivated grant blocks the call. An unset selection = host
        // default STT.
        string? sttAlias = ConfigOrNull("stt_model");
        string? sttModel = null;
        if (sttAlias != null)
        {
            sttModel = ResolveTargetModel(sttAlias);
            if (sttModel == null)
            {
                SendPatch(SetOp("pipeline", "Wybrany model transkrypcji jest niedostępny — sprawdź Ustawienia"));
                return Ok();
            }
        }

        // This fresh utterance supersedes any still-streaming previous one — we
        // want the latest speech, not a backlog. The bump also lets the older
        // stream (possibly on another worker) abort and free its LLM slot.
        long gen = BumpGeneration(userId);
        SendPatch(SetOp("pipeline", "Transkrypcja mowy…"));

        // Read the captured WAV back from the document store and transcribe it.
        // "auto" is not an ISO code — with auto-detection (or an explicit "auto"
        // speaker) we omit the language and let the STT engine detect it.
        var (audio, audioMime) = Documents.Get(docRef);
        string? sttLanguage = (AutoDetect() || speaker == "auto") ? null : speaker;
        var stt = Stt.Transcribe(audio, string.IsNullOrEmpty(audioMime) ? mime : audioMime,
            new SttOptions
            {
                SampleRate = sampleRate,
                Language = sttLanguage,
                Model = sttModel,
            });

        string orig = stt.Text.Trim();
        if (orig.Length == 0)
        {
            // Terminal path: clear the streaming caret, but only if this is still
            // the current generation (a newer utterance owns the flag otherwise) —
            // a prior superseded utterance may have left `translating` true.
            if (!IsStaleGeneration(userId, gen))
            {
                SendPatch(
                    SetOp("translating", TfValue.Bool(false)),
                    SetOp("pipeline", "Gotowy — mów, aby rozpocząć"));
            }
            return Ok();
        }
        string detectedSpeaker = stt.DetectedLanguage ?? (speaker == "auto" ? "en" : speaker);

        // A newer utterance or a mode switch already took over during transcription.
        if (IsStaleGeneration(userId, gen))
        {
            return Ok();
        }

        // Surface the two most recent finished lines as fading context while the
        // new translation streams into the big current line.
        var buffer = ReadLiveBuffer(userId);
        string prev1 = buffer.Count >= 1 ? buffer[^1].Trans : "";
        string prev2 = buffer.Count >= 2 ? buffer[^2].Trans : "";
        SendPatch(
            SetOp("live_prev2", prev2),
            SetOp("live_prev1", prev1),
            SetOp("live_orig", orig),
            SetOp("live_cur", ""),
            // `translating` drives the streaming caret on the current line.
            SetOp("translating", TfValue.Bool(true)),
            SetOp("pipeline", $"{ModelLabel()} — tłumaczenie…"));

        var (trans, finish, _) = StreamTranslation(
            orig, BuildSystemPrompt(detectedSpeaker, subtitle), "live_cur",
            () => IsStaleGeneration(userId, gen));

        // Superseded mid-translation — don't append the stale line or clobber the
        // newest utterance's display.
        if (IsStaleGeneration(userId, gen))
        {
            return Ok();
        }

        if (finish == "error")
        {
            SendPatch(
                SetOp("translating", TfValue.Bool(false)),
                SetOp("pipeline", "Błąd tłumaczenia — spróbuj ponownie"));
            return Ok();
        }

        buffer.Add((orig, trans));
        while (buffer.Count > LiveWindow)
        {
            buffer.RemoveAt(0);
        }
        WriteLiveBuffer(userId, buffer);

        SendPatch(
            SetOp("translating", TfValue.Bool(false)),
            SetOp("pipeline", "Gotowy — mów dalej"));

        if (SaveHistory())
        {
            SaveTranslationHistory(detectedSpeaker, subtitle, orig, trans);
        }
        return Ok();
    }

    // -------------------------------------------------------------------------
    // Live subtitle buffer (per-user, local only)
    // -------------------------------------------------------------------------

    private static string LiveBufKey(string userId) => $"live.buf.{userId}";

    private List<(string Orig, string Trans)> ReadLiveBuffer(string userId)
    {
        var lines = new List<(string, string)>();
        string json = Storage.Get(LiveBufKey(userId)) ?? "";
        if (json.Length == 0)
        {
            return lines;
        }
        try
        {
            using var doc = JsonDocument.Parse(json);
            if (doc.RootElement.ValueKind == JsonValueKind.Array)
            {
                foreach (var pair in doc.RootElement.EnumerateArray())
                {
                    if (pair.ValueKind == JsonValueKind.Array && pair.GetArrayLength() >= 2)
                    {
                        lines.Add((pair[0].GetString() ?? "", pair[1].GetString() ?? ""));
                    }
                }
            }
        }
        catch (JsonException)
        {
        }
        return lines;
    }

    private void WriteLiveBuffer(string userId, List<(string Orig, string Trans)> lines)
    {
        using var stream = new System.IO.MemoryStream();
        using (var jw = new Utf8JsonWriter(stream))
        {
            jw.WriteStartArray();
            foreach (var (orig, trans) in lines)
            {
                jw.WriteStartArray();
                jw.WriteStringValue(orig);
                jw.WriteStringValue(trans);
                jw.WriteEndArray();
            }
            jw.WriteEndArray();
        }
        Storage.Set(LiveBufKey(userId), Encoding.UTF8.GetString(stream.ToArray()));
    }

    // -------------------------------------------------------------------------
    // Generation guard (per-user "current translation" token)
    // -------------------------------------------------------------------------
    //
    // A monotonically increasing per-user counter in storage identifies the
    // freshest translation/live utterance. Every new translate/utterance and
    // every mode change bumps it; an in-flight StreamTranslation polls it and
    // aborts (Cancel()) as soon as a newer generation appears. Storage is the
    // source of truth because addon calls may land on different pooled workers,
    // so an instance field would not be visible across them.

    private static string GenKey(string userId) => $"xlate.gen.{userId}";

    private long ReadGeneration(string userId)
    {
        string v = Storage.Get(GenKey(userId)) ?? "";
        return long.TryParse(v, out long g) ? g : 0;
    }

    private long BumpGeneration(string userId)
    {
        long next = ReadGeneration(userId) + 1;
        Storage.Set(GenKey(userId), next.ToString(CultureInfo.InvariantCulture));
        return next;
    }

    private bool IsStaleGeneration(string userId, long gen) => ReadGeneration(userId) != gen;

    // -------------------------------------------------------------------------
    // Translation streaming
    // -------------------------------------------------------------------------

    /// <summary>Per-batch wait (ms) for the first delta of a batch.</summary>
    private const ulong StreamBatchTimeoutMs = 5000;

    /// <summary>Hard wall-clock ceiling for a single translation (ms).</summary>
    private const long StreamDeadlineMs = 120_000;

    /// <summary>
    /// Consecutive empty (timed-out) batches after which we give up — a stalled
    /// backend must not pin the addon action forever. 5 s × 12 ≈ 60 s of silence.
    /// </summary>
    private const int StreamMaxEmptyBatches = 12;

    /// <summary>
    /// Streams a translation into `statePath`, emitting a StatePatch per batch.
    /// Returns (full text, finish reason, token count). Bounded by a wall-clock
    /// deadline and a consecutive-empty-batch cap; the stream is always cancelled
    /// unless it finished on its own, so a failed or stalled generation cannot
    /// leak a host stream slot (quota is 4 per addon). When `isStale` is provided
    /// and starts returning true (a newer generation superseded this one, or the
    /// user left the mode), the loop stops and Cancel() frees the slot at once.
    /// </summary>
    private (string Text, string Finish, int Tokens) StreamTranslation(
        string userText, string system, string statePath, Func<bool>? isStale = null)
    {
        var sb = new StringBuilder();
        int tokens = 0;
        string finish = "stop";

        LlmStream stream;
        try
        {
            // The instruction goes in the SYSTEM message; the user message is the
            // pure source text — no "Text:"/"Translation:" scaffolding, so an
            // instruction-tuned model won't echo a label.
            stream = Llm.GenerateStream(userText, ConfigOrNull("llm_model"),
                "{\"temperature\":0.2,\"max_tokens\":2048}", system);
        }
        catch (HostCallException e)
        {
            Log.Error($"stream start failed: {e.Code}");
            return ("", "error", 0);
        }

        long start = DateTimeOffset.UtcNow.ToUnixTimeMilliseconds();
        int emptyBatches = 0;
        bool finishedCleanly = false;
        try
        {
            while (true)
            {
                if (isStale != null && isStale())
                {
                    Log.Info("translation superseded — aborting");
                    finish = "stop";
                    break;
                }
                if (DateTimeOffset.UtcNow.ToUnixTimeMilliseconds() - start > StreamDeadlineMs)
                {
                    Log.Warn("translation exceeded wall-clock deadline — aborting");
                    finish = "error";
                    break;
                }

                LlmStreamBatch batch;
                try
                {
                    batch = stream.NextBatch(StreamBatchTimeoutMs);
                }
                catch (HostCallException e)
                {
                    Log.Error($"stream next failed: {e.Code}");
                    finish = "error";
                    break;
                }

                if (batch.Chunks.Count > 0)
                {
                    emptyBatches = 0;
                    foreach (var c in batch.Chunks)
                    {
                        sb.Append(c);
                        tokens++;
                    }
                    SendPatch(SetOp(statePath, sb.ToString()));
                }
                else if (!batch.Finished)
                {
                    // Timed-out poll with no data — a live backend keeps producing;
                    // a stalled one trips this cap and we bail.
                    if (++emptyBatches >= StreamMaxEmptyBatches)
                    {
                        Log.Warn("translation stalled (no data) — aborting");
                        finish = "error";
                        break;
                    }
                }

                if (batch.Finished)
                {
                    finish = batch.Error != null ? "error" : batch.FinishReason ?? "stop";
                    finishedCleanly = true;
                    break;
                }
            }
        }
        finally
        {
            // The stream already reached its terminal batch on a clean finish;
            // otherwise (deadline / stall / error / early return) free the host
            // slot now instead of waiting for the 60 s idle reaper.
            if (!finishedCleanly)
            {
                try
                {
                    stream.Cancel();
                }
                catch (HostCallException e)
                {
                    Log.Warn($"stream cancel failed: {e.Code}");
                }
            }
        }
        return (sb.ToString(), finish, tokens);
    }

    // System instruction for the translator. The source text is sent SEPARATELY
    // as the user message, so there is no "Text:"/"Translation:" scaffolding for
    // an instruction-tuned model to echo back as a label.
    private static string BuildSystemPrompt(string srcCode, string tgtCode)
    {
        string tgt = EnglishName(tgtCode);
        string src = srcCode == "auto"
            ? "the source language (detect it automatically)"
            : EnglishName(srcCode);
        return
            $"You are a professional translator. Translate from {src} to {tgt}. " +
            "Preserve the original meaning, tone, line breaks and formatting. " +
            "Output ONLY the translation — no labels, no preamble, no quotes, no explanations.";
    }

    // -------------------------------------------------------------------------
    // Translation history (local, gated by the "save history" setting)
    // -------------------------------------------------------------------------

    /// <summary>Max rows kept in translation_history.</summary>
    private const long HistoryMaxRows = 500;

    private void SaveTranslationHistory(string src, string tgt, string source, string target)
    {
        Sql.Exec(
            "INSERT INTO translation_history (src_lang, tgt_lang, source_text, target_text, created_at) " +
            "VALUES (?, ?, ?, ?, ?)",
            JsonArray(src, tgt, source, target, NowSeconds()));
        PruneHistory();
    }

    // Caps the translation history to the newest N rows.
    private void PruneHistory()
    {
        Sql.Exec(
            "DELETE FROM translation_history WHERE id NOT IN " +
            "(SELECT id FROM translation_history ORDER BY id DESC LIMIT ?)",
            JsonArray(HistoryMaxRows));
    }

    // -------------------------------------------------------------------------
    // Config / draft storage
    // -------------------------------------------------------------------------

    private static string DraftKey(string userId, string key) => $"draft.{userId}.{key}";

    private static string ConfigKey(string key) => $"cfg.{key}";

    private string DraftOr(string userId, string key, string fallback)
    {
        string v = Storage.Get(DraftKey(userId, key)) ?? "";
        return v.Length > 0 ? v : fallback;
    }

    private string? ConfigOrNull(string key)
    {
        string v;
        try
        {
            v = (Storage.Get(ConfigKey(key)) ?? "").Trim();
        }
        catch (HostCallException e) when (e.Code == LegacyAbi.ErrPermission)
        {
            // No storage.read grant (permission-limited boot) — fall back to the
            // neutral default. Any OTHER host error is a real fault, so re-throw.
            return null;
        }
        return v.Length > 0 ? v : null;
    }

    private string DefaultSrc() => ConfigOrNull("default_src") ?? "auto";

    private string DefaultTgt() => ConfigOrNull("default_tgt") ?? "pl";

    private bool AutoDetect() => (Storage.Get(ConfigKey("auto_detect")) ?? "true") != "false";

    private bool AutoTranslate() => (Storage.Get(ConfigKey("auto_translate")) ?? "false") == "true";

    private bool SaveHistory() => (Storage.Get(ConfigKey("save_history")) ?? "false") == "true";

    // Readable translation-model label: the concrete model the picked alias
    // resolves to (cfg.llm_model is an alias_id; runtime keeps using the alias,
    // the UI shows the resolved model name). Neutral prompt when unselected.
    private string ModelLabel() => ModelDisplay(ConfigOrNull("llm_model"));

    // -------------------------------------------------------------------------
    // Model access (admin-assigned via aliases; addon lists + uses the grants)
    // -------------------------------------------------------------------------

    // Cache of alias_id → resolved target_model, only for display labels (avoids
    // a host call per status patch). Never used for the runtime path — STT
    // re-resolves live per call so a revoked grant is honored immediately.
    private readonly Dictionary<string, string> _modelDisplayCache = new();

    // Resolves an alias_id to its concrete target_model against the CURRENT
    // grants, re-validating grant status + active per call. Returns null when the
    // alias is no longer granted / active / resolved (revoked or deactivated).
    private string? ResolveTargetModel(string? aliasId)
    {
        if (string.IsNullOrEmpty(aliasId))
        {
            return null;
        }
        foreach (var a in AvailableModels())
        {
            if (a.AliasId == aliasId)
            {
                if (!IsGranted(a) || !a.Active || string.IsNullOrEmpty(a.TargetModel))
                {
                    return null;
                }
                return a.TargetModel;
            }
        }
        return null;
    }

    // Human-readable model name for UI labels. Positive resolutions are cached;
    // negatives are not, so a re-granted model shows up without a stale label.
    private string ModelDisplay(string? aliasId)
    {
        if (string.IsNullOrEmpty(aliasId))
        {
            return "Wybierz model";
        }
        if (_modelDisplayCache.TryGetValue(aliasId, out var cached))
        {
            return cached;
        }
        string? target = ResolveTargetModel(aliasId);
        if (target == null)
        {
            return "Model niedostępny";
        }
        _modelDisplayCache[aliasId] = target;
        return target;
    }

    // Method tags that mark an alias as usable for text generation vs speech.
    private static readonly string[] LlmMethods =
        { "chat", "generate", "complete", "completion", "llm", "translate", "text" };
    private static readonly string[] SttMethods =
        { "transcribe", "recognize", "stt", "asr", "speech" };

    // Models this addon may consume (its [[uses_alias]] grants). Missing the
    // alias.read grant is not a render failure — we simply have nothing to offer.
    private List<AvailableAlias> AvailableModels()
    {
        try
        {
            return Aliases.ListAvailable();
        }
        catch (HostCallException e) when (e.Code == (int)AbiError.Permission)
        {
            return new List<AvailableAlias>();
        }
    }

    private static bool IsGranted(AvailableAlias a) =>
        a.GrantStatus == "granted" || a.GrantStatus == "auto_granted";

    private static bool HasAnyMethod(AvailableAlias a, string[] wanted)
    {
        foreach (var m in a.Methods)
        {
            foreach (var w in wanted)
            {
                if (string.Equals(m, w, StringComparison.OrdinalIgnoreCase))
                {
                    return true;
                }
            }
        }
        return false;
    }

    // Granted + active models usable for translation: LLM-capable, or generic (no
    // declared methods) so an admin can map a plain model without tagging it.
    private List<AvailableAlias> LlmModels(List<AvailableAlias> all)
    {
        var outp = new List<AvailableAlias>();
        foreach (var a in all)
        {
            if (IsGranted(a) && a.Active && (a.Methods.Count == 0 || HasAnyMethod(a, LlmMethods)))
            {
                outp.Add(a);
            }
        }
        return outp;
    }

    // Granted + active models usable for speech-to-text.
    private List<AvailableAlias> SttModels(List<AvailableAlias> all)
    {
        var outp = new List<AvailableAlias>();
        foreach (var a in all)
        {
            if (IsGranted(a) && a.Active && HasAnyMethod(a, SttMethods))
            {
                outp.Add(a);
            }
        }
        return outp;
    }

    // -------------------------------------------------------------------------
    // UI: shell + per-mode content
    // -------------------------------------------------------------------------

    private void SendShell(ulong epoch)
    {
        // App title with a leading accent glyph (a badge carrying the addon's
        // translate icon — no CSS ::before).
        var title = new Box
        {
            Id = "app-title",
            Direction = FlexDirection.Row,
            Align = FlexAlign.Center,
            Gap = Spacing.Sm,
            Children = new List<Component>
            {
                new Badge
                {
                    Id = "app-glyph",
                    Variant = BadgeVariant.Soft,
                    Tone = Tone.Primary,
                    Icon = new IconRefNamed { Name = IconName.Globe },
                    Label = Bind.Lit(""),
                },
                new Heading { Id = "app-title-text", Content = Bind.Lit("Tłumacz"), Level = 3 },
            },
        };

        var topBar = new Box
        {
            Id = "topbar",
            Direction = FlexDirection.Row,
            Align = FlexAlign.Center,
            Justify = FlexJustify.SpaceBetween,
            Gap = Spacing.Md,
            Style = new BoxStyle
            {
                Padding = AllEdges(Spacing.Md),
                Border = AllBorders(1, BorderColor.Default),
                Radius = AllCorners(RadiusToken.Lg),
                Background = BackgroundToken.Subtle,
            },
            // Center once narrow; stack title / mode switch / status pill into a
            // column on a phone so the row never overflows.
            Responsive = new List<ResponsiveRule>
            {
                new ResponsiveRule { MaxWidth = Px(680), Justify = FlexJustify.Center },
                new ResponsiveRule { MaxWidth = Px(460), Direction = FlexDirection.Column, Align = FlexAlign.Center },
            },
            Children = new List<Component>
            {
                title,
                ModeSwitch(),
                new Badge
                {
                    Id = "model-pill",
                    Variant = BadgeVariant.Pulse,
                    Tone = Tone.Success,
                    Label = Bind.State("status"),
                },
            },
        };

        // Layout is just the top bar; the main content slot is appended after
        // it by the panel host. Wrapping the slot in a titled Inspector card
        // duplicated the app title and boxed the content — the dashboard panel
        // already provides the outer chrome.
        var layout = new Stack
        {
            Id = "root",
            Gap = Spacing.Md,
            Align = FlexAlign.Stretch,
            Children = new List<Component> { topBar },
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
                    Id = ContentSlot,
                    Semantics = SlotSemantics.MainContent,
                    DefaultState = SlotDefault.Loading,
                    CachePolicy = CachePolicy.None,
                    Visibility = SlotVisibility.Always,
                },
            },
            InitialState = InitialState(),
        });
    }

    private List<StateEntry> InitialState()
    {
        string model = ModelLabel();
        return new List<StateEntry>
        {
            Entry("mode", TfValue.Text("text")),
            Entry("src_lang", TfValue.Text("auto")),
            Entry("tgt_lang", TfValue.Text("pl")),
            Entry("source_text", TfValue.Text("")),
            Entry("target_text", TfValue.Text("")),
            Entry("char_count", TfValue.Text("0")),
            Entry("detected", TfValue.Text("")),
            Entry("translating", TfValue.Bool(false)),
            Entry("auto_translate", TfValue.Bool(false)),
            Entry("status", TfValue.Text($"{model} · gotowy")),
            Entry("engine_label", TfValue.Text(model)),
            Entry("stat_time", TfValue.Text("—")),
            Entry("stat_tps", TfValue.Text("—")),
            Entry("stat_tokens", TfValue.Text("—")),
            Entry("settings_saved", TfValue.Text("")),
            Entry("speaker_lang", TfValue.Text("en")),
            Entry("subtitle_lang", TfValue.Text("pl")),
            Entry("recording", TfValue.Bool(false)),
            Entry("pipeline", TfValue.Text("Gotowy — mów, aby rozpocząć")),
            Entry("live_orig", TfValue.Text("")),
            Entry("live_cur", TfValue.Text("")),
            Entry("live_prev1", TfValue.Text("")),
            Entry("live_prev2", TfValue.Text("")),
            Entry("set_llm_model", TfValue.Text(ConfigOrNull("llm_model") ?? "")),
            Entry("set_stt_model", TfValue.Text(ConfigOrNull("stt_model") ?? "")),
            Entry("set_default_src", TfValue.Text("auto")),
            Entry("set_default_tgt", TfValue.Text("pl")),
            Entry("set_auto_detect", TfValue.Bool(true)),
            Entry("set_subtitle_delay", TfValue.Float(1.1)),
            Entry("set_save_history", TfValue.Bool(false)),
        };
    }

    private Component ModeSwitch()
    {
        return new SegmentedControl
        {
            Id = "mode-switch",
            BindPath = StatePath.Keys("mode"),
            Size = SegmentSize.Md,
            Options = new List<SegmentOption>
            {
                Segment("text", "Tekst"),
                Segment("live", "Na żywo"),
                Segment("settings", "Ustawienia"),
            },
            Handlers = Backend(EventKind.Change, "set_mode"),
        };
    }

    private void RenderMode(string mode)
    {
        SendPatch(SetOp("mode", mode));
        Component fragment = mode switch
        {
            "live" => LiveMode(),
            "settings" => SettingsMode(),
            _ => TextMode(),
        };
        Ui.Render(new SlotContent
        {
            AddonId = AddonId,
            PanelId = PanelId,
            PanelEpoch = _epoch,
            SlotId = ContentSlot,
            Fragment = fragment,
        });
    }

    private Component TextMode()
    {
        var langBar = new Box
        {
            Id = "lang-bar",
            Direction = FlexDirection.Row,
            Align = FlexAlign.Center,
            Justify = FlexJustify.Center,
            Gap = Spacing.Sm,
            Children = new List<Component>
            {
                LangSelect("src-select", "src_lang", "src", withAuto: true),
                new IconButton
                {
                    Id = "swap-btn",
                    Icon = new IconRefNamed { Name = IconName.Refresh },
                    Variant = ButtonVariant.Ghost,
                    Tone = Tone.Neutral,
                    Size = ButtonSize.Md,
                    AriaLabel = "Zamień języki",
                    Handlers = Backend(EventKind.Click, "swap_langs"),
                },
                LangSelect("tgt-select", "tgt_lang", "tgt", withAuto: false),
                new Badge
                {
                    Id = "detected-chip",
                    Variant = BadgeVariant.Soft,
                    Tone = Tone.Info,
                    Label = Bind.State("detected"),
                },
            },
        };

        var panels = new Box
        {
            Id = "panes",
            Direction = FlexDirection.Row,
            Gap = Spacing.Md,
            Align = FlexAlign.Stretch,
            // Two equal panes side by side; stack into a column on a narrow panel.
            Responsive = new List<ResponsiveRule>
            {
                new ResponsiveRule { MaxWidth = Px(680), Direction = FlexDirection.Column },
            },
            Children = new List<Component>
            {
                PanelBox("pane-src", "Tekst źródłowy", accent: false,
                    new IconButton
                    {
                        Id = "clear-btn",
                        Icon = new IconRefNamed { Name = IconName.X },
                        Variant = ButtonVariant.Ghost,
                        Tone = Tone.Neutral,
                        Size = ButtonSize.Sm,
                        AriaLabel = "Wyczyść",
                        Handlers = Backend(EventKind.Click, "clear_text"),
                    },
                    new Textarea
                    {
                        Id = "source-text",
                        BindPath = StatePath.Keys("source_text"),
                        Placeholder = Bind.Lit("Wpisz lub wklej tekst do przetłumaczenia…"),
                        Rows = 12,
                        Autoresize = true,
                        MaxRows = 24,
                        A11y = new Accessibility { Label = Bind.Lit("Tekst źródłowy") },
                        Handlers = BackendKV(EventKind.Change, "set_draft", "source"),
                    },
                    new Text
                    {
                        Id = "char-count",
                        Content = Bind.State("char_count"),
                        Style = TextStyle.Caption,
                        Tone = Tone.Muted,
                    }),
                PanelBox("pane-tgt", "Tłumaczenie", accent: true,
                    new Box
                    {
                        Id = "tgt-actions",
                        Direction = FlexDirection.Row,
                        Align = FlexAlign.Center,
                        Gap = Spacing.Xs,
                        Children = new List<Component>
                        {
                            new IconButton
                            {
                                Id = "copy-btn",
                                Icon = new IconRefNamed { Name = IconName.Copy },
                                Variant = ButtonVariant.Ghost,
                                Tone = Tone.Neutral,
                                Size = ButtonSize.Sm,
                                AriaLabel = "Kopiuj tłumaczenie",
                                Handlers = Backend(EventKind.Click, "copy_result"),
                            },
                            new IconButton
                            {
                                Id = "speak-btn",
                                Icon = new IconRefNamed { Name = IconName.Volume },
                                Variant = ButtonVariant.Ghost,
                                Tone = Tone.Neutral,
                                Size = ButtonSize.Sm,
                                AriaLabel = "Odsłuchaj tłumaczenie",
                                Handlers = Backend(EventKind.Click, "speak_result"),
                            },
                        },
                    },
                    new Text
                    {
                        Id = "target-text",
                        Content = Bind.State("target_text"),
                        Style = TextStyle.Body,
                        Wrap = TextWrap.Wrap,
                        // Streaming caret while a translation is being produced.
                        Streaming = Bind.State("translating"),
                    }),
            },
        };

        var controls = new Box
        {
            Id = "text-controls",
            Direction = FlexDirection.Row,
            Align = FlexAlign.Center,
            Justify = FlexJustify.SpaceBetween,
            Gap = Spacing.Md,
            // Stack the toggle over the (full-width) button on a narrow panel so
            // the controls never overflow.
            Responsive = new List<ResponsiveRule>
            {
                new ResponsiveRule { MaxWidth = Px(460), Direction = FlexDirection.Column, Align = FlexAlign.Stretch },
            },
            Children = new List<Component>
            {
                new Toggle
                {
                    Id = "auto-translate",
                    BindPath = StatePath.Keys("auto_translate"),
                    Label = Bind.Lit("Tłumacz automatycznie podczas pisania"),
                    Size = ToggleSize.Md,
                    Handlers = BackendKV(EventKind.Change, "set_config", "auto_translate"),
                },
                new Button
                {
                    Id = "translate-btn",
                    Variant = ButtonVariant.Primary,
                    Tone = Tone.Primary,
                    Label = Bind.Lit("Przetłumacz"),
                    Size = ButtonSize.Md,
                    Loading = Bind.State("translating"),
                    IconLeading = new IconRefNamed { Name = IconName.Globe },
                    Handlers = Backend(EventKind.Click, "translate"),
                },
            },
        };

        var statusBar = new Box
        {
            Id = "status-bar",
            Direction = FlexDirection.Row,
            Align = FlexAlign.Center,
            Gap = Spacing.Md,
            // The engine pill + stat chips stack on a narrow panel instead of
            // overflowing (Box has no flex-wrap; a column is the safe fallback).
            Responsive = new List<ResponsiveRule>
            {
                new ResponsiveRule { MaxWidth = Px(460), Direction = FlexDirection.Column, Align = FlexAlign.Stretch },
            },
            Children = new List<Component>
            {
                new Badge
                {
                    Id = "engine-pill",
                    Variant = BadgeVariant.Pulse,
                    Tone = Tone.Success,
                    Label = Bind.State("engine_label"),
                },
                StatChip("stat-time", IconName.Clock, "stat_time"),
                StatChip("stat-tps", IconName.Zap, "stat_tps"),
                StatChip("stat-tokens", IconName.Bolt, "stat_tokens"),
            },
        };

        return new Stack
        {
            Id = "text-mode",
            Gap = Spacing.Md,
            Align = FlexAlign.Stretch,
            Children = new List<Component> { langBar, panels, controls, statusBar },
        };
    }

    // Small icon + value chip used on the text-mode status bar (time / tok-s /
    // token count). The value is state-bound so a running translation updates it.
    private static Component StatChip(string id, IconName icon, string statePath) =>
        new Badge
        {
            Id = id,
            Variant = BadgeVariant.Soft,
            Tone = Tone.Neutral,
            Icon = new IconRefNamed { Name = icon },
            Label = Bind.State(statePath),
        };

    // Single-user live captioning, phone-first: stacked language rows, a big mic
    // button, and a large scalable subtitle stage. Everything stays local — the
    // subtitles are rendered onto this user's own screen only.
    private Component LiveMode()
    {
        var langCard = new Stack
        {
            Id = "live-langs",
            Gap = Spacing.Sm,
            Align = FlexAlign.Stretch,
            Style = new BoxStyle
            {
                Padding = AllEdges(Spacing.Md),
                Border = AllBorders(1, BorderColor.Default),
                Radius = AllCorners(RadiusToken.Lg),
                Background = BackgroundToken.Subtle,
            },
            // On a phone the reading stage goes first — keep the language pair on top.
            Responsive = new List<ResponsiveRule>
            {
                new ResponsiveRule { MaxWidth = Px(460), Order = 0 },
            },
            Children = new List<Component>
            {
                LiveLangRow("live-hear", "Słyszę:", "speaker-select", "speaker_lang", "speaker", withAuto: true),
                LiveLangRow("live-show", "Napisy:", "subtitle-select", "subtitle_lang", "subtitle", withAuto: false),
                new Toggle
                {
                    Id = "live-autodetect",
                    BindPath = StatePath.Keys("set_auto_detect"),
                    Label = Bind.Lit("Wykryj język mówcy automatycznie"),
                    Size = ToggleSize.Sm,
                    Handlers = BackendKV(EventKind.Change, "set_config", "auto_detect"),
                },
            },
        };

        var mic = new Box
        {
            Id = "mic-stage",
            Direction = FlexDirection.Column,
            Align = FlexAlign.Center,
            Gap = Spacing.Sm,
            Style = new BoxStyle
            {
                Padding = AllEdges(Spacing.Md),
                Border = AllBorders(1, BorderColor.Default),
                Radius = AllCorners(RadiusToken.Lg),
                Background = BackgroundToken.Subtle,
            },
            // On a phone the mic is demoted below the subtitle stage.
            Responsive = new List<ResponsiveRule>
            {
                new ResponsiveRule { MaxWidth = Px(460), Order = 2 },
            },
            Children = new List<Component>
            {
                new AudioCapture
                {
                    Id = "mic-capture",
                    ActionId = "audio_utterance",
                    Mode = AudioCaptureMode.Vad,
                    RecordingPath = StatePath.Keys("recording"),
                    LanguageHint = "en",
                },
                new Text { Id = "pipeline-status", Content = Bind.State("pipeline"), Style = TextStyle.Caption, Tone = Tone.Muted, Align = TextAlign.Center, Wrap = TextWrap.Wrap },
            },
        };

        // The dominant surface: two fading prior translations, the source line
        // (uppercase accent), and the big current translation streaming in. Accent
        // surface + glow declared via BoxStyle. On a phone it fills more height and
        // sits directly under the language pair (order 1, above the mic).
        var stage = new Box
        {
            Id = "live-stage",
            Grow = true,
            Direction = FlexDirection.Column,
            Align = FlexAlign.Stretch,
            Justify = FlexJustify.End,
            Gap = Spacing.Sm,
            Style = new BoxStyle
            {
                Padding = AllEdges(Spacing.Lg),
                MinHeight = PxDim(240),
                // Dark panel surface with a subtle accent halo (glow + accent
                // border), not a saturated purple fill.
                Background = BackgroundToken.Subtle,
                Border = AllBorders(1, BorderColor.Accent),
                Radius = AllCorners(RadiusToken.Md),
                Shadow = ShadowToken.AccentGlow,
            },
            Responsive = new List<ResponsiveRule>
            {
                new ResponsiveRule { MaxWidth = Px(460), Order = 1, MinHeight = PxDim(420) },
            },
            Children = new List<Component>
            {
                new Text { Id = "live-prev2", Content = Bind.State("live_prev2"), Style = TextStyle.Body, Tone = Tone.Muted, Wrap = TextWrap.Wrap },
                new Text { Id = "live-prev1", Content = Bind.State("live_prev1"), Style = TextStyle.Body, Tone = Tone.Muted, Wrap = TextWrap.Wrap },
                new Text { Id = "live-orig", Content = Bind.State("live_orig"), Style = TextStyle.Overline, Tone = Tone.Primary, Wrap = TextWrap.Wrap },
                new Text { Id = "live-cur", Content = Bind.State("live_cur"), Style = TextStyle.Display, Wrap = TextWrap.Wrap, Streaming = Bind.State("translating") },
            },
        };

        var pipeline = new Box
        {
            Id = "live-pipeline",
            Direction = FlexDirection.Row,
            Align = FlexAlign.Center,
            Justify = FlexJustify.Center,
            Gap = Spacing.Xs,
            Responsive = new List<ResponsiveRule>
            {
                new ResponsiveRule { MaxWidth = Px(460), Order = 3 },
            },
            Children = new List<Component>
            {
                new Badge { Id = "pipe-mic", Variant = BadgeVariant.Soft, Tone = Tone.Neutral, Icon = new IconRefNamed { Name = IconName.Mic }, Label = Bind.Lit("Mikrofon") },
                new Text { Id = "pipe-a1", Content = Bind.Lit("→"), Style = TextStyle.Caption, Tone = Tone.Muted },
                new Badge { Id = "pipe-stt", Variant = BadgeVariant.Soft, Tone = Tone.Neutral, Label = Bind.Lit("Transkrypcja") },
                new Text { Id = "pipe-a2", Content = Bind.Lit("→"), Style = TextStyle.Caption, Tone = Tone.Muted },
                new Badge { Id = "pipe-model", Variant = BadgeVariant.Soft, Tone = Tone.Primary, Icon = new IconRefNamed { Name = IconName.Cpu }, Label = Bind.Lit(ModelLabel()) },
            },
        };

        // Width-capped, centered column (no margin-auto — center via the parent).
        var column = new Box
        {
            Id = "live-mode",
            Direction = FlexDirection.Column,
            Align = FlexAlign.Stretch,
            Gap = Spacing.Md,
            Style = new BoxStyle { MaxWidth = PxDim(720) },
            Children = new List<Component> { langCard, mic, stage, pipeline },
        };

        return new Stack
        {
            Id = "live-mode-center",
            Gap = Spacing.Md,
            Align = FlexAlign.Center,
            Children = new List<Component> { column },
        };
    }

    // One language row for the live reader: a caption label + a growing select,
    // so a "Słyszę: [Angielski]" / "Napisy: [Polski]" pair fits a narrow screen
    // without a fixed column width.
    private Component LiveLangRow(
        string id, string label, string selectId, string statePath, string draftKey, bool withAuto) =>
        new Box
        {
            Id = id,
            Direction = FlexDirection.Row,
            Align = FlexAlign.Center,
            Gap = Spacing.Sm,
            // Label over control once the row is too narrow for both side by side.
            Responsive = new List<ResponsiveRule>
            {
                new ResponsiveRule { MaxWidth = Px(460), Direction = FlexDirection.Column, Align = FlexAlign.Stretch },
            },
            Children = new List<Component>
            {
                new Text { Id = $"{id}-lbl", Content = Bind.Lit(label), Style = TextStyle.Caption, Tone = Tone.Muted },
                new Box
                {
                    Id = $"{id}-sel",
                    Grow = true,
                    Children = new List<Component> { LangSelect(selectId, statePath, draftKey, withAuto) },
                },
            },
        };

    private Component SettingsMode()
    {
        var available = AvailableModels();

        var saveBar = new Box
        {
            Id = "settings-save",
            Direction = FlexDirection.Row,
            Align = FlexAlign.Center,
            Justify = FlexJustify.SpaceBetween,
            Gap = Spacing.Md,
            Children = new List<Component>
            {
                new Text { Id = "save-note", Content = Bind.State("settings_saved"), Style = TextStyle.Caption, Tone = Tone.Success },
                new Button
                {
                    Id = "save-btn",
                    Variant = ButtonVariant.Primary,
                    Tone = Tone.Primary,
                    Label = Bind.Lit("Zapisz ustawienia"),
                    Size = ButtonSize.Md,
                    IconLeading = new IconRefNamed { Name = IconName.Check },
                    Handlers = Backend(EventKind.Click, "save_settings"),
                },
            },
        };

        var column = new Box
        {
            Id = "settings-mode",
            Direction = FlexDirection.Column,
            Align = FlexAlign.Stretch,
            Gap = Spacing.Md,
            Style = new BoxStyle { MaxWidth = PxDim(760) },
            Children = new List<Component>
            {
                SettingsCard("Modele", new List<Component>
                {
                    SettingRow("Model tłumaczenia (LLM)",
                        "Wybierz spośród modeli przydzielonych temu addonowi przez administratora.",
                        ModelSelect("model-llm", "set_llm_model", "llm_model", LlmModels(available),
                            stt: false)),
                    SettingRow("Model transkrypcji (STT)",
                        "Rozpoznawanie mowy dla trybu „Na żywo”. Puste = domyślny model hosta.",
                        ModelSelect("model-stt", "set_stt_model", "stt_model", SttModels(available),
                            stt: true)),
                }),
                SettingsCard("Języki", new List<Component>
                {
                    SettingRow("Domyślny język źródłowy",
                        "Używany przy starcie addonu w trybie tekstowym.",
                        LangSelectRaw("set-default-src", "set_default_src", "set_config", "default_src", withAuto: true)),
                    SettingRow("Domyślny język docelowy",
                        "Język, na który tłumaczymy domyślnie.",
                        LangSelectRaw("set-default-tgt", "set_default_tgt", "set_config", "default_tgt", withAuto: false)),
                    SettingRow("Automatyczne wykrywanie języka mowy",
                        "Model transkrypcji sam rozpoznaje język mówcy — nie trzeba wybierać przed sesją.",
                        new Toggle
                        {
                            Id = "set-auto-detect",
                            BindPath = StatePath.Keys("set_auto_detect"),
                            Size = ToggleSize.Md,
                            A11y = new Accessibility { Label = Bind.Lit("Automatyczne wykrywanie języka mowy") },
                            Handlers = BackendKV(EventKind.Change, "set_config", "auto_detect"),
                        }),
                }),
                SettingsCard("Napisy na żywo", new List<Component>
                {
                    SettingRow("Opóźnienie napisów (s)",
                        "Dłuższy bufor stabilizuje tekst (mniej korekt), krótszy — szybsze napisy.",
                        new Slider
                        {
                            Id = "set-delay",
                            BindPath = StatePath.Keys("set_subtitle_delay"),
                            Min = 0,
                            Max = 3,
                            Step = 0.1,
                            ShowValue = true,
                            Tone = Tone.Primary,
                            A11y = new Accessibility { Label = Bind.Lit("Opóźnienie napisów (s)") },
                            Handlers = BackendKV(EventKind.Change, "set_config", "subtitle_delay"),
                        }),
                    SettingRow("Zapisuj historię tłumaczeń",
                        "Sesje i tłumaczenia trafiają do lokalnej bazy addonu. Wyłączone = nic nie zostaje na dysku.",
                        new Toggle
                        {
                            Id = "set-history",
                            BindPath = StatePath.Keys("set_save_history"),
                            Size = ToggleSize.Md,
                            A11y = new Accessibility { Label = Bind.Lit("Zapisuj historię tłumaczeń") },
                            Handlers = BackendKV(EventKind.Change, "set_config", "save_history"),
                        }),
                }),
                saveBar,
            },
        };

        return new Stack
        {
            Id = "settings-mode-center",
            Gap = Spacing.Md,
            Align = FlexAlign.Center,
            Children = new List<Component> { column },
        };
    }

    // Dropdown of the granted models for a picker; an empty list yields a clear
    // note instead of a control. The stored value is ALWAYS the alias_id (for
    // both LLM and STT) so access stays permission-driven: the LLM host path
    // re-checks the alias gate, and the STT path re-resolves the alias to a
    // concrete model per call. The option LABEL shows the resolved model name.
    private Component ModelSelect(
        string id, string statePath, string key, List<AvailableAlias> models, bool stt)
    {
        if (models.Count == 0)
        {
            string msg = stt
                ? "Brak przydzielonych modeli transkrypcji — używany jest domyślny model hosta."
                : "Brak przydzielonych modeli — przydziel dostęp do modelu w uprawnieniach addonu.";
            return new Text
            {
                Id = $"{id}-empty",
                Content = Bind.Lit(msg),
                Style = TextStyle.Caption,
                Tone = Tone.Muted,
                Wrap = TextWrap.Wrap,
            };
        }

        var options = new List<SelectOption>
        {
            new SelectOption
            {
                Value = new SelectValueText { Value = "" },
                Label = Bind.Lit(stt ? "Model domyślny (host)" : "Wybierz model"),
            },
        };
        foreach (var a in models)
        {
            if (a.AliasId.Length == 0 || string.IsNullOrEmpty(a.TargetModel))
            {
                continue; // unresolved (pending) — nothing concrete to select
            }
            options.Add(new SelectOption
            {
                Value = new SelectValueText { Value = a.AliasId },
                Label = Bind.Lit(a.TargetModel!),
            });
        }
        return new Select
        {
            Id = id,
            BindPath = StatePath.Keys(statePath),
            Options = options,
            Size = InputSize.Md,
            A11y = new Accessibility { Label = Bind.Lit("Wybór modelu") },
            Handlers = BackendKV(EventKind.Change, "set_config", key),
        };
    }

    // -------------------------------------------------------------------------
    // UI builders
    // -------------------------------------------------------------------------

    // Equal text pane. `accent` marks the output pane, which carries the accent
    // surface + glow (declared via BoxStyle, not CSS). The header is an uppercase
    // accent Overline. On a narrow container (≤680px) the pane shrinks and its
    // parent restacks the two panes into a column.
    private static Component PanelBox(
        string id, string title, bool accent, Component? headerAction, params Component[] body)
    {
        var header = new Box
        {
            Id = $"{id}-head",
            Direction = FlexDirection.Row,
            Align = FlexAlign.Center,
            Justify = FlexJustify.SpaceBetween,
            Children = new List<Component>
            {
                new Text { Id = $"{id}-title", Content = Bind.Lit(title), Style = TextStyle.Overline, Tone = Tone.Primary },
                headerAction ?? new Text { Id = $"{id}-spacer", Content = Bind.Lit(""), Style = TextStyle.Caption },
            },
        };
        var children = new List<Component> { header };
        children.AddRange(body);
        return new Box
        {
            // Grow + MinWidth 0 makes the two panes share the row equally (flex:
            // 1 1 0) and shrink below their content, so a long text wraps inside
            // its half instead of pushing the pane wide and starving the other.
            Id = id,
            Grow = true,
            Direction = FlexDirection.Column,
            Gap = Spacing.Sm,
            Style = new BoxStyle
            {
                Padding = AllEdges(Spacing.Md),
                MinWidth = PxDim(0),
                MinHeight = PxDim(420),
                Border = AllBorders(1, accent ? BorderColor.Accent : BorderColor.Default),
                Radius = AllCorners(RadiusToken.Md),
                // Same dark surface as the source pane; the accent is only a halo
                // (glow + accent border), not a saturated fill.
                Background = BackgroundToken.Subtle,
                Shadow = accent ? ShadowToken.AccentGlow : (ShadowToken?)null,
            },
            Responsive = new List<ResponsiveRule>
            {
                new ResponsiveRule { MaxWidth = Px(680), MinHeight = PxDim(220) },
            },
            Children = children,
        };
    }

    private static Component SettingsCard(string title, List<Component> rows)
    {
        var children = new List<Component>
        {
            new Text { Id = $"card-{Slug(title)}", Content = Bind.Lit(title), Style = TextStyle.Overline, Tone = Tone.Primary },
        };
        children.AddRange(rows);
        return new Box
        {
            Id = $"settings-{Slug(title)}",
            Direction = FlexDirection.Column,
            Gap = Spacing.Sm,
            Style = new BoxStyle
            {
                Padding = AllEdges(Spacing.Md),
                Border = AllBorders(1, BorderColor.Default),
                Radius = AllCorners(RadiusToken.Lg),
                Background = BackgroundToken.Subtle,
            },
            Children = children,
        };
    }

    private static Component SettingRow(string label, string description, Component control)
    {
        var labelCol = new Box
        {
            Id = $"lblcol-{Slug(label)}",
            Grow = true,
            Direction = FlexDirection.Column,
            Gap = Spacing.Xxs,
            Children = new List<Component>
            {
                new Text { Id = $"lbl-{Slug(label)}", Content = Bind.Lit(label), Style = TextStyle.Body },
                new Text { Id = $"desc-{Slug(label)}", Content = Bind.Lit(description), Style = TextStyle.Caption, Tone = Tone.Muted, Wrap = TextWrap.Wrap },
            },
        };
        return new Box
        {
            Id = $"row-{Slug(label)}",
            Direction = FlexDirection.Row,
            Align = FlexAlign.Center,
            Justify = FlexJustify.SpaceBetween,
            Gap = Spacing.Md,
            // Stack label over control on a narrow panel.
            Responsive = new List<ResponsiveRule>
            {
                new ResponsiveRule { MaxWidth = Px(460), Direction = FlexDirection.Column, Align = FlexAlign.Stretch },
            },
            Children = new List<Component> { labelCol, control },
        };
    }

    // Language select whose change mirrors the picked code into the per-user
    // draft store (used by the active translation flow).
    private Component LangSelect(string id, string statePath, string draftKey, bool withAuto) =>
        LangSelectRaw(id, statePath, "set_draft", draftKey, withAuto);

    private Component LangSelectRaw(
        string id, string statePath, string actionId, string key, bool withAuto)
    {
        var options = new List<SelectOption>();
        IEnumerable<(string Code, string Display)> langs = withAuto
            ? SourceLanguages
            : TargetLanguages();
        foreach (var (code, display) in langs)
        {
            options.Add(new SelectOption
            {
                Value = new SelectValueText { Value = code },
                Label = Bind.Lit(display),
            });
        }
        return new Select
        {
            Id = id,
            BindPath = StatePath.Keys(statePath),
            Options = options,
            Size = InputSize.Md,
            A11y = new Accessibility { Label = Bind.Lit("Wybór języka") },
            Handlers = BackendKV(EventKind.Change, actionId, key),
        };
    }

    private static SegmentOption Segment(string value, string label) => new()
    {
        Value = new SelectValueText { Value = value },
        Label = Bind.Lit(label),
    };

    // -------------------------------------------------------------------------
    // State / patch plumbing
    // -------------------------------------------------------------------------

    private void AdoptEpoch(JsonElement pars)
    {
        if (pars.ValueKind == JsonValueKind.Object
            && pars.TryGetProperty("__panel_epoch", out var e)
            && e.TryGetUInt64(out ulong epoch)
            && epoch != _epoch)
        {
            _epoch = epoch;
            _revision = 0;
        }
    }

    private void SendPatch(params PatchOp[] ops)
    {
        var patch = new StatePatch
        {
            AddonId = AddonId,
            PanelId = PanelId,
            PanelEpoch = _epoch,
            BaseRevision = _revision,
            NewRevision = _revision + 1,
            Ops = new List<PatchOp>(ops),
        };
        try
        {
            Ui.Render(patch);
            _revision++;
        }
        catch (HostCallException e)
        {
            // Advancing locally on rejection would drift the counters apart; leave
            // the revision as-is so the next patch retries from the host's value.
            Log.Warn($"state patch rejected ({e.Code})");
        }
    }

    private static PatchOp SetOp(string path, string value) =>
        SetOp(path, TfValue.Text(value));

    private static PatchOp SetOp(string path, TfValue value) => new()
    {
        Path = StatePath.Keys(path),
        Op = new PatchOpKindSet { Value = value },
    };

    private static StateEntry Entry(string path, TfValue value) =>
        new(StatePath.Keys(path), value);

    private static HandlerMap Backend(EventKind kind, string actionId) =>
        new(kind, new HandlerBackend { ActionId = actionId, OnFailure = new FailurePolicyToast() });

    private static HandlerMap BackendKV(EventKind kind, string actionId, string key)
    {
        var pars = TfValue.Map(new List<KeyValuePair<TfValue, TfValue>>
        {
            new(TfValue.Text("key"), TfValue.Text(key)),
        });
        return new HandlerMap(kind, new HandlerBackend
        {
            ActionId = actionId,
            Params = pars,
            OnFailure = new FailurePolicyToast(),
        });
    }

    // -------------------------------------------------------------------------
    // BoxStyle helpers
    // -------------------------------------------------------------------------

    private static EdgeValues AllEdges(Spacing s)
    {
        var v = new SpaceValueToken { Value = s };
        return new EdgeValues { Top = v, Right = v, Bottom = v, Left = v };
    }

    private static CornerValues AllCorners(RadiusToken r)
    {
        var v = new RadiusValueToken { Value = r };
        return new CornerValues { TopLeft = v, TopRight = v, BottomRight = v, BottomLeft = v };
    }

    private static BorderEdges AllBorders(byte width, BorderColor color)
    {
        var side = new BorderSide { WidthPx = width, Color = color, Style = BorderLineStyle.Solid };
        return new BorderEdges { Top = side, Right = side, Bottom = side, Left = side };
    }

    // -------------------------------------------------------------------------
    // Responsive helpers (container-query reflow, declared by the addon)
    // -------------------------------------------------------------------------

    private static ContainerWidth Px(ushort px) => new ContainerWidthPx { Value = px };

    private static DimensionToken PxDim(ushort px) => new DimensionTokenPx { Value = px };

    // -------------------------------------------------------------------------
    // Small utilities
    // -------------------------------------------------------------------------

    private static long NowSeconds() =>
        DateTimeOffset.UtcNow.ToUnixTimeSeconds();

    private static string Slug(string s)
    {
        var sb = new StringBuilder(s.Length);
        foreach (char c in s.ToLowerInvariant())
        {
            sb.Append(char.IsLetterOrDigit(c) ? c : '-');
        }
        return sb.ToString();
    }

    // -------------------------------------------------------------------------
    // JSON helpers (params in / SQL params + rows)
    // -------------------------------------------------------------------------

    private static string? GetString(JsonElement pars, string name)
    {
        if (pars.ValueKind == JsonValueKind.Object && pars.TryGetProperty(name, out var v))
        {
            return v.ValueKind switch
            {
                JsonValueKind.String => v.GetString(),
                JsonValueKind.Number => v.ToString(),
                JsonValueKind.True => "true",
                JsonValueKind.False => "false",
                _ => null,
            };
        }
        return null;
    }

    private static uint? GetUInt(JsonElement pars, string name)
    {
        if (pars.ValueKind == JsonValueKind.Object
            && pars.TryGetProperty(name, out var v)
            && v.ValueKind == JsonValueKind.Number
            && v.TryGetUInt32(out uint n))
        {
            return n;
        }
        return null;
    }

    // The change-event detail carries "value" as string/bool/number; normalize to
    // the canonical string the addon persists ("true"/"false" for booleans).
    private static string GetValueAsString(JsonElement pars)
    {
        if (pars.ValueKind != JsonValueKind.Object || !pars.TryGetProperty("value", out var v))
        {
            return "";
        }
        return v.ValueKind switch
        {
            JsonValueKind.String => v.GetString() ?? "",
            JsonValueKind.Number => v.ToString(),
            JsonValueKind.True => "true",
            JsonValueKind.False => "false",
            _ => "",
        };
    }

    private static string JsonArray(params object[] values)
    {
        using var stream = new System.IO.MemoryStream();
        using (var jw = new Utf8JsonWriter(stream))
        {
            jw.WriteStartArray();
            foreach (var v in values)
            {
                switch (v)
                {
                    case string s: jw.WriteStringValue(s); break;
                    case long l: jw.WriteNumberValue(l); break;
                    case int i: jw.WriteNumberValue(i); break;
                    default: jw.WriteStringValue(v.ToString()); break;
                }
            }
            jw.WriteEndArray();
        }
        return Encoding.UTF8.GetString(stream.ToArray());
    }

    // -------------------------------------------------------------------------
    // Response envelopes
    // -------------------------------------------------------------------------

    private static string Ok() => "{\"ok\":true}";

    private static string Err(string message)
    {
        using var stream = new System.IO.MemoryStream();
        using (var jw = new Utf8JsonWriter(stream))
        {
            jw.WriteStartObject();
            jw.WriteBoolean("ok", false);
            jw.WriteString("error", message);
            jw.WriteEndObject();
        }
        return Encoding.UTF8.GetString(stream.ToArray());
    }
}
