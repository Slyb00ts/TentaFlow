// =============================================================================
// File: addons/notes/src/ui_dictate.rs
// Purpose: Dictation mode of the note editor (mockup n04). The toolbar mic
//          enters the mode; a floating dock (AudioCapture in the docked
//          variant + Stopwatch + recording chip + Pauza / "Zakończ i zapisz")
//          drives the flow. Each VAD utterance is transcribed through the
//          notes-stt alias; the newest segment stays as a state-bound partial
//          line, the previous one is committed into the note content (origin
//          flips to 'dictated') and re-enqueued for auto-graph analysis.
// =============================================================================

use serde_json::{json, Value as JsonValue};

use tentaflow_addon_sdk::ui_v1::{
    self as ui, backend, bound, lit, state_path, AudioCapture, AudioCaptureMode,
    AudioCaptureVariant, BindRef, BorderColor, BorderEdges, BorderSide, Button, ButtonSize,
    ButtonVariant, Chip, ChipVariant, Cluster, Component, CornerValues, Density, Divider,
    DividerOrientation, DividerVariant, DrawerSide, EventKind, FlexAlign, FlexDirection,
    FlexJustify, IconButton, IconName, PatchOp, PatchOpKind, RadiusValue, ShadowToken, Spacing,
    StateEntry, Stopwatch, StopwatchVariant, TextStyle, Tone, Tooltip, Value as CborValue,
};
use tentaflow_addon_sdk::{
    alias_get, document_delete, document_get, document_list, log, stt_transcribe,
    SttTranscribeOptions,
};

use crate::analysis;
use crate::db::{self, UserCtx};
use crate::ui::{
    backend_params, icon, load_session, send_state_patch, store_session, text_c,
    with_visible_bound,
};

/// Alias the dictation STT calls resolve through (owned by this addon).
pub(crate) const STT_ALIAS: &str = "notes-stt";

// Panel state paths of the dictation subtree.
const SP_D_ON: &str = "dictation.on";
const SP_D_ACTIVE: &str = "dictation.active";
const SP_D_RECORDING: &str = "dictation.recording";
const SP_D_PARTIAL: &str = "dictation.partial";
const SP_D_PARTIAL_VIS: &str = "dictation.partial_visible";
const SP_D_STATUS: &str = "dictation.status";
const SP_D_STARTED_AT: &str = "dictation.started_at";
const SP_D_CHIP: &str = "dictation.chip";
const SP_D_PAUSE_LABEL: &str = "dictation.pause_label";
/// Editor meta chip „dyktowana" (mockup n04) — bound so the first committed
/// segment can reveal it without re-rendering the editor slot.
pub(crate) const SP_ORIGIN_DICTATED: &str = "note.origin_dictated";

const CHIP_RECORDING: &str = "Nagrywanie";
const CHIP_PAUSED: &str = "Pauza";
const LABEL_PAUSE: &str = "Pauza";
const LABEL_RESUME: &str = "Wznów";

// =============================================================================
// Alias readiness
// =============================================================================

/// True when notes-stt is bound to a live target — gates the toolbar mic.
pub(crate) fn stt_ready() -> bool {
    resolve_stt_model().is_some()
}

/// Concrete model behind notes-stt. The host `stt` permission does NOT
/// resolve aliases, so the addon re-validates the LIVE binding on every call
/// and passes the resolved target (translator pattern).
fn resolve_stt_model() -> Option<String> {
    alias_get(STT_ALIAS)
        .ok()
        .filter(|info| info.is_active && !info.current_target.is_empty())
        .map(|info| info.current_target)
}

// =============================================================================
// Pure helpers — voice commands + segment merging (unit-tested natively)
// =============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DictationCommand {
    NewLine,
    NewParagraph,
    Finish,
}

/// Voice-command phrases, matched against the WHOLE normalized segment.
/// ASCII fallbacks cover STT outputs without Polish diacritics.
const COMMAND_PATTERNS: &[(&[&str], DictationCommand)] = &[
    (&["nowa", "linia"], DictationCommand::NewLine),
    (&["nowy", "akapit"], DictationCommand::NewParagraph),
    (&["zakończ", "dyktowanie"], DictationCommand::Finish),
    (&["zakoncz", "dyktowanie"], DictationCommand::Finish),
];

