// =============================================================================
// File: addons-pro/tentavision/src/lib.rs
// TentaVision addon — video surveillance with 14 panels, CBOR SDK.
// =============================================================================

#![allow(clippy::too_many_lines, clippy::collapsible_else_if, dead_code)]

#[used]
static BUILD_TS: &str = "20260526-1210";

extern crate alloc;

mod db;

use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use serde_json::{self, json, Value as JsonValue};
use tentaflow_sdk_spec::{
    Component, SlotContent, SlotDecl, PanelShell, UiPayload,
    SlotDefault, SlotSemantics, CachePolicy, SlotVisibility, StateEntry,
    StatePatch, HandlerMap, Handler, FailurePolicy,
    Value, PathSegment, StatePath, PatchOp, PatchOpKind,
};
use tentaflow_sdk_spec::protocol::control::CborMap;
use tentaflow_sdk_spec::protocol::camera::{
    CameraAddInput, CameraAddOutput, CameraDiscoverOut, CameraIdInput, CameraInfoOut,
    CameraListOut, CameraRemoveOut, CameraTestConnectionInput, CameraTestConnectionOut,
    DiscoveredCameraOut, LocalCameraDeviceOut, LocalCameraDevicesOut,
};
use tentaflow_sdk_spec::protocol::ui::{
    bind::BindRef,
    a11y::Accessibility,
    layout::{Stack, Flex, Grid, Card, SectionCard, Divider},
    layout::nav::NavTabs as NavTabsStruct,
    data::{Text as TextComp, Heading as HeadingComp, Badge as BadgeComp, Chip as ChipComp,
           KeyValue as KvComp, StatCard as StatCardComp, Avatar as AvatarComp,
           Sparkline as SparklineComp, Heatmap as HeatmapComp,
           ProgressBar as ProgressBarComp},
    data::tables::Table as TableComp,
    actions::{Button as ButtonComp, IconButton as IconButtonComp, Link as LinkComp,
              FilterChips as FilterChipsComp},
    feedback::{Alert as AlertComp, Spinner as SpinnerComp, GateScreen as GateScreenComp},
    feedback::overlays::Modal as ModalComp,
    molecules::EmptyState as EmptyStateComp,
    specialized::{VideoStream as VideoStreamComp, StepProgress as StepProgressComp},
    tokens::*,
    inline::*,
    icon_name::IconName,
};

// =============================================================================
// Host function imports
// =============================================================================

#[link(wasm_import_module = "tentaflow")]
extern "C" {
    fn ui_render_cbor(cbor_ptr: i32, cbor_len: i32) -> i32;
    fn store_get(key_ptr: i32, key_len: i32, out_ptr: i32, out_cap: i32) -> i32;
    fn store_set(key_ptr: i32, key_len: i32, val_ptr: i32, val_len: i32) -> i32;
    fn event_publish(
        event_type_ptr: i32, event_type_len: i32,
        payload_ptr: i32, payload_len: i32,
    ) -> i32;
    fn ui_notify(
        title_ptr: i32, title_len: i32,
        body_ptr: i32, body_len: i32,
        level_ptr: i32, level_len: i32,
    ) -> i32;
    fn log_info(msg_ptr: i32, msg_len: i32) -> i32;
    fn log_warn(msg_ptr: i32, msg_len: i32) -> i32;
    fn log_error(msg_ptr: i32, msg_len: i32) -> i32;
    fn camera_list_v1(out_ptr: i32, out_cap: i32, out_len_ptr: i32) -> i32;
    fn camera_add_v1(
        input_ptr: i32, input_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;
    fn camera_remove_v1(
        input_ptr: i32, input_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;
    fn camera_discover_v1(out_ptr: i32, out_cap: i32, out_len_ptr: i32) -> i32;
    fn camera_local_devices_v1(out_ptr: i32, out_cap: i32, out_len_ptr: i32) -> i32;
    fn camera_test_connection_v1(
        input_ptr: i32, input_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;
}

// =============================================================================
// Host function wrappers
// =============================================================================

mod log {
    use super::*;
    pub fn info(msg: &str) {
        unsafe { log_info(msg.as_ptr() as i32, msg.len() as i32); }
    }
    pub fn warn(msg: &str) {
        unsafe { log_warn(msg.as_ptr() as i32, msg.len() as i32); }
    }
    pub fn error(msg: &str) {
        unsafe { log_error(msg.as_ptr() as i32, msg.len() as i32); }
    }
}

fn notify(title: &str, body: &str) {
    let level = "info";
    unsafe {
        ui_notify(
            title.as_ptr() as i32, title.len() as i32,
            body.as_ptr() as i32, body.len() as i32,
            level.as_ptr() as i32, level.len() as i32,
        );
    }
}

// =============================================================================
// Camera ABI wrappers
// =============================================================================

/// Canonical ABI error codes returned by the camera host functions. Values
/// match `tentaflow_core::addon::errors::AbiError` (positive 1..24; `0` = Ok).
/// The host returns these directly as the i32 host-function result, so an addon
/// must NOT negate the return value when classifying an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
enum AbiError {
    Permission = 1,
    NotFound = 2,
    NoAvailableTarget = 3,
    Timeout = 4,
    Operation = 5,
    OutputBufferTooSmall = 6,
    Conflict = 7,
    QuotaExceeded = 11,
    CameraUnreachable = 12,
    CameraAuthFailed = 13,
    CameraVendorUnsupported = 14,
    PayloadTooLarge = 21,
    Unknown = 99,
}

impl AbiError {
    fn from_code(code: i32) -> Self {
        match code {
            1 => Self::Permission,
            2 => Self::NotFound,
            3 => Self::NoAvailableTarget,
            4 => Self::Timeout,
            5 => Self::Operation,
            6 => Self::OutputBufferTooSmall,
            7 => Self::Conflict,
            11 => Self::QuotaExceeded,
            12 => Self::CameraUnreachable,
            13 => Self::CameraAuthFailed,
            14 => Self::CameraVendorUnsupported,
            21 => Self::PayloadTooLarge,
            _ => Self::Unknown,
        }
    }
}

impl core::fmt::Display for AbiError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "AbiError({})", *self as i32)
    }
}

/// Encodes `input` to CBOR and decodes the CBOR response of a host function
/// with the standard `(input_ptr, input_len, out_ptr, out_cap, out_len_ptr)`
/// ABI shape. On `OutputBufferTooSmall` the host writes the required size into
/// `out_len_ptr`; we grow once and retry so a large response is not lost.
fn call_cbor_in_out<I, O>(
    input: &I,
    host_fn: unsafe extern "C" fn(i32, i32, i32, i32, i32) -> i32,
) -> Result<O, AbiError>
where
    I: minicbor::Encode<()>,
    O: for<'b> minicbor::Decode<'b, ()>,
{
    let mut input_bytes = Vec::new();
    minicbor::encode(input, &mut input_bytes).map_err(|_| AbiError::Operation)?;
    let mut cap = 16384usize;
    loop {
        let mut out = vec![0u8; cap];
        let mut out_len: i32 = 0;
        let ret = unsafe {
            host_fn(
                input_bytes.as_ptr() as i32,
                input_bytes.len() as i32,
                out.as_mut_ptr() as i32,
                out.len() as i32,
                &mut out_len as *mut i32 as i32,
            )
        };
        if ret == AbiError::OutputBufferTooSmall as i32 {
            cap = (out_len as usize).max(cap.saturating_mul(2));
            continue;
        }
        if ret != 0 {
            return Err(AbiError::from_code(ret));
        }
        out.truncate(out_len as usize);
        return minicbor::decode(&out).map_err(|_| AbiError::Operation);
    }
}

/// Decodes the CBOR response of a host function with the read-only
/// `(out_ptr, out_cap, out_len_ptr)` ABI shape (`camera_list` / `camera_discover` /
/// `camera_local_devices`).
fn call_cbor_out<O>(
    host_fn: unsafe extern "C" fn(i32, i32, i32) -> i32,
) -> Result<O, AbiError>
where
    O: for<'b> minicbor::Decode<'b, ()>,
{
    let mut cap = 16384usize;
    loop {
        let mut out = vec![0u8; cap];
        let mut out_len: i32 = 0;
        let ret = unsafe {
            host_fn(
                out.as_mut_ptr() as i32,
                out.len() as i32,
                &mut out_len as *mut i32 as i32,
            )
        };
        if ret == AbiError::OutputBufferTooSmall as i32 {
            cap = (out_len as usize).max(cap.saturating_mul(2));
            continue;
        }
        if ret != 0 {
            return Err(AbiError::from_code(ret));
        }
        out.truncate(out_len as usize);
        return minicbor::decode(&out).map_err(|_| AbiError::Operation);
    }
}

fn camera_list() -> Result<Vec<CameraInfoOut>, AbiError> {
    let out: CameraListOut = call_cbor_out(camera_list_v1)?;
    Ok(out.camera)
}

fn camera_add(spec: CameraAddInput) -> Result<CameraAddOutput, AbiError> {
    call_cbor_in_out(&spec, camera_add_v1)
}

fn camera_remove(id: &str) -> Result<(), AbiError> {
    let input = CameraIdInput { camera_id: id.to_string() };
    let _: CameraRemoveOut = call_cbor_in_out(&input, camera_remove_v1)?;
    Ok(())
}

fn camera_discover() -> Result<Vec<DiscoveredCameraOut>, AbiError> {
    let out: CameraDiscoverOut = call_cbor_out(camera_discover_v1)?;
    Ok(out.discovered)
}

fn camera_local_devices() -> Result<Vec<LocalCameraDeviceOut>, AbiError> {
    let out: LocalCameraDevicesOut = call_cbor_out(camera_local_devices_v1)?;
    Ok(out.devices)
}

fn camera_test_connection(vendor: &str, url: &str) -> Result<CameraTestConnectionOut, AbiError> {
    let input = CameraTestConnectionInput { vendor: vendor.to_string(), url: url.to_string() };
    call_cbor_in_out(&input, camera_test_connection_v1)
}

// =============================================================================
// CBOR send helpers
// =============================================================================

const ADDON_ID: &str = "tentavision";
const PANEL_ID: &str = "overview";

static PANEL_EPOCH: AtomicU64 = AtomicU64::new(1);
static STATE_REVISION: AtomicU64 = AtomicU64::new(0);

/// Installs a process-wide panic hook exactly once. Without it a guest panic
/// surfaces on the host only as a bare wasm trap with numeric `fnNNN` frames,
/// which is unusable for diagnosis. The hook forwards the panic payload and its
/// `file:line` source location to the host log so the actual cause of any
/// future addon panic is recorded verbatim instead of being lost in the trap.
fn install_panic_hook() {
    use core::sync::atomic::AtomicBool;
    static INSTALLED: AtomicBool = AtomicBool::new(false);
    // `swap` makes installation atomic and idempotent across on_start /
    // on_panel_open and any later re-entry.
    if INSTALLED.swap(true, Ordering::Relaxed) {
        return;
    }
    std::panic::set_hook(alloc::boxed::Box::new(|info: &std::panic::PanicHookInfo| {
        let location = info
            .location()
            .map(|l| alloc::format!("{}:{}", l.file(), l.line()))
            .unwrap_or_else(|| "<unknown>".into());
        let message = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| (*s).to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "<non-string panic payload>".into());
        log::error(&alloc::format!(
            "TentaVision PANIC at {}: {}",
            location, message
        ));
    }));
}

fn next_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    alloc::format!("c{}", n)
}

fn send_ui(payload: &UiPayload) -> i32 {
    let mut buf = Vec::with_capacity(4096);
    if minicbor::encode(payload, &mut buf).is_err() {
        log::error("TentaVision: CBOR encode failed");
        return -1;
    }
    let ret = unsafe { ui_render_cbor(buf.as_ptr() as i32, buf.len() as i32) };
    if ret < 0 {
        log::error("TentaVision: ui_render_cbor returned error");
    }
    ret
}

fn send_panel_shell(layout: Component, slots: Vec<SlotDecl>, initial_state: Vec<StateEntry>) {
    let epoch = PANEL_EPOCH.load(Ordering::Relaxed);
    let payload = UiPayload::PanelShell(PanelShell {
        addon_id: ADDON_ID.into(),
        panel_id: PANEL_ID.into(),
        panel_epoch: epoch,
        layout,
        slots,
        initial_state,
        initial_commands: vec![],
    });
    send_ui(&payload);
}

fn send_slot_content(slot_id: &str, fragment: Component) {
    send_slot_content_with_overlay(slot_id, fragment, None);
}

/// Like `send_slot_content` but seeds store keys via `state_overlay` before the
/// fragment renders. Used to seed the reactive wizard's initial state into the
/// store the moment the `add_camera_body` fragment is delivered, so bound
/// visibility flags resolve correctly on first paint.
fn send_slot_content_with_overlay(slot_id: &str, fragment: Component, overlay: Option<Vec<StateEntry>>) {
    let epoch = PANEL_EPOCH.load(Ordering::Relaxed);
    let payload = UiPayload::SlotContent(SlotContent {
        addon_id: ADDON_ID.into(),
        panel_id: PANEL_ID.into(),
        panel_epoch: epoch,
        slot_id: slot_id.into(),
        fragment,
        state_overlay: overlay,
    });
    send_ui(&payload);
}

fn send_state_patch(key: &str, value: Value) {
    send_state_patches(vec![(key.into(), value)]);
}

/// Applies several store keys in one atomic `StatePatch` (single revision bump).
/// The reactive wizard uses this so that, e.g., advancing a step toggles the
/// step visibility flags and footer-button flags together without the client
/// observing a half-applied intermediate state.
fn send_state_patches(pairs: Vec<(String, Value)>) {
    if pairs.is_empty() {
        return;
    }
    let base = STATE_REVISION.load(Ordering::Relaxed);
    let new_rev = base + 1;
    let epoch = PANEL_EPOCH.load(Ordering::Relaxed);
    let ops = pairs
        .into_iter()
        .map(|(key, value)| PatchOp {
            path: StatePath::new(vec![PathSegment::Key(key)]),
            op: PatchOpKind::Set { value },
        })
        .collect();
    let payload = UiPayload::StatePatch(StatePatch {
        addon_id: ADDON_ID.into(),
        panel_id: PANEL_ID.into(),
        panel_epoch: epoch,
        base_revision: base,
        new_revision: new_rev,
        ops,
    });
    // The host advances its expected revision only when it accepts the patch;
    // advancing locally on rejection would drift the counters apart forever.
    if send_ui(&payload) == 0 {
        STATE_REVISION.store(new_rev, Ordering::Relaxed);
    }
}

// =============================================================================
// Component construction helpers — typed structs from tentaflow-sdk-spec
// =============================================================================

fn lit(s: &str) -> BindRef {
    BindRef::Literal(Value::Text(s.into()))
}

/// A reactive `BindRef` pointing at a top-level store key. Lets text/alert
/// content track wizard state without re-sending the fragment.
fn bound(key: &str) -> BindRef {
    BindRef::Bound(StatePath::new(vec![PathSegment::Key(key.into())]))
}

/// Wraps a component so the renderer hides it whenever the bound boolean store
/// key is `false`. This is the core of the reactive wizard: every step and
/// per-type config block stays in the DOM and only toggles `hidden` as the
/// store changes, so interactions never rebuild the panel.
fn with_visible(mut component: Component, key: &str) -> Component {
    use tentaflow_sdk_spec::protocol::ui::a11y::Visibility;
    component.visibility = Some(Visibility {
        visible: Some(bound(key)),
        display_above_breakpoint: None,
        display_below_breakpoint: None,
        hidden_for_assistive: false,
    });
    component
}

fn with_a11y_label(mut component: Component, label: &str) -> Component {
    component.a11y = Some(Accessibility {
        label: Some(lit(label)),
        ..Default::default()
    });
    component
}

fn icon_named(name: IconName) -> IconRef {
    IconRef::Named { name, size: None, tone: None }
}

fn parse_tone(s: &str) -> Tone {
    match s {
        "primary" => Tone::Primary,
        "success" => Tone::Success,
        "warning" => Tone::Warning,
        "critical" => Tone::Critical,
        "info" => Tone::Info,
        "muted" => Tone::Muted,
        _ => Tone::Neutral,
    }
}

fn parse_spacing(s: &str) -> Spacing {
    match s {
        "zero" => Spacing::Zero,
        "xxs" => Spacing::Xxs,
        "xs" => Spacing::Xs,
        "sm" => Spacing::Sm,
        "lg" => Spacing::Lg,
        "xl" => Spacing::Xl,
        "xxl" => Spacing::Xxl,
        _ => Spacing::Md,
    }
}

fn parse_button_variant(s: &str) -> ButtonVariant {
    match s {
        "secondary" => ButtonVariant::Secondary,
        "tertiary" => ButtonVariant::Tertiary,
        "ghost" => ButtonVariant::Ghost,
        "destructive" => ButtonVariant::Destructive,
        "link" => ButtonVariant::Link,
        _ => ButtonVariant::Primary,
    }
}

fn parse_icon_name(s: &str) -> IconName {
    match s {
        "plus" => IconName::Plus,
        "search" => IconName::Search,
        "settings" => IconName::Settings,
        "bell" => IconName::Bell,
        "video" => IconName::Video,
        "cameras" => IconName::Cameras,
        "brain" => IconName::Brain,
        "cpu" => IconName::Cpu,
        "dashboard" => IconName::Dashboard,
        "users" => IconName::Users,
        "zones" => IconName::Zones,
        "audit" => IconName::Audit,
        "evidence" => IconName::Evidence,
        "link" => IconName::ExternalLink,
        "lock" => IconName::Lock,
        "info" => IconName::Info,
        "check" => IconName::Check,
        "clock" => IconName::Clock,
        "shield" => IconName::Shield,
        _ => IconName::Info,
    }
}

fn text(content: &str) -> Component {
    TextComp {
        content: lit(content),
        style: TextStyle::Body,
        tone: None,
        align: None,
        wrap: None,
        max_lines: None,
        format: None,
    }.into_component(next_id()).expect("Text")
}

fn text_styled(content: &str, style: &str) -> Component {
    let ts = match style {
        "body_strong" => TextStyle::BodyStrong,
        "caption" => TextStyle::Caption,
        "overline" => TextStyle::Overline,
        "title" => TextStyle::Title,
        "h1" => TextStyle::H1,
        "h2" => TextStyle::H2,
        "h3" => TextStyle::H3,
        "h4" => TextStyle::H4,
        "code" => TextStyle::Code,
        "mono" => TextStyle::Mono,
        _ => TextStyle::Body,
    };
    TextComp {
        content: lit(content),
        style: ts,
        tone: None,
        align: None,
        wrap: None,
        max_lines: None,
        format: None,
    }.into_component(next_id()).expect("Text")
}

/// Text whose content tracks a store key reactively (used for the live
/// connection-test outcome line in the wizard).
fn text_bound(key: &str) -> Component {
    TextComp {
        content: bound(key),
        style: TextStyle::Body,
        tone: None,
        align: None,
        wrap: None,
        max_lines: None,
        format: None,
    }.into_component(next_id()).expect("Text")
}

fn text_colored(content: &str, style: &str, color: &str) -> Component {
    let ts = match style {
        "body_strong" => TextStyle::BodyStrong,
        "caption" => TextStyle::Caption,
        _ => TextStyle::Body,
    };
    TextComp {
        content: lit(content),
        style: ts,
        tone: Some(parse_tone(color)),
        align: None,
        wrap: None,
        max_lines: None,
        format: None,
    }.into_component(next_id()).expect("Text")
}

fn heading(level: u8, content: &str) -> Component {
    HeadingComp {
        content: lit(content),
        level,
        tone: None,
        align: None,
    }.into_component(next_id()).expect("Heading")
}

fn badge(label: &str, variant: &str) -> Component {
    let bv = match variant {
        "danger" | "critical" => BadgeVariant::Solid,
        "warning" => BadgeVariant::Soft,
        "info" => BadgeVariant::Outline,
        _ => BadgeVariant::Soft,
    };
    let tone = match variant {
        "danger" | "critical" => Tone::Critical,
        "warning" => Tone::Warning,
        "info" => Tone::Info,
        "success" => Tone::Success,
        _ => Tone::Neutral,
    };
    BadgeComp {
        variant: bv,
        tone,
        label: lit(label),
        icon: None,
        count: None,
        max: 99,
        pulse: false,
    }.into_component(next_id()).expect("Badge")
}

fn chip(label: &str, _variant: &str) -> Component {
    ChipComp {
        variant: ChipVariant::Soft,
        tone: Tone::Neutral,
        label: lit(label),
        icon: None,
        avatar: None,
        selected: None,
        removable: false,
    }.into_component(next_id()).expect("Chip")
}

fn chip_with_icon(label: &str, _variant: &str, icon: &str) -> Component {
    ChipComp {
        variant: ChipVariant::Soft,
        tone: Tone::Neutral,
        label: lit(label),
        icon: Some(icon_named(parse_icon_name(icon))),
        avatar: None,
        selected: None,
        removable: false,
    }.into_component(next_id()).expect("Chip")
}

fn chip_toned(label: &str, tone_str: &str) -> Component {
    ChipComp {
        variant: ChipVariant::Soft,
        tone: parse_tone(tone_str),
        label: lit(label),
        icon: None,
        avatar: None,
        selected: None,
        removable: false,
    }.into_component(next_id()).expect("Chip")
}

fn chip_toned_icon(label: &str, tone_str: &str, icon: &str) -> Component {
    ChipComp {
        variant: ChipVariant::Soft,
        tone: parse_tone(tone_str),
        label: lit(label),
        icon: Some(icon_named(parse_icon_name(icon))),
        avatar: None,
        selected: None,
        removable: false,
    }.into_component(next_id()).expect("Chip")
}

fn stat_card(value: &str, label: &str, sublabel: Option<&str>, icon: Option<&str>, accent: Option<&str>) -> Component {
    let tone = accent.map(parse_tone).unwrap_or(Tone::Neutral);
    StatCardComp {
        label: lit(label),
        icon: icon.map(|i| icon_named(parse_icon_name(i))),
        value: lit(value),
        value_suffix: None,
        format: None,
        trend: None,
        footnote: sublabel.map(|s| Footnote {
            tone,
            icon: None,
            content: lit(s),
        }),
        accent: Some(tone),
        clickable: false,
    }.into_component(next_id()).expect("StatCard")
}

fn button(label: &str, action: &str, variant: &str) -> Component {
    let mut c = ButtonComp {
        variant: parse_button_variant(variant),
        tone: Tone::Neutral,
        label: lit(label),
        icon_leading: None,
        icon_trailing: None,
        size: ButtonSize::Md,
        full_width: false,
        disabled: None,
        loading: None,
        density: Density::Default,
    }.into_component(next_id()).expect("Button");
    c.handlers = Some(HandlerMap(vec![(
        tentaflow_sdk_spec::EventKind::Click,
        Handler::Backend {
            action_id: action.into(),
            params: CborMap::default(),
            optimistic: None,
            on_failure: FailurePolicy::Toast,
        },
    )]));
    c
}

fn button_with_icon(label: &str, action: &str, variant: &str, icon: &str) -> Component {
    let mut c = ButtonComp {
        variant: parse_button_variant(variant),
        tone: Tone::Neutral,
        label: lit(label),
        icon_leading: Some(icon_named(parse_icon_name(icon))),
        icon_trailing: None,
        size: ButtonSize::Md,
        full_width: false,
        disabled: None,
        loading: None,
        density: Density::Default,
    }.into_component(next_id()).expect("Button");
    c.handlers = Some(HandlerMap(vec![(
        tentaflow_sdk_spec::EventKind::Click,
        Handler::Backend {
            action_id: action.into(),
            params: CborMap::default(),
            optimistic: None,
            on_failure: FailurePolicy::Toast,
        },
    )]));
    c
}

fn button_with_params(label: &str, action: &str, variant: &str, params: CborMap) -> Component {
    let mut c = ButtonComp {
        variant: parse_button_variant(variant),
        tone: Tone::Neutral,
        label: lit(label),
        icon_leading: None,
        icon_trailing: None,
        size: ButtonSize::Md,
        full_width: false,
        disabled: None,
        loading: None,
        density: Density::Default,
    }.into_component(next_id()).expect("Button");
    c.handlers = Some(HandlerMap(vec![(
        tentaflow_sdk_spec::EventKind::Click,
        Handler::Backend {
            action_id: action.into(),
            params,
            optimistic: None,
            on_failure: FailurePolicy::Toast,
        },
    )]));
    c
}