/// Word lowered with the edge punctuation stripped, so «nowa linia», "Nowa
/// linia." and "nowa linia," all match the same pattern.
fn normalized_word(word: &str) -> String {
    word.trim_matches(|c: char| !c.is_alphanumeric())
        .to_lowercase()
}

/// Recognizes a voice command. A command triggers ONLY when the utterance IS
/// the command phrase and nothing else (punctuation-tolerant) — the speaker
/// pauses and says just «nowa linia», so VAD delivers it as its own segment.
/// Legitimate dictated text containing the words ("produkt nazywa się Nowa
/// Linia") is never swallowed. Returns the segment text and the command;
/// a matched command always yields an empty text.
pub(crate) fn parse_dictation_segment(text: &str) -> (String, Option<DictationCommand>) {
    let words: Vec<String> = text
        .split_whitespace()
        .map(normalized_word)
        .filter(|w| !w.is_empty())
        .collect();
    for (pattern, command) in COMMAND_PATTERNS {
        if words.len() == pattern.len()
            && words.iter().zip(pattern.iter()).all(|(w, p)| w == p)
        {
            return (String::new(), Some(*command));
        }
    }
    (text.trim().to_string(), None)
}

/// Appends a committed segment to the note content: space-joined within a
/// line, verbatim after an explicit line break.
pub(crate) fn merge_segment(content: &str, segment: &str) -> String {
    if segment.is_empty() {
        return content.to_string();
    }
    if content.is_empty() {
        return segment.to_string();
    }
    if content.ends_with('\n') {
        format!("{content}{segment}")
    } else {
        format!("{content} {segment}")
    }
}

/// Separator appended after a committed segment for a line-break command.
pub(crate) fn command_separator(command: DictationCommand) -> &'static str {
    match command {
        DictationCommand::NewLine => "\n",
        DictationCommand::NewParagraph => "\n\n",
        DictationCommand::Finish => "",
    }
}

// =============================================================================
// Fragments
// =============================================================================

/// Toolbar mic (mockup n01): entry point of the dictation mode. Without a
/// bound notes-stt alias the button is disabled and wrapped in a tooltip
/// pointing the admin at the alias configuration.
pub(crate) fn toolbar_mic(note_id: &str, gen: i64) -> Component {
    let ready = stt_ready();
    let mut btn = IconButton {
        icon: icon(IconName::Mic),
        variant: ButtonVariant::Ghost,
        tone: Tone::Neutral,
        size: ButtonSize::Md,
        aria_label: "Dyktuj notatkę".into(),
        disabled: if ready {
            None
        } else {
            Some(BindRef::Literal(CborValue::Bool(true)))
        },
        loading: None,
    }
    .into_component("btn-dictate")
    .expect("IconButton encode");
    if ready {
        btn.handlers = Some(backend_params(
            EventKind::Click,
            "dictation_start",
            vec![
                ("note_id", CborValue::Text(note_id.to_string())),
                ("egen", CborValue::Text(gen.to_string())),
            ],
        ));
        return btn;
    }
    Tooltip {
        child: btn,
        content: lit("Skonfiguruj alias notes-stt"),
        side: DrawerSide::Top,
        max_width_px: 260,
    }
    .into_component("btn-dictate-tip")
    .expect("Tooltip encode")
}

/// Meta chip „dyktowana" (mockup n04) — visibility bound so the first commit
/// reveals it live.
pub(crate) fn dictated_chip() -> Component {
    with_visible_bound(
        Chip {
            variant: ChipVariant::Soft,
            tone: Tone::Primary,
            label: lit("dyktowana"),
            icon: Some(icon(IconName::Mic)),
            avatar: None,
            selected: None,
            removable: false,
            dot: None,
        }
        .into_component("chip-dictated")
        .expect("Chip encode"),
        SP_ORIGIN_DICTATED,
    )
}