fn icon_button(icon: &str, action: &str, variant: &str) -> Component {
    let mut c = IconButtonComp {
        icon: icon_named(parse_icon_name(icon)),
        variant: parse_button_variant(variant),
        tone: Tone::Neutral,
        size: ButtonSize::Md,
        aria_label: icon.into(),
        disabled: None,
        loading: None,
    }.into_component(next_id()).expect("IconButton");
    c.handlers = Some(HandlerMap(vec![(
        tentaflow_sdk_spec::EventKind::Click,
        Handler::Backend {
            action_id: action.into(),
            params: CborMap::default(),
            optimistic: None,
            on_failure: FailurePolicy::Toast,
        },
    )]));
    c
}

fn link(label: &str, panel_id: &str) -> Component {
    let mut c = LinkComp {
        label: lit(label),
        underline: LinkUnderline::Hover,
        tone: Tone::Primary,
        leading_icon: None,
        trailing_icon: None,
    }.into_component(next_id()).expect("Link");
    c.handlers = Some(HandlerMap(vec![(
        tentaflow_sdk_spec::EventKind::Click,
        Handler::Backend {
            action_id: panel_id.into(),
            params: CborMap::default(),
            optimistic: None,
            on_failure: FailurePolicy::Toast,
        },
    )]));
    c
}

fn card(title: Option<&str>, children: Vec<Component>) -> Component {
    if let Some(t) = title {
        SectionCard {
            title: lit(t),
            subtitle: None,
            header_actions: vec![],
            header_divider: false,
            body: children,
            footer: None,
            padding: Spacing::Lg,
            gap: Spacing::Md,
            variant: CardVariant::Outlined,
            radius: RadiusToken::Lg,
            shadow: ShadowToken::Subtle,
            border: BorderToken::Hairline,
            background: BackgroundToken::None,
            accent: None,
        }.into_component(next_id()).expect("SectionCard")
    } else {
        Card {
            variant: CardVariant::Outlined,
            padding: Spacing::Lg,
            gap: Spacing::Md,
            radius: RadiusToken::Lg,
            shadow: ShadowToken::None,
            border: BorderToken::Hairline,
            background: BackgroundToken::None,
            accent: None,
            children,
            interactive: false,
            clickable: false,
        }.into_component(next_id()).expect("Card")
    }
}

fn card_with_icon(title: &str, _icon: &str, children: Vec<Component>) -> Component {
    card_with_icon_action(title, _icon, None, children)
}

fn card_with_icon_action(title: &str, _icon: &str, action_label: Option<&str>, children: Vec<Component>) -> Component {
    let header_actions = match action_label {
        Some(label) => vec![ButtonComp {
            variant: ButtonVariant::Ghost,
            tone: Tone::Primary,
            label: lit(label),
            icon_leading: None,
            icon_trailing: None,
            size: ButtonSize::Sm,
            full_width: false,
            disabled: None,
            loading: None,
            density: Density::Default,
        }.into_component(next_id()).expect("Button")],
        None => vec![],
    };
    SectionCard {
        title: lit(title),
        subtitle: None,
        header_actions,
        header_divider: false,
        body: children,
        footer: None,
        padding: Spacing::Lg,
        gap: Spacing::Md,
        variant: CardVariant::Outlined,
        radius: RadiusToken::Lg,
        shadow: ShadowToken::Subtle,
        border: BorderToken::Hairline,
        background: BackgroundToken::None,
        accent: None,
    }.into_component(next_id()).expect("SectionCard")
}

fn stack_v(children: Vec<Component>) -> Component {
    Stack {
        gap: Spacing::Md,
        align: FlexAlign::Stretch,
        children,
        padding: None,
    }.into_component(next_id()).expect("Stack")
}

fn stack_h(children: Vec<Component>) -> Component {
    Flex {
        direction: FlexDirection::Row,
        gap: Spacing::Md,
        justify: FlexJustify::Start,
        align: FlexAlign::Center,
        wrap: FlexWrap::NoWrap,
        children,
        padding: None,
        background: None,
        radius: None,
    }.into_component(next_id()).expect("Flex")
}

fn stack_h_gap(gap: &str, children: Vec<Component>) -> Component {
    Flex {
        direction: FlexDirection::Row,
        gap: parse_spacing(gap),
        justify: FlexJustify::Start,
        align: FlexAlign::Center,
        wrap: FlexWrap::NoWrap,
        children,
        padding: None,
        background: None,
        radius: None,
    }.into_component(next_id()).expect("Flex")
}

fn stack_v_gap(gap: &str, children: Vec<Component>) -> Component {
    Stack {
        gap: parse_spacing(gap),
        align: FlexAlign::Stretch,
        children,
        padding: None,
    }.into_component(next_id()).expect("Stack")
}

fn grid(columns: u32, children: Vec<Component>) -> Component {
    let grid_children: Vec<GridChild> = children.into_iter().map(|c| GridChild {
        component: c,
        col_span: 1,
        row_span: 1,
        col_start: None,
        row_start: None,
        align_self: None,
        justify_self: None,
    }).collect();
    Grid {
        columns: GridTrack::Equal { count: columns as u8 },
        gap: Spacing::Md,
        row_gap: None,
        column_gap: None,
        children: grid_children,
        padding: None,
        align_items: None,
    }.into_component(next_id()).expect("Grid")
}

fn divider() -> Component {
    Divider {
        orientation: DividerOrientation::Horizontal,
        variant: DividerVariant::Default,
        spacing: Spacing::Md,
        label: None,
    }.into_component(next_id()).expect("Divider")
}

fn table(columns: Vec<Value>, _rows: Vec<Value>) -> Component {
    let table_cols: Vec<TableColumn> = columns.iter().enumerate().map(|(i, v)| {
        let header_text = match v {
            Value::Text(s) => s.clone(),
            _ => alloc::format!("col{}", i),
        };
        let col_id = header_text.to_ascii_lowercase().replace(' ', "_");
        TableColumn {
            id: col_id.clone(),
            header: lit(&header_text),
            field_path: vec![PathSegment::Key(col_id)],
            width: TableColumnWidth::Auto,
            render: ColumnRender::Text,
            format: None,
            align: None,
            sortable: false,
            hidden_by_default: false,
            sticky_left: false,
        }
    }).collect();
    TableComp {
        columns: table_cols,
        rows_path: StatePath::new(vec![PathSegment::Key("rows".into())]),
        row_key_field: "id".into(),
        variant: TableVariant::Default,
        density: Density::Default,
        sortable: false,
        sort_by: None,
        selectable: TableSelectMode::None,
        selected_ids: None,
        sticky_header: false,
        sticky_columns: 0,
        pagination: None,
        empty_state: None,
        row_actions: vec![],
        bulk_actions: vec![],
        virtualize: false,
        row_expandable: false,
        expanded_row_template_id: None,
    }.into_component(next_id()).expect("Table")
}

fn avatar(initials: &str, size: &str) -> Component {
    let sz = match size {
        "xs" => AvatarSize::Xs,
        "sm" => AvatarSize::Sm,
        "lg" => AvatarSize::Lg,
        "xl" => AvatarSize::Xl,
        _ => AvatarSize::Md,
    };
    AvatarComp {
        source: AvatarRef::Initials { initials: initials.into() },
        size: sz,
        shape: AvatarShape::Circle,
        status: None,
        tone: None,
    }.into_component(next_id()).expect("Avatar")
}

fn empty_state(title: &str, message: Option<&str>, icon: Option<&str>) -> Component {
    let built = EmptyStateComp {
        icon: icon_named(parse_icon_name(icon.unwrap_or("info"))),
        heading: lit(title),
        message: message.map(lit),
        primary_action: None,
        secondary_action: None,
        variant: EmptyStateVariant::Default,
    }.into_component(next_id());
    // EmptyState sits on hot wizard/empty-list render paths. A validation
    // failure here must degrade to a readable text node, never trap the whole
    // on_request and corrupt guest memory; the real reason is logged so the
    // panic hook / host log still pinpoints it.
    match built {
        Ok(c) => c,
        Err(e) => {
            log::error(&alloc::format!("TentaVision: EmptyState into_component failed: {:?}", e));
            text(title)
        }
    }
}

fn spinner(size: &str) -> Component {
    let sz = match size {
        "xs" => SpinnerSize::Xs,
        "sm" => SpinnerSize::Sm,
        "lg" => SpinnerSize::Lg,
        "xl" => SpinnerSize::Xl,
        _ => SpinnerSize::Md,
    };
    SpinnerComp {
        size: sz,
        tone: Tone::Neutral,
        label: None,
        variant: SpinnerVariant::Default,
    }.into_component(next_id()).expect("Spinner")
}

fn alert(message: &str, tone: &str) -> Component {
    AlertComp {
        tone: parse_tone(tone),
        variant: AlertVariant::Default,
        icon: None,
        title: None,
        message: lit(message),
        actions: None,
        dismissible: false,
    }.into_component(next_id()).expect("Alert")
}

/// Alert whose message tracks a store key reactively. Visibility is toggled by
/// the caller via `with_visible` so the wizard can show/hide errors and test
/// results purely through `StatePatch`.
fn alert_bound(message_key: &str, tone: &str) -> Component {
    AlertComp {
        tone: parse_tone(tone),
        variant: AlertVariant::Default,
        icon: None,
        title: None,
        message: bound(message_key),
        actions: None,
        dismissible: false,
    }.into_component(next_id()).expect("Alert")
}

fn progress_bar(value: f64, max: f64) -> Component {
    ProgressBarComp {
        value: BindRef::Literal(Value::F64(value)),
        max,
        variant: ProgressVariant::Default,
        tone: Tone::Primary,
        show_label: false,
        label: None,
        size: ProgressSize::Md,
    }.into_component(next_id()).expect("ProgressBar")
}

fn key_value(items: Vec<(&str, &str)>) -> Component {
    let kv_items: Vec<KvItem> = items.into_iter().map(|(k, v)| KvItem {
        label: lit(k),
        value: lit(v),
        hint: None,
        icon: None,
        action_id: None,
        format: None,
    }).collect();
    KvComp {
        items: kv_items,
        density: Density::Default,
        layout: KvLayout::Horizontal,
        label_width: None,
    }.into_component(next_id()).expect("KeyValue")
}

fn video_stream(src: &str) -> Component {
    VideoStreamComp {
        stream_id: lit(src),
        width_px: None,
        aspect_ratio: AspectRatio::R16To9,
        controls: VideoControls::Minimal,
        autoplay: true,
        muted: true,
        object_fit: ImageFit::Cover,
        poster_ref: None,
    }.into_component(next_id()).expect("VideoStream")
}

fn heatmap(_rows: u32, _cols: u32, _values: Vec<Vec<f64>>, row_labels: Vec<&str>, col_labels: Vec<&str>) -> Component {
    let hm_rows: Vec<HeatmapRow> = row_labels.into_iter().enumerate().map(|(i, label)| HeatmapRow {
        id: alloc::format!("r{}", i),
        label: lit(label),
    }).collect();
    let hm_cols: Vec<HeatmapColumn> = col_labels.into_iter().enumerate().map(|(i, label)| HeatmapColumn {
        id: alloc::format!("c{}", i),
        label: lit(label),
    }).collect();
    HeatmapComp {
        rows: hm_rows,
        columns: hm_cols,
        cells_path: StatePath::new(vec![PathSegment::Key("heatmap_cells".into())]),
        scale: HeatmapScale::Linear {
            min: 0.0,
            max: 1.0,
            color_from: Tone::Muted,
            color_to: Tone::Critical,
        },
        legend_position: HeatmapLegendPosition::TopRight,
        cell_size_px: 24,
        tooltip: true,
    }.into_component(next_id()).expect("Heatmap")
}

fn nav_tabs(items: Vec<NavTab>, active_id: &str) -> Component {
    NavTabsStruct {
        items,
        active_id: lit(active_id),
        variant: NavTabsVariant::Default,
        scroll_overflow: false,
    }.into_component(next_id()).expect("NavTabs")
}

fn input(label: &str, placeholder: &str, field_id: &str) -> Component {
    use tentaflow_sdk_spec::protocol::ui::form::Input;
    Input {
        r#type: InputType::Text,
        bind_path: StatePath::new(vec![PathSegment::Key(field_id.into())]),
        placeholder: Some(lit(placeholder)),
        label: Some(lit(label)),
        hint: None,
        leading_icon: None,
        trailing_icon: None,
        prefix: None,
        suffix: None,
        validators: vec![],
        max_length: None,
        min_length: None,
        pattern: None,
        autocomplete: None,
        input_mode: None,
        disabled: None,
        readonly: None,
        error: None,
        size: InputSize::Md,
    }.into_component(field_id).expect("Input")
}

fn select(label: &str, options: Vec<SelectOption>, field_id: &str) -> Component {
    use tentaflow_sdk_spec::protocol::ui::form::Select;
    Select {
        bind_path: StatePath::new(vec![PathSegment::Key(field_id.into())]),
        options,
        placeholder: None,
        label: Some(lit(label)),
        searchable: false,
        clearable: false,
        virtualize: false,
        disabled: None,
        size: InputSize::Md,
        groups: None,
    }.into_component(field_id).expect("Select")
}

/// Text input that mirrors its value into backend wizard state on every change
/// via the `wizard-field-change` action, tagged with `field`. Used by every
/// per-type wizard field so step navigation never loses typed values.
fn wizard_input(label: &str, placeholder: &str, field: &str, password: bool) -> Component {
    use tentaflow_sdk_spec::protocol::ui::form::Input;
    // An empty placeholder literal (`Some(Text("")))`) is meaningless and some
    // optional credential fields pass "" — encode it as absent rather than a
    // zero-length literal so the field stays canonical.
    let placeholder_ref = if placeholder.is_empty() { None } else { Some(lit(placeholder)) };
    let mut comp = Input {
        r#type: if password { InputType::Password } else { InputType::Text },
        bind_path: StatePath::new(vec![PathSegment::Key(field.into())]),
        placeholder: placeholder_ref,
        label: Some(lit(label)),
        hint: None,
        leading_icon: None,
        trailing_icon: None,
        prefix: None,
        suffix: None,
        validators: vec![],
        max_length: None,
        min_length: None,
        pattern: None,
        autocomplete: None,
        input_mode: None,
        disabled: None,
        readonly: None,
        error: None,
        size: InputSize::Md,
    }.into_component(field).expect("Input");
    // Backend wizard state is the source of truth for validation (resolve_target
    // on Next/Test/Submit), so every keystroke must commit. Using `Input` rather
    // than `Change` avoids the lost-update race where the user types and clicks
    // "Dalej" before the blur-fired `change` ever reaches the backend.
    let mut params = CborMap::default();
    params.0.push(("field".into(), Value::Text(field.into())));
    comp.handlers = Some(HandlerMap(vec![(
        tentaflow_sdk_spec::EventKind::Input,
        Handler::Backend {
            action_id: "wizard-field-change".into(),
            params,
            optimistic: None,
            on_failure: FailurePolicy::Toast,
        },
    )]));
    comp
}

/// Select that commits its picked value to backend wizard state on change,
/// tagged with `field` (used for the USB device dropdown).
fn wizard_select(label: &str, options: Vec<SelectOption>, field: &str) -> Component {
    use tentaflow_sdk_spec::protocol::ui::form::Select;
    let mut comp = Select {
        bind_path: StatePath::new(vec![PathSegment::Key(field.into())]),
        options,
        placeholder: None,
        label: Some(lit(label)),
        searchable: false,
        clearable: false,
        virtualize: false,
        disabled: None,
        size: InputSize::Md,
        groups: None,
    }.into_component(field).expect("Select");
    let mut params = CborMap::default();
    params.0.push(("field".into(), Value::Text(field.into())));
    comp.handlers = Some(HandlerMap(vec![(
        tentaflow_sdk_spec::EventKind::Change,
        Handler::Backend {
            action_id: "wizard-field-change".into(),
            params,
            optimistic: None,
            on_failure: FailurePolicy::Toast,
        },
    )]));
    comp
}

fn toggle(label: &str, field_id: &str) -> Component {
    use tentaflow_sdk_spec::protocol::ui::form::Toggle;
    Toggle {
        bind_path: StatePath::new(vec![PathSegment::Key(field_id.into())]),
        label: Some(lit(label)),
        hint: None,
        size: ToggleSize::Md,
        tone: Tone::Primary,
        disabled: None,
        label_position: TogglePosition::Trailing,
    }.into_component(field_id).expect("Toggle")
}

/// Single-handle slider bound to `field_id`, showing its current value. Used by
/// the profiles builder's quick-params (FPS sampling, detection confidence).
fn slider(label: &str, field_id: &str, min: f64, max: f64, step: f64) -> Component {
    use tentaflow_sdk_spec::protocol::ui::form::Slider;
    Slider {
        bind_path: StatePath::new(vec![PathSegment::Key(field_id.into())]),
        min,
        max,
        step,
        label: Some(lit(label)),
        show_value: true,
        format: None,
        marks: None,
        tone: Tone::Primary,
    }.into_component(field_id).expect("Slider")
}

fn filter_chips(items: Vec<FilterChipDef>, _active: &str) -> Component {
    FilterChipsComp {
        chips: items,
        selected_ids: StatePath::new(vec![PathSegment::Key("filter_active".into())]),
        mode: FilterChipsMode::Single,
        clearable: true,
    }.into_component(next_id()).expect("FilterChips")
}

fn mono_block(content: &str) -> Component {
    use tentaflow_sdk_spec::protocol::ui::data::MonoBlock;
    MonoBlock {
        content: lit(content),
        max_height_px: None,
        word_wrap: true,
        copyable: true,
    }.into_component(next_id()).expect("MonoBlock")
}

fn gate_screen(title: &str, message: &str, icon: &str) -> Component {
    GateScreenComp {
        icon: icon_named(parse_icon_name(icon)),
        title: lit(title),
        message: lit(message),
        actions: vec![],
        variant: GateVariant::PermissionDenied,
    }.into_component(next_id()).expect("GateScreen")
}

fn step_progress(steps: Vec<StepDef>, _current_id: &str) -> Component {
    StepProgressComp {
        steps,
        current_id_path: StatePath::new(vec![PathSegment::Key("onboarding_step".into())]),
        variant: StepProgressVariant::Horizontal,
        clickable_completed: false,
    }.into_component(next_id()).expect("StepProgress")
}

fn canvas(commands: Vec<Value>) -> Component {
    // VideoStream doubles as canvas surface in the spec (tag 0x0604).
    // For canvas drawing commands, pass through as a raw VideoStream with
    // the commands encoded in stream_id as a JSON payload.
    let json_str = commands.iter().map(|_| "cmd").collect::<Vec<_>>().join(",");
    VideoStreamComp {
        stream_id: lit(&alloc::format!("canvas:{}", json_str)),
        width_px: None,
        aspect_ratio: AspectRatio::R16To9,
        controls: VideoControls::None,
        autoplay: false,
        muted: true,
        object_fit: ImageFit::Contain,
        poster_ref: None,
    }.into_component(next_id()).expect("Canvas")
}

fn sparkline(_points: Vec<f64>) -> Component {
    SparklineComp {
        data_path: StatePath::new(vec![PathSegment::Key("sparkline_data".into())]),
        variant: SparklineVariant::Line,
        tone: Tone::Primary,
        width_px: 120,
        height_px: 32,
        show_min_max: false,
    }.into_component(next_id()).expect("Sparkline")
}

// =============================================================================
// In-WASM ephemeral panel state
// =============================================================================

struct PanelState {
    current_panel: String,
    add_form_visible: bool,
    wizard_step: u8,
    cameras_filter: String,
    // Camera selected via a table row click, pending a delete confirmation.
    camera_pending_remove: Option<String>,
    error_message: Option<String>,
    success_message: Option<String>,
    discover: DiscoverState,
    profiles: ProfilesState,
    alarms: AlarmsState,
    search: SearchState,
    reid: ReidState,
    models: ModelsState,
    zones: ZonesState,
    audit: AuditState,
    evidence: EvidenceState,
    settings: SettingsState,
    onboarding: OnboardingState,
    bindings: BindingsState,
}

struct OnboardingState {
    step: u8,
    deployment_profile: Option<String>,
    selected_models: Vec<String>,
    notification_channel: Option<String>,
}

impl OnboardingState {
    const fn new() -> Self {
        Self { step: 0, deployment_profile: None, selected_models: Vec::new(), notification_channel: None }
    }
}

struct BindingsState {
    expanded_rows: Vec<String>,
    filter_addon: Option<String>,
    filter_type: Option<String>,
    filter_status: Option<String>,
}

impl BindingsState {
    const fn new() -> Self {
        Self { expanded_rows: Vec::new(), filter_addon: None, filter_type: None, filter_status: None }
    }
    fn toggle_expanded(&mut self, id: &str) {
        if let Some(idx) = self.expanded_rows.iter().position(|x| x == id) {
            self.expanded_rows.remove(idx);
        } else {
            self.expanded_rows.push(id.to_string());
        }
    }
    fn has_any_filter(&self) -> bool {
        self.filter_addon.is_some() || self.filter_type.is_some() || self.filter_status.is_some()
    }
    fn clear(&mut self) {
        self.filter_addon = None;
        self.filter_type = None;
        self.filter_status = None;
    }
}

struct AuditState {
    date_preset: String,
    users: Vec<String>,
    actions: Vec<String>,
    risk_class: String,
    result: String,
    query: String,
    expanded_id: Option<String>,
    cursor: Option<String>,
}

impl AuditState {
    const fn new() -> Self {
        Self {
            date_preset: String::new(), users: Vec::new(), actions: Vec::new(),
            risk_class: String::new(), result: String::new(), query: String::new(),
            expanded_id: None, cursor: None,
        }
    }
    fn clear_filters(&mut self) {
        self.date_preset.clear(); self.users.clear(); self.actions.clear();
        self.risk_class.clear(); self.result.clear(); self.query.clear();
        self.cursor = None;
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum EvidenceTab { Active, Archive, All }

struct EvidenceState {
    tab: EvidenceTab,
    drawer_open_id: Option<String>,
    drawer_tab: String,
    edited_descriptions: Vec<(String, String)>,
}

impl EvidenceState {
    const fn new() -> Self {
        Self { tab: EvidenceTab::Active, drawer_open_id: None, drawer_tab: String::new(), edited_descriptions: Vec::new() }
    }
    fn drawer_tab_or_default(&self) -> &str {
        if self.drawer_tab.is_empty() { "summary" } else { &self.drawer_tab }
    }
    fn description_for(&self, id: &str) -> Option<&str> {
        self.edited_descriptions.iter().find(|(k, _)| k == id).map(|(_, v)| v.as_str())
    }
    fn set_description(&mut self, id: &str, value: String) {
        if let Some(slot) = self.edited_descriptions.iter_mut().find(|(k, _)| k == id) {
            slot.1 = value;
        } else {
            self.edited_descriptions.push((id.to_string(), value));
        }
    }
}

struct SettingsState {
    tab: String,
    fields: Vec<(String, String)>,
    notification_channel: String,
    selected_addon_id: String,
    access_overrides: Vec<AccessOverride>,
}

#[derive(Clone)]
struct AccessOverride {
    addon_id: String,
    role: String,
    permission: String,
    granted: bool,
}

impl SettingsState {
    const fn new() -> Self {
        Self {
            tab: String::new(), fields: Vec::new(), notification_channel: String::new(),
            selected_addon_id: String::new(), access_overrides: Vec::new(),
        }
    }
    fn tab_or_default(&self) -> &str {
        if self.tab.is_empty() { "general" } else { &self.tab }
    }
    fn notification_channel_or_default(&self) -> &str {
        if self.notification_channel.is_empty() { "slack" } else { &self.notification_channel }
    }
    fn selected_addon_or_default(&self) -> &str {
        if self.selected_addon_id.is_empty() { "tentavision" } else { &self.selected_addon_id }
    }
    fn field(&self, tab: &str, field_id: &str) -> Option<&str> {
        let key = alloc::format!("{}::{}", tab, field_id);
        self.fields.iter().find(|(k, _)| k == &key).map(|(_, v)| v.as_str())
    }
    fn set_field(&mut self, tab: &str, field_id: &str, value: String) {
        let key = alloc::format!("{}::{}", tab, field_id);
        if let Some(slot) = self.fields.iter_mut().find(|(k, _)| k == &key) {
            slot.1 = value;
        } else {
            self.fields.push((key, value));
        }
    }
    fn access_granted(&self, addon_id: &str, role: &str, permission: &str) -> Option<bool> {
        self.access_overrides.iter()
            .find(|o| o.addon_id == addon_id && o.role == role && o.permission == permission)
            .map(|o| o.granted)
    }
    fn set_access(&mut self, addon_id: &str, role: &str, permission: &str, granted: bool) {
        if let Some(o) = self.access_overrides.iter_mut().find(|o| {
            o.addon_id == addon_id && o.role == role && o.permission == permission
        }) {
            o.granted = granted;
        } else {
            self.access_overrides.push(AccessOverride {
                addon_id: addon_id.to_string(), role: role.to_string(),
                permission: permission.to_string(), granted,
            });
        }
    }
}

struct ReidState { gate_passed: bool }
impl ReidState { const fn new() -> Self { Self { gate_passed: false } } }

struct ModelsState { expanded_id: Option<String> }
impl ModelsState { const fn new() -> Self { Self { expanded_id: None } } }

#[derive(Clone)]
struct ZonePoint { x: f64, y: f64 }

#[derive(Clone)]
struct ZoneFixture {
    id: String, name: String, zone_type: String,
    points: Vec<ZonePoint>, schedule: [[bool; 24]; 7],
    min_confidence: f64, models: Vec<String>, alarm_on_detect: bool,
}

struct ZonesState {
    selected_camera_id: Option<String>,
    zones: Vec<ZoneFixture>,
    selected_zone_id: Option<String>,
    drawing_mode: bool,
    drawing_points: Vec<ZonePoint>,
    cursor: Option<ZonePoint>,
}

impl ZonesState {
    const fn const_placeholder() -> Self {
        Self {
            selected_camera_id: None, zones: Vec::new(), selected_zone_id: None,
            drawing_mode: false, drawing_points: Vec::new(), cursor: None,
        }
    }
    fn ensure_seeded(&mut self) {
        if !self.zones.is_empty() { return; }
        *self = Self::new();
    }
    fn new() -> Self {
        let always: [[bool; 24]; 7] = [[true; 24]; 7];
        Self {
            selected_camera_id: None,
            zones: vec![
                ZoneFixture { id: "z1".into(), name: "Peron główny".into(), zone_type: "detection".into(),
                    points: vec![ZonePoint{x:80.0,y:280.0}, ZonePoint{x:420.0,y:280.0}, ZonePoint{x:420.0,y:520.0}, ZonePoint{x:80.0,y:520.0}],
                    schedule: always, min_confidence: 0.6, models: vec!["yolo".into()], alarm_on_detect: false },
                ZoneFixture { id: "z2".into(), name: "Ławka (ignore)".into(), zone_type: "exclusion".into(),
                    points: vec![ZonePoint{x:500.0,y:380.0}, ZonePoint{x:660.0,y:380.0}, ZonePoint{x:660.0,y:500.0}, ZonePoint{x:500.0,y:500.0}],
                    schedule: always, min_confidence: 0.5, models: vec![], alarm_on_detect: false },
                ZoneFixture { id: "z3".into(), name: "Wjazd ADR".into(), zone_type: "alert".into(),
                    points: vec![ZonePoint{x:200.0,y:100.0}, ZonePoint{x:360.0,y:100.0}, ZonePoint{x:280.0,y:240.0}],
                    schedule: always, min_confidence: 0.75, models: vec!["yolo".into(), "ocr".into()], alarm_on_detect: true },
            ],
            selected_zone_id: Some("z1".into()),
            drawing_mode: false, drawing_points: Vec::new(), cursor: None,
        }
    }
    fn find_zone_mut(&mut self, id: &str) -> Option<&mut ZoneFixture> {
        self.zones.iter_mut().find(|z| z.id == id)
    }
}

/// State for the Profiles tab. `category` holds the active risk-class filter
/// chip (A/B/C; empty = all). The remaining fields back the analytic-profile
/// builder form (left/right of the mockup): a draft profile being created or the
/// snapshot of the profile under edit. `builder_visible` gates whether the
/// builder section is shown above the library table.
struct ProfilesState {
    category: String,
    builder_visible: bool,
    // id of the profile being edited; None = creating a new one.
    editing_id: Option<String>,
    // id of the profile selected for deletion (arms the confirm bar).
    pending_remove: Option<String>,
    // Builder form fields.
    name: String,
    flow_id: String,
    risk_class: String,
    schedule: String,
    fps: f64,
    min_confidence: f64,
    // Selected camera ids assigned to the profile.
    cameras: Vec<String>,
}

impl ProfilesState {
    const fn new() -> Self {
        Self {
            category: String::new(),
            builder_visible: false,
            editing_id: None,
            pending_remove: None,
            name: String::new(),
            flow_id: String::new(),
            risk_class: String::new(),
            schedule: String::new(),
            fps: 5.0,
            min_confidence: 0.65,
            cameras: Vec::new(),
        }
    }

    fn category_or_all(&self) -> &str {
        if self.category.is_empty() { "all" } else { &self.category }
    }

    /// Resets the builder form to a clean "create" draft.
    fn reset_form(&mut self) {
        self.editing_id = None;
        self.name.clear();
        self.flow_id = "tv-realtime-adr".into();
        self.risk_class = "A".into();
        self.schedule = "24/7".into();
        self.fps = 5.0;
        self.min_confidence = 0.65;
        self.cameras.clear();
    }

    /// Loads an existing profile row into the builder for editing.
    fn load_for_edit(&mut self, p: &db::ProfileRow, camera_ids: Vec<String>) {
        self.editing_id = Some(p.id.clone());
        self.name = p.name.clone();
        self.flow_id = if p.flow_id.is_empty() { "tv-realtime-adr".into() } else { p.flow_id.clone() };
        self.risk_class = if p.risk_class.is_empty() { "A".into() } else { p.risk_class.clone() };
        self.schedule = if p.schedule.is_empty() { "24/7".into() } else { p.schedule.clone() };
        self.cameras = camera_ids;
    }

    fn toggle_camera(&mut self, id: &str) {
        if let Some(pos) = self.cameras.iter().position(|c| c == id) {
            self.cameras.remove(pos);
        } else {
            self.cameras.push(id.to_string());
        }
    }
}

struct AlarmsState { selected_id: Option<String>, severity_filter: String, sound_muted: bool }
impl AlarmsState {
    const fn new() -> Self { Self { selected_id: None, severity_filter: String::new(), sound_muted: false } }
    fn severity_or_all(&self) -> &str { if self.severity_filter.is_empty() { "all" } else { &self.severity_filter } }
}

struct SearchState {
    query: String, date_from: String, date_to: String,
    cameras: Vec<String>, profiles: Vec<String>,
    min_confidence: f64, object_type: String,
    only_with_evidence: bool, submitted: bool,
}
impl SearchState {
    const fn new() -> Self {
        Self {
            query: String::new(), date_from: String::new(), date_to: String::new(),
            cameras: Vec::new(), profiles: Vec::new(), min_confidence: 0.7,
            object_type: String::new(), only_with_evidence: false, submitted: false,
        }
    }
    fn has_any_filter(&self) -> bool {
        !self.query.is_empty() || !self.date_from.is_empty() || !self.date_to.is_empty()
            || !self.cameras.is_empty() || !self.profiles.is_empty()
            || self.min_confidence > 0.0 || !self.object_type.is_empty() || self.only_with_evidence
    }
    fn clear_all(&mut self) {
        self.query.clear(); self.date_from.clear(); self.date_to.clear();
        self.cameras.clear(); self.profiles.clear(); self.min_confidence = 0.7;
        self.object_type.clear(); self.only_with_evidence = false; self.submitted = false;
    }
}

/// The four camera source types the backend supports. Drives every per-step
/// branch of the add-camera wizard. `vendor()` maps each type to the stable
/// TentaFlow vendor string the `camera_add` host function expects.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SourceType { Onvif, Rtsp, Usb, File }

impl SourceType {
    fn from_str(s: &str) -> Option<Self> {
        match s {
            "onvif" => Some(Self::Onvif),
            "rtsp" => Some(Self::Rtsp),
            "usb" => Some(Self::Usb),
            "file" => Some(Self::File),
            _ => None,
        }
    }
    fn as_str(self) -> &'static str {
        match self { Self::Onvif => "onvif", Self::Rtsp => "rtsp", Self::Usb => "usb", Self::File => "file" }
    }
    fn vendor(self) -> &'static str {
        // USB enumeration reports `v4l2` on Linux; the local-device list carries
        // the authoritative vendor, but the manual-path fallback uses this.
        match self { Self::Onvif => "onvif", Self::Rtsp => "rtsp", Self::Usb => "v4l2", Self::File => "fake_file" }
    }
}

/// One locally enumerated USB/v4l2 device offered in the wizard's device select.
struct LocalDevice { device_path: String, label: String, vendor: String }

/// Working state of the source-type-driven "Add camera" wizard. Each per-type
/// field is committed to the backend on input change (`wizard-field-change`) so
/// the test step and submit read consistent values across step navigation
/// instead of relying on a single live form snapshot.
struct DiscoverState {
    source_type: Option<SourceType>,
    // ONVIF discovery results.
    scanning: bool,
    cameras: Vec<DiscoveredCam>,
    selected_index: Option<usize>,
    // USB/v4l2 enumeration results and the picked device path.
    usb_devices: Vec<LocalDevice>,
    usb_loaded: bool,
    usb_device_path: String,
    // Per-type manual entry fields.
    onvif_url: String,
    rtsp_url: String,
    file_path: String,
    cred_user: String,
    cred_pass: String,
    // Step 3 connection test outcome (real probe, never faked).
    test_result: Option<Result<String, String>>,
    testing: bool,
    // Step 4 metadata.
    name: String,
    retention: String,
    fps: String,
    // Analytics profile chosen in step 4. Committed from the profile select so
    // the pick is authoritative on submit instead of a frontend-only value.
    profile: String,
    error_message: Option<String>,
}
struct DiscoveredCam { vendor: String, url: String, suggested_name: String, profile_token: Option<String> }
impl DiscoverState {
    const fn new() -> Self {
        Self {
            source_type: None,
            scanning: false, cameras: Vec::new(), selected_index: None,
            usb_devices: Vec::new(), usb_loaded: false, usb_device_path: String::new(),
            onvif_url: String::new(), rtsp_url: String::new(), file_path: String::new(),
            cred_user: String::new(), cred_pass: String::new(),
            test_result: None, testing: false,
            name: String::new(), retention: String::new(), fps: String::new(),
            profile: String::new(),
            error_message: None,
        }
    }
    fn reset(&mut self) {
        *self = Self::new();
    }
    /// Resolves the effective (vendor, url) for the current source type from the
    /// committed wizard fields. Returns `Err` with a user-facing message when the
    /// required field for the chosen type is missing or malformed.
    fn resolve_target(&self) -> Result<(String, String, Option<String>), &'static str> {
        match self.source_type {
            Some(SourceType::Onvif) => {
                if let Some(i) = self.selected_index {
                    if let Some(cam) = self.cameras.get(i) {
                        return Ok((cam.vendor.clone(), cam.url.clone(), cam.profile_token.clone()));
                    }
                }
                let url = self.onvif_url.trim();
                if url.is_empty() {
                    return Err("Wybierz wykrytą kamerę ONVIF lub podaj adres URL urządzenia.");
                }
                if !(url.starts_with("http://") || url.starts_with("https://")) {
                    return Err("Adres ONVIF musi zaczynać się od http:// lub https://.");
                }
                Ok(("onvif".to_string(), url.to_string(), None))
            }
            Some(SourceType::Rtsp) => {
                let url = self.rtsp_url.trim();
                if url.is_empty() {
                    return Err("Podaj adres strumienia RTSP.");
                }
                let lower = url.to_ascii_lowercase();
                if !(lower.starts_with("rtsp://") || lower.starts_with("rtsps://")) {
                    return Err("Adres RTSP musi zaczynać się od rtsp:// lub rtsps://.");
                }
                Ok(("rtsp".to_string(), url.to_string(), None))
            }
            Some(SourceType::Usb) => {
                let path = self.usb_device_path.trim();
                if path.is_empty() {
                    return Err("Wybierz lub podaj ścieżkę urządzenia (np. /dev/video0).");
                }
                let vendor = self.usb_devices.iter()
                    .find(|d| d.device_path == path)
                    .map(|d| d.vendor.clone())
                    .unwrap_or_else(|| SourceType::Usb.vendor().to_string());
                Ok((vendor, path.to_string(), None))
            }
            Some(SourceType::File) => {
                let path = self.file_path.trim();
                if path.is_empty() {
                    return Err("Podaj ścieżkę pliku wideo.");
                }
                Ok(("fake_file".to_string(), path.to_string(), None))
            }
            None => Err("Wybierz typ źródła kamery."),
        }
    }
    fn retention_or_default(&self) -> &str {
        if self.retention.is_empty() { "C" } else { &self.retention }
    }
    fn profile_or_default(&self) -> &str {
        let p = self.profile.trim();
        if p.is_empty() { "default" } else { p }
    }
    fn fps_value(&self) -> u32 {
        self.fps.trim().parse::<u32>().ok().filter(|f| *f >= 1 && *f <= 60).unwrap_or(15)
    }
}

impl PanelState {
    const fn new() -> Self {
        Self {
            current_panel: String::new(),
            add_form_visible: false, wizard_step: 0, cameras_filter: String::new(),
            camera_pending_remove: None,
            error_message: None, success_message: None,
            discover: DiscoverState::new(), profiles: ProfilesState::new(),
            alarms: AlarmsState::new(), search: SearchState::new(),
            reid: ReidState::new(), models: ModelsState::new(),
            zones: ZonesState::const_placeholder(), audit: AuditState::new(),
            evidence: EvidenceState::new(), settings: SettingsState::new(),
            onboarding: OnboardingState::new(), bindings: BindingsState::new(),
        }
    }
    fn clear_messages(&mut self) { self.error_message = None; self.success_message = None; }
}

static STATE: Mutex<PanelState> = Mutex::new(PanelState::new());

/// Rows computed by `build_cameras_content` and handed to `render_panel` so the
/// cameras Table mounts with its rows already in the slot's state_overlay
/// snapshot (avoids a first empty rebuild that would flash the empty-state).
static PENDING_CAMERA_ROWS: Mutex<Option<Value>> = Mutex::new(None);

/// Same mechanism as `PENDING_CAMERA_ROWS` but for the profiles library Table:
/// rows seeded into the slot's state_overlay so the Table mounts populated.
static PENDING_PROFILE_ROWS: Mutex<Option<Value>> = Mutex::new(None);

fn with_state<F, R>(f: F) -> R where F: FnOnce(&mut PanelState) -> R {
    let mut guard = match STATE.lock() { Ok(g) => g, Err(p) => p.into_inner() };
    f(&mut guard)
}

fn set_current_panel(panel: &str) {
    with_state(|s| { s.current_panel.clear(); s.current_panel.push_str(panel); });
}

// =============================================================================
// Lifecycle
// =============================================================================

// Guest memory exports for host → guest data transfer (on_panel_open, on_request)
#[no_mangle]
pub extern "C" fn alloc(size: i32) -> i32 {
    let layout = std::alloc::Layout::from_size_align(size as usize, 8).unwrap();
    unsafe { std::alloc::alloc(layout) as i32 }
}

#[no_mangle]
pub extern "C" fn dealloc(ptr: i32, size: i32) {
    let layout = std::alloc::Layout::from_size_align(size as usize, 8).unwrap();
    unsafe { std::alloc::dealloc(ptr as *mut u8, layout) }
}

#[no_mangle]
pub extern "C" fn on_install() -> i32 { 0 }

#[no_mangle]
pub extern "C" fn on_start() -> i32 {
    install_panic_hook();
    log::info("TentaVision: on_start (CBOR SDK)");
    send_initial_shell();
    render_panel("overview");
    0
}

#[no_mangle]
pub extern "C" fn on_stop() -> i32 {
    log::info("TentaVision: on_stop");
    0
}

#[no_mangle]
pub extern "C" fn on_event(_input_ptr: i32, _input_len: i32) -> i32 { 0 }

/// Called by host when user opens a panel on an already-running instance.
/// Re-emits PanelShell + SlotContent without restarting the addon.
#[no_mangle]
pub extern "C" fn on_panel_open(panel_id_ptr: i32, panel_id_len: i32, epoch: i64) -> i32 {
    install_panic_hook();
    let panel_id = read_guest_string(panel_id_ptr, panel_id_len);
    PANEL_EPOCH.store(epoch as u64, core::sync::atomic::Ordering::Relaxed);
    log::info(&alloc::format!("TentaVision: on_panel_open panel='{}' epoch={}", panel_id, epoch));
    // A fresh panel open starts a new view context; carrying a transient
    // success/error banner over from the previous session would surface stale
    // toasts (e.g. "Kamera dodana") on an unrelated tab.
    with_state(|s| s.clear_messages());
    send_initial_shell();
    let target = if panel_id.is_empty() { "overview" } else { &panel_id };
    render_panel(target);
    0
}

/// Wasm ABI: on_request(input_ptr, input_len, out_ptr, out_cap, out_len_ptr) -> i32
#[no_mangle]
pub extern "C" fn on_request(
    input_ptr: i32, input_len: i32,
    out_ptr: i32, out_cap: i32,
    out_len_ptr: i32,
) -> i32 {
    let input_json = read_guest_string(input_ptr, input_len);
    let request: JsonValue = match serde_json::from_str(&input_json) {
        Ok(v) => v,
        Err(e) => {
            log::error(&alloc::format!("TentaVision: invalid on_request JSON: {}", e));
            return 1;
        }
    };
    let tool = request.get("tool").and_then(|v| v.as_str()).unwrap_or("");
    let params = request.get("params").cloned().unwrap_or(JsonValue::Null);

    let action = tool
        .strip_prefix("ui.dashboard.")
        .or_else(|| tool.strip_prefix("ui.cameras."))
        .or_else(|| tool.strip_prefix("ui.overview."))
        .or_else(|| tool.strip_prefix("ui.live."))
        .or_else(|| tool.strip_prefix("ui.profiles."))
        .or_else(|| tool.strip_prefix("ui.alarms."))
        .or_else(|| tool.strip_prefix("ui.search."))
        .or_else(|| tool.strip_prefix("ui.reid."))
        .or_else(|| tool.strip_prefix("ui.models."))
        .or_else(|| tool.strip_prefix("ui.zones."))
        .or_else(|| tool.strip_prefix("ui.audit."))
        .or_else(|| tool.strip_prefix("ui.evidence."))
        .or_else(|| tool.strip_prefix("ui.settings."));

    let response = match action {
        Some(a) => handle_action(a, &params),
        None => json!({ "error": alloc::format!("unknown tool '{}'", tool) }),
    };

    // Each handler owns its own UI side effects: panel/tab navigation and modal
    // open re-send SlotContent, while reactive wizard actions emit StatePatch
    // only. There is intentionally NO unconditional `render_panel` here — that
    // global re-render was the source of the modal tearing down and inputs
    // losing focus on every wizard interaction.

    let response_str = response.to_string();
    let written = write_guest_string(out_ptr, out_cap, &response_str);
    if written < 0 { return 2; }
    unsafe {
        let p = out_len_ptr as *mut i32;
        *p = response_str.len() as i32;
    }
    0
}

// =============================================================================
// Guest memory helpers
// =============================================================================

fn read_guest_string(ptr: i32, len: i32) -> String {
    if len <= 0 { return String::new(); }
    unsafe {
        let slice = core::slice::from_raw_parts(ptr as *const u8, len as usize);
        String::from_utf8_lossy(slice).into_owned()
    }
}

fn write_guest_string(ptr: i32, cap: i32, s: &str) -> i32 {
    let bytes = s.as_bytes();
    if bytes.len() > cap as usize { return -1; }
    unsafe {
        core::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr as *mut u8, bytes.len());
    }
    bytes.len() as i32
}

// =============================================================================
// Initial PanelShell — sent once on on_start
// =============================================================================

fn send_initial_shell() {
    let layout = build_shell_layout();
    // "content" is the static main panel. The "Add camera" wizard's
    // `add_camera_body` / `add_camera_footer` slots are also declared here, but
    // as Modal+Hidden overlay slots: the declaration satisfies the Core's slot
    // ownership check (a slot must be declared in the PanelShell), while the
    // Modal/Hidden semantics tell addon-app's `isOverlaySlot` to skip building a
    // static placeholder container for them. Their real containers are created
    // dynamically by the Modal and auto-registered by the host's
    // `observe(shell)`, with their SlotContent buffered until registration.
    let slots = vec![
        SlotDecl {
            id: "content".into(),
            semantics: SlotSemantics::MainContent,
            default_state: SlotDefault::Loading,
            cache_policy: CachePolicy::OnNavigateBack,
            visibility: SlotVisibility::Always,
            max_payload_bytes: Some(256 * 1024),
        },
        SlotDecl {
            id: "add_camera_body".into(),
            semantics: SlotSemantics::Modal,
            default_state: SlotDefault::Empty,
            cache_policy: CachePolicy::None,
            visibility: SlotVisibility::Hidden,
            max_payload_bytes: Some(256 * 1024),
        },
        SlotDecl {
            id: "add_camera_footer".into(),
            semantics: SlotSemantics::Modal,
            default_state: SlotDefault::Empty,
            cache_policy: CachePolicy::None,
            visibility: SlotVisibility::Hidden,
            max_payload_bytes: Some(64 * 1024),
        },
    ];
    // The frontend resets its reactive store revision to 0 whenever it receives
    // a PanelShell (new shell/epoch). Reset the guest counter in lockstep so the
    // first StatePatch after this shell carries base_revision = 0; otherwise a
    // stale (higher) base from a previous shell would be rejected by the host
    // and the UI would never update.
    STATE_REVISION.store(0, Ordering::Relaxed);
    send_panel_shell(layout, slots, vec![]);
}

fn build_shell_layout() -> Component {
    let nav_items = build_nav_tab_items("overview");
    let mut nav = nav_tabs(nav_items, "overview");
    nav.handlers = Some(HandlerMap(vec![(
        tentaflow_sdk_spec::EventKind::Select,
        Handler::Backend {
            action_id: "panel-navigate".into(),
            params: CborMap::default(),
            optimistic: None,
            on_failure: FailurePolicy::Toast,
        },
    )]));
    stack_v(vec![nav])
}

// =============================================================================
// Panel navigation — sends SlotContent for the "content" slot
// =============================================================================