/// Partial line under the note body (mockup n04): tag chip + the newest STT
/// hypothesis in a muted italic style, both fully state-driven.
pub(crate) fn partial_line() -> Component {
    let tag = Chip {
        variant: ChipVariant::Soft,
        tone: Tone::Neutral,
        label: lit("partial"),
        icon: None,
        avatar: None,
        selected: None,
        removable: false,
        dot: None,
    }
    .into_component("dict-partial-tag")
    .expect("Chip encode");
    let text = text_c(
        "dict-partial-text",
        bound(SP_D_PARTIAL),
        TextStyle::Quote,
        Some(Tone::Muted),
    );
    with_visible_bound(
        Cluster {
            gap: Spacing::Sm,
            align: FlexAlign::Center,
            justify: FlexJustify::Start,
            children: vec![tag, text],
            wrap: Some(false),
        }
        .into_component("dict-partial")
        .expect("Cluster encode"),
        SP_D_PARTIAL_VIS,
    )
}

/// The floating dictation dock (mockup n04): mic + waveform (AudioCapture in
/// the docked variant), ticking timer, recording chip, Pauza and the primary
/// "Zakończ i zapisz", with the voice-command hint and a status line below.
pub(crate) fn dock_area() -> Component {
    let capture = AudioCapture {
        action_id: "dictation_utterance".into(),
        mode: AudioCaptureMode::Vad,
        silence_ms: None,
        min_speech_ms: None,
        language_hint: None,
        recording_path: Some(state_path(SP_D_RECORDING)),
        disabled: None,
        active_path: Some(state_path(SP_D_ACTIVE)),
        variant: Some(AudioCaptureVariant::Docked),
    }
    .into_component("dict-capture")
    .expect("AudioCapture encode");

    let timer = Stopwatch {
        started_at_path: state_path(SP_D_STARTED_AT),
        variant: StopwatchVariant::Minutes,
        tone: Tone::Neutral,
    }
    .into_component("dict-timer")
    .expect("Stopwatch encode");

    let rec_chip = Chip {
        variant: ChipVariant::Soft,
        tone: Tone::Critical,
        label: bound(SP_D_CHIP),
        icon: None,
        avatar: None,
        selected: None,
        removable: false,
        dot: Some(Tone::Critical),
    }
    .into_component("dict-rec-chip")
    .expect("Chip encode");

    let sep = Divider {
        orientation: DividerOrientation::Vertical,
        variant: DividerVariant::Subtle,
        spacing: Spacing::Xs,
        label: None,
    }
    .into_component("dict-sep")
    .expect("Divider encode");

    let mut pause_btn = Button {
        variant: ButtonVariant::Ghost,
        tone: Tone::Neutral,
        label: bound(SP_D_PAUSE_LABEL),
        icon_leading: Some(icon(IconName::Pause)),
        icon_trailing: None,
        size: ButtonSize::Sm,
        full_width: false,
        disabled: None,
        loading: None,
        density: Density::Default,
    }
    .into_component("dict-pause")
    .expect("Button encode");
    pause_btn.handlers = Some(backend(EventKind::Click, "dictation_pause"));

    let mut finish_btn = Button {
        variant: ButtonVariant::Primary,
        tone: Tone::Primary,
        label: lit("Zakończ i zapisz"),
        icon_leading: Some(icon(IconName::Check)),
        icon_trailing: None,
        size: ButtonSize::Sm,
        full_width: false,
        disabled: None,
        loading: None,
        density: Density::Default,
    }
    .into_component("dict-finish")
    .expect("Button encode");
    finish_btn.handlers = Some(backend(EventKind::Click, "dictation_finish"));

    let dock = ui::Flex {
        direction: FlexDirection::Row,
        gap: Spacing::Sm,
        justify: FlexJustify::Center,
        align: FlexAlign::Center,
        wrap: ui::FlexWrap::NoWrap,
        children: vec![capture, timer, rec_chip, sep, pause_btn, finish_btn],
        padding: Some(Spacing::Sm),
        background: Some(ui::BackgroundToken::Subtle),
        radius: None,
        style: Some(ui::BoxStyle {
            border: Some(BorderEdges::all(BorderSide::new(1, BorderColor::Accent))),
            radius: Some(CornerValues::all(RadiusValue::Token {
                value: ui::RadiusToken::Xl,
            })),
            shadow: Some(ShadowToken::AccentGlow),
            ..Default::default()
        }),
        // Phone widths stack the dock controls (a wrapped flex line would
        // overflow the bordered box) — mockup n04's 600px rule equivalent.
        responsive: Some(vec![ui::ResponsiveRule {
            max_width: ui::ContainerWidth::Px(460),
            direction: Some(FlexDirection::Column),
            gap: Some(Spacing::Sm),
            align: None,
            justify: None,
            padding: None,
            min_height: None,
            order: None,
            hidden: None,
            width: None,
        }]),
    }
    .into_component("dict-dock")
    .expect("Flex encode");

    let hint = text_c(
        "dict-hint",
        lit("Zrób pauzę i powiedz: «nowa linia», «nowy akapit» albo «zakończ dyktowanie»"),
        TextStyle::Caption,
        Some(Tone::Muted),
    );
    let status = text_c(
        "dict-status",
        bound(SP_D_STATUS),
        TextStyle::Caption,
        Some(Tone::Critical),
    );

    with_visible_bound(
        ui::Box {
            width: None,
            grow: None,
            align_self: None,
            padding: Some(Spacing::Sm),
            margin: None,
            children: vec![dock, hint, status],
            style: None,
            direction: Some(FlexDirection::Column),
            gap: Some(Spacing::Sm),
            align: Some(FlexAlign::Center),
            justify: None,
            responsive: None,
        }
        .into_component("dict-dock-area")
        .expect("Box encode"),
        SP_D_ON,
    )
}

/// Initial shell entries of the dictation subtree (everything hidden/idle).
pub(crate) fn initial_dictation_state() -> Vec<StateEntry> {
    let set = |path: &str, value: CborValue| StateEntry {
        path: state_path(path),
        value,
    };
    vec![
        set(SP_D_ON, CborValue::Bool(false)),
        set(SP_D_ACTIVE, CborValue::Bool(false)),
        set(SP_D_RECORDING, CborValue::Bool(false)),
        set(SP_D_PARTIAL, CborValue::Text(String::new())),
        set(SP_D_PARTIAL_VIS, CborValue::Bool(false)),
        set(SP_D_STATUS, CborValue::Text(String::new())),
        set(SP_D_STARTED_AT, CborValue::Null),
        set(SP_D_CHIP, CborValue::Text(CHIP_RECORDING.into())),
        set(SP_D_PAUSE_LABEL, CborValue::Text(LABEL_PAUSE.into())),
        set(SP_ORIGIN_DICTATED, CborValue::Bool(false)),
    ]
}

/// Editor-slot overlay entries: dictation subtree reset + the origin chip of
/// the freshly opened note. Every editor (re)render leaves dictation OFF.
pub(crate) fn editor_overlay_entries(origin_dictated: bool) -> Vec<StateEntry> {
    let mut entries = initial_dictation_state();
    if origin_dictated {
        entries.pop();
        entries.push(StateEntry {
            path: state_path(SP_ORIGIN_DICTATED),
            value: CborValue::Bool(true),
        });
    }
    entries
}

// =============================================================================
// Actions
// =============================================================================

fn set_op(path: &str, value: CborValue) -> PatchOp {
    PatchOp {
        path: state_path(path),
        op: PatchOpKind::Set { value },
    }
}

fn patch_status(message: &str) {
    send_state_patch(vec![set_op(
        SP_D_STATUS,
        CborValue::Text(message.to_string()),
    )]);
}

fn now_ms() -> u64 {
    db::now_unix_ms() as u64
}