fn render_panel(panel_id: &str) {
    set_current_panel(panel_id);
    let content = match panel_id {
        "overview" => build_overview_content(),
        "live" => build_live_content(),
        "cameras" => build_cameras_content(),
        "alarms" => build_alarms_content(),
        "search" => build_search_content(),
        "profiles" => build_profiles_content(),
        "reid" => build_reid_content(),
        "models" => build_models_content(),
        "zones" => build_zones_content(),
        "audit" => build_audit_content(),
        "evidence" => build_evidence_content(),
        "settings" => build_settings_content(),
        "onboarding" => build_onboarding_content(),
        "bindings" => build_bindings_content(),
        _ => build_overview_content(),
    };
    // Send "content" first so the host has the Modal (and thus the dynamic
    // body/footer slot containers) in the DOM before we push their content.
    // The cameras panel seeds its table rows via the slot's state_overlay so
    // the Table mounts with rows already in the store snapshot — otherwise its
    // first rebuild sees an empty rows_path and leaves the empty-state visible.
    if panel_id == "cameras" {
        let overlay = PENDING_CAMERA_ROWS
            .lock()
            .ok()
            .and_then(|mut g| g.take())
            .map(|rows| vec![StateEntry {
                path: StatePath::new(vec![PathSegment::Key("cameras_rows".into())]),
                value: rows,
            }]);
        send_slot_content_with_overlay("content", content, overlay);
    } else if panel_id == "profiles" {
        let mut entries: Vec<StateEntry> = Vec::new();
        if let Some(rows) = PENDING_PROFILE_ROWS.lock().ok().and_then(|mut g| g.take()) {
            entries.push(StateEntry {
                path: StatePath::new(vec![PathSegment::Key("profiles_rows".into())]),
                value: rows,
            });
        }
        // When the builder is open, seed the bound form keys so the inputs,
        // selects and sliders mount with the draft / edited profile's values.
        if with_state(|s| s.profiles.builder_visible) {
            entries.extend(profile_builder_overlay());
        }
        let overlay = if entries.is_empty() { None } else { Some(entries) };
        send_slot_content_with_overlay("content", content, overlay);
    } else if panel_id == "overview" {
        // Seed the activity heatmap's cells into the slot snapshot so the
        // Heatmap mounts with its data already in the store (same pattern as the
        // cameras Table). Without this, `heatmap_cells` is undefined on first
        // paint and every cell renders at level 0.
        let overlay = vec![StateEntry {
            path: StatePath::new(vec![PathSegment::Key("heatmap_cells".into())]),
            value: heatmap_cells_value(),
        }];
        send_slot_content_with_overlay("content", content, Some(overlay));
    } else {
        send_slot_content("content", content);
    }

    // When the "Add camera" wizard is open on the cameras panel, fill the
    // Modal's body/footer slots. These must be sent AFTER "content" so their
    // target data-slot-id containers already exist.
    if panel_id == "cameras" && with_state(|s| s.add_form_visible) {
        // Seed the full wizard store state alongside the body so the bound
        // visibility flags, StepProgress and inputs resolve on first paint.
        send_slot_content_with_overlay("add_camera_body", build_add_camera_body(), Some(wizard_full_overlay()));
        send_slot_content("add_camera_footer", build_add_camera_footer());
    }
}

// =============================================================================
// Action handlers
// =============================================================================

fn handle_action(action: &str, params: &JsonValue) -> JsonValue {
    log::info(&alloc::format!("TentaVision UI action '{}'", action));
    match action {
        "camera-add-show" => handle_camera_add_show(),
        "camera-add-cancel" => { with_state(|s| { s.add_form_visible = false; s.wizard_step = 0; s.discover.reset(); s.clear_messages(); }); render_panel("cameras"); json!({"ok":true}) }
        "wizard-source-select" => handle_wizard_source_select(params),
        "wizard-field-change" => handle_wizard_field_change(params),
        "wizard-test" => handle_wizard_test(),
        "wizard-next" => handle_wizard_next(),
        "wizard-prev" => handle_wizard_prev(),
        "cameras-filter-change" => { let v = params.get("value").and_then(|x| x.as_str()).or_else(|| params.get("chipId").and_then(|x| x.as_str())).unwrap_or("all").to_string(); with_state(|s| { s.cameras_filter = if v == "all" { String::new() } else { v }; }); json!({"ok":true}) }
        "camera-add-submit" => handle_camera_add_submit(params),
        "camera-row-select" => { let id = params.get("row_id").and_then(|x| x.as_str()).or_else(|| params.get("camera_id").and_then(|x| x.as_str())).unwrap_or("").trim().to_string(); with_state(|s| { s.clear_messages(); s.camera_pending_remove = if id.is_empty() { None } else { Some(id) }; }); json!({"ok":true}) }
        "camera-remove-cancel" => { with_state(|s| { s.camera_pending_remove = None; s.clear_messages(); }); json!({"ok":true}) }
        "camera-remove" => handle_camera_remove(params),
        "discover-scan" => handle_discover_scan(),
        "discover-select" => handle_discover_select(params),
        "cameras-refresh" | "overview-refresh" => { with_state(|s| s.clear_messages()); json!({"ok":true}) }
        "panel-navigate" => {
            let target = params.get("panel_id")
                .or_else(|| params.get("item_id"))
                .and_then(|v| v.as_str()).unwrap_or("overview").to_string();
            render_panel(&target);
            json!({"ok":true, "panel_id": target})
        }
        "profiles-filter-change" => { let v = params.get("value").and_then(|x| x.as_str()).or_else(|| params.get("chipId").and_then(|x| x.as_str())).unwrap_or("all").to_string(); with_state(|s| { s.profiles.category = if v == "all" { String::new() } else { v }; }); json!({"ok":true}) }
        "profile-add-show" => { with_state(|s| { s.clear_messages(); s.profiles.builder_visible = true; s.profiles.pending_remove = None; s.profiles.reset_form(); }); render_panel("profiles"); json!({"ok":true}) }
        "profile-builder-cancel" => { with_state(|s| { s.profiles.builder_visible = false; s.profiles.editing_id = None; s.clear_messages(); }); render_panel("profiles"); json!({"ok":true}) }
        "profile-field-change" => handle_profile_field_change(params),
        "profile-camera-toggle" => { let id = params.get("camera_id").and_then(|x| x.as_str()).or_else(|| params.get("row_id").and_then(|x| x.as_str())).unwrap_or("").trim().to_string(); if !id.is_empty() { with_state(|s| s.profiles.toggle_camera(&id)); } render_panel("profiles"); json!({"ok":true}) }
        "profile-add-submit" => handle_profile_add_submit(),
        "profile-edit" => handle_profile_edit(params),
        "profile-toggle-enabled" => handle_profile_toggle_enabled(params),
        "profile-row-select" => { let id = params.get("row_id").and_then(|x| x.as_str()).or_else(|| params.get("profile_id").and_then(|x| x.as_str())).unwrap_or("").trim().to_string(); with_state(|s| { s.clear_messages(); s.profiles.pending_remove = if id.is_empty() { None } else { Some(id) }; }); render_panel("profiles"); json!({"ok":true}) }
        "profile-remove-cancel" => { with_state(|s| { s.profiles.pending_remove = None; s.clear_messages(); }); render_panel("profiles"); json!({"ok":true}) }
        "profile-remove" => handle_profile_remove(params),
        "alarm-select" => { let id = params.get("alarm_id").and_then(|x| x.as_str()).or_else(|| params.get("rowId").and_then(|x| x.as_str())).unwrap_or("").to_string(); with_state(|s| { s.alarms.selected_id = if id.is_empty() { None } else { Some(id) }; }); json!({"ok":true}) }
        "alarm-acknowledge" => { let id = params.get("alarm_id").and_then(|x| x.as_str()).unwrap_or("").to_string(); with_state(|s| { s.success_message = Some(alloc::format!("Potwierdzono alarm {}.", id)); }); json!({"ok":true}) }
        "alarm-acknowledge-all" => { with_state(|s| { s.success_message = Some("Potwierdzono wszystkie niepotwierdzone.".into()); s.alarms.selected_id = None; }); json!({"ok":true}) }
        "alarm-filter-severity" => { let v = params.get("value").and_then(|x| x.as_str()).or_else(|| params.get("chipId").and_then(|x| x.as_str())).unwrap_or("all").to_string(); with_state(|s| { s.alarms.severity_filter = if v == "all" { String::new() } else { v }; }); json!({"ok":true}) }
        "alarm-mute-sound" => { with_state(|s| { s.alarms.sound_muted = !s.alarms.sound_muted; }); json!({"ok":true}) }
        "search-query-change" => { let v = params.get("value").and_then(|x| x.as_str()).unwrap_or("").to_string(); with_state(|s| s.search.query = v); json!({"ok":true}) }
        "search-submit" => { let v = params.get("value").and_then(|x| x.as_str()).map(|s| s.to_string()); with_state(|s| { if let Some(q) = v { s.search.query = q; } s.search.submitted = true; }); json!({"ok":true}) }
        "search-clear-all" => { with_state(|s| s.search.clear_all()); json!({"ok":true}) }
        "reid-bypass-gate" => { with_state(|s| { s.reid.gate_passed = !s.reid.gate_passed; }); json!({"ok":true}) }
        "model-row-expand" => { let id = params.get("id").and_then(|x| x.as_str()).or_else(|| params.get("rowId").and_then(|x| x.as_str())).unwrap_or("").to_string(); with_state(|s| { s.models.expanded_id = if id.is_empty() || s.models.expanded_id.as_deref() == Some(id.as_str()) { None } else { Some(id) }; }); json!({"ok":true}) }
        "zone-select-camera" => { let id = params.get("value").and_then(|x| x.as_str()).or_else(|| params.get("camera_id").and_then(|x| x.as_str())).unwrap_or("").to_string(); with_state(|s| { s.zones.ensure_seeded(); s.zones.selected_camera_id = if id.is_empty() { None } else { Some(id) }; }); json!({"ok":true}) }
        "zone-add-start" => { with_state(|s| { s.zones.ensure_seeded(); s.zones.drawing_mode = true; s.zones.drawing_points.clear(); s.zones.selected_zone_id = None; s.zones.cursor = None; }); json!({"ok":true}) }
        "zone-cancel-drawing" => { with_state(|s| { s.zones.drawing_mode = false; s.zones.drawing_points.clear(); s.zones.cursor = None; }); json!({"ok":true}) }
        "zone-finish-drawing" => { with_state(|s| { if s.zones.drawing_points.len() < 3 { s.error_message = Some("Strefa wymaga przynajmniej 3 wierzchołków.".into()); } else { let next_id = alloc::format!("z{}", s.zones.zones.len() + 1); let name = alloc::format!("Strefa {}", s.zones.zones.len() + 1); let points = core::mem::take(&mut s.zones.drawing_points); s.zones.zones.push(ZoneFixture { id: next_id.clone(), name, zone_type: "detection".into(), points, schedule: [[true; 24]; 7], min_confidence: 0.6, models: vec!["yolo".into()], alarm_on_detect: false }); s.zones.selected_zone_id = Some(next_id); s.zones.drawing_mode = false; s.zones.cursor = None; s.success_message = Some("Dodano nową strefę.".into()); } }); json!({"ok":true}) }
        "zone-select" => { let id = params.get("zone_id").and_then(|x| x.as_str()).or_else(|| params.get("rowId").and_then(|x| x.as_str())).unwrap_or("").to_string(); with_state(|s| { s.zones.ensure_seeded(); s.zones.selected_zone_id = if id.is_empty() { None } else { Some(id) }; }); json!({"ok":true}) }
        "zone-delete" => { let id = params.get("zone_id").and_then(|x| x.as_str()).unwrap_or("").to_string(); with_state(|s| { s.zones.zones.retain(|z| z.id != id); if s.zones.selected_zone_id.as_deref() == Some(id.as_str()) { s.zones.selected_zone_id = None; } s.success_message = Some(alloc::format!("Usunięto strefę '{}'.", id)); }); json!({"ok":true}) }
        "zone-name-change" => { let v = params.get("value").and_then(|x| x.as_str()).unwrap_or("").to_string(); with_state(|s| { let sel = s.zones.selected_zone_id.clone(); if let Some(id) = sel { if let Some(z) = s.zones.find_zone_mut(&id) { z.name = v; } } }); json!({"ok":true}) }
        "zone-type-change" => { let v = params.get("value").and_then(|x| x.as_str()).unwrap_or("detection").to_string(); with_state(|s| { let sel = s.zones.selected_zone_id.clone(); if let Some(id) = sel { if let Some(z) = s.zones.find_zone_mut(&id) { z.zone_type = v; } } }); json!({"ok":true}) }
        "zone-confidence-change" => { let v = params.get("value").and_then(|x| x.as_f64()).unwrap_or(0.6); with_state(|s| { let sel = s.zones.selected_zone_id.clone(); if let Some(id) = sel { if let Some(z) = s.zones.find_zone_mut(&id) { z.min_confidence = v; } } }); json!({"ok":true}) }
        "zone-canvas-pointer" => handle_zone_canvas_pointer(params),
        "audit-filter-change" => { with_state(|s| { let id = params.get("id").and_then(|x| x.as_str()).unwrap_or(""); match id { "date_preset" => s.audit.date_preset = params.get("value").and_then(|x| x.as_str()).unwrap_or("").to_string(), "query" => s.audit.query = params.get("value").and_then(|x| x.as_str()).unwrap_or("").to_string(), _ => {} } }); json!({"ok":true}) }
        "audit-clear-filters" => { with_state(|s| s.audit.clear_filters()); json!({"ok":true}) }
        "audit-row-expand" => { let id = params.get("id").and_then(|x| x.as_str()).or_else(|| params.get("rowId").and_then(|x| x.as_str())).unwrap_or("").to_string(); with_state(|s| { s.audit.expanded_id = if id.is_empty() || s.audit.expanded_id.as_deref() == Some(id.as_str()) { None } else { Some(id) }; }); json!({"ok":true}) }
        "evidence-tab-change" => { let v = params.get("value").and_then(|x| x.as_str()).unwrap_or("active").to_string(); with_state(|s| { s.evidence.tab = match v.as_str() { "archive" => EvidenceTab::Archive, "all" => EvidenceTab::All, _ => EvidenceTab::Active }; }); json!({"ok":true}) }
        "evidence-open" => { let id = params.get("id").and_then(|x| x.as_str()).or_else(|| params.get("evidence_id").and_then(|x| x.as_str())).unwrap_or("").to_string(); with_state(|s| { s.evidence.drawer_open_id = if id.is_empty() { None } else { Some(id) }; if s.evidence.drawer_tab.is_empty() { s.evidence.drawer_tab = "summary".into(); } }); json!({"ok":true}) }
        "evidence-close-drawer" => { with_state(|s| { s.evidence.drawer_open_id = None; }); json!({"ok":true}) }
        "settings-tab-change" => { let v = params.get("value").and_then(|x| x.as_str()).or_else(|| params.get("tab_id").and_then(|x| x.as_str())).unwrap_or("general").to_string(); with_state(|s| s.settings.tab = v); json!({"ok":true}) }
        "settings-field-change" => { let tab = params.get("tab").and_then(|x| x.as_str()).unwrap_or("general").to_string(); let field = params.get("field").and_then(|x| x.as_str()).or_else(|| params.get("id").and_then(|x| x.as_str())).unwrap_or("").to_string(); let value = params.get("value").and_then(|x| x.as_str()).unwrap_or("").to_string(); with_state(|s| { if field == "notification_channel" { s.settings.notification_channel = value.clone(); }
                if !field.is_empty() { s.settings.set_field(&tab, &field, value); } }); json!({"ok":true}) }
        "settings-save" => { let tab = params.get("tab").and_then(|x| x.as_str()).unwrap_or("general").to_string(); with_state(|s| { s.success_message = Some(alloc::format!("Zapis ustawień '{}' — wymaga settings_set_v1.", tab)); }); json!({"ok":true}) }
        "onboarding-next" => { with_state(|s| { if s.onboarding.step < 3 { s.onboarding.step += 1; } }); json!({"ok":true}) }
        "onboarding-prev" => { with_state(|s| { if s.onboarding.step > 0 { s.onboarding.step -= 1; } }); json!({"ok":true}) }
        "onboarding-finish" => { with_state(|s| { s.success_message = Some("Onboarding zakończony.".into()); }); json!({"ok":true}) }
        "binding-row-expand" => { let id = params.get("id").and_then(|x| x.as_str()).or_else(|| params.get("rowId").and_then(|x| x.as_str())).unwrap_or("").to_string(); with_state(|s| s.bindings.toggle_expanded(&id)); json!({"ok":true}) }
        "binding-filter-change" => { let id = params.get("id").and_then(|x| x.as_str()).unwrap_or("").to_string(); let value = params.get("value").and_then(|x| x.as_str()).map(|s| s.to_string()).filter(|v| !v.is_empty()); with_state(|s| match id.as_str() { "addon" => s.bindings.filter_addon = value, "type" => s.bindings.filter_type = value, "status" => s.bindings.filter_status = value, _ => {} }); json!({"ok":true}) }
        "binding-clear-filters" => { with_state(|s| s.bindings.clear()); json!({"ok":true}) }
        _ => json!({"error": alloc::format!("unknown action '{}'", action)}),
    }
}

// =============================================================================
// Camera action handlers
// =============================================================================

/// Opens the "Add camera" wizard. Resets backend wizard state, eagerly
/// enumerates local USB/v4l2 devices (their Select options are static component
/// fields baked into the body sent here, so they must be known before the body
/// is built), then sends the cameras content (with the Modal) plus the wizard
/// body and footer fragments exactly once, seeding all wizard store keys via the
/// body's `state_overlay`. Every later interaction mutates the store, not the DOM.
fn handle_camera_add_show() -> JsonValue {
    with_state(|s| { s.add_form_visible = true; s.wizard_step = 0; s.discover.reset(); s.clear_messages(); });
    // Enumerate USB devices up front so the device Select can carry real options.
    let devices = camera_local_devices();
    with_state(|s| {
        s.discover.usb_loaded = true;
        if let Ok(list) = devices {
            s.discover.usb_devices = list.into_iter()
                .map(|d| LocalDevice { device_path: d.device_path, label: d.label, vendor: d.vendor })
                .collect();
        }
    });
    render_panel("cameras");
    json!({"ok":true})
}

/// Steps back one wizard step. Pure `StatePatch`: flips the step visibility /
/// footer flags and clears any error. No fragment is re-sent.
fn handle_wizard_prev() -> JsonValue {
    let step = with_state(|s| {
        if s.wizard_step > 0 { s.wizard_step -= 1; }
        s.error_message = None;
        s.wizard_step
    });
    let mut pairs = wizard_step_pairs(step);
    pairs.extend(wizard_error_pairs(None));
    send_state_patches(pairs);
    json!({"ok":true})
}

/// Commits the chosen source type (step 1) to backend state and patches the
/// per-type config visibility flags. Resetting the per-type fields here keeps a
/// switched type from carrying stale values into step 2's validation; the reset
/// is mirrored into the store so the bound inputs clear too. Pure `StatePatch` —
/// the RadioCardGroup highlight follows `wiz_src` and the config blocks toggle
/// via `wiz_is_*` without rebuilding the body.
fn handle_wizard_source_select(params: &JsonValue) -> JsonValue {
    let raw = params.get("source_type").and_then(|v| v.as_str())
        .or_else(|| params.get("value").and_then(|v| v.as_str()))
        .unwrap_or("");
    let t = match SourceType::from_str(raw) {
        Some(t) => t,
        None => return json!({"ok":false,"error":"unknown source_type"}),
    };
    let changed = with_state(|s| {
        let changed = s.discover.source_type != Some(t);
        if changed {
            s.discover.source_type = Some(t);
            s.discover.cameras.clear();
            s.discover.selected_index = None;
            s.discover.usb_device_path.clear();
            s.discover.onvif_url.clear();
            s.discover.rtsp_url.clear();
            s.discover.file_path.clear();
            s.discover.test_result = None;
        }
        s.error_message = None;
        changed
    });
    let mut pairs = wizard_source_pairs(Some(t));
    pairs.extend(wizard_error_pairs(None));
    if changed {
        // Mirror the per-type field reset into the bound store keys so any
        // previously typed value disappears from the inputs as well.
        for key in ["onvif_url", "rtsp_url", "usb_device_path", "file_path"] {
            pairs.push((key.into(), Value::Text(String::new())));
        }
        pairs.extend(wizard_test_pairs(&DiscoverState::new()));
        // Reset the ONVIF discovery sub-state (cameras were cleared above).
        pairs.extend(wizard_onvif_pairs(&DiscoverState::new()));
    }
    send_state_patches(pairs);
    json!({"ok":true})
}

/// Commits a single typed wizard field to backend state on every input change,
/// keyed by the `field` discriminator the renderer carries in `handler.params`.
/// The value already lives in the store via the input's two-way `bind_path`, so
/// this emits no `StatePatch` and no re-render — it only mirrors the value into
/// backend state for step-3 testing and submit validation.
fn handle_wizard_field_change(params: &JsonValue) -> JsonValue {
    let field = params.get("field").and_then(|v| v.as_str()).unwrap_or("");
    let value = params.get("value").and_then(|v| v.as_str()).unwrap_or("").to_string();
    with_state(|s| {
        match field {
            "onvif_url" => s.discover.onvif_url = value,
            "rtsp_url" => s.discover.rtsp_url = value,
            "usb_device_path" => s.discover.usb_device_path = value,
            "file_path" => s.discover.file_path = value,
            "cred_user" => s.discover.cred_user = value,
            "cred_pass" => s.discover.cred_pass = value,
            "name" => s.discover.name = value,
            "retention" => s.discover.retention = value,
            "fps" => s.discover.fps = value,
            "profile" => s.discover.profile = value,
            _ => {}
        }
    });
    json!({"ok":true})
}

/// Runs a real `camera_test_connection` probe for the resolved per-type target
/// and patches the step-3 result flags + text. Never fabricates success. Pure
/// `StatePatch`: the test block visibility and message follow the store.
fn handle_wizard_test() -> JsonValue {
    let target = with_state(|s| { s.discover.testing = true; s.discover.test_result = None; s.error_message = None; s.discover.resolve_target() });
    // Emit the spinner state as its own patch BEFORE the blocking probe so the
    // client paints the "testing" block; otherwise the only patch would arrive
    // after the probe returns and the spinner would never be seen.
    send_state_patches(with_state(|s| wizard_test_pairs(&s.discover)));
    let (vendor, url) = match target {
        Ok((v, u, _)) => (v, u),
        Err(msg) => {
            // Surface the validation message in the step-3 result block (the
            // dedicated error path), not the wizard-wide error alert.
            let pairs = with_state(|s| {
                s.discover.testing = false;
                s.discover.test_result = Some(Err(msg.to_string()));
                wizard_test_pairs(&s.discover)
            });
            send_state_patches(pairs);
            return json!({"ok":false,"error":"invalid target"});
        }
    };
    let result = camera_test_connection(&vendor, &url);
    let pairs = with_state(|s| {
        s.discover.testing = false;
        s.discover.test_result = Some(match result {
            Ok(out) if out.ok => Ok(out.message),
            Ok(out) => Err(out.message),
            Err(e) => Err(alloc::format!("Test nieudany: {}", abi_message(e))),
        });
        wizard_test_pairs(&s.discover)
    });
    send_state_patches(pairs);
    json!({"ok":true})
}

/// Advances the wizard, gating each transition on the current step's required
/// state: a source type must be chosen on step 1, and step 2 must resolve a
/// valid per-type target before moving on. Pure `StatePatch`: on success it
/// flips the step visibility / footer flags; on failure it patches the error
/// alert. The body is never re-sent.
fn handle_wizard_next() -> JsonValue {
    let step = with_state(|s| s.wizard_step);
    match step {
        0 => {
            let chosen = with_state(|s| s.discover.source_type.is_some());
            if !chosen {
                send_state_patches(wizard_error_pairs(Some("Wybierz typ źródła kamery.")));
                return json!({"ok":false,"error":"no source type"});
            }
            with_state(|s| { s.wizard_step = 1; s.error_message = None; });
            advance_patch(1);
            json!({"ok":true})
        }
        1 => {
            let resolved = with_state(|s| s.discover.resolve_target());
            match resolved {
                Ok(_) => {
                    with_state(|s| { s.wizard_step = 2; s.discover.test_result = None; s.error_message = None; });
                    let mut pairs = wizard_step_pairs(2);
                    pairs.extend(wizard_error_pairs(None));
                    pairs.extend(with_state(|s| wizard_test_pairs(&s.discover)));
                    send_state_patches(pairs);
                    json!({"ok":true})
                }
                Err(msg) => { send_state_patches(wizard_error_pairs(Some(msg))); json!({"ok":false,"error":"invalid target"}) }
            }
        }
        2 => {
            // Pre-fill a metadata name from a discovered camera when the user has
            // not typed one yet, otherwise leave it empty (no fake placeholder).
            let prefill_name = with_state(|s| {
                if s.discover.name.trim().is_empty() {
                    if let Some(i) = s.discover.selected_index {
                        if let Some(cam) = s.discover.cameras.get(i) {
                            s.discover.name = cam.suggested_name.clone();
                            return Some(s.discover.name.clone());
                        }
                    }
                }
                None
            });
            with_state(|s| { s.wizard_step = 3; s.error_message = None; });
            let mut pairs = wizard_step_pairs(3);
            pairs.extend(wizard_error_pairs(None));
            if let Some(name) = prefill_name {
                pairs.push(("name".into(), Value::Text(name)));
            }
            send_state_patches(pairs);
            json!({"ok":true})
        }
        _ => json!({"ok":true}),
    }
}