/// Retention sweep at dictation entry: drops the caller's leftover microphone
/// WAVs (a crashed/interrupted session may have uploaded a recording whose
/// transcription never ran). Best effort — the host document store hides other
/// users' captures from list/delete, so only own recordings are touched.
fn sweep_own_capture_docs() {
    let docs = match document_list() {
        Ok(d) => d,
        Err(e) => {
            log::warn(&format!("notes: capture cleanup list failed: {e:?}"));
            return;
        }
    };
    for doc in docs {
        if doc.source.as_deref() != Some("audio_capture") {
            continue;
        }
        if let Err(e) = document_delete(&doc.doc_id) {
            log::warn(&format!(
                "notes: orphaned capture delete failed for '{}': {e:?}",
                doc.doc_id
            ));
        }
    }
}

/// Enters dictation mode for the active (writable) note.
pub(crate) fn action_dictation_start(ctx: &UserCtx, params: &JsonValue) -> JsonValue {
    let note_id = match params.get("note_id").and_then(|v| v.as_str()) {
        Some(id) if !id.is_empty() => id.to_string(),
        _ => return json!({"ok": false, "error": "Brak note_id"}),
    };
    if resolve_stt_model().is_none() {
        return json!({"ok": false, "error": "Skonfiguruj alias notes-stt"});
    }
    match db::get_note(ctx, &note_id) {
        Ok(Some(n)) if n.can_write => {}
        Ok(Some(_)) => return json!({"ok": false, "error": "Brak uprawnień do edycji tej notatki."}),
        Ok(None) => return json!({"ok": false, "error": "Notatka nie istnieje."}),
        Err(e) => return json!({"ok": false, "error": e}),
    }
    let mut sess = load_session();
    if sess.dictating && sess.active == note_id {
        return json!({"ok": true});
    }
    sweep_own_capture_docs();
    sess.active = note_id;
    sess.dictating = true;
    sess.d_partial = String::new();
    sess.d_paused = false;
    sess.d_started_ms = now_ms() as i64;
    sess.d_elapsed_ms = 0;
    sess.d_seq = -1;
    store_session(&sess);
    // F64, not U64: a CBOR u64 decodes to a BigInt in the browser and the
    // Stopwatch renderer treats non-number values as "no data" ("—").
    let started = sess.d_started_ms as f64;
    send_state_patch(vec![
        set_op(SP_D_ON, CborValue::Bool(true)),
        set_op(SP_D_ACTIVE, CborValue::Bool(true)),
        set_op(SP_D_PARTIAL, CborValue::Text(String::new())),
        set_op(SP_D_PARTIAL_VIS, CborValue::Bool(false)),
        set_op(SP_D_STATUS, CborValue::Text(String::new())),
        set_op(SP_D_STARTED_AT, CborValue::F64(started)),
        set_op(SP_D_CHIP, CborValue::Text(CHIP_RECORDING.into())),
        set_op(SP_D_PAUSE_LABEL, CborValue::Text(LABEL_PAUSE.into())),
    ]);
    json!({"ok": true})
}

/// Pauza / Wznów: flips the AudioCapture active_path and freezes/resumes the
/// stopwatch by re-basing started_at on the accumulated elapsed time.
pub(crate) fn action_dictation_pause(_ctx: &UserCtx) -> JsonValue {
    let mut sess = load_session();
    if !sess.dictating {
        return json!({"ok": true});
    }
    if sess.d_paused {
        sess.d_paused = false;
        sess.d_started_ms = now_ms() as i64;
        store_session(&sess);
        let rebased = (now_ms() as i64 - sess.d_elapsed_ms).max(0) as f64;
        send_state_patch(vec![
            set_op(SP_D_ACTIVE, CborValue::Bool(true)),
            set_op(SP_D_CHIP, CborValue::Text(CHIP_RECORDING.into())),
            set_op(SP_D_PAUSE_LABEL, CborValue::Text(LABEL_PAUSE.into())),
            set_op(SP_D_STARTED_AT, CborValue::F64(rebased)),
        ]);
    } else {
        sess.d_paused = true;
        sess.d_elapsed_ms += (now_ms() as i64 - sess.d_started_ms).max(0);
        store_session(&sess);
        send_state_patch(vec![
            set_op(SP_D_ACTIVE, CborValue::Bool(false)),
            set_op(SP_D_CHIP, CborValue::Text(CHIP_PAUSED.into())),
            set_op(SP_D_PAUSE_LABEL, CborValue::Text(LABEL_RESUME.into())),
            set_op(SP_D_STARTED_AT, CborValue::Null),
        ]);
    }
    json!({"ok": true})
}