/// Helper: emit the step navigation patch + clear error for a forward move.
fn advance_patch(step: u8) {
    let mut pairs = wizard_step_pairs(step);
    pairs.extend(wizard_error_pairs(None));
    send_state_patches(pairs);
}

/// Reports a submit failure: surfaces the message in the wizard-wide error alert
/// (`wiz_error` + `wiz_has_error`) so it is visible inside the open modal, since
/// the wizard no longer re-renders the body on each action.
fn submit_fail(msg: &str, err_code: &str) -> JsonValue {
    with_state(|s| { s.error_message = Some(msg.to_string()); });
    send_state_patches(wizard_error_pairs(Some(msg)));
    json!({"ok":false,"error":err_code})
}

fn handle_camera_add_submit(_params: &JsonValue) -> JsonValue {
    let (target, name, fps, profile, source_type) = with_state(|s| (
        s.discover.resolve_target(),
        s.discover.name.trim().to_string(),
        s.discover.fps_value(),
        s.discover.profile_or_default().to_string(),
        s.discover.source_type,
    ));
    with_state(|s| s.clear_messages());

    if name.is_empty() || name.chars().count() > 60 {
        return submit_fail("Nazwa musi mieć 1–60 znaków.", "invalid name");
    }
    let (vendor, url, _profile_token) = match target {
        Ok(t) => t,
        Err(msg) => return submit_fail(msg, "invalid target"),
    };

    // Persist the camera to SQLite — the cameras list reads exclusively from
    // there, so the row survives panel reopen and process restart. The URL is
    // routed into onvif_url / rtsp_url depending on the chosen source type so
    // later tabs (live, zones) can pick the right transport.
    let (onvif_url, rtsp_url) = match source_type {
        Some(SourceType::Onvif) => (url.clone(), String::new()),
        _ => (String::new(), url.clone()),
    };
    let new_cam = db::NewCamera {
        name: name.clone(),
        location: profile.clone(),
        rtsp_url,
        onvif_url,
        status: "offline".into(),
        fps: i64::from(fps),
        detectors: vendor,
    };
    match db::insert_camera(&new_cam) {
        Ok(id) => {
            with_state(|s| { s.add_form_visible = false; s.discover.reset(); s.success_message = Some(alloc::format!("Kamera dodana ({}).", id)); });
            // Close the modal and refresh the camera list so the new camera
            // appears. render_panel re-sends the "cameras" content fragment
            // without the modal, which is the intended end state on success.
            render_panel("cameras");
            json!({"ok":true,"camera_id":id})
        }
        Err(e) => submit_fail(&alloc::format!("Błąd dodawania: {}", abi_message(e)), &alloc::format!("{}", e)),
    }
}

fn handle_camera_remove(params: &JsonValue) -> JsonValue {
    let camera_id = params.get("camera_id").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    with_state(|s| s.clear_messages());
    if camera_id.is_empty() { with_state(|s| { s.error_message = Some("Wybierz kamerę do usunięcia.".to_string()); }); return json!({"ok":false,"error":"empty camera_id"}); }
    match db::delete_camera(&camera_id) {
        Ok(_) => { with_state(|s| { s.camera_pending_remove = None; s.success_message = Some("Kamera usunięta.".to_string()); }); json!({"ok":true}) }
        Err(e) => { with_state(|s| { s.error_message = Some(alloc::format!("Błąd usuwania: {}", abi_message(e))); }); json!({"ok":false,"error":alloc::format!("{}",e)}) }
    }
}

// =============================================================================
// Profile action handlers
// =============================================================================

/// Mirrors a single builder field into backend profile state on change. The
/// value also lives in the store via the input's bind_path, so this only keeps
/// the backend authoritative for submit. Sliders carry a numeric value; text /
/// select fields carry a string.
fn handle_profile_field_change(params: &JsonValue) -> JsonValue {
    let field = params.get("field").and_then(|v| v.as_str()).unwrap_or("");
    let value_str = params.get("value").and_then(|v| v.as_str()).map(|s| s.to_string());
    let value_num = params.get("value").and_then(|v| v.as_f64());
    with_state(|s| match field {
        "profile_name" => { if let Some(v) = value_str { s.profiles.name = v; } }
        "profile_flow_id" => { if let Some(v) = value_str { s.profiles.flow_id = v; } }
        "profile_risk_class" => { if let Some(v) = value_str { s.profiles.risk_class = v; } }
        "profile_schedule" => { if let Some(v) = value_str { s.profiles.schedule = v; } }
        "profile_fps" => { if let Some(v) = value_num { s.profiles.fps = v; } }
        "profile_min_conf" => { if let Some(v) = value_num { s.profiles.min_confidence = v; } }
        _ => {}
    });
    json!({"ok":true})
}

/// Creates a new profile (or updates the one under edit) from the builder form.
fn handle_profile_add_submit() -> JsonValue {
    let (editing_id, name, flow_id, risk_class, schedule, cameras) = with_state(|s| (
        s.profiles.editing_id.clone(),
        s.profiles.name.trim().to_string(),
        s.profiles.flow_id.trim().to_string(),
        s.profiles.risk_class.trim().to_string(),
        s.profiles.schedule.trim().to_string(),
        s.profiles.cameras.clone(),
    ));
    with_state(|s| s.clear_messages());

    if name.is_empty() || name.chars().count() > 60 {
        with_state(|s| s.error_message = Some("Nazwa profilu musi mieć 1–60 znaków.".into()));
        render_panel("profiles");
        return json!({"ok":false,"error":"invalid name"});
    }
    let cameras_json = serde_json::to_string(&cameras).unwrap_or_else(|_| "[]".into());

    let result = match editing_id {
        Some(id) => {
            // Edit in place: re-read the row for its timestamps, then update.
            match db::get_profile(&id) {
                Ok(Some(mut row)) => {
                    row.name = name.clone();
                    row.flow_id = flow_id;
                    row.risk_class = risk_class;
                    row.schedule = schedule;
                    row.cameras = cameras_json;
                    db::update_profile(&row).map(|_| id)
                }
                Ok(None) => {
                    with_state(|s| s.error_message = Some("Profil nie istnieje.".into()));
                    render_panel("profiles");
                    return json!({"ok":false,"error":"not found"});
                }
                Err(e) => Err(e),
            }
        }
        None => {
            let new_profile = db::NewProfile {
                name: name.clone(),
                flow_id,
                risk_class,
                schedule,
                cameras: cameras_json,
                enabled: true,
            };
            db::insert_profile(&new_profile)
        }
    };

    match result {
        Ok(id) => {
            with_state(|s| {
                s.profiles.builder_visible = false;
                s.profiles.editing_id = None;
                s.success_message = Some(alloc::format!("Profil zapisany ({}).", id));
            });
            render_panel("profiles");
            json!({"ok":true,"profile_id":id})
        }
        Err(e) => {
            with_state(|s| s.error_message = Some(alloc::format!("Błąd zapisu profilu: {}", abi_message(e))));
            render_panel("profiles");
            json!({"ok":false,"error":alloc::format!("{}",e)})
        }
    }
}

/// Opens the builder pre-filled with the selected profile's persisted values.
fn handle_profile_edit(params: &JsonValue) -> JsonValue {
    let id = params.get("row_id").and_then(|v| v.as_str())
        .or_else(|| params.get("profile_id").and_then(|v| v.as_str()))
        .unwrap_or("").trim().to_string();
    if id.is_empty() {
        return json!({"ok":false,"error":"empty profile_id"});
    }
    match db::get_profile(&id) {
        Ok(Some(row)) => {
            let camera_ids = parse_profile_cameras(&row.cameras);
            with_state(|s| {
                s.clear_messages();
                s.profiles.pending_remove = None;
                s.profiles.builder_visible = true;
                s.profiles.load_for_edit(&row, camera_ids);
            });
            render_panel("profiles");
            json!({"ok":true})
        }
        Ok(None) => { with_state(|s| s.error_message = Some("Profil nie istnieje.".into())); render_panel("profiles"); json!({"ok":false,"error":"not found"}) }
        Err(e) => { with_state(|s| s.error_message = Some(alloc::format!("Błąd: {}", abi_message(e)))); render_panel("profiles"); json!({"ok":false,"error":alloc::format!("{}",e)}) }
    }
}

/// Flips the selected profile's enabled flag (drives the Dashboard "Aktywne
/// detektory" KPI, which counts profiles WHERE enabled = 1).
fn handle_profile_toggle_enabled(params: &JsonValue) -> JsonValue {
    let id = params.get("row_id").and_then(|v| v.as_str())
        .or_else(|| params.get("profile_id").and_then(|v| v.as_str()))
        .unwrap_or("").trim().to_string();
    if id.is_empty() {
        return json!({"ok":false,"error":"empty profile_id"});
    }
    with_state(|s| s.clear_messages());
    match db::get_profile(&id) {
        Ok(Some(row)) => {
            let next = !row.enabled;
            match db::toggle_profile(&id, next) {
                Ok(_) => {
                    with_state(|s| s.success_message = Some(if next { "Profil włączony.".into() } else { "Profil wyłączony.".into() }));
                    render_panel("profiles");
                    json!({"ok":true,"enabled":next})
                }
                Err(e) => { with_state(|s| s.error_message = Some(alloc::format!("Błąd: {}", abi_message(e)))); render_panel("profiles"); json!({"ok":false,"error":alloc::format!("{}",e)}) }
            }
        }
        Ok(None) => { with_state(|s| s.error_message = Some("Profil nie istnieje.".into())); render_panel("profiles"); json!({"ok":false,"error":"not found"}) }
        Err(e) => { with_state(|s| s.error_message = Some(alloc::format!("Błąd: {}", abi_message(e)))); render_panel("profiles"); json!({"ok":false,"error":alloc::format!("{}",e)}) }
    }
}

fn handle_profile_remove(params: &JsonValue) -> JsonValue {
    let id = params.get("profile_id").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    with_state(|s| s.clear_messages());
    if id.is_empty() {
        with_state(|s| s.error_message = Some("Wybierz profil do usunięcia.".into()));
        return json!({"ok":false,"error":"empty profile_id"});
    }
    match db::delete_profile(&id) {
        Ok(_) => { with_state(|s| { s.profiles.pending_remove = None; s.success_message = Some("Profil usunięty.".into()); }); render_panel("profiles"); json!({"ok":true}) }
        Err(e) => { with_state(|s| s.error_message = Some(alloc::format!("Błąd usuwania: {}", abi_message(e)))); render_panel("profiles"); json!({"ok":false,"error":alloc::format!("{}",e)}) }
    }
}

/// Runs ONVIF discovery. The discovered camera cards are a genuinely dynamic
/// list (count + per-row click handlers), so this re-sends the `add_camera_body`
/// fragment — the only wizard action besides modal open that does. The
/// discovery-section visibility flags are patched to match the new results.
fn handle_discover_scan() -> JsonValue {
    with_state(|s| { s.discover.scanning = true; s.discover.error_message = None; s.discover.cameras.clear(); s.discover.selected_index = None; s.error_message = None; });
    // Patch the scanning flag BEFORE the blocking discovery call so the scan
    // spinner becomes visible; the final fragment re-send below carries results.
    send_state_patches(with_state(|s| wizard_onvif_pairs(&s.discover)));
    let result = camera_discover();
    with_state(|s| {
        s.discover.scanning = false;
        match result {
            Ok(found) => { s.discover.cameras = found.iter().map(discovered_to_cam).collect(); }
            Err(e) => { s.error_message = Some(alloc::format!("Błąd skanowania: {}", abi_message(e))); }
        }
    });
    if with_state(|s| s.add_form_visible) {
        send_slot_content_with_overlay("add_camera_body", build_add_camera_body(), Some(wizard_full_overlay()));
    }
    json!({"ok":true})
}

/// Selects a discovered ONVIF camera. Mirrors the picked device URL into the
/// manual ONVIF field and re-sends the body (the row highlight is part of the
/// dynamic discovered-list fragment, not a store flag), seeding the full wizard
/// overlay so `onvif_url` and the row selection both reflect the pick.
fn handle_discover_select(params: &JsonValue) -> JsonValue {
    let index = params.get("index").and_then(|v| v.as_u64());
    with_state(|s| {
        s.error_message = None;
        match index {
            Some(i) if (i as usize) < s.discover.cameras.len() => {
                let i = i as usize;
                s.discover.selected_index = Some(i);
                // Mirror the picked device URL into the manual ONVIF field so the
                // resolved target and submit credentials use one consistent value.
                s.discover.onvif_url = s.discover.cameras[i].url.clone();
            }
            _ => { s.discover.selected_index = None; s.error_message = Some("Wybierz kamerę z listy.".to_string()); }
        }
    });
    if with_state(|s| s.add_form_visible) {
        send_slot_content_with_overlay("add_camera_body", build_add_camera_body(), Some(wizard_full_overlay()));
    }
    json!({"ok":true})
}

fn handle_zone_canvas_pointer(params: &JsonValue) -> JsonValue {
    let x = params.get("x").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let y = params.get("y").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let event = params.get("event").and_then(|v| v.as_str()).unwrap_or("move");
    with_state(|s| {
        if !s.zones.drawing_mode { return; }
        match event {
            "click" => s.zones.drawing_points.push(ZonePoint { x, y }),
            _ => s.zones.cursor = Some(ZonePoint { x, y }),
        }
    });
    json!({"ok":true})
}

// =============================================================================
// Validation helpers
// =============================================================================

/// Resolves wizard credential inputs into the `credentials_b64` field the host
/// expects on `camera_add`. The host decodes this as STANDARD base64 of a
/// `user:pass` string and requires it for `vendor == "onvif"` while treating it
/// as optional for `rtsp`. We enforce the same rule here so an ONVIF camera
/// surfaces a readable wizard error instead of the host's raw
/// `missing_credentials` rejection. Returning `Ok(None)` means "no credentials"
/// (valid only for non-ONVIF vendors). The plaintext lives only for the span of
/// this call and is never logged.
fn build_credentials_b64(
    vendor: &str,
    user: &str,
    pass: &str,
) -> Result<Option<String>, &'static str> {
    let user = user.trim();
    let pass = pass.trim();
    if user.is_empty() && pass.is_empty() {
        if vendor == "onvif" {
            return Err("Kamera ONVIF wymaga użytkownika i hasła.");
        }
        return Ok(None);
    }
    if user.is_empty() {
        return Err("Podaj użytkownika kamery.");
    }
    if pass.is_empty() {
        return Err("Podaj hasło kamery.");
    }
    let plain = alloc::format!("{user}:{pass}");
    Ok(Some(
        base64::engine::Engine::encode(&base64::engine::general_purpose::STANDARD, plain),
    ))
}

fn is_valid_camera_id(id: &str) -> bool {
    if id.len() != 40 || !id.starts_with("cam_") { return false; }
    id.chars().skip(4).all(|c| c.is_ascii_hexdigit() || c == '-')
}

/// Maps a host-discovered ONVIF device into the wizard's working representation.
/// ONVIF WS-Discovery reports `xaddrs` (device service endpoints) rather than a
/// ready stream URL; the first `xaddr` is the canonical ONVIF service URL, and we
/// fall back to the device-service path on the bare `address` when none is given.
fn discovered_to_cam(d: &DiscoveredCameraOut) -> DiscoveredCam {
    let url = d
        .xaddrs
        .first()
        .cloned()
        .filter(|x| !x.trim().is_empty())
        .unwrap_or_else(|| alloc::format!("http://{}/onvif/device_service", d.address));
    DiscoveredCam {
        vendor: "onvif".to_string(),
        url,
        suggested_name: suggested_name_for_discovered(d),
        profile_token: None,
    }
}

fn suggested_name_for_discovered(d: &DiscoveredCameraOut) -> String {
    let make = d.manufacturer.trim();
    let model = d.model.trim();
    match (make.is_empty(), model.is_empty()) {
        (false, false) => alloc::format!("{} {}", make, model),
        (false, true) => make.to_string(),
        (true, false) => model.to_string(),
        (true, true) => {
            let host = extract_host_port(&alloc::format!("onvif://{}", d.address))
                .map(|(h, _)| h)
                .filter(|h| !h.is_empty())
                .unwrap_or_else(|| d.address.clone());
            alloc::format!("ONVIF — {}", host)
        }
    }
}

fn extract_host_port(url: &str) -> Option<(String, Option<u16>)> {
    let after_scheme = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
    let authority = after_scheme.split(['/', '?', '#']).next().unwrap_or("");
    if authority.is_empty() { return None; }
    let host_part = authority.rsplit_once('@').map(|(_, h)| h).unwrap_or(authority);
    if let Some(rest) = host_part.strip_prefix('[') {
        if let Some((host_inner, tail)) = rest.split_once(']') {
            let host = alloc::format!("[{}]", host_inner);
            let port = tail.strip_prefix(':').and_then(|p| p.parse::<u16>().ok());
            return Some((host, port));
        }
        return Some((host_part.to_string(), None));
    }
    if let Some((host, port)) = host_part.rsplit_once(':') {
        if let Ok(p) = port.parse::<u16>() { return Some((host.to_string(), Some(p))); }
    }
    Some((host_part.to_string(), None))
}

fn abi_message(e: AbiError) -> &'static str {
    match e {
        AbiError::Permission => "brak uprawnień",
        AbiError::NotFound => "nie znaleziono",
        AbiError::Conflict => "konflikt (duplikat?)",
        AbiError::QuotaExceeded => "przekroczono limit",
        AbiError::CameraUnreachable => "kamera nieosiągalna",
        AbiError::CameraAuthFailed => "błąd uwierzytelniania kamery",
        AbiError::CameraVendorUnsupported => "nieobsługiwany typ kamery",
        AbiError::PayloadTooLarge => "zbyt duży payload",
        AbiError::Timeout => "przekroczono czas oczekiwania",
        AbiError::NoAvailableTarget => "brak dostępnego targetu",
        _ => "błąd operacji",
    }
}

// =============================================================================
// NavTabs construction
// =============================================================================

fn build_nav_tab_items(_active: &str) -> Vec<NavTab> {
    let entries: &[(&str, &str, &str)] = &[
        ("overview", "Dashboard", "dashboard"),
        ("live", "Live view", "video"),
        ("cameras", "Kamery", "cameras"),
        ("profiles", "Profile analityczne", "brain"),
        ("alarms", "Alarmy", "bell"),
        ("search", "Wyszukiwarka", "search"),
        ("reid", "Re-ID", "users"),
        ("models", "Modele", "cpu"),
        ("zones", "Strefy i reguły", "zones"),
        ("bindings", "Powiązania", "link"),
        ("audit", "Audyt i RODO", "audit"),
        ("evidence", "Eksport dowodowy", "evidence"),
        ("settings", "Ustawienia", "settings"),
    ];
    entries.iter().map(|(id, label, icon)| {
        NavTab {
            id: (*id).into(),
            label: lit(label),
            icon: Some(icon_named(parse_icon_name(icon))),
            badge: None,
            panel_id: Some((*id).into()),
            locked: false,
        }
    }).collect()
}

// =============================================================================
// Panel content builders
// =============================================================================

fn build_overview_content() -> Component {
    let messages = build_messages_section();

    // KPI tiles — every number is computed from SQLite.
    let total_cams = db::count_cameras().unwrap_or(0);
    let online_cams = db::count_online_cameras().unwrap_or(0);
    let offline_cams = (total_cams - online_cams).max(0);
    let active_detectors = db::count_active_profiles().unwrap_or(0);
    let alarms_24h = db::count_alarms_last_24h().unwrap_or(0);
    let critical_24h = db::count_critical_alarms_last_24h().unwrap_or(0);

    let cam_val = alloc::format!("{} / {}", online_cams, total_cams);
    let cam_note = if total_cams == 0 {
        "Brak skonfigurowanych kamer".to_string()
    } else if offline_cams > 0 {
        alloc::format!("{} offline", offline_cams)
    } else {
        "wszystkie online".to_string()
    };
    let alarms_note = if critical_24h > 0 {
        alloc::format!("{} krytycznych", critical_24h)
    } else {
        "brak krytycznych".to_string()
    };
    let alarms_tone = if critical_24h > 0 { "danger" } else { "success" };

    let kpi_row = grid(4, vec![
        stat_card(&cam_val, "Aktywne kamery", Some(&cam_note), Some("cameras"),
                  Some(if offline_cams > 0 { "warning" } else { "success" })),
        stat_card(&alloc::format!("{}", active_detectors), "Aktywne detektory", None, Some("brain"), Some("accent")),
        stat_card(&alloc::format!("{}", alarms_24h), "Alarmy 24h", Some(&alarms_note), Some("bell"), Some(alarms_tone)),
        stat_card("68%", "GPU / latencja p95", Some("1.2 s"), Some("cpu"), Some("success")),
    ]);

    // Latest alarms — newest first, joined with the camera name.
    let recent = db::list_recent_alarms(6).unwrap_or_default();
    let alarms_body = if recent.is_empty() {
        // No outer card: empty_state sits straight inside the section card body.
        empty_state("Brak alarmów", Some("Gdy analityka wykryje zdarzenie, pojawi się tutaj."), Some("bell"))
    } else {
        let rows: Vec<Component> = recent.iter().map(build_alarm_card).collect();
        stack_v_gap("sm", rows)
    };
    let alarms_header_label = alloc::format!("Wszystkie {} >", alarms_24h);
    let recent_alarms = card_with_icon_action("Ostatnie alarmy", "bell", Some(&alarms_header_label), vec![alarms_body]);

    let runtime = card_with_icon("Stan natywnego runtime", "cpu", vec![
        build_runtime_table(),
    ]);

    let two_col = grid(2, vec![recent_alarms, runtime]);

    let heatmap_card = build_activity_heatmap();

    stack_v(vec![messages, kpi_row, two_col, heatmap_card])
}

/// Maps an alarm severity token to a badge variant. Persisted severities are
/// `critical` / `warning` / `info`; anything else degrades to a neutral info pill.
fn alarm_severity_variant(severity: &str) -> &'static str {
    match severity {
        "critical" => "danger",
        "warning" => "warning",
        _ => "info",
    }
}

fn alarm_severity_label(severity: &str) -> &'static str {
    match severity {
        "critical" => "krytyczne",
        "warning" => "ostrzeżenie",
        _ => "info",
    }
}

/// Formats a unix timestamp as HH:MM:SS in UTC. The dashboard only needs the
/// clock face for the "latest alarms" list; a full date column lives in the
/// Alarms tab.
fn format_alarm_time(ts: i64) -> String {
    if ts <= 0 {
        return "—".into();
    }
    let secs_in_day = ts.rem_euclid(86_400);
    let h = secs_in_day / 3600;
    let m = (secs_in_day % 3600) / 60;
    let s = secs_in_day % 60;
    alloc::format!("{:02}:{:02}:{:02}", h, m, s)
}

/// Static alarm row used by the Alarms tab (a later tab-by-tab effort). The
/// dashboard uses `build_alarm_card` with real `db::AlarmRow` data instead.
fn build_alarm_row(title: &str, camera: &str, time: &str, severity: &str) -> Component {
    let title_text = text_styled(title, "body_strong");
    let meta_row = stack_h_gap("sm", vec![
        chip_with_icon(camera, "category", "cameras"),
        chip_with_icon(time, "category", "clock"),
        badge(alarm_severity_label(match severity { "danger" => "critical", other => other }), severity),
    ]);
    let center = stack_v_gap("xs", vec![title_text, meta_row]);
    let action = ButtonComp {
        variant: ButtonVariant::Ghost,
        tone: Tone::Neutral,
        label: lit("Otwórz"),
        icon_leading: None,
        icon_trailing: None,
        size: ButtonSize::Sm,
        full_width: false,
        disabled: None,
        loading: None,
        density: Density::Default,
    }.into_component(next_id()).expect("Button");
    Flex {
        direction: FlexDirection::Row,
        gap: Spacing::Md,
        justify: FlexJustify::SpaceBetween,
        align: FlexAlign::Center,
        wrap: FlexWrap::NoWrap,
        children: vec![center, action],
        padding: Some(Spacing::Sm),
        background: None,
        radius: None,
    }.into_component(next_id()).expect("Flex")
}

fn build_alarm_card(a: &db::AlarmRow) -> Component {
    // Title: detector type + message, falling back to whichever is present.
    let title = if !a.kind.is_empty() && !a.message.is_empty() {
        alloc::format!("{} · {}", a.kind, a.message)
    } else if !a.message.is_empty() {
        a.message.clone()
    } else if !a.kind.is_empty() {
        a.kind.clone()
    } else {
        "Zdarzenie".into()
    };
    let camera_label = if !a.camera_name.is_empty() {
        a.camera_name.clone()
    } else if !a.camera_id.is_empty() {
        a.camera_id.clone()
    } else {
        "—".into()
    };
    let variant = alarm_severity_variant(&a.severity);

    let title_text = text_styled(&title, "body_strong");
    let meta_row = stack_h_gap("sm", vec![
        chip_with_icon(&camera_label, "category", "cameras"),
        chip_with_icon(&format_alarm_time(a.ts), "category", "clock"),
        badge(alarm_severity_label(&a.severity), variant),
    ]);
    let center = stack_v_gap("xs", vec![title_text, meta_row]);

    let action = ButtonComp {
        variant: ButtonVariant::Ghost,
        tone: Tone::Neutral,
        label: lit("Otwórz"),
        icon_leading: None,
        icon_trailing: None,
        size: ButtonSize::Sm,
        full_width: false,
        disabled: None,
        loading: None,
        density: Density::Default,
    }.into_component(next_id()).expect("Button");

    Flex {
        direction: FlexDirection::Row,
        gap: Spacing::Md,
        justify: FlexJustify::SpaceBetween,
        align: FlexAlign::Center,
        wrap: FlexWrap::NoWrap,
        children: vec![center, action],
        padding: Some(Spacing::Sm),
        background: None,
        radius: None,
    }.into_component(next_id()).expect("Flex")
}

fn build_runtime_kv_row(label: &str, value_children: Vec<Component>) -> Component {
    Flex {
        direction: FlexDirection::Row,
        gap: Spacing::Sm,
        justify: FlexJustify::SpaceBetween,
        align: FlexAlign::Center,
        wrap: FlexWrap::NoWrap,
        children: vec![
            text_styled(label, "caption"),
            Flex {
                direction: FlexDirection::Row,
                gap: Spacing::Xs,
                justify: FlexJustify::End,
                align: FlexAlign::Center,
                wrap: FlexWrap::NoWrap,
                children: value_children,
                padding: None,
                background: None,
                radius: None,
            }.into_component(next_id()).expect("Flex"),
        ],
        padding: None,
        background: None,
        radius: None,
    }.into_component(next_id()).expect("Flex")
}

fn build_runtime_table() -> Component {
    stack_v_gap("sm", vec![
        build_runtime_kv_row("Frame bus throughput", vec![
            text("312 fps łącznie"),
        ]),
        build_runtime_kv_row("Queue depth (max)", vec![
            text("12 (kam. C-04, tier 0)"),
        ]),
        build_runtime_kv_row("Drop rate 1h", vec![
            chip_toned("0.4%", "success"),
        ]),
        build_runtime_kv_row("VRAM użycie", vec![
            text("8.2 / 12 GB"),
            chip_toned("tight", "warning"),
        ]),
        build_runtime_kv_row("Modele załadowane", vec![
            text("6 (YOLO11m, PP-OCRv5, BoT-SORT…)"),
        ]),
        build_runtime_kv_row("Clock-sync dryf max", vec![
            text("12 ms (PTP OK)"),
        ]),
        build_runtime_kv_row("Audit log → WORM", vec![
            chip_toned("synced (5 min temu)", "success"),
        ]),
        build_runtime_kv_row("Eval harness (daily)", vec![
            text("ostatnio 03:00"),
            chip_toned("P/R w celu", "success"),
        ]),
    ])
}

const HEATMAP_COLS: usize = 24;
/// Cap the number of camera rows so a large fleet does not produce an unwieldy
/// grid; the dashboard heatmap is an at-a-glance overview, not the Alarms tab.
const HEATMAP_MAX_ROWS: usize = 12;

/// The cameras shown as heatmap rows, in stable display order (by name). Shared
/// by `build_activity_heatmap` (labels) and `heatmap_cells_value` (data) so row
/// indices line up with the `rN` ids the heatmap helper generates.
fn heatmap_camera_rows() -> Vec<db::CameraRow> {
    let mut cams = db::list_cameras().unwrap_or_default();
    cams.truncate(HEATMAP_MAX_ROWS);
    cams
}

fn build_activity_heatmap() -> Component {
    let cams = heatmap_camera_rows();
    let col_labels: Vec<&str> = (0..HEATMAP_COLS)
        .map(|h| match h {
            0 => "0", 2 => "2", 4 => "4", 6 => "6", 8 => "8", 10 => "10",
            12 => "12", 14 => "14", 16 => "16", 18 => "18", 20 => "20", 22 => "22",
            _ => "",
        })
        .collect();

    if cams.is_empty() {
        // No cameras → no rows to chart; keep a clean empty state rather than an
        // empty zero-row grid the renderer would draw as a bare header strip.
        return card_with_icon(
            "Mapa cieplna aktywności · ostatnie 24h × kamera",
            "dashboard",
            vec![empty_state("Brak danych aktywności", Some("Dodaj kamery, aby zbierać aktywność 24h."), Some("dashboard"))],
        );
    }

    let row_labels: Vec<&str> = cams.iter().map(|c| c.name.as_str()).collect();
    // Values come from the store via `heatmap_cells`; the literal grid passed
    // here is unused by the renderer (the helper ignores `_values`), so pass an
    // empty placeholder of the right shape.
    let values: Vec<Vec<f64>> = Vec::new();
    card_with_icon("Mapa cieplna aktywności · ostatnie 24h × kamera", "dashboard", vec![
        heatmap(cams.len() as u32, HEATMAP_COLS as u32, values, row_labels, col_labels),
    ])
}

/// Builds the `heatmap_cells` store value (`[{row_id, col_id, value}]`) from the
/// real per-camera-per-hour alarm aggregate. `value` is normalized to 0..1 so
/// the linear scale + tf-heatmap level buckets light up; the busiest cell in the
/// window maps to 1.0. Cameras/hours with no alarms are emitted as 0 so the grid
/// is fully populated (no blank rows), matching the mockup's dense look.
fn heatmap_cells_value() -> Value {
    let cams = heatmap_camera_rows();
    if cams.is_empty() {
        return Value::Array(Vec::new());
    }
    let buckets = db::alarm_heatmap_last_24h().unwrap_or_default();
    let row_index: alloc::collections::BTreeMap<&str, usize> =
        cams.iter().enumerate().map(|(i, c)| (c.id.as_str(), i)).collect();

    // Dense count grid [row][hour], then normalize by the global max.
    let mut counts = alloc::vec![[0i64; HEATMAP_COLS]; cams.len()];
    let mut max_count = 0i64;
    for b in &buckets {
        if let Some(&r) = row_index.get(b.camera_id.as_str()) {
            let h = (b.hour_offset as usize).min(HEATMAP_COLS - 1);
            counts[r][h] += b.count;
            max_count = max_count.max(counts[r][h]);
        }
    }
    let denom = if max_count > 0 { max_count as f64 } else { 1.0 };

    let mut cells: Vec<Value> = Vec::with_capacity(cams.len() * HEATMAP_COLS);
    for (r, row) in counts.iter().enumerate() {
        for (h, &c) in row.iter().enumerate() {
            let value = (c as f64) / denom;
            cells.push(Value::Map(vec![
                (Value::Text("row_id".into()), Value::Text(alloc::format!("r{}", r))),
                (Value::Text("col_id".into()), Value::Text(alloc::format!("c{}", h))),
                (Value::Text("value".into()), Value::F64(value)),
            ]));
        }
    }
    Value::Array(cells)
}

fn build_messages_section() -> Component {
    let (err, succ) = with_state(|s| (s.error_message.clone(), s.success_message.clone()));
    let mut children = Vec::new();
    if let Some(e) = err { children.push(alert(&e, "danger")); }
    if let Some(s) = succ { children.push(alert(&s, "success")); }
    if children.is_empty() { return divider(); }
    stack_v_gap("sm", children)
}

fn build_live_content() -> Component {
    let messages = build_messages_section();
    let cameras = match camera_list() {
        Ok(c) => c,
        Err(e) => {
            return stack_v(vec![
                messages,
                alert(&alloc::format!("Nie udało się pobrać kamer: {}", abi_message(e)), "critical"),
            ]);
        }
    };
    if cameras.is_empty() {
        return stack_v(vec![
            messages,
            // No outer Outlined Card: matches the dashboard stack so the empty
            // state does not get a stray white frame around it.
            empty_state("Brak kamer", Some("Dodaj kamerę aby zobaczyć podgląd na żywo."), Some("video")),
        ]);
    }
    // Live tiles subscribe to the core's per-camera fMP4 publisher over the
    // binary protocol (`camera:<id>` → streamSubscribeRequest). The raw camera
    // URL is never handed to the browser — it cannot play rtsp(s):// directly.
    let streams: Vec<Component> = cameras.iter().take(4).map(|c| {
        card(Some(&c.display_name), vec![video_stream(&alloc::format!("camera:{}", c.camera_id))])
    }).collect();
    stack_v(core::iter::once(messages).chain(streams).collect())
}

fn build_cameras_content() -> Component {
    let list_result = db::list_cameras();
    let messages = build_messages_section();
    let (add_visible, filter) = with_state(|s| (s.add_form_visible, s.cameras_filter.clone()));

    let mut children = vec![messages];

    // Header: heading + search + add button
    let search_input = with_a11y_label({
        use tentaflow_sdk_spec::protocol::ui::form::Input;
        Input {
            r#type: InputType::Search,
            bind_path: StatePath::new(vec![PathSegment::Key("cameras_search".into())]),
            placeholder: Some(lit("Szukaj po nazwie, IP, vendorze...")),
            label: None,
            hint: None,
            leading_icon: Some(icon_named(parse_icon_name("search"))),
            trailing_icon: None,
            prefix: None,
            suffix: None,
            validators: vec![],
            max_length: None,
            min_length: None,
            pattern: None,
            autocomplete: None,
            input_mode: None,
            disabled: None,
            readonly: None,
            error: None,
            size: InputSize::Md,
        }.into_component("cameras_search").expect("Input")
    }, "Szukaj kamer");
    let toolbar = stack_h(vec![
        heading(2, "Kamery"),
        search_input,
        button_with_icon("Dodaj kamerę", "camera-add-show", "primary", "plus"),
    ]);
    children.push(toolbar);

    // A DB/permission error must never be masked as "no cameras"; surface the
    // real reason and stop rendering the list.
    let cameras = match list_result {
        Ok(c) => c,
        Err(e) => {
            children.push(alert(&alloc::format!("Nie udało się pobrać kamer: {}", abi_message(e)), "critical"));
            return stack_v(children);
        }
    };

    // Filter counts derived from persisted camera status.
    let total = cameras.len();
    let online = cameras.iter().filter(|c| c.status == "online").count();
    let offline = cameras.iter().filter(|c| c.status == "offline").count();
    let warnings = cameras.iter().filter(|c| camera_row_has_warning(c)).count();

    let active_filter = if filter.is_empty() { "all" } else { &filter };
    let sub_tabs = filter_chips(
        vec![
            FilterChipDef { id: "all".into(), label: lit(&alloc::format!("Wszystkie ({})", total)), icon: None, badge: None, count_path: None },
            FilterChipDef { id: "online".into(), label: lit(&alloc::format!("Online ({})", online)), icon: None, badge: None, count_path: None },
            FilterChipDef { id: "offline".into(), label: lit(&alloc::format!("Offline ({})", offline)), icon: None, badge: None, count_path: None },
            FilterChipDef { id: "warnings".into(), label: lit(&alloc::format!("Ostrzeżenia ({})", warnings)), icon: None, badge: None, count_path: None },
        ],
        active_filter,
    );
    children.push(sub_tabs);

    // The "Add camera" wizard renders as a Modal overlay. While visible, the
    // Modal shell is placed in the content tree; its body/footer are filled by
    // SlotContent (see render_panel). When hidden, the Modal is absent so the
    // host unregisters its dynamic body/footer slots.
    if add_visible {
        children.push(build_add_camera_modal());
    }

    let filtered: Vec<&db::CameraRow> = cameras
        .iter()
        .filter(|c| match active_filter {
            "online" => c.status == "online",
            "offline" => c.status == "offline",
            "warnings" => camera_row_has_warning(c),
            _ => true,
        })
        .collect();

    // A delete-confirmation bar appears above the table once a row is selected.
    if let Some(pending) = with_state(|s| s.camera_pending_remove.clone()) {
        if cameras.iter().any(|c| c.id == pending) {
            children.push(build_camera_remove_confirm(&pending, &cameras));
        } else {
            with_state(|s| s.camera_pending_remove = None);
        }
    }

    if cameras.is_empty() {
        // No outer Outlined Card: the dashboard pushes its sections straight
        // into the stack, so wrapping these in card(None, ...) would draw a
        // double container (a stray white frame around the content).
        children.push(empty_state("Brak kamer", Some("Dodaj kamerę aby rozpocząć monitorowanie."), Some("cameras")));
    } else {
        // Stash the filtered rows (read from SQLite) for render_panel to seed
        // into the content slot's state_overlay under the Table's rows_path, so
        // the Table mounts with rows present in its first store snapshot.
        let rows: Vec<Value> = filtered.iter().map(|c| camera_table_row_value(c)).collect();
        if let Ok(mut g) = PENDING_CAMERA_ROWS.lock() {
            *g = Some(Value::Array(rows));
        }
        // Table carries its own surface styling; an extra Outlined Card here
        // would nest a white frame around it, unlike the dashboard layout.
        children.push(build_cameras_table());
    }

    stack_v(children)
}

/// A camera is "warning" when its persisted status is neither cleanly online
/// nor offline (e.g. "degraded").
fn camera_row_has_warning(c: &db::CameraRow) -> bool {
    c.status != "online" && c.status != "offline"
}

/// Renders the persisted address: ONVIF url if present, else RTSP url.
fn camera_row_addr(c: &db::CameraRow) -> String {
    let addr = if !c.onvif_url.trim().is_empty() { &c.onvif_url } else { &c.rtsp_url };
    if addr.trim().is_empty() { "\u{2014}".to_string() } else { redact_url_for_display(addr) }
}

/// FPS cell: configured target fps, or em-dash when 0.
fn camera_row_fps(c: &db::CameraRow) -> String {
    if c.fps > 0 { alloc::format!("{}", c.fps) } else { "\u{2014}".to_string() }
}

/// Builds one Table row as a `Value::Map` keyed by the column field paths.
/// `camera_id` is the row key the Table uses to scope per-row actions.
/// Builds a toned chip cell value `{ label, status }`. The data-table renderer
/// honors `status` for chip columns so status pills / risk badges render their
/// mockup colors (ok=green, warn=amber, err=red, muted=grey) instead of a flat
/// neutral tone.
fn chip_cell(label: &str, status: &str) -> Value {
    Value::Map(vec![
        (Value::Text("label".into()), Value::Text(label.to_string())),
        (Value::Text("status".into()), Value::Text(status.to_string())),
    ])
}

/// Maps a persisted camera status to a chip label + tone.
fn camera_status_cell(status: &str) -> Value {
    match status {
        "online" => chip_cell("online", "ok"),
        "offline" => chip_cell("offline", "err"),
        other => chip_cell(other, "warn"),
    }
}

fn camera_table_row_value(c: &db::CameraRow) -> Value {
    let location = if c.location.trim().is_empty() { "\u{2014}".to_string() } else { c.location.clone() };
    let detectors = if c.detectors.trim().is_empty() { "\u{2014}".to_string() } else { c.detectors.clone() };
    let entries: Vec<(Value, Value)> = vec![
        (Value::Text("camera_id".into()), Value::Text(c.id.clone())),
        (Value::Text("name".into()), Value::Text(c.name.clone())),
        (Value::Text("location".into()), Value::Text(location)),
        (Value::Text("addr".into()), Value::Text(camera_row_addr(c))),
        (Value::Text("status".into()), camera_status_cell(&c.status)),
        (Value::Text("detectors".into()), Value::Text(detectors)),
        (Value::Text("fps".into()), Value::Text(camera_row_fps(c))),
    ];
    Value::Map(entries)
}

fn camera_table_column(id: &str, header: &str, render: ColumnRender) -> TableColumn {
    TableColumn {
        id: id.into(),
        header: lit(header),
        field_path: vec![PathSegment::Key(id.into())],
        width: TableColumnWidth::Auto,
        render,
        format: None,
        align: None,
        sortable: true,
        hidden_by_default: false,
        sticky_left: false,
    }
}

fn build_cameras_table() -> Component {
    let columns = vec![
        camera_table_column("name", "Nazwa", ColumnRender::Text),
        camera_table_column("location", "Lokalizacja", ColumnRender::Text),
        camera_table_column("addr", "Adres", ColumnRender::Text),
        camera_table_column("status", "Status", ColumnRender::Chip),
        camera_table_column("detectors", "Detektory", ColumnRender::Text),
        camera_table_column("fps", "FPS", ColumnRender::Text),
    ];

    // The per-row "⋯" menu carries the deletion action. The Table renderer
    // injects the row key into the menu-item action params as both `row_id`
    // and the concrete `row_key_field` (`camera_id`), so this Button dispatches
    // `camera-row-select` with the clicked camera_id. Deletion stays gated:
    // `camera-row-select` only arms the pending-remove confirmation bar
    // (`build_camera_remove_confirm`); the real `camera-remove` runs from that
    // bar's explicit Usuń button.
    let remove_action = button("Usuń", "camera-row-select", "destructive");

    TableComp {
        columns,
        rows_path: StatePath::new(vec![PathSegment::Key("cameras_rows".into())]),
        row_key_field: "camera_id".into(),
        variant: TableVariant::Default,
        density: Density::Default,
        sortable: true,
        sort_by: None,
        selectable: TableSelectMode::None,
        selected_ids: None,
        sticky_header: true,
        sticky_columns: 0,
        pagination: None,
        empty_state: None,
        row_actions: vec![remove_action],
        bulk_actions: vec![],
        virtualize: false,
        row_expandable: false,
        expanded_row_template_id: None,
    }.into_component(next_id()).expect("Table")
}

/// Confirmation bar for deleting the selected camera. Usuń dispatches
/// `camera-remove` with the explicit `camera_id`; Anuluj clears the selection.
fn build_camera_remove_confirm(camera_id: &str, cameras: &[db::CameraRow]) -> Component {
    let name = cameras
        .iter()
        .find(|c| c.id == camera_id)
        .map(|c| c.name.as_str())
        .unwrap_or(camera_id);

    let mut params = CborMap::default();
    params.0.push(("camera_id".into(), Value::Text(camera_id.into())));

    let confirm_btn = button_with_params("Usuń", "camera-remove", "destructive", params);
    let cancel_btn = button("Anuluj", "camera-remove-cancel", "ghost");

    card(None, vec![stack_v(vec![
        text_styled(&alloc::format!("Usunąć kamerę \"{}\"?", name), "body_strong"),
        text("Tej operacji nie można cofnąć."),
        stack_h(vec![confirm_btn, cancel_btn]),
    ])])
}

/// A camera is "warning" when it is not cleanly online or carries a
/// status_message diagnostic from the supervisor.
fn camera_has_warning(c: &CameraInfoOut) -> bool {
    if c.status != "online" && c.status != "offline" {
        return true;
    }
    c.status_message.as_deref().map(|m| !m.trim().is_empty()).unwrap_or(false)
}

/// Renders "<actual>/<target>" fps when a live measurement exists, otherwise the
/// configured target only, or em-dash for offline cameras with no target.
fn camera_fps_display(c: &CameraInfoOut) -> String {
    match c.fps_actual {
        Some(actual) if c.status == "online" => alloc::format!("{:.0}/{}", actual, c.target_fps),
        _ if c.target_fps > 0 => alloc::format!("\u{2014}/{}", c.target_fps),
        _ => "\u{2014}".to_string(),
    }
}

/// Maps a camera into a diagnostics label + tone for the table cell.
fn camera_diagnostics(c: &CameraInfoOut) -> (String, &'static str) {
    if let Some(msg) = c.status_message.as_deref().filter(|m| !m.trim().is_empty()) {
        let tone = if c.status == "online" { "warning" } else { "critical" };
        return (msg.to_string(), tone);
    }
    match c.status.as_str() {
        "online" => ("OK".to_string(), "success"),
        "offline" => ("offline".to_string(), "critical"),
        other => (other.to_string(), "warning"),
    }
}


/// Total number of wizard steps. The wizard is 0-indexed internally.
const ADD_CAMERA_WIZARD_STEPS: u8 = 4;

/// Store key carrying the active wizard step id (`"step0".."step3"`) consumed by
/// `StepProgress.current_id_path`.
fn wiz_step_id(step: u8) -> String {
    alloc::format!("step{}", step)
}

/// Builds the per-step visibility / navigation patch pairs for `step`. Every
/// step container and footer button reads one of these booleans through
/// `with_visible`, so navigation is a pure `StatePatch` — no fragment rebuild.
fn wizard_step_pairs(step: u8) -> Vec<(String, Value)> {
    let last = ADD_CAMERA_WIZARD_STEPS - 1;
    vec![
        ("wiz_step".into(), Value::Text(wiz_step_id(step))),
        ("wiz_show_0".into(), Value::Bool(step == 0)),
        ("wiz_show_1".into(), Value::Bool(step == 1)),
        ("wiz_show_2".into(), Value::Bool(step == 2)),
        ("wiz_show_3".into(), Value::Bool(step == 3)),
        ("wiz_show_back".into(), Value::Bool(step > 0)),
        ("wiz_show_next".into(), Value::Bool(step < last)),
        ("wiz_show_finish".into(), Value::Bool(step >= last)),
    ]
}

/// Visibility pairs for the four per-type config blocks of step 2. Exactly one
/// is `true` for the chosen source; switching type is a `StatePatch` that flips
/// these, revealing the matching config without rebuilding the body.
fn wizard_source_pairs(src: Option<SourceType>) -> Vec<(String, Value)> {
    let s = src.map(SourceType::as_str).unwrap_or("");
    vec![
        ("wiz_src".into(), Value::Text(s.into())),
        ("wiz_is_onvif".into(), Value::Bool(src == Some(SourceType::Onvif))),
        ("wiz_is_rtsp".into(), Value::Bool(src == Some(SourceType::Rtsp))),
        ("wiz_is_usb".into(), Value::Bool(src == Some(SourceType::Usb))),
        ("wiz_is_file".into(), Value::Bool(src == Some(SourceType::File))),
    ]
}

/// Patch pairs describing the step-2 ONVIF discovery sub-state (scan spinner,
/// discovered-list visibility, count line, manual-entry visibility).
fn wizard_onvif_pairs(s: &DiscoverState) -> Vec<(String, Value)> {
    let count = s.cameras.len();
    vec![
        ("wiz_onvif_scanning".into(), Value::Bool(s.scanning)),
        ("wiz_onvif_has_results".into(), Value::Bool(!s.scanning && count > 0)),
        ("wiz_onvif_no_results".into(), Value::Bool(!s.scanning && count == 0)),
        (
            "wiz_onvif_count".into(),
            Value::Text(alloc::format!(
                "Znaleziono {} kamer. Wybierz jedną lub podaj URL ręcznie.",
                count
            )),
        ),
    ]
}

/// Patch pairs describing the step-3 connection-test sub-state (spinner, result
/// alerts, result text). Mutually exclusive visibility flags drive which block
/// is shown.
fn wizard_test_pairs(s: &DiscoverState) -> Vec<(String, Value)> {
    let (ok, err, text, idle) = match (&s.testing, &s.test_result) {
        (true, _) => (false, false, String::new(), false),
        (false, Some(Ok(m))) => {
            let detail = if m.is_empty() { "Połączenie nawiązane.".to_string() } else { m.clone() };
            (true, false, alloc::format!("Połączenie OK. {}", detail), false)
        }
        (false, Some(Err(m))) => (false, true, m.clone(), false),
        (false, None) => (false, false, String::new(), true),
    };
    vec![
        ("wiz_testing".into(), Value::Bool(s.testing)),
        ("wiz_test_ok".into(), Value::Bool(ok)),
        ("wiz_test_err".into(), Value::Bool(err)),
        ("wiz_test_idle".into(), Value::Bool(idle)),
        ("wiz_test_text".into(), Value::Text(text)),
    ]
}