/// One VAD utterance: read the WAV back, transcribe through notes-stt, commit
/// the PREVIOUS partial into the note and keep the new segment as the partial.
/// Trailing voice commands act on the freshly transcribed segment.
pub(crate) fn action_dictation_utterance(ctx: &UserCtx, params: &JsonValue) -> JsonValue {
    let sess = load_session();
    if !sess.dictating || sess.d_paused {
        // A flush racing the finish/pause click — drop silently.
        return json!({"ok": true, "dropped": true});
    }
    // Out-of-order / duplicate delivery (renderer seq is monotonic per mount).
    let seq = params.get("seq").and_then(|v| v.as_i64());
    if let Some(seq) = seq {
        if seq <= sess.d_seq {
            log::info(&format!(
                "notes: dropped out-of-order utterance seq={seq} (last applied {})",
                sess.d_seq
            ));
            return json!({"ok": true, "dropped": true});
        }
    }
    let doc_ref = match params.get("doc_ref").and_then(|v| v.as_str()) {
        Some(r) if !r.is_empty() => r.to_string(),
        _ => return json!({"ok": false, "error": "missing doc_ref"}),
    };
    let mime = params
        .get("mime")
        .and_then(|v| v.as_str())
        .unwrap_or("audio/wav")
        .to_string();
    let sample_rate = params
        .get("sample_rate")
        .and_then(|v| v.as_u64())
        .map(|v| v as u32);

    let Some(model) = resolve_stt_model() else {
        patch_status("Alias notes-stt jest odpięty — dyktowanie wstrzymane.");
        return json!({"ok": false, "error": "notes-stt unavailable"});
    };
    let (audio, doc_mime) = match document_get(&doc_ref) {
        Ok(v) => v,
        Err(e) => {
            patch_status("Nie udało się odczytać nagrania — spróbuj ponownie.");
            return json!({"ok": false, "error": format!("document_get: {e:?}")});
        }
    };
    let effective_mime = if doc_mime.is_empty() { mime } else { doc_mime };
    let stt = match stt_transcribe(
        &audio,
        &effective_mime,
        &SttTranscribeOptions {
            sample_rate,
            model: Some(model),
            language: None,
            prompt: None,
        },
    ) {
        Ok(t) => t,
        Err(e) => {
            // The already-dictated partial stays intact on screen and in the
            // session — only this segment is lost.
            patch_status("Błąd transkrypcji — powtórz ostatnie zdanie.");
            return json!({"ok": false, "error": format!("stt_transcribe: {e:?}")});
        }
    };
    // One-shot recording: drop the stored WAV once transcribed. A failure is
    // a retention problem (the voice recording lingers) — surface it in logs;
    // the entry sweep of the next dictation cleans stragglers up.
    if let Err(e) = document_delete(&doc_ref) {
        log::warn(&format!(
            "notes: capture delete failed for '{doc_ref}' after transcription: {e:?}"
        ));
    }

    let text = stt.text.trim().to_string();
    if text.is_empty() {
        return json!({"ok": true});
    }
    let (clean, command) = parse_dictation_segment(&text);
    apply_segment(ctx, &clean, command, seq)
}