/// Error-alert patch pair. `wiz_has_error` toggles the alert's visibility while
/// `wiz_error` carries its message.
fn wizard_error_pairs(message: Option<&str>) -> Vec<(String, Value)> {
    vec![
        ("wiz_has_error".into(), Value::Bool(message.is_some())),
        ("wiz_error".into(), Value::Text(message.unwrap_or("").into())),
    ]
}

/// The full set of wizard store keys derived from the current backend wizard
/// state, seeded into the `add_camera_body` SlotContent `state_overlay`. Sent
/// whenever the body fragment is delivered (modal open or an ONVIF
/// scan/select re-send) so every bound visibility flag, the StepProgress and
/// the field inputs resolve to the authoritative backend state on first paint.
fn wizard_full_overlay() -> Vec<StateEntry> {
    let (step, src, err) = with_state(|s| (s.wizard_step, s.discover.source_type, s.error_message.clone()));
    let mut pairs: Vec<(String, Value)> = Vec::new();
    pairs.extend(wizard_step_pairs(step));
    pairs.extend(wizard_source_pairs(src));
    pairs.extend(with_state(|s| wizard_test_pairs(&s.discover)));
    pairs.extend(with_state(|s| wizard_onvif_pairs(&s.discover)));
    pairs.extend(wizard_error_pairs(err.as_deref()));
    // Field bind paths reflect the committed backend values so the two-way-bound
    // inputs show the right text without any further round-trip.
    let fields = with_state(|s| [
        ("onvif_url", s.discover.onvif_url.clone()),
        ("rtsp_url", s.discover.rtsp_url.clone()),
        ("usb_device_path", s.discover.usb_device_path.clone()),
        ("file_path", s.discover.file_path.clone()),
        ("cred_user", s.discover.cred_user.clone()),
        ("cred_pass", s.discover.cred_pass.clone()),
        ("name", s.discover.name.clone()),
        ("retention", s.discover.retention.clone()),
        ("fps", s.discover.fps.clone()),
    ]);
    for (key, value) in fields {
        pairs.push((key.into(), Value::Text(value)));
    }
    // Reflect the committed profile (defaulting to "default") so the select
    // shows the authoritative backend value rather than always resetting to it.
    pairs.push(("profile".into(), Value::Text(with_state(|s| s.discover.profile_or_default().to_string()))));
    pairs
        .into_iter()
        .map(|(key, value)| StateEntry {
            path: StatePath::new(vec![PathSegment::Key(key)]),
            value,
        })
        .collect()
}

/// The "Add camera" wizard lives in a Modal overlay. This builds the Modal
/// shell that is placed in the "content" slot tree while `add_form_visible`.
/// Its body/footer are filled separately via SlotContent on the dynamic slots
/// `add_camera_body` / `add_camera_footer`, which the host registers only while
/// this Modal is in the DOM. The Dismiss event (×/backdrop/ESC) is routed to
/// the same `camera-add-cancel` action as the footer cancel button so closing
/// the dialog any way resets the wizard state.
fn build_add_camera_modal() -> Component {
    let step = with_state(|s| s.wizard_step);
    let title = alloc::format!("Dodaj kamerę \u{2014} krok {} z {}", step + 1, ADD_CAMERA_WIZARD_STEPS);

    let mut modal = ModalComp {
        title: lit(&title),
        subtitle: None,
        body_slot: "add_camera_body".into(),
        footer_slot: Some("add_camera_footer".into()),
        size: ModalSize::Lg,
        dismissible: true,
        prevent_scroll: true,
        closable: true,
    }.into_component(next_id()).expect("Modal");
    modal.handlers = Some(HandlerMap(vec![(
        tentaflow_sdk_spec::EventKind::Dismiss,
        Handler::Backend {
            action_id: "camera-add-cancel".into(),
            params: CborMap::default(),
            optimistic: None,
            on_failure: FailurePolicy::Toast,
        },
    )]));
    modal
}

/// Builds the wizard body fragment for the `add_camera_body` slot ONCE, when the
/// modal opens. Every step lives in the DOM simultaneously, wrapped in a
/// `with_visible` container bound to a `wiz_show_N` store flag; the active step
/// is revealed purely by `StatePatch`. The StepProgress, the per-type config
/// blocks, the test-result block and the error alert are all store-bound, so no
/// wizard interaction (source pick, Next/Back, typing) rebuilds this fragment.
fn build_add_camera_body() -> Component {
    let step_progress = build_wizard_step_progress();

    let step0 = with_visible(build_wizard_step_source_type(), "wiz_show_0");
    let step1 = with_visible(build_wizard_step_config(), "wiz_show_1");
    let step2 = with_visible(build_wizard_step_test(), "wiz_show_2");
    let step3 = with_visible(build_wizard_step_metadata(), "wiz_show_3");

    let error_alert = with_visible(alert_bound("wiz_error", "critical"), "wiz_has_error");

    stack_v(vec![step_progress, step0, step1, step2, step3, error_alert])
}

/// StepProgress bound to `wiz_step`. Status per step is derived by the renderer
/// from `current_id_path` position, so advancing a step is a single patch.
fn build_wizard_step_progress() -> Component {
    let step_labels = ["Typ źródła", "Konfiguracja", "Test połączenia", "Metadane"];
    StepProgressComp {
        steps: step_labels.iter().enumerate().map(|(i, label)| StepDef {
            id: wiz_step_id(i as u8),
            label: lit(label),
            optional: false,
            status: None,
            description: None,
        }).collect(),
        current_id_path: StatePath::new(vec![PathSegment::Key("wiz_step".into())]),
        variant: StepProgressVariant::Horizontal,
        clickable_completed: false,
    }.into_component(next_id()).expect("StepProgress")
}

/// Builds the wizard navigation buttons for the `add_camera_footer` slot ONCE.
/// All four buttons live in the DOM; Back/Next/Finish toggle visibility through
/// store flags (`wiz_show_back/next/finish`) so navigation never rebuilds the
/// footer. The Next label is intentionally generic — the step number lives in
/// the StepProgress, not the button text, so it needs no per-step patching.
fn build_add_camera_footer() -> Component {
    let back = with_visible(button_with_icon("Wstecz", "wizard-prev", "ghost", "info"), "wiz_show_back");
    let cancel = button("Anuluj", "camera-add-cancel", "ghost");
    let next = with_visible(button("Dalej", "wizard-next", "primary"), "wiz_show_next");
    let finish = with_visible(button("Zakończ", "camera-add-submit", "primary"), "wiz_show_finish");
    stack_h(vec![back, cancel, next, finish])
}

/// Step 1 — source-type chooser as a store-bound `RadioCardGroup`. The pick is
/// written to `wiz_src` reactively (client highlight) and forwarded to the
/// backend `wizard-source-select` action, which patches the per-type config
/// visibility flags. No card is rebuilt on selection.
fn build_wizard_step_source_type() -> Component {
    use tentaflow_sdk_spec::protocol::ui::form::RadioCardGroup;
    let options = vec![
        RadioCardOption {
            value: SelectValue::Text(SourceType::Onvif.as_str().into()),
            icon: icon_named(parse_icon_name("search")),
            title: lit("Kamera sieciowa ONVIF"),
            description: Some(lit("Automatyczne wykrywanie kamer ONVIF w sieci lokalnej.")),
            badge: None,
            disabled: false,
        },
        RadioCardOption {
            value: SelectValue::Text(SourceType::Rtsp.as_str().into()),
            icon: icon_named(parse_icon_name("video")),
            title: lit("Strumień RTSP/RTSPS"),
            description: Some(lit("Ręczny adres strumienia rtsp:// lub rtsps://.")),
            badge: None,
            disabled: false,
        },
        RadioCardOption {
            value: SelectValue::Text(SourceType::Usb.as_str().into()),
            icon: icon_named(parse_icon_name("cameras")),
            title: lit("Kamera lokalna / USB"),
            description: Some(lit("Urządzenie wideo podłączone do tego hosta (v4l2).")),
            badge: None,
            disabled: false,
        },
        RadioCardOption {
            value: SelectValue::Text(SourceType::File.as_str().into()),
            icon: icon_named(parse_icon_name("evidence")),
            title: lit("Plik testowy"),
            description: Some(lit("Lokalny plik wideo używany jako źródło testowe.")),
            badge: None,
            disabled: false,
        },
    ];

    let mut group = RadioCardGroup {
        bind_path: StatePath::new(vec![PathSegment::Key("wiz_src".into())]),
        options,
        columns: 2,
        variant: RadioCardVariant::Default,
    }.into_component(next_id()).expect("RadioCardGroup");
    group = with_a11y_label(group, "Typ źródła kamery");
    // The change carries the picked SelectValue as `{value, kind}` detail; the
    // backend reads `value` to branch step 2 and patch the per-type flags.
    group.handlers = Some(HandlerMap(vec![(
        tentaflow_sdk_spec::EventKind::Change,
        Handler::Backend {
            action_id: "wizard-source-select".into(),
            params: CborMap::default(),
            optimistic: None,
            on_failure: FailurePolicy::Toast,
        },
    )]));

    stack_v(vec![
        text("Wybierz typ źródła kamery. Dalsze kroki dopasują się do wybranego typu."),
        group,
    ])
}

/// Step 2 — all four per-type config blocks present at once, each wrapped in a
/// `with_visible` container bound to its `wiz_is_X` flag. Switching source type
/// is a `StatePatch` that flips exactly one flag visible.
fn build_wizard_step_config() -> Component {
    stack_v(vec![
        with_visible(build_config_onvif(), "wiz_is_onvif"),
        with_visible(build_config_rtsp(), "wiz_is_rtsp"),
        with_visible(build_config_usb(), "wiz_is_usb"),
        with_visible(build_config_file(), "wiz_is_file"),
    ])
}

/// ONVIF config block. The discovery list and selectable results are genuinely
/// dynamic (populated by an explicit `discover-scan`), so their visibility is
/// store-bound and the scan/select actions re-send this body; the manual URL +
/// credential inputs are always-present, two-way-bound fields.
fn build_config_onvif() -> Component {
    let scanning = with_state(|s| s.discover.scanning);
    let selected_idx = with_state(|s| s.discover.selected_index);

    let scan_spinner = with_visible(
        stack_v(vec![spinner("md"), text("Skanowanie sieci (ONVIF WS-Discovery)...")]),
        "wiz_onvif_scanning",
    );

    let no_results = with_visible(
        stack_v(vec![
            text("Zeskanuj sieć w poszukiwaniu kamer ONVIF lub podaj adres URL urządzenia ręcznie."),
            stack_h(vec![button_with_icon("Skanuj sieć", "discover-scan", "primary", "search")]),
        ]),
        "wiz_onvif_no_results",
    );

    let discovered = with_state(|s| s.discover.cameras.iter().enumerate()
        .map(|(i, c)| (i, c.suggested_name.clone(), c.url.clone())).collect::<Vec<_>>());
    let mut cam_rows: Vec<Component> = Vec::new();
    for (i, name, url) in &discovered {
        let is_sel = !scanning && selected_idx == Some(*i);
        let row_content = stack_v_gap("xs", vec![
            text_styled(name, "body_strong"),
            text_styled(url, "caption"),
        ]);
        let mut row_card = Card {
            variant: if is_sel { CardVariant::Filled } else { CardVariant::Outlined },
            padding: Spacing::Sm,
            gap: Spacing::Sm,
            radius: RadiusToken::Sm,
            shadow: ShadowToken::None,
            border: BorderToken::Hairline,
            background: BackgroundToken::None,
            accent: if is_sel { Some(Tone::Primary) } else { None },
            children: vec![row_content],
            interactive: true,
            clickable: true,
        }.into_component(next_id()).expect("Card");
        let mut params = CborMap::default();
        params.0.push(("index".into(), Value::U64(*i as u64)));
        row_card.handlers = Some(HandlerMap(vec![(
            tentaflow_sdk_spec::EventKind::Click,
            Handler::Backend {
                action_id: "discover-select".into(),
                params,
                optimistic: None,
                on_failure: FailurePolicy::Toast,
            },
        )]));
        cam_rows.push(row_card);
    }
    let has_results = with_visible(
        stack_v(vec![
            text_bound("wiz_onvif_count"),
            stack_v_gap("xs", cam_rows),
            button_with_icon("Skanuj ponownie", "discover-scan", "ghost", "search"),
        ]),
        "wiz_onvif_has_results",
    );

    let url_input = wizard_input("URL urządzenia ONVIF", "http://10.0.0.5/onvif/device_service", "onvif_url", false);
    let user_input = wizard_input("Użytkownik", "", "cred_user", false);
    let pass_input = wizard_input("Hasło", "", "cred_pass", true);

    stack_v(vec![
        scan_spinner,
        no_results,
        has_results,
        url_input,
        grid(2, vec![user_input, pass_input]),
        text_styled("Kamera ONVIF wymaga użytkownika i hasła.", "caption"),
    ])
}

fn build_config_rtsp() -> Component {
    let url_input = wizard_input("URL strumienia RTSP", "rtsp://host:554/stream", "rtsp_url", false);
    let user_input = wizard_input("Użytkownik (opcjonalnie)", "", "cred_user", false);
    let pass_input = wizard_input("Hasło (opcjonalnie)", "", "cred_pass", true);
    stack_v(vec![
        text("Podaj adres strumienia RTSP/RTSPS. Poświadczenia są opcjonalne."),
        url_input,
        grid(2, vec![user_input, pass_input]),
    ])
}

/// USB config block. Local devices are enumerated eagerly at modal open so the
/// Select options can be baked into this fragment (Select options are static
/// component fields, not store-bound). A manual path input is always present so
/// the step is never a dead end when no device is detected. The device Select
/// and the manual input both two-way bind `usb_device_path`.
fn build_config_usb() -> Component {
    let devices = with_state(|s| s.discover.usb_devices.iter()
        .map(|d| (d.device_path.clone(), d.label.clone())).collect::<Vec<_>>());

    if devices.is_empty() {
        return stack_v(vec![
            alert("Nie wykryto lokalnych urządzeń wideo (v4l2). Podaj ścieżkę ręcznie.", "info"),
            wizard_input("Ścieżka urządzenia", "/dev/video0", "usb_device_path", false),
        ]);
    }

    let options: Vec<SelectOption> = devices.iter().map(|(path, label)| SelectOption {
        value: SelectValue::Text(path.clone()),
        label: lit(&alloc::format!("{} ({})", label, path)),
        icon: None,
        disabled: false,
        group_id: None,
        description: None,
    }).collect();
    let device_select = wizard_select("Wykryte urządzenie", options, "usb_device_path");

    stack_v(vec![
        text(&alloc::format!("Wykryto {} urządzeń lokalnych. Wybierz źródło wideo.", devices.len())),
        device_select,
    ])
}

fn build_config_file() -> Component {
    stack_v(vec![
        text("Podaj ścieżkę lokalnego pliku wideo używanego jako źródło testowe."),
        wizard_input("Ścieżka pliku wideo", "/var/lib/tentaflow/sample.mp4", "file_path", false),
    ])
}

/// Step 3 — connection probe. Spinner, success alert, error alert and idle
/// empty-state all live in the DOM, toggled by `wiz_testing/test_ok/test_err/
/// test_idle` flags; the result text is bound to `wiz_test_text`. Running the
/// test is a `StatePatch`, never a rebuild. No fabricated preview frame.
fn build_wizard_step_test() -> Component {
    let testing_block = with_visible(
        stack_v(vec![spinner("md"), text("Testowanie połączenia z kamerą...")]),
        "wiz_testing",
    );
    let ok_block = with_visible(alert_bound("wiz_test_text", "success"), "wiz_test_ok");
    let err_block = with_visible(alert_bound("wiz_test_text", "critical"), "wiz_test_err");
    let idle_block = with_visible(
        empty_state("Brak testu", Some("Uruchom test, aby sprawdzić połączenie z kamerą."), Some("info")),
        "wiz_test_idle",
    );

    stack_v(vec![
        text("Sprawdź połączenie ze źródłem przed dodaniem kamery."),
        stack_h(vec![button_with_icon("Testuj połączenie", "wizard-test", "primary", "check")]),
        testing_block,
        ok_block,
        err_block,
        idle_block,
        text_styled("Podgląd na żywo będzie dostępny po dodaniu kamery.", "caption"),
    ])
}

/// Step 4 — camera metadata. All fields two-way bind their store keys; no preset
/// values. The metadata name is pre-filled from a discovered camera on the
/// step-2→3 transition via a `StatePatch`, not by rebuilding this fragment.
fn build_wizard_step_metadata() -> Component {
    let name_input = wizard_input("Nazwa kamery", "np. Brama wjazdowa", "name", false);
    let retention_select = wizard_select("Klasa retencji", vec![
        SelectOption { value: SelectValue::Text("A".into()), label: lit("A — długa retencja"), icon: None, disabled: false, group_id: None, description: None },
        SelectOption { value: SelectValue::Text("B".into()), label: lit("B — średnia retencja"), icon: None, disabled: false, group_id: None, description: None },
        SelectOption { value: SelectValue::Text("C".into()), label: lit("C — krótka retencja"), icon: None, disabled: false, group_id: None, description: None },
        SelectOption { value: SelectValue::Text("Unclassified".into()), label: lit("Niesklasyfikowana"), icon: None, disabled: false, group_id: None, description: None },
    ], "retention");
    let fps_input = wizard_input("Docelowe FPS", "15", "fps", false);
    let profile_select = wizard_select("Profil analityczny", vec![
        SelectOption { value: SelectValue::Text("default".into()), label: lit("default"), icon: None, disabled: false, group_id: None, description: None },
    ], "profile");

    stack_v(vec![
        text("Uzupełnij metadane kamery przed jej dodaniem."),
        grid(2, vec![name_input, retention_select, fps_input, profile_select]),
    ])
}

fn build_alarms_content() -> Component {
    let severity = with_state(|s| s.alarms.severity_or_all().to_string());
    let messages = build_messages_section();
    let chips = filter_chips(
        vec![
            FilterChipDef { id: "all".into(), label: lit("all"), icon: None, badge: None, count_path: None },
            FilterChipDef { id: "critical".into(), label: lit("critical"), icon: None, badge: None, count_path: None },
            FilterChipDef { id: "warning".into(), label: lit("warning"), icon: None, badge: None, count_path: None },
            FilterChipDef { id: "info".into(), label: lit("info"), icon: None, badge: None, count_path: None },
        ],
        &severity,
    );
    let toolbar = stack_h(vec![
        heading(2, "Alarmy"),
        chips,
        button("Potwierdź wszystkie", "alarm-acknowledge-all", "secondary"),
    ]);
    let alarm_rows = vec![
        build_alarm_row("D2 · podejrzenie agresji", "C-04 wjazd", "12:43:21", "danger"),
        build_alarm_row("D1 · nieczytelna tablica ADR (UN 1203)", "C-01 brama", "12:38:04", "warning"),
        build_alarm_row("D3 · pozostawiony bagaż > 90s", "C-07 peron", "12:31:55", "warning"),
    ];
    stack_v(core::iter::once(messages).chain(core::iter::once(toolbar)).chain(alarm_rows).collect())
}

fn build_search_content() -> Component {
    let messages = build_messages_section();
    let toolbar = stack_h(vec![
        heading(2, "Wyszukiwarka"),
        button("Wyczyść", "search-clear-all", "ghost"),
    ]);
    let search_input = input("Szukaj", "Wpisz zapytanie...", "search_query");
    let submitted = with_state(|s| s.search.submitted);
    let results = if submitted {
        card(None, vec![text("Wyniki wyszukiwania — wymaga backendu historycznego indeksu.")])
    } else {
        empty_state("Wprowadź zapytanie", Some("Wpisz frazę i naciśnij Enter."), Some("search"))
    };
    stack_v(vec![messages, toolbar, search_input, results])
}

/// Input bound to a store key that also mirrors its value into backend profile
/// state on every keystroke (tagged `field`), so submit validation reads the
/// authoritative value even if the user clicks "Zapisz" before blur.
fn profile_input(label: &str, placeholder: &str, field: &str) -> Component {
    let mut comp = input(label, placeholder, field);
    let mut params = CborMap::default();
    params.0.push(("field".into(), Value::Text(field.into())));
    comp.handlers = Some(HandlerMap(vec![(
        tentaflow_sdk_spec::EventKind::Input,
        Handler::Backend {
            action_id: "profile-field-change".into(),
            params,
            optimistic: None,
            on_failure: FailurePolicy::Toast,
        },
    )]));
    comp
}

/// Select bound to a store key that mirrors its picked value into backend
/// profile state on change (tagged `field`).
fn profile_select(label: &str, options: Vec<SelectOption>, field: &str) -> Component {
    let mut comp = select(label, options, field);
    let mut params = CborMap::default();
    params.0.push(("field".into(), Value::Text(field.into())));
    comp.handlers = Some(HandlerMap(vec![(
        tentaflow_sdk_spec::EventKind::Change,
        Handler::Backend {
            action_id: "profile-field-change".into(),
            params,
            optimistic: None,
            on_failure: FailurePolicy::Toast,
        },
    )]));
    comp
}

/// Slider bound to a store key that mirrors its value into backend profile state
/// on change (tagged `field`).
fn profile_slider(label: &str, field: &str, min: f64, max: f64, step: f64) -> Component {
    let mut comp = slider(label, field, min, max, step);
    let mut params = CborMap::default();
    params.0.push(("field".into(), Value::Text(field.into())));
    comp.handlers = Some(HandlerMap(vec![(
        tentaflow_sdk_spec::EventKind::Change,
        Handler::Backend {
            action_id: "profile-field-change".into(),
            params,
            optimistic: None,
            on_failure: FailurePolicy::Toast,
        },
    )]));
    comp
}

/// Risk-class badge matching the mockup tones: A = success (green), B = warning
/// (amber), C = critical (red); anything else = neutral.
fn risk_badge(risk_class: &str) -> Component {
    let tone = match risk_class {
        "A" => "success",
        "B" => "warning",
        "C" => "danger",
        _ => "info",
    };
    let label = if risk_class.is_empty() { "—" } else { risk_class };
    chip_toned(label, match tone { "danger" => "critical", other => other })
}

/// Available analytic Flows the profile can bind to. In the mockup this list is
/// filtered to Flows that expose TentaVision vision capabilities; here it is a
/// stable set the builder writes verbatim into `flow_id`.
fn profile_flow_options() -> Vec<SelectOption> {
    ["tv-realtime-adr", "tv-realtime-public", "tv-security-night", "tv-anpr", "tv-reid-historical"]
        .iter()
        .map(|f| SelectOption {
            value: SelectValue::Text((*f).into()),
            label: lit(f),
            icon: None,
            disabled: false,
            group_id: None,
            description: None,
        })
        .collect()
}

fn profile_risk_options() -> Vec<SelectOption> {
    [("A", "A — bezosobowe / długa retencja"), ("B", "B — średnie ryzyko"), ("C", "C — wrażliwe / krótka retencja")]
        .iter()
        .map(|(v, l)| SelectOption {
            value: SelectValue::Text((*v).into()),
            label: lit(l),
            icon: None,
            disabled: false,
            group_id: None,
            description: None,
        })
        .collect()
}

fn profile_schedule_options() -> Vec<SelectOption> {
    ["24/7", "06:00–22:00", "22:00–06:00", "04:30–24:00"]
        .iter()
        .map(|s| SelectOption {
            value: SelectValue::Text((*s).into()),
            label: lit(s),
            icon: None,
            disabled: false,
            group_id: None,
            description: None,
        })
        .collect()
}

/// Renders the camera-assignment list: every real camera from SQLite as a
/// toggle button. Assigned cameras show a "success" status chip; clicking a row
/// toggles membership via `profile-camera-toggle`.
fn build_profile_camera_assignment(cameras: &[db::CameraRow], assigned: &[String]) -> Component {
    if cameras.is_empty() {
        return empty_state(
            "Brak kamer",
            Some("Dodaj kamerę w zakładce Kamery, aby przypisać ją do profilu."),
            Some("cameras"),
        );
    }
    let rows: Vec<Component> = cameras
        .iter()
        .map(|c| {
            let is_on = assigned.iter().any(|a| a == &c.id);
            let mut params = CborMap::default();
            params.0.push(("camera_id".into(), Value::Text(c.id.clone())));
            let label = if is_on { alloc::format!("✓ {}", c.name) } else { c.name.clone() };
            let variant = if is_on { "primary" } else { "secondary" };
            let toggle_btn = button_with_params(&label, "profile-camera-toggle", variant, params);
            let status = chip_toned(&c.status, if c.status == "online" { "success" } else { "warning" });
            stack_h(vec![toggle_btn, status])
        })
        .collect();
    stack_v_gap("sm", rows)
}

/// One profile-library Table row keyed by `profile_id`.
/// Maps an analytical-profile risk class to a chip tone (A=green, B=amber,
/// C=red), matching the mockup's `.risk` badge colors.
fn profile_risk_cell(risk: &str) -> Value {
    match risk {
        "A" => chip_cell("A", "ok"),
        "B" => chip_cell("B", "warn"),
        "C" => chip_cell("C", "err"),
        other => chip_cell(if other.is_empty() { "—" } else { other }, "info"),
    }
}

fn profile_table_row_value(p: &db::ProfileRow, camera_count: usize) -> Value {
    let flow = if p.flow_id.is_empty() { "—".to_string() } else { p.flow_id.clone() };
    let schedule = if p.schedule.is_empty() { "—".to_string() } else { p.schedule.clone() };
    let entries: Vec<(Value, Value)> = vec![
        (Value::Text("profile_id".into()), Value::Text(p.id.clone())),
        (Value::Text("name".into()), Value::Text(p.name.clone())),
        (Value::Text("flow".into()), Value::Text(flow)),
        (Value::Text("risk".into()), profile_risk_cell(&p.risk_class)),
        (Value::Text("cameras".into()), Value::Text(alloc::format!("{}", camera_count))),
        (Value::Text("schedule".into()), Value::Text(schedule)),
        (Value::Text("enabled".into()), if p.enabled { chip_cell("TAK", "ok") } else { chip_cell("NIE", "muted") }),
    ];
    Value::Map(entries)
}

fn profile_table_column(id: &str, header: &str, render: ColumnRender) -> TableColumn {
    TableColumn {
        id: id.into(),
        header: lit(header),
        field_path: vec![PathSegment::Key(id.into())],
        width: TableColumnWidth::Auto,
        render,
        format: None,
        align: None,
        sortable: true,
        hidden_by_default: false,
        sticky_left: false,
    }
}

fn build_profiles_table() -> Component {
    let columns = vec![
        profile_table_column("name", "Nazwa", ColumnRender::Text),
        profile_table_column("flow", "Flow", ColumnRender::Text),
        profile_table_column("risk", "Klasa", ColumnRender::Chip),
        profile_table_column("cameras", "Kamery", ColumnRender::Text),
        profile_table_column("schedule", "Harmonogram", ColumnRender::Text),
        profile_table_column("enabled", "Aktywny", ColumnRender::Chip),
    ];

    // Per-row actions: edit opens the builder pre-filled, toggle flips enabled,
    // and Usuń arms the delete-confirmation bar (the real delete runs from it).
    let edit_action = button("Edytuj", "profile-edit", "secondary");
    let toggle_action = button("Włącz/wyłącz", "profile-toggle-enabled", "ghost");
    let remove_action = button("Usuń", "profile-row-select", "destructive");

    TableComp {
        columns,
        rows_path: StatePath::new(vec![PathSegment::Key("profiles_rows".into())]),
        row_key_field: "profile_id".into(),
        variant: TableVariant::Default,
        density: Density::Default,
        sortable: true,
        sort_by: None,
        selectable: TableSelectMode::None,
        selected_ids: None,
        sticky_header: true,
        sticky_columns: 0,
        pagination: None,
        empty_state: None,
        row_actions: vec![edit_action, toggle_action, remove_action],
        bulk_actions: vec![],
        virtualize: false,
        row_expandable: false,
        expanded_row_template_id: None,
    }.into_component(next_id()).expect("Table")
}

/// Confirmation bar for deleting the selected profile.
fn build_profile_remove_confirm(profile_id: &str, profiles: &[db::ProfileRow]) -> Component {
    let name = profiles
        .iter()
        .find(|p| p.id == profile_id)
        .map(|p| p.name.as_str())
        .unwrap_or(profile_id);
    let mut params = CborMap::default();
    params.0.push(("profile_id".into(), Value::Text(profile_id.into())));
    let confirm_btn = button_with_params("Usuń", "profile-remove", "destructive", params);
    let cancel_btn = button("Anuluj", "profile-remove-cancel", "ghost");
    card(None, vec![stack_v(vec![
        text_styled(&alloc::format!("Usunąć profil \"{}\"?", name), "body_strong"),
        text("Tej operacji nie można cofnąć."),
        stack_h(vec![confirm_btn, cancel_btn]),
    ])])
}

/// The analytic-profile builder: left column (Flow + quick params), right column
/// (profile config + camera assignment). Mirrors the m04 mockup's `.col-2`.
fn build_profile_builder(cameras: &[db::CameraRow]) -> Component {
    let (name, flow_id, risk_class, schedule, assigned, editing) = with_state(|s| (
        s.profiles.name.clone(),
        s.profiles.flow_id.clone(),
        s.profiles.risk_class.clone(),
        s.profiles.schedule.clone(),
        s.profiles.cameras.clone(),
        s.profiles.editing_id.is_some(),
    ));

    // LEFT: Flow assignment + quick params (overrides to Flow inputs).
    let left = card(Some("Flow przypisany do profilu"), vec![
        text("Lista Flow filtrowana do tych, które używają capabilities TentaVision (vision.detect, vision.ocr, video.recording)."),
        profile_select("Flow", profile_flow_options(), "profile_flow_id"),
        heading(4, "Quick params — overrides do Flow inputs"),
        profile_slider("FPS sampling kamery", "profile_fps", 1.0, 15.0, 1.0),
        profile_slider("Min. próg detekcji", "profile_min_conf", 0.0, 1.0, 0.05),
        text("Quick params zapisują się jako overrides do inputs Flow. Aby zmienić strukturę grafu — otwórz w FlowBuilder."),
    ]);

    // RIGHT: profile config + camera assignment.
    let right = card(Some("Konfiguracja profilu"), vec![
        profile_input("Nazwa", "np. ADR-brama", "profile_name"),
        profile_select("Klasa ryzyka", profile_risk_options(), "profile_risk_class"),
        stack_h(vec![text("Aktualna klasa:"), risk_badge(&risk_class)]),
        profile_select("Harmonogram", profile_schedule_options(), "profile_schedule"),
        heading(4, "Kamery w profilu"),
        build_profile_camera_assignment(cameras, &assigned),
    ]);

    let _ = (name, flow_id, schedule);

    let save_label = if editing { "Zapisz zmiany" } else { "Utwórz profil" };
    let actions = stack_h(vec![
        button(save_label, "profile-add-submit", "primary"),
        button("Anuluj", "profile-builder-cancel", "ghost"),
    ]);

    card(None, vec![
        grid(2, vec![left, right]),
        actions,
    ])
}

fn build_profiles_content() -> Component {
    let messages = build_messages_section();
    let list_result = db::list_profiles();
    let (category, builder_visible) = with_state(|s| (s.profiles.category_or_all().to_string(), s.profiles.builder_visible));

    let mut children = vec![messages];

    let chips = filter_chips(
        vec![
            FilterChipDef { id: "all".into(), label: lit("Wszystkie"), icon: None, badge: None, count_path: None },
            FilterChipDef { id: "A".into(), label: lit("Klasa A"), icon: None, badge: None, count_path: None },
            FilterChipDef { id: "B".into(), label: lit("Klasa B"), icon: None, badge: None, count_path: None },
            FilterChipDef { id: "C".into(), label: lit("Klasa C"), icon: None, badge: None, count_path: None },
        ],
        &category,
    );
    let toolbar = stack_h(vec![
        heading(2, "Profile analityczne"),
        chips,
        button("Nowy profil", "profile-add-show", "primary"),
    ]);
    children.push(toolbar);

    // A DB/permission error must never be masked as "no profiles".
    let profiles = match list_result {
        Ok(p) => p,
        Err(e) => {
            children.push(alert(&alloc::format!("Nie udało się pobrać profili: {}", abi_message(e)), "critical"));
            return stack_v(children);
        }
    };

    // Cameras for the builder's assignment list and the library's per-row count.
    let cameras = db::list_cameras().unwrap_or_default();

    if builder_visible {
        children.push(build_profile_builder(&cameras));
    }

    // Delete-confirmation bar above the table once a row is armed.
    if let Some(pending) = with_state(|s| s.profiles.pending_remove.clone()) {
        if profiles.iter().any(|p| p.id == pending) {
            children.push(build_profile_remove_confirm(&pending, &profiles));
        } else {
            with_state(|s| s.profiles.pending_remove = None);
        }
    }

    let active_filter = if category == "all" { "" } else { category.as_str() };
    let filtered: Vec<&db::ProfileRow> = profiles
        .iter()
        .filter(|p| active_filter.is_empty() || p.risk_class == active_filter)
        .collect();

    if profiles.is_empty() {
        children.push(empty_state(
            "Brak profili analitycznych",
            Some("Utwórz pierwszy profil: wybierz Flow, klasę ryzyka i przypisz kamery."),
            Some("brain"),
        ));
    } else {
        let rows: Vec<Value> = filtered
            .iter()
            .map(|p| profile_table_row_value(p, profile_camera_count(p)))
            .collect();
        if let Ok(mut g) = PENDING_PROFILE_ROWS.lock() {
            *g = Some(Value::Array(rows));
        }
        children.push(build_profiles_table());
    }

    stack_v(children)
}

/// Parses a profile's `cameras` JSON array into a list of camera ids.
fn parse_profile_cameras(cameras_json: &str) -> Vec<String> {
    serde_json::from_str::<Vec<String>>(cameras_json).unwrap_or_default()
}

/// Number of cameras assigned to a profile (length of its `cameras` JSON array).
fn profile_camera_count(p: &db::ProfileRow) -> usize {
    parse_profile_cameras(&p.cameras).len()
}

/// Seeds the builder's bound store keys from current backend profile state so
/// the form mounts with the draft (create) or loaded (edit) values in place.
fn profile_builder_overlay() -> Vec<StateEntry> {
    with_state(|s| {
        let p = &s.profiles;
        let key = |k: &str, v: Value| StateEntry {
            path: StatePath::new(vec![PathSegment::Key(k.into())]),
            value: v,
        };
        vec![
            key("profile_name", Value::Text(p.name.clone())),
            key("profile_flow_id", Value::Text(p.flow_id.clone())),
            key("profile_risk_class", Value::Text(p.risk_class.clone())),
            key("profile_schedule", Value::Text(p.schedule.clone())),
            key("profile_fps", Value::F64(p.fps)),
            key("profile_min_conf", Value::F64(p.min_confidence)),
        ]
    })
}

fn build_reid_content() -> Component {
    let gate_passed = with_state(|s| s.reid.gate_passed);
    let messages = build_messages_section();
    if !gate_passed {
        return stack_v(vec![
            messages,
            gate_screen("Re-ID wyłączone", "Wymaga: załadowany model face-embedding, GPU ready, indeks ≥10 twarzy.", "lock"),
            button("Odblokuj (dev)", "reid-bypass-gate", "secondary"),
        ]);
    }
    stack_v(vec![
        messages,
        heading(2, "Re-ID — wyszukiwanie po twarzy"),
        text("Backend embedding indeksu w trakcie budowy."),
        button("Zablokuj ponownie", "reid-bypass-gate", "ghost"),
    ])
}

fn build_models_content() -> Component {
    let messages = build_messages_section();
    let models_data: &[(&str, &str, &str, &str, &str)] = &[
        ("yolo11m", "YOLO v11 medium", "object_detection", "active", "420 MB"),
        ("pp-ocrv5", "PP-OCRv5", "ocr", "active", "180 MB"),
        ("bot-sort", "BoT-SORT", "tracking", "active", "95 MB"),
        ("yolov8-face", "YOLOv8 Face", "face_detection", "disabled", "210 MB"),
    ];
    let cols = vec![
        Value::Text("ID".into()), Value::Text("Nazwa".into()),
        Value::Text("Typ".into()), Value::Text("Status".into()),
        Value::Text("Rozmiar".into()),
    ];
    let rows: Vec<Value> = models_data.iter().map(|(id, name, typ, status, size)| {
        Value::Array(vec![
            Value::Text((*id).into()), Value::Text((*name).into()),
            Value::Text((*typ).into()), Value::Text((*status).into()),
            Value::Text((*size).into()),
        ])
    }).collect();
    stack_v(vec![
        messages,
        stack_h(vec![heading(2, "Modele"), button("Import", "model-import-show", "secondary")]),
        card(None, vec![table(cols, rows)]),
    ])
}

fn build_zones_content() -> Component {
    let messages = build_messages_section();
    let (zones, selected_id, drawing_mode, drawing_points) = with_state(|s| {
        s.zones.ensure_seeded();
        (s.zones.zones.clone(), s.zones.selected_zone_id.clone(), s.zones.drawing_mode, s.zones.drawing_points.clone())
    });

    let toolbar = stack_h(vec![
        heading(2, "Strefy i reguły"),
        button_with_icon("Nowa strefa", "zone-add-start", "primary", "plus"),
    ]);

    let mut canvas_commands: Vec<Value> = Vec::new();
    for zone in &zones {
        let is_selected = selected_id.as_deref() == Some(&zone.id);
        let color = match zone.zone_type.as_str() {
            "exclusion" => "red", "alert" => "orange", _ => "green"
        };
        if zone.points.len() >= 2 {
            let pts: Vec<Value> = zone.points.iter().map(|p| Value::Array(vec![Value::F64(p.x), Value::F64(p.y)])).collect();
            canvas_commands.push(Value::Map(vec![
                (Value::Text("type".into()), Value::Text("polygon".into())),
                (Value::Text("points".into()), Value::Array(pts)),
                (Value::Text("stroke".into()), Value::Text(color.into())),
                (Value::Text("fill".into()), Value::Text(if is_selected { "rgba(0,255,0,0.2)" } else { "rgba(0,0,0,0)" }.into())),
            ]));
        }
    }
    if drawing_mode && !drawing_points.is_empty() {
        let pts: Vec<Value> = drawing_points.iter().map(|p| Value::Array(vec![Value::F64(p.x), Value::F64(p.y)])).collect();
        canvas_commands.push(Value::Map(vec![
            (Value::Text("type".into()), Value::Text("polyline".into())),
            (Value::Text("points".into()), Value::Array(pts)),
            (Value::Text("stroke".into()), Value::Text("yellow".into())),
        ]));
    }

    let canvas_comp = canvas(canvas_commands);

    let zone_list: Vec<Component> = zones.iter().map(|z| {
        let is_sel = selected_id.as_deref() == Some(&z.id);
        let label = alloc::format!("{} ({})", z.name, z.zone_type);
        if is_sel { chip_with_icon(&label, "status", "check") } else { chip(&label, "category") }
    }).collect();

    let mut right_panel = vec![stack_v_gap("xs", zone_list)];
    if drawing_mode {
        right_panel.push(stack_h(vec![
            button("Zakończ", "zone-finish-drawing", "primary"),
            button("Anuluj", "zone-cancel-drawing", "ghost"),
        ]));
    }

    let main_area = grid(2, vec![canvas_comp, stack_v(right_panel)]);
    stack_v(vec![messages, toolbar, main_area])
}

fn build_audit_content() -> Component {
    let messages = build_messages_section();
    let toolbar = stack_h(vec![
        heading(2, "Audyt i RODO"),
        button("Wyczyść filtry", "audit-clear-filters", "ghost"),
        button("Eksport", "audit-export", "secondary"),
    ]);
    let fixture_rows: &[(&str, &str, &str, &str, &str, &str)] = &[
        ("ae-001", "2026-05-19T17:42:11Z", "admin", "addon-install", "B", "success"),
        ("ae-002", "2026-05-19T17:30:42Z", "admin", "camera-add", "C", "success"),
        ("ae-003", "2026-05-19T16:55:03Z", "operator1", "evidence-create", "C", "success"),
        ("ae-004", "2026-05-19T16:21:08Z", "viewer1", "frame-pickup", "C", "success"),
        ("ae-005", "2026-05-19T15:48:55Z", "admin", "settings-modify", "B", "success"),
        ("ae-009", "2026-05-19T11:20:33Z", "anon", "login", "A", "denied"),
    ];
    let cols = vec![
        Value::Text("Czas".into()), Value::Text("Użytkownik".into()),
        Value::Text("Akcja".into()), Value::Text("Ryzyko".into()),
        Value::Text("Wynik".into()),
    ];
    let rows: Vec<Value> = fixture_rows.iter().map(|(_, ts, user, action, risk, result)| {
        Value::Array(vec![
            Value::Text((*ts).into()), Value::Text((*user).into()),
            Value::Text((*action).into()), Value::Text((*risk).into()),
            Value::Text((*result).into()),
        ])
    }).collect();
    stack_v(vec![messages, toolbar, card(None, vec![table(cols, rows)])])
}

fn build_evidence_content() -> Component {
    let messages = build_messages_section();
    let tab = with_state(|s| match s.evidence.tab { EvidenceTab::Active => "active", EvidenceTab::Archive => "archive", EvidenceTab::All => "all" }.to_string());
    let toolbar = stack_h(vec![
        heading(2, "Eksport dowodowy"),
        filter_chips(
            vec![
                FilterChipDef { id: "active".into(), label: lit("active"), icon: None, badge: None, count_path: None },
                FilterChipDef { id: "archive".into(), label: lit("archive"), icon: None, badge: None, count_path: None },
                FilterChipDef { id: "all".into(), label: lit("all"), icon: None, badge: None, count_path: None },
            ],
            &tab,
        ),
        button("Nowa paczka", "evidence-new", "primary"),
    ]);
    let fixture: &[(&str, &str, &str, &str)] = &[
        ("case-2026-04-12", "Incydent C-04 wjazd", "active", "2026-04-12"),
        ("case-2026-03-04", "Kradzież — parking B", "archive", "2026-03-04"),
    ];
    let cols = vec![
        Value::Text("ID".into()), Value::Text("Tytuł".into()),
        Value::Text("Status".into()), Value::Text("Data".into()),
    ];
    let rows: Vec<Value> = fixture.iter().map(|(id, title, status, date)| {
        Value::Array(vec![
            Value::Text((*id).into()), Value::Text((*title).into()),
            Value::Text((*status).into()), Value::Text((*date).into()),
        ])
    }).collect();
    stack_v(vec![messages, toolbar, card(None, vec![table(cols, rows)])])
}

fn build_settings_content() -> Component {
    let messages = build_messages_section();
    let active_tab = with_state(|s| s.settings.tab_or_default().to_string());
    let tabs = filter_chips(
        vec![
            FilterChipDef { id: "general".into(), label: lit("general"), icon: None, badge: None, count_path: None },
            FilterChipDef { id: "storage".into(), label: lit("storage"), icon: None, badge: None, count_path: None },
            FilterChipDef { id: "notifications".into(), label: lit("notifications"), icon: None, badge: None, count_path: None },
            FilterChipDef { id: "access".into(), label: lit("access"), icon: None, badge: None, count_path: None },
        ],
        &active_tab,
    );
    let toolbar = stack_h(vec![heading(2, "Ustawienia"), tabs]);
    let content = match active_tab.as_str() {
        "general" => card(Some("Ogólne"), vec![
            key_value(vec![
                ("Wersja", "0.0.1"),
                ("Deployment", "depo-warszawa-pn"),
                ("Licencja", "enterprise"),
            ]),
            button("Zapisz", "settings-save", "primary"),
        ]),
        "storage" => card(Some("Przechowywanie"), vec![
            key_value(vec![
                ("Retencja wideo", "30 dni"),
                ("Retencja metadanych", "365 dni"),
                ("Retencja audit log", "1825 dni"),
            ]),
            button("Zapisz", "settings-save", "primary"),
        ]),
        "notifications" => card(Some("Powiadomienia"), vec![
            text("Kanał powiadomień: Slack / Email / Webhook"),
            button("Zapisz", "settings-save", "primary"),
        ]),
        "access" => card(Some("Kontrola dostępu"), vec![
            text("Matryca uprawnień per addon / rola — wkrótce."),
            button("Zapisz", "settings-save", "primary"),
        ]),
        _ => empty_state("Nieznana zakładka", None, None),
    };
    stack_v(vec![messages, toolbar, content])
}

fn build_onboarding_content() -> Component {
    let messages = build_messages_section();
    let step = with_state(|s| s.onboarding.step);
    let steps_data: Vec<StepDef> = vec![
        StepDef { id: "step0".into(), label: lit("Profil deploymentu"), optional: false, status: None, description: None },
        StepDef { id: "step1".into(), label: lit("Wybór modeli"), optional: false, status: None, description: None },
        StepDef { id: "step2".into(), label: lit("Powiadomienia"), optional: false, status: None, description: None },
        StepDef { id: "step3".into(), label: lit("Podsumowanie"), optional: false, status: None, description: None },
    ];
    let current_step_id = alloc::format!("step{}", step);
    let progress = step_progress(steps_data, &current_step_id);
    let step_content = match step {
        0 => card(Some("Krok 1: Profil deploymentu"), vec![
            text("Wybierz profil: depo / biuro / retail / custom"),
            button("Dalej", "onboarding-next", "primary"),
        ]),
        1 => card(Some("Krok 2: Modele"), vec![
            text("Wybierz modele detekcji do załadowania."),
            stack_h(vec![button("Wstecz", "onboarding-prev", "ghost"), button("Dalej", "onboarding-next", "primary")]),
        ]),
        2 => card(Some("Krok 3: Powiadomienia"), vec![
            text("Skonfiguruj kanał powiadomień (Slack / Email / Webhook)."),
            stack_h(vec![button("Wstecz", "onboarding-prev", "ghost"), button("Dalej", "onboarding-next", "primary")]),
        ]),
        _ => card(Some("Krok 4: Podsumowanie"), vec![
            text("Wszystko gotowe. Kliknij 'Zakończ' aby rozpocząć."),
            stack_h(vec![button("Wstecz", "onboarding-prev", "ghost"), button("Zakończ", "onboarding-finish", "primary")]),
        ]),
    };
    stack_v(vec![messages, progress, step_content])
}

fn build_bindings_content() -> Component {
    let messages = build_messages_section();
    let has_filter = with_state(|s| s.bindings.has_any_filter());
    let toolbar = stack_h(vec![
        heading(2, "Powiązania"),
        if has_filter { button("Wyczyść filtry", "binding-clear-filters", "ghost") } else { divider() },
    ]);

    let fixture: &[(&str, &str, &str, &str, &str)] = &[
        ("b-001", "eureka", "tentavision", "data_source", "active"),
        ("b-002", "contacts", "crm", "entity_lookup", "active"),
        ("b-003", "tentavision", "scheduler", "cron_trigger", "paused"),
    ];
    let cols = vec![
        Value::Text("Consumer".into()), Value::Text("Provider".into()),
        Value::Text("Typ".into()), Value::Text("Status".into()),
    ];
    let rows: Vec<Value> = fixture.iter().map(|(_, consumer, provider, typ, status)| {
        Value::Array(vec![
            Value::Text((*consumer).into()), Value::Text((*provider).into()),
            Value::Text((*typ).into()), Value::Text((*status).into()),
        ])
    }).collect();
    stack_v(vec![messages, toolbar, card(None, vec![table(cols, rows)])])
}

fn redact_url_for_display(url: &str) -> String {
    match extract_host_port(url) {
        Some((host, Some(port))) => alloc::format!("{}:{}", host, port),
        Some((host, None)) => host,
        None => "(nieznany host)".to_string(),
    }
}