/// Commits segments according to the parsed command and updates the panel.
fn apply_segment(
    ctx: &UserCtx,
    clean: &str,
    command: Option<DictationCommand>,
    seq: Option<i64>,
) -> JsonValue {
    let mut sess = load_session();
    // Re-check after the (slow) STT call: a later utterance may have finished
    // first — committing this one now would interleave stale speech.
    if let Some(seq) = seq {
        if seq <= sess.d_seq {
            log::info(&format!(
                "notes: dropped superseded utterance seq={seq} (last applied {})",
                sess.d_seq
            ));
            return json!({"ok": true, "dropped": true});
        }
        sess.d_seq = seq;
    }
    let note_id = sess.active.clone();
    let note = match db::get_note(ctx, &note_id) {
        Ok(Some(n)) if n.can_write => n,
        Ok(_) => {
            patch_status("Notatka nie jest już dostępna do edycji.");
            return json!({"ok": false, "error": "note not writable"});
        }
        Err(e) => {
            patch_status("Błąd odczytu notatki.");
            return json!({"ok": false, "error": e});
        }
    };

    let mut content = note.content.clone();
    let mut partial = sess.d_partial.clone();
    match command {
        None => {
            content = merge_segment(&content, &partial);
            partial = clean.to_string();
        }
        Some(DictationCommand::Finish) => {
            content = merge_segment(&content, &partial);
            content = merge_segment(&content, clean);
            partial = String::new();
        }
        Some(cmd) => {
            content = merge_segment(&content, &partial);
            content = merge_segment(&content, clean);
            if !content.is_empty() {
                content.push_str(command_separator(cmd));
            }
            partial = String::new();
        }
    }

    let mut ops: Vec<PatchOp> = Vec::new();
    if content != note.content {
        if let Err(e) = db::commit_dictated_content(ctx, &note_id, &content) {
            patch_status(&e);
            return json!({"ok": false, "error": e});
        }
        // Fresh committed text deserves fresh analysis; the 3 s debounce
        // batches consecutive segments, so entities show up mid-dictation
        // without a per-utterance embed+extract cost.
        analysis::enqueue(&note_id);
        ops.push(set_op(
            crate::ui::SP_CONTENT,
            CborValue::Text(content.clone()),
        ));
        ops.push(set_op(
            crate::ui::SP_CHAR_COUNT,
            CborValue::Text(db::counter_label(&content)),
        ));
        ops.push(set_op(SP_ORIGIN_DICTATED, CborValue::Bool(true)));
    }
    sess.d_partial = partial.clone();
    let finishing = command == Some(DictationCommand::Finish);
    if finishing {
        sess.dictating = false;
        sess.d_paused = false;
    }
    store_session(&sess);

    ops.push(set_op(SP_D_PARTIAL, CborValue::Text(partial.clone())));
    ops.push(set_op(SP_D_PARTIAL_VIS, CborValue::Bool(!partial.is_empty())));
    ops.push(set_op(SP_D_STATUS, CborValue::Text(String::new())));
    if finishing {
        ops.push(set_op(SP_D_ON, CborValue::Bool(false)));
        ops.push(set_op(SP_D_ACTIVE, CborValue::Bool(false)));
        ops.push(set_op(SP_D_STARTED_AT, CborValue::Null));
    }
    send_state_patch(ops);
    if finishing {
        // The list card preview/updated-at changed; the editor slot is left
        // untouched so the committed text stays exactly where the user sees it.
        crate::ui::send_list(ctx);
    }
    json!({"ok": true})
}

/// "Zakończ i zapisz": commits the remaining partial and leaves the mode.
pub(crate) fn action_dictation_finish(ctx: &UserCtx) -> JsonValue {
    let sess = load_session();
    if !sess.dictating {
        return json!({"ok": true});
    }
    let result = apply_segment(ctx, "", Some(DictationCommand::Finish), None);
    if result.get("ok").and_then(|v| v.as_bool()) != Some(true) {
        // The commit failed (e.g. write access revoked) — still leave the
        // mode instead of trapping the user in a broken dock.
        let mut sess = load_session();
        sess.dictating = false;
        sess.d_paused = false;
        store_session(&sess);
        send_state_patch(vec![
            set_op(SP_D_ON, CborValue::Bool(false)),
            set_op(SP_D_ACTIVE, CborValue::Bool(false)),
            set_op(SP_D_STARTED_AT, CborValue::Null),
        ]);
        log::warn("notes: dictation finish commit failed; mode exited");
    }
    result
}

// =============================================================================
// Tests — pure helpers (native target)
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_segment_has_no_command() {
        let (clean, cmd) = parse_dictation_segment("To jest zwykłe zdanie.");
        assert_eq!(clean, "To jest zwykłe zdanie.");
        assert_eq!(cmd, None);
    }

    #[test]
    fn standalone_command_is_detected_case_and_punctuation_insensitively() {
        for input in ["nowa linia", "Nowa Linia", "Nowa linia.", "NOWA LINIA!", " nowa linia, "] {
            let (clean, cmd) = parse_dictation_segment(input);
            assert_eq!(cmd, Some(DictationCommand::NewLine), "input: {input}");
            assert_eq!(clean, "", "input: {input}");
        }
        let (clean, cmd) = parse_dictation_segment("Nowy akapit.");
        assert_eq!(cmd, Some(DictationCommand::NewParagraph));
        assert_eq!(clean, "");
    }

    #[test]
    fn finish_command_matches_with_and_without_diacritics() {
        for input in ["zakończ dyktowanie", "Zakoncz dyktowanie.", "ZAKOŃCZ DYKTOWANIE"] {
            let (clean, cmd) = parse_dictation_segment(input);
            assert_eq!(cmd, Some(DictationCommand::Finish), "input: {input}");
            assert_eq!(clean, "", "input: {input}");
        }
    }

    #[test]
    fn command_words_inside_a_sentence_never_trigger() {
        for input in [
            // Command words embedded in legitimate dictated text.
            "nowa linia produkcyjna ruszyła",
            "produkt nazywa się Nowa Linia",
            "Pierwszy punkt nowa linia",
            "To jest koniec. Nowy akapit.",
            "Ostatnie zdanie zakończ dyktowanie",
        ] {
            let (clean, cmd) = parse_dictation_segment(input);
            assert_eq!(cmd, None, "input: {input}");
            assert_eq!(clean, input.trim(), "input: {input}");
        }
    }

    #[test]
    fn merge_segment_joins_with_space_within_a_line() {
        assert_eq!(merge_segment("", "abc"), "abc");
        assert_eq!(merge_segment("abc", ""), "abc");
        assert_eq!(merge_segment("abc", "def"), "abc def");
    }

    #[test]
    fn merge_segment_respects_explicit_line_breaks() {
        assert_eq!(merge_segment("abc\n", "def"), "abc\ndef");
        assert_eq!(merge_segment("abc\n\n", "def"), "abc\n\ndef");
    }

    #[test]
    fn command_separators_map_to_line_breaks() {
        assert_eq!(command_separator(DictationCommand::NewLine), "\n");
        assert_eq!(command_separator(DictationCommand::NewParagraph), "\n\n");
        assert_eq!(command_separator(DictationCommand::Finish), "");
    }

    #[test]
    fn partial_then_commit_flow_builds_expected_content() {
        // Utterance 1 becomes the partial; utterance 2 commits it and takes
        // its place; a standalone new-paragraph command commits everything
        // with a break; the next utterance starts on the fresh paragraph.
        let mut content = String::from("Wstęp.");
        let mut partial = String::new();

        let (clean, cmd) = parse_dictation_segment("Pierwsze zdanie.");
        assert_eq!(cmd, None);
        content = merge_segment(&content, &partial);
        partial = clean;
        assert_eq!(content, "Wstęp.");
        assert_eq!(partial, "Pierwsze zdanie.");

        let (clean, cmd) = parse_dictation_segment("nowy akapit");
        assert_eq!(cmd, Some(DictationCommand::NewParagraph));
        content = merge_segment(&content, &partial);
        content = merge_segment(&content, &clean);
        content.push_str(command_separator(cmd.unwrap()));
        partial = String::new();
        assert_eq!(content, "Wstęp. Pierwsze zdanie.\n\n");
        assert!(partial.is_empty());

        let (clean, cmd) = parse_dictation_segment("Drugie zdanie.");
        assert_eq!(cmd, None);
        content = merge_segment(&content, &partial);
        partial = clean;
        assert_eq!(content, "Wstęp. Pierwsze zdanie.\n\n");
        assert_eq!(partial, "Drugie zdanie.");
        assert_eq!(merge_segment(&content, &partial), "Wstęp. Pierwsze zdanie.\n\nDrugie zdanie.");
    }
}
