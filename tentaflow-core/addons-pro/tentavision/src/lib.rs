// =============================================================================
// File: addons-pro/tentavision/src/lib.rs
// TentaVision addon — video surveillance with 14 panels, CBOR SDK.
// =============================================================================

#![allow(clippy::too_many_lines, clippy::collapsible_else_if, dead_code)]

extern crate alloc;

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
use tentaflow_sdk_spec::protocol::ui::{
    bind::BindRef,
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

#[derive(Debug, Clone)]
struct CameraInfo {
    camera_id: String,
    display_name: String,
    url: String,
    vendor: String,
    status: String,
}

#[derive(Debug, Clone)]
struct CameraAddSpec {
    display_name: String,
    vendor: String,
    url: String,
    target_fps: u32,
    resolution: Option<String>,
    retention_class: String,
    profile: String,
}

#[derive(Debug, Clone)]
struct CameraAddResult {
    camera_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
enum AbiError {
    Permission = 1,
    NotFound = 2,
    Conflict = 3,
    QuotaExceeded = 4,
    CameraUnreachable = 5,
    CameraAuthFailed = 6,
    CameraVendorUnsupported = 7,
    PayloadTooLarge = 8,
    Timeout = 9,
    Unknown = 99,
}

impl AbiError {
    fn from_code(code: i32) -> Self {
        match code {
            1 => Self::Permission,
            2 => Self::NotFound,
            3 => Self::Conflict,
            4 => Self::QuotaExceeded,
            5 => Self::CameraUnreachable,
            6 => Self::CameraAuthFailed,
            7 => Self::CameraVendorUnsupported,
            8 => Self::PayloadTooLarge,
            9 => Self::Timeout,
            _ => Self::Unknown,
        }
    }
}

impl core::fmt::Display for AbiError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "AbiError({})", *self as i32)
    }
}

fn camera_list() -> Result<Vec<CameraInfo>, AbiError> {
    let mut buf = vec![0u8; 16384];
    let mut out_len: i32 = 0;
    let ret = unsafe {
        camera_list_v1(
            buf.as_mut_ptr() as i32, buf.len() as i32,
            &mut out_len as *mut i32 as i32,
        )
    };
    if ret < 0 {
        return Err(AbiError::from_code(-ret));
    }
    let len = out_len as usize;
    if len == 0 {
        return Ok(Vec::new());
    }
    buf.truncate(len);
    let json_str = String::from_utf8(buf).map_err(|_| AbiError::Unknown)?;
    let arr: Vec<serde_json::Value> =
        serde_json::from_str(&json_str).map_err(|_| AbiError::Unknown)?;
    let cameras = arr
        .iter()
        .map(|v| CameraInfo {
            camera_id: v.get("camera_id").and_then(|x| x.as_str()).unwrap_or("").to_string(),
            display_name: v.get("display_name").and_then(|x| x.as_str()).unwrap_or("").to_string(),
            url: v.get("url").and_then(|x| x.as_str()).unwrap_or("").to_string(),
            vendor: v.get("vendor").and_then(|x| x.as_str()).unwrap_or("").to_string(),
            status: v.get("status").and_then(|x| x.as_str()).unwrap_or("offline").to_string(),
        })
        .collect();
    Ok(cameras)
}

fn camera_add(spec: &CameraAddSpec) -> Result<CameraAddResult, AbiError> {
    let spec_json = serde_json::json!({
        "display_name": spec.display_name,
        "vendor": spec.vendor,
        "url": spec.url,
        "target_fps": spec.target_fps,
        "resolution": spec.resolution,
        "retention_class": spec.retention_class,
        "profile": spec.profile,
    });
    let spec_bytes = spec_json.to_string();
    let mut out = vec![0u8; 4096];
    let mut out_len: i32 = 0;
    let ret = unsafe {
        camera_add_v1(
            spec_bytes.as_ptr() as i32, spec_bytes.len() as i32,
            out.as_mut_ptr() as i32, out.len() as i32,
            &mut out_len as *mut i32 as i32,
        )
    };
    if ret < 0 {
        return Err(AbiError::from_code(-ret));
    }
    let len = out_len as usize;
    out.truncate(len);
    let json_str = String::from_utf8(out).map_err(|_| AbiError::Unknown)?;
    let v: serde_json::Value = serde_json::from_str(&json_str).map_err(|_| AbiError::Unknown)?;
    Ok(CameraAddResult {
        camera_id: v.get("camera_id").and_then(|x| x.as_str()).unwrap_or("").to_string(),
    })
}

fn camera_remove(id: &str) -> Result<(), AbiError> {
    let input = serde_json::json!({ "camera_id": id }).to_string();
    let mut out = vec![0u8; 256];
    let mut out_len: i32 = 0;
    let ret = unsafe {
        camera_remove_v1(
            input.as_ptr() as i32, input.len() as i32,
            out.as_mut_ptr() as i32, out.len() as i32,
            &mut out_len as *mut i32 as i32,
        )
    };
    if ret < 0 {
        return Err(AbiError::from_code(-ret));
    }
    Ok(())
}

fn camera_discover() -> Result<Vec<CameraInfo>, AbiError> {
    let mut buf = vec![0u8; 16384];
    let mut out_len: i32 = 0;
    let ret = unsafe {
        camera_discover_v1(
            buf.as_mut_ptr() as i32, buf.len() as i32,
            &mut out_len as *mut i32 as i32,
        )
    };
    if ret < 0 {
        return Err(AbiError::from_code(-ret));
    }
    let len = out_len as usize;
    if len == 0 {
        return Ok(Vec::new());
    }
    buf.truncate(len);
    let json_str = String::from_utf8(buf).map_err(|_| AbiError::Unknown)?;
    let arr: Vec<serde_json::Value> =
        serde_json::from_str(&json_str).map_err(|_| AbiError::Unknown)?;
    let cameras = arr
        .iter()
        .map(|v| CameraInfo {
            camera_id: String::new(),
            display_name: v.get("display_name").and_then(|x| x.as_str()).unwrap_or("").to_string(),
            url: v.get("url").and_then(|x| x.as_str()).unwrap_or("").to_string(),
            vendor: v.get("vendor").and_then(|x| x.as_str()).unwrap_or("").to_string(),
            status: "discovered".to_string(),
        })
        .collect();
    Ok(cameras)
}

// =============================================================================
// CBOR send helpers
// =============================================================================

const ADDON_ID: &str = "tentavision";
const PANEL_ID: &str = "overview";

static PANEL_EPOCH: AtomicU64 = AtomicU64::new(1);
static STATE_REVISION: AtomicU64 = AtomicU64::new(0);

fn next_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    alloc::format!("c{}", n)
}

fn send_ui(payload: &UiPayload) {
    let mut buf = Vec::with_capacity(4096);
    if minicbor::encode(payload, &mut buf).is_err() {
        log::error("TentaVision: CBOR encode failed");
        return;
    }
    let ret = unsafe { ui_render_cbor(buf.as_ptr() as i32, buf.len() as i32) };
    if ret < 0 {
        log::error("TentaVision: ui_render_cbor returned error");
    }
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
    let epoch = PANEL_EPOCH.load(Ordering::Relaxed);
    let payload = UiPayload::SlotContent(SlotContent {
        addon_id: ADDON_ID.into(),
        panel_id: PANEL_ID.into(),
        panel_epoch: epoch,
        slot_id: slot_id.into(),
        fragment,
        state_overlay: None,
    });
    send_ui(&payload);
}

fn send_state_patch(key: &str, value: Value) {
    let base = STATE_REVISION.load(Ordering::Relaxed);
    let new_rev = base + 1;
    STATE_REVISION.store(new_rev, Ordering::Relaxed);
    let epoch = PANEL_EPOCH.load(Ordering::Relaxed);
    let payload = UiPayload::StatePatch(StatePatch {
        addon_id: ADDON_ID.into(),
        panel_id: PANEL_ID.into(),
        panel_epoch: epoch,
        base_revision: base,
        new_revision: new_rev,
        ops: vec![PatchOp {
            path: StatePath::new(vec![PathSegment::Key(key.into())]),
            op: PatchOpKind::Set { value },
        }],
    });
    send_ui(&payload);
}

// =============================================================================
// Component construction helpers — typed structs from tentaflow-sdk-spec
// =============================================================================

fn lit(s: &str) -> BindRef {
    BindRef::Literal(Value::Text(s.into()))
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
    EmptyStateComp {
        icon: icon_named(parse_icon_name(icon.unwrap_or("info"))),
        heading: lit(title),
        message: message.map(lit),
        primary_action: None,
        secondary_action: None,
        variant: EmptyStateVariant::Default,
    }.into_component(next_id()).expect("EmptyState")
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
    form_name: String,
    form_url: String,
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum ProfilesView { Grid, List }

struct ProfilesState { view_mode: ProfilesView, category: String }
impl ProfilesState {
    const fn new() -> Self { Self { view_mode: ProfilesView::Grid, category: String::new() } }
    fn category_or_all(&self) -> &str { if self.category.is_empty() { "all" } else { &self.category } }
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

struct DiscoverState {
    visible: bool, scanning: bool, cameras: Vec<DiscoveredCam>,
    selected_index: Option<usize>, custom_name: String, error_message: Option<String>,
}
struct DiscoveredCam { vendor: String, url: String, suggested_name: String }
impl DiscoverState {
    const fn new() -> Self {
        Self { visible: false, scanning: false, cameras: Vec::new(), selected_index: None, custom_name: String::new(), error_message: None }
    }
    fn reset(&mut self) {
        self.visible = false; self.scanning = false; self.cameras.clear();
        self.selected_index = None; self.custom_name.clear(); self.error_message = None;
    }
}

impl PanelState {
    const fn new() -> Self {
        Self {
            current_panel: String::new(),
            add_form_visible: false, wizard_step: 0, cameras_filter: String::new(),
            form_name: String::new(), form_url: String::new(),
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

fn with_state<F, R>(f: F) -> R where F: FnOnce(&mut PanelState) -> R {
    let mut guard = match STATE.lock() { Ok(g) => g, Err(p) => p.into_inner() };
    f(&mut guard)
}

fn get_current_panel() -> String {
    with_state(|s| {
        if s.current_panel.is_empty() { "overview".to_string() } else { s.current_panel.clone() }
    })
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
    let panel_id = read_guest_string(panel_id_ptr, panel_id_len);
    PANEL_EPOCH.store(epoch as u64, core::sync::atomic::Ordering::Relaxed);
    log::info(&alloc::format!("TentaVision: on_panel_open panel='{}' epoch={}", panel_id, epoch));
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

    let current = get_current_panel();
    render_panel(&current);

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
    let slots = vec![
        SlotDecl {
            id: "content".into(),
            semantics: SlotSemantics::MainContent,
            default_state: SlotDefault::Loading,
            cache_policy: CachePolicy::OnNavigateBack,
            visibility: SlotVisibility::Always,
            max_payload_bytes: Some(256 * 1024),
        },
    ];
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
    send_slot_content("content", content);
}

// =============================================================================
// Action handlers
// =============================================================================

fn handle_action(action: &str, params: &JsonValue) -> JsonValue {
    log::info(&alloc::format!("TentaVision UI action '{}'", action));
    match action {
        "camera-add-show" => { with_state(|s| { s.add_form_visible = true; s.wizard_step = 0; s.discover.reset(); s.discover.visible = false; s.clear_messages(); s.form_name.clear(); s.form_url.clear(); }); json!({"ok":true}) }
        "camera-add-cancel" => { with_state(|s| { s.add_form_visible = false; s.wizard_step = 0; s.discover.reset(); s.clear_messages(); s.form_name.clear(); s.form_url.clear(); }); json!({"ok":true}) }
        "wizard-next" => { with_state(|s| { if s.wizard_step < 3 { s.wizard_step += 1; } }); json!({"ok":true}) }
        "wizard-prev" => { with_state(|s| { if s.wizard_step > 0 { s.wizard_step -= 1; } }); json!({"ok":true}) }
        "cameras-filter-change" => { let v = params.get("value").and_then(|x| x.as_str()).or_else(|| params.get("chipId").and_then(|x| x.as_str())).unwrap_or("all").to_string(); with_state(|s| { s.cameras_filter = if v == "all" { String::new() } else { v }; }); json!({"ok":true}) }
        "camera-add-submit" => handle_camera_add_submit(params),
        "camera-remove" => handle_camera_remove(params),
        "discover-show" => { with_state(|s| { s.add_form_visible = true; s.wizard_step = 0; s.discover.reset(); s.clear_messages(); }); json!({"ok":true}) }
        "discover-cancel" => { with_state(|s| { s.discover.reset(); s.clear_messages(); }); json!({"ok":true}) }
        "discover-scan" => handle_discover_scan(),
        "discover-select" => handle_discover_select(params),
        "discover-select-by-index" => {
            let v = params.get("value").and_then(|x| x.as_str()).unwrap_or("");
            let idx = v.parse::<u64>().ok();
            handle_discover_select(&json!({"index": idx}))
        }
        "discover-name-change" => handle_discover_name_change(params),
        "discover-add" => handle_discover_add(),
        "cameras-refresh" | "overview-refresh" => { with_state(|s| s.clear_messages()); json!({"ok":true}) }
        "panel-navigate" => {
            let target = params.get("panel_id")
                .or_else(|| params.get("item_id"))
                .and_then(|v| v.as_str()).unwrap_or("overview").to_string();
            render_panel(&target);
            json!({"ok":true, "panel_id": target})
        }
        "profile-view-toggle" => { let v = params.get("value").and_then(|x| x.as_str()).unwrap_or(""); with_state(|s| { s.profiles.view_mode = if v == "list" { ProfilesView::List } else { ProfilesView::Grid }; }); json!({"ok":true}) }
        "profile-filter-category" => { let v = params.get("value").and_then(|x| x.as_str()).or_else(|| params.get("chipId").and_then(|x| x.as_str())).unwrap_or("all").to_string(); with_state(|s| { s.profiles.category = if v == "all" { String::new() } else { v }; }); json!({"ok":true}) }
        "profile-add-show" => { with_state(|s| { s.success_message = Some("Kreator profilu — wkrótce (wymaga backendu profili).".into()); }); json!({"ok":true}) }
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

fn handle_camera_add_submit(params: &JsonValue) -> JsonValue {
    let values = params.get("values");
    let name = values.and_then(|v| v.get("name")).and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    let url = values.and_then(|v| v.get("url")).and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    with_state(|s| { s.form_name = name.clone(); s.form_url = url.clone(); s.clear_messages(); });
    if name.is_empty() || name.chars().count() > 60 {
        with_state(|s| { s.error_message = Some("Nazwa musi mieć 1–60 znaków.".to_string()); });
        return json!({"ok":false,"error":"invalid name"});
    }
    if url.is_empty() {
        with_state(|s| { s.error_message = Some("URL nie może być pusty.".to_string()); });
        return json!({"ok":false,"error":"invalid url"});
    }
    let vendor = match detect_vendor(&url) {
        Some(v) => v,
        None => { with_state(|s| { s.error_message = Some("Nieobsługiwany protokół (wspierane: rtsp://, rtsps://, http(s)://.../onvif).".to_string()); }); return json!({"ok":false,"error":"unsupported protocol"}); }
    };
    let spec = CameraAddSpec { display_name: name, vendor: vendor.to_string(), url, target_fps: 15, resolution: None, retention_class: "C".to_string(), profile: "default".to_string() };
    match camera_add(&spec) {
        Ok(result) => { with_state(|s| { s.add_form_visible = false; s.form_name.clear(); s.form_url.clear(); s.success_message = Some(alloc::format!("Kamera dodana ({}).", result.camera_id)); }); json!({"ok":true,"camera_id":result.camera_id}) }
        Err(e) => { with_state(|s| { s.error_message = Some(alloc::format!("Błąd dodawania: {}", abi_message(e))); }); json!({"ok":false,"error":alloc::format!("{}",e)}) }
    }
}

fn handle_camera_remove(params: &JsonValue) -> JsonValue {
    let camera_id = params.get("camera_id").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    with_state(|s| s.clear_messages());
    if camera_id.is_empty() { with_state(|s| { s.error_message = Some("Wybierz kamerę do usunięcia.".to_string()); }); return json!({"ok":false,"error":"empty camera_id"}); }
    if !is_valid_camera_id(&camera_id) { with_state(|s| { s.error_message = Some("Niepoprawny identyfikator kamery.".to_string()); }); return json!({"ok":false,"error":"invalid camera_id"}); }
    match camera_remove(&camera_id) {
        Ok(()) => { with_state(|s| { s.success_message = Some("Kamera usunięta.".to_string()); }); json!({"ok":true}) }
        Err(e) => { with_state(|s| { s.error_message = Some(alloc::format!("Błąd usuwania: {}", abi_message(e))); }); json!({"ok":false,"error":alloc::format!("{}",e)}) }
    }
}

fn handle_discover_scan() -> JsonValue {
    with_state(|s| { s.discover.scanning = true; s.discover.error_message = None; s.discover.cameras.clear(); s.discover.selected_index = None; s.clear_messages(); });
    let result = camera_discover();
    with_state(|s| {
        s.discover.scanning = false;
        match result {
            Ok(found) => {
                s.discover.cameras = found.iter().map(|c| DiscoveredCam { vendor: c.vendor.clone(), url: c.url.clone(), suggested_name: default_name_for_discovered(c) }).collect();
                if !s.discover.cameras.is_empty() { s.discover.selected_index = Some(0); s.discover.custom_name = s.discover.cameras[0].suggested_name.clone(); }
            }
            Err(e) => { s.discover.error_message = Some(alloc::format!("Błąd skanowania: {}", abi_message(e))); }
        }
    });
    json!({"ok":true})
}

fn handle_discover_select(params: &JsonValue) -> JsonValue {
    let index = params.get("index").and_then(|v| v.as_u64());
    with_state(|s| {
        s.discover.error_message = None;
        match index {
            Some(i) if (i as usize) < s.discover.cameras.len() => { let i = i as usize; s.discover.selected_index = Some(i); if s.discover.custom_name.trim().is_empty() { s.discover.custom_name = s.discover.cameras[i].suggested_name.clone(); } }
            _ => { s.discover.selected_index = None; s.discover.error_message = Some("Wybierz kamerę z listy.".to_string()); }
        }
    });
    json!({"ok":true})
}

fn handle_discover_name_change(params: &JsonValue) -> JsonValue {
    let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
    with_state(|s| { s.discover.custom_name = name; s.discover.error_message = None; });
    json!({"ok":true})
}

fn handle_discover_add() -> JsonValue {
    let snapshot = with_state(|s| { s.clear_messages(); s.discover.error_message = None; (s.discover.selected_index, s.discover.custom_name.trim().to_string(), s.discover.cameras.len()) });
    let (selected_index, name, total) = snapshot;
    let index = match selected_index { Some(i) if i < total => i, _ => { with_state(|s| { s.discover.error_message = Some("Wybierz kamerę z listy.".to_string()); }); return json!({"ok":false,"error":"no selection"}); } };
    if name.is_empty() || name.chars().count() > 60 { with_state(|s| { s.discover.error_message = Some("Nazwa musi mieć 1–60 znaków.".to_string()); }); return json!({"ok":false,"error":"invalid name"}); }
    let (vendor, url) = with_state(|s| { let cam = &s.discover.cameras[index]; (cam.vendor.clone(), cam.url.clone()) });
    let spec = CameraAddSpec { display_name: name, vendor, url, target_fps: 15, resolution: None, retention_class: "C".to_string(), profile: "default".to_string() };
    match camera_add(&spec) {
        Ok(_) => { with_state(|s| { s.discover.reset(); s.success_message = Some("Kamera dodana z ONVIF.".to_string()); }); json!({"ok":true}) }
        Err(e) => { with_state(|s| { s.discover.error_message = Some(alloc::format!("Błąd dodawania: {}", abi_message(e))); }); json!({"ok":false,"error":alloc::format!("{}",e)}) }
    }
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

fn detect_vendor(url: &str) -> Option<&'static str> {
    let lower = url.to_ascii_lowercase();
    if lower.starts_with("rtsp://") || lower.starts_with("rtsps://") { return Some("rtsp"); }
    if (lower.starts_with("http://") || lower.starts_with("https://")) && lower.contains("/onvif") { return Some("onvif"); }
    None
}

fn is_valid_camera_id(id: &str) -> bool {
    if id.len() != 40 || !id.starts_with("cam_") { return false; }
    id.chars().skip(4).all(|c| c.is_ascii_hexdigit() || c == '-')
}

fn default_name_for_discovered(cam: &CameraInfo) -> String {
    let host = extract_host_port(&cam.url).map(|(h, _)| h).unwrap_or_else(|| "ONVIF".to_string());
    if !cam.display_name.trim().is_empty() { cam.display_name.clone() } else { alloc::format!("ONVIF — {}", host) }
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
    let cameras = camera_list().unwrap_or_default();
    let total_cams = cameras.len();
    let online_cams = cameras.iter().filter(|c| c.status == "online").count();
    let offline_cams = total_cams.saturating_sub(online_cams);

    let (cam_val, cam_note) = if total_cams == 0 {
        ("22 / 24".to_string(), "2 offline (C-12, C-19)".to_string())
    } else {
        (alloc::format!("{} / {}", online_cams, total_cams),
         if offline_cams > 0 { alloc::format!("{} offline", offline_cams) } else { "wszystkie online".to_string() })
    };

    let kpi_row = grid(4, vec![
        stat_card(&cam_val, "Aktywne kamery", Some(&cam_note), Some("cameras"), Some("success")),
        stat_card("8", "Aktywne detektory", None, Some("brain"), Some("accent")),
        stat_card("147", "Alarmy 24h", Some("▲ 23% vs. wczoraj · 3 critical"), Some("bell"), Some("danger")),
        stat_card("68%", "GPU / latencja p95", Some("1.2 s"), Some("cpu"), Some("success")),
    ]);

    let recent_alarms = card_with_icon_action("Ostatnie alarmy", "bell", Some("Wszystkie 147 >"), vec![
        stack_v(vec![
            build_alarm_row("D2 · podejrzenie agresji", "C-04 wjazd", "12:43:21", "danger"),
            build_alarm_row("D1 · nieczytelna tablica ADR", "C-01 brama", "12:38:04", "warning"),
            build_alarm_row("D3 · pozostawiony bagaż > 90s", "C-07 peron", "12:31:55", "warning"),
            build_alarm_row("D6 · pojazd w strefie zakazu", "C-15 magazyn", "12:22:09", "info"),
        ]),
    ]);

    let runtime = card_with_icon("Stan natywnego runtime", "cpu", vec![
        build_runtime_table(),
    ]);

    let two_col = grid(2, vec![recent_alarms, runtime]);

    let heatmap_card = build_activity_heatmap();

    let messages = build_messages_section();

    stack_v(vec![messages, kpi_row, two_col, heatmap_card])
}

fn build_alarm_row(title: &str, camera: &str, time: &str, severity: &str) -> Component {
    let accent_tone = parse_tone(severity);
    let severity_label = match severity {
        "danger" => "krytyczne",
        "warning" => "ostrzeżenie",
        "info" => "info",
        _ => severity,
    };

    let title_text = text_styled(title, "body_strong");
    let meta_row = stack_h_gap("sm", vec![
        chip_with_icon(camera, "category", "cameras"),
        chip_with_icon(time, "category", "clock"),
        badge(severity_label, severity),
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

    Card {
        variant: CardVariant::Outlined,
        padding: Spacing::Sm,
        gap: Spacing::Sm,
        radius: RadiusToken::Md,
        shadow: ShadowToken::None,
        border: BorderToken::Hairline,
        background: BackgroundToken::None,
        accent: Some(accent_tone),
        children: vec![
            Flex {
                direction: FlexDirection::Row,
                gap: Spacing::Md,
                justify: FlexJustify::SpaceBetween,
                align: FlexAlign::Center,
                wrap: FlexWrap::NoWrap,
                children: vec![center, action],
                padding: None,
                background: None,
                radius: None,
            }.into_component(next_id()).expect("Flex"),
        ],
        interactive: false,
        clickable: false,
    }.into_component(next_id()).expect("Card")
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

fn build_activity_heatmap() -> Component {
    const ROWS: usize = 8;
    const COLS: usize = 24;
    let row_labels = vec!["C-01 brama","C-04 wjazd","C-07 peron","C-09 hala","C-12 magazyn","C-15 parking","C-18 dok","C-22 wjazd-2"];
    let col_labels: Vec<&str> = (0..COLS).map(|h| if h % 2 == 0 { match h { 0=>"0",2=>"2",4=>"4",6=>"6",8=>"8",10=>"10",12=>"12",14=>"14",16=>"16",18=>"18",20=>"20",22=>"22",_=>"" } } else { "" }).collect();
    let values: Vec<Vec<f64>> = (0..ROWS).map(|r| {
        let boost = if r == 1 { 1.4 } else { 1.0 };
        (0..COLS).map(|h| {
            let peak = if h > 7 && h < 18 { 1.0 } else { 0.2 };
            let seed = (r as f64 * 24.0 + h as f64) * 12.9898;
            let noise = (seed.sin() * 43758.5453).fract().abs();
            (peak * noise * boost).clamp(0.0, 1.0)
        }).collect()
    }).collect();
    card_with_icon("Mapa cieplna aktywności · ostatnie 24h × kamera", "dashboard", vec![
        heatmap(ROWS as u32, COLS as u32, values, row_labels, col_labels),
    ])
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
    let cameras = camera_list().unwrap_or_default();
    if cameras.is_empty() {
        return card(None, vec![empty_state("Brak kamer", Some("Dodaj kamerę aby zobaczyć podgląd na żywo."), Some("video"))]);
    }
    let streams: Vec<Component> = cameras.iter().take(4).map(|c| {
        card(Some(&c.display_name), vec![video_stream(&c.url)])
    }).collect();
    let messages = build_messages_section();
    stack_v(core::iter::once(messages).chain(streams).collect())
}

fn build_cameras_content() -> Component {
    let cameras = camera_list().unwrap_or_default();
    let messages = build_messages_section();
    let (add_visible, filter) = with_state(|s| (s.add_form_visible, s.cameras_filter.clone()));

    let mut children = vec![messages];

    // Header: heading + search + add button
    let search_input = {
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
    };
    let toolbar = stack_h(vec![
        heading(2, "Kamery"),
        search_input,
        button_with_icon("Dodaj kamerę", "camera-add-show", "primary", "plus"),
    ]);
    children.push(toolbar);

    // Use demo data when no real cameras exist
    let use_demo = cameras.is_empty();
    struct CamRow { name: &'static str, vendor: &'static str, addr: &'static str, status: &'static str, profile: &'static str, fps: &'static str, diag: &'static str, diag_tone: &'static str }
    let demo_rows: Vec<CamRow> = vec![
        CamRow { name: "C-01 brama wjazdowa", vendor: "Hikvision \u{00b7} ISAPI + RTSP", addr: "192.168.40.11", status: "online", profile: "ADR-brama", fps: "5/5", diag: "OK", diag_tone: "success" },
        CamRow { name: "C-04 wjazd-2", vendor: "Axis \u{00b7} VAPIX + ACAP", addr: "192.168.40.14", status: "online", profile: "Bezpieczeństwo-noc", fps: "12/15", diag: "backpressure", diag_tone: "warning" },
        CamRow { name: "C-07 peron", vendor: "ONVIF Profile T+M", addr: "192.168.40.17", status: "online", profile: "Peron-publiczny", fps: "10/10", diag: "OK", diag_tone: "success" },
        CamRow { name: "C-12 magazyn", vendor: "Dahua \u{00b7} CGI", addr: "192.168.40.22", status: "offline", profile: "\u{2014}", fps: "\u{2014}", diag: "brak heartbeat", diag_tone: "critical" },
        CamRow { name: "C-15 parking", vendor: "UniFi Protect \u{00b7} v3.x", addr: "unifi://nvr-1/cam-15", status: "online", profile: "ANPR-parking", fps: "5/5", diag: "obraz przyciemniony", diag_tone: "warning" },
        CamRow { name: "C-18 dok", vendor: "Hanwha \u{00b7} ONVIF S+T", addr: "192.168.40.28", status: "online", profile: "ADR-dok", fps: "5/5", diag: "OK", diag_tone: "success" },
        CamRow { name: "C-22 wjazd ADR", vendor: "Bosch \u{00b7} ONVIF S", addr: "192.168.40.32", status: "degraded", profile: "ADR-brama", fps: "3/5", diag: "drift 38ms", diag_tone: "warning" },
    ];

    // Compute filter counts
    let (total, online, offline, warnings, unlinked) = if use_demo {
        let t = demo_rows.len();
        let on = demo_rows.iter().filter(|r| r.status == "online").count();
        let off = demo_rows.iter().filter(|r| r.status == "offline").count();
        let warn = demo_rows.iter().filter(|r| r.diag_tone == "warning" || r.diag_tone == "critical").count();
        (t, on, off, warn, 3usize)
    } else {
        let t = cameras.len();
        let on = cameras.iter().filter(|c| c.status == "online").count();
        let off = cameras.iter().filter(|c| c.status == "offline").count();
        (t, on, off, 0usize, 0usize)
    };

    // Sub-filter tabs
    let active_filter = if filter.is_empty() { "all" } else { &filter };
    let sub_tabs = filter_chips(
        vec![
            FilterChipDef { id: "all".into(), label: lit(&alloc::format!("Wszystkie ({})", total)), icon: None, badge: None, count_path: None },
            FilterChipDef { id: "online".into(), label: lit(&alloc::format!("Online ({})", online)), icon: None, badge: None, count_path: None },
            FilterChipDef { id: "offline".into(), label: lit(&alloc::format!("Offline ({})", offline)), icon: None, badge: None, count_path: None },
            FilterChipDef { id: "warnings".into(), label: lit(&alloc::format!("Ostrzeżenia ({})", warnings)), icon: None, badge: None, count_path: None },
            FilterChipDef { id: "unlinked".into(), label: lit(&alloc::format!("Niepowiązane ({})", unlinked)), icon: None, badge: None, count_path: None },
        ],
        active_filter,
    );
    children.push(sub_tabs);

    // Wizard modal (when visible)
    if add_visible {
        children.push(build_add_camera_wizard());
    }

    // Camera table
    if use_demo {
        let mut table_rows: Vec<Component> = vec![build_cameras_table_header()];
        for r in &demo_rows {
            table_rows.push(build_camera_row(r.name, r.vendor, r.addr, r.status, r.profile, r.fps, r.diag, r.diag_tone));
        }
        children.push(card(None, vec![stack_v_gap("xs", table_rows)]));
    } else if cameras.is_empty() {
        children.push(card(None, vec![empty_state("Brak kamer", Some("Dodaj kamerę aby rozpocząć monitorowanie."), Some("cameras"))]));
    } else {
        let mut table_rows: Vec<Component> = vec![build_cameras_table_header()];
        for c in &cameras {
            table_rows.push(build_camera_row(&c.display_name, &c.vendor, &redact_url_for_display(&c.url), &c.status, "\u{2014}", "\u{2014}", "\u{2014}", "muted"));
        }
        children.push(card(None, vec![stack_v_gap("xs", table_rows)]));
    }

    stack_v(children)
}

fn build_camera_status_chip(status: &str) -> Component {
    let (label, tone) = match status {
        "online" => ("online", "success"),
        "offline" => ("offline", "critical"),
        "degraded" => ("degraded", "warning"),
        _ => (status, "muted"),
    };
    chip_toned(label, tone)
}

fn build_camera_row(name: &str, vendor: &str, addr: &str, status: &str, profile: &str, fps: &str, diag: &str, diag_tone: &str) -> Component {
    let name_cell = text_styled(name, "body_strong");
    let vendor_cell = text(vendor);
    let addr_cell = text_styled(addr, "mono");
    let status_cell = build_camera_status_chip(status);
    let profile_cell = if profile == "\u{2014}" {
        text("\u{2014}")
    } else {
        ChipComp {
            variant: ChipVariant::Soft,
            tone: Tone::Primary,
            label: lit(profile),
            icon: None,
            avatar: None,
            selected: None,
            removable: false,
        }.into_component(next_id()).expect("Chip")
    };
    let fps_cell = if fps == "\u{2014}" {
        text("\u{2014}")
    } else {
        text_styled(fps, "body_strong")
    };
    let diag_cell = match diag_tone {
        "success" => chip_toned_icon(diag, "success", "check"),
        "warning" => chip_toned(diag, "warning"),
        "critical" => chip_toned_icon(diag, "critical", "info"),
        _ => chip_toned(diag, "muted"),
    };
    let action_cell = button("\u{22ef}", "camera-row-action", "ghost");

    Flex {
        direction: FlexDirection::Row,
        gap: Spacing::Md,
        justify: FlexJustify::Start,
        align: FlexAlign::Center,
        wrap: FlexWrap::NoWrap,
        children: vec![name_cell, vendor_cell, addr_cell, status_cell, profile_cell, fps_cell, diag_cell, action_cell],
        padding: None,
        background: None,
        radius: None,
    }.into_component(next_id()).expect("Flex")
}

fn build_cameras_table_header() -> Component {
    let headers: Vec<Component> = ["Nazwa", "Vendor / Protokół", "Adres", "Status", "Profil", "FPS", "Diagnostyka", ""]
        .iter().map(|h| text_styled(h, "caption")).collect();
    Flex {
        direction: FlexDirection::Row,
        gap: Spacing::Md,
        justify: FlexJustify::Start,
        align: FlexAlign::Center,
        wrap: FlexWrap::NoWrap,
        children: headers,
        padding: None,
        background: None,
        radius: None,
    }.into_component(next_id()).expect("Flex")
}

fn build_add_camera_wizard() -> Component {
    let (step, scanning, discovered_count, err) = with_state(|s| {
        (s.wizard_step, s.discover.scanning, s.discover.cameras.len(), s.error_message.clone())
    });

    let step_labels = ["Odkrywanie", "Wybór & poświadczenia", "Podgląd & kalibracja", "Profil analityczny"];
    let step_indicator = step_progress(
        step_labels.iter().enumerate().map(|(i, label)| StepDef {
            id: alloc::format!("step{}", i),
            label: lit(label),
            optional: false,
            status: if (i as u8) < step { Some(lit("done")) } else if i as u8 == step { Some(lit("active")) } else { None },
            description: None,
        }).collect(),
        &alloc::format!("step{}", step),
    );

    let title = alloc::format!("Dodaj kamerę \u{2014} krok {} z 4", step + 1);

    let body = match step {
        0 => build_wizard_step_discovery(scanning, discovered_count),
        1 => build_wizard_step_selection(),
        2 => build_wizard_step_preview(),
        3 => build_wizard_step_profile(),
        _ => text("Nieznany krok."),
    };

    let mut body_children = vec![step_indicator, body];
    if let Some(e) = err { body_children.push(alert(&e, "critical")); }

    // Footer buttons
    let mut footer = Vec::new();
    if step > 0 {
        footer.push(button_with_icon("Wstecz", "wizard-prev", "ghost", "info"));
    }
    footer.push(button("Anuluj", "camera-add-cancel", "ghost"));
    if step < 3 {
        let next_label = match step {
            0 => "Dalej: Wybór",
            1 => "Dalej: Podgląd",
            2 => "Dalej: Profil",
            _ => "Dalej",
        };
        footer.push(button(next_label, "wizard-next", "primary"));
    } else {
        footer.push(button("Zakończ", "camera-add-submit", "primary"));
    }

    SectionCard {
        title: lit(&title),
        subtitle: None,
        header_actions: vec![button("×", "camera-add-cancel", "ghost")],
        header_divider: true,
        body: body_children,
        footer: Some(vec![stack_h(footer)]),
        padding: Spacing::Lg,
        gap: Spacing::Md,
        variant: CardVariant::Outlined,
        radius: RadiusToken::Lg,
        shadow: ShadowToken::Medium,
        border: BorderToken::Hairline,
        background: BackgroundToken::None,
        accent: None,
    }.into_component(next_id()).expect("SectionCard")
}

fn build_wizard_step_discovery(scanning: bool, discovered_count: usize) -> Component {
    if scanning {
        return stack_v(vec![
            spinner("lg"),
            text("Skanowanie sieci kamerowej (ONVIF WS-Discovery + mDNS + ARP)..."),
        ]);
    }
    if discovered_count == 0 {
        return stack_v(vec![
            text("Automatyczne wyszukiwanie kamer w sieci lokalnej."),
            stack_h(vec![
                button_with_icon("Skanuj sieć", "discover-scan", "primary", "search"),
            ]),
        ]);
    }
    let discovered = with_state(|s| s.discover.cameras.iter().enumerate().map(|(i, c)| {
        (i, c.suggested_name.clone(), c.url.clone(), c.vendor.clone())
    }).collect::<Vec<_>>());
    let selected_idx = with_state(|s| s.discover.selected_index);

    let mut cam_rows: Vec<Component> = Vec::new();
    for (i, name, url, vendor) in &discovered {
        let is_sel = selected_idx == Some(*i);
        let label = text_styled(name, "body_strong");
        let meta = text_styled(&alloc::format!("{} \u{00b7} {}", url, vendor), "caption");
        let capability = if vendor.contains("ONVIF") {
            chip_toned_icon("ONVIF OK", "success", "check")
        } else if vendor.contains("ACAP") || vendor.contains("edge") {
            chip_toned("edge analytics", "info")
        } else if vendor.contains("RTSP") || vendor.to_ascii_lowercase().contains("rtsp") {
            chip_toned("tylko RTSP", "warning")
        } else {
            chip_toned("standard", "muted")
        };
        let row_content = stack_h(vec![
            stack_v_gap("xs", vec![label, meta]),
            capability,
        ]);
        let tone = if is_sel { Tone::Primary } else { Tone::Neutral };
        let mut row_card = Card {
            variant: if is_sel { CardVariant::Filled } else { CardVariant::Outlined },
            padding: Spacing::Sm,
            gap: Spacing::Sm,
            radius: RadiusToken::Sm,
            shadow: ShadowToken::None,
            border: BorderToken::Hairline,
            background: BackgroundToken::None,
            accent: if is_sel { Some(tone) } else { None },
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

    stack_v(vec![
        text(&alloc::format!("Znaleziono {} kamer w sieci kamerowej.", discovered_count)),
        stack_v_gap("xs", cam_rows),
        button_with_icon("Skanuj ponownie", "discover-scan", "ghost", "search"),
    ])
}

fn build_wizard_step_selection() -> Component {
    let discovered_count = with_state(|s| s.discover.cameras.len());

    let discovery_summary = if discovered_count > 0 {
        text(&alloc::format!("Znaleziono {} nowe kamery w sieci kamerowej (VLAN 40). Wybierz tę, którą chcesz dodać.", discovered_count))
    } else {
        text("Brak wyników skanowania. Wróć do kroku 1 aby skanować lub wprowadź dane ręcznie.")
    };

    // Credential form fields
    let name_input = input("Nazwa kamery", "C-23 wjazd-ADR-2", "name");
    let location_select = select("Lokalizacja / strefa", vec![
        SelectOption { value: SelectValue::Text("brama".into()), label: lit("Brama wjazdowa"), icon: None, disabled: false, group_id: None, description: None },
        SelectOption { value: SelectValue::Text("parking".into()), label: lit("Parking"), icon: None, disabled: false, group_id: None, description: None },
        SelectOption { value: SelectValue::Text("hala".into()), label: lit("Hala"), icon: None, disabled: false, group_id: None, description: None },
    ], "camera_location");
    let user_input = input("Użytkownik", "admin", "camera_user");
    let password_input = {
        use tentaflow_sdk_spec::protocol::ui::form::Input;
        Input {
            r#type: InputType::Password,
            bind_path: StatePath::new(vec![PathSegment::Key("camera_password".into())]),
            placeholder: Some(lit("zapisz w vault TentaFlow")),
            label: Some(lit("Hasło")),
            hint: Some(lit("Poświadczenia w secret store. Rotacja co 90 dni.")),
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
        }.into_component("camera_password").expect("Input")
    };
    let form_grid = grid(2, vec![name_input, location_select, user_input, password_input]);

    // Firmware warning alert
    let firmware_alert = AlertComp {
        tone: parse_tone("warning"),
        variant: AlertVariant::Default,
        icon: Some(icon_named(parse_icon_name("info"))),
        title: Some(lit("Wykryto wariancje firmware")),
        message: lit("Hikvision firmware 5.7.x ma wyłączony ONVIF domyślnie. Po podaniu poświadczeń włączymy go automatycznie (wymaga uprawnień admin). Alternatywnie: RTSP fallback."),
        actions: None,
        dismissible: false,
    }.into_component(next_id()).expect("Alert");

    stack_v(vec![discovery_summary, form_grid, firmware_alert])
}

fn build_wizard_step_preview() -> Component {
    stack_v(vec![
        text("Podgląd strumienia i kalibracja kamery."),
        empty_state("Oczekiwanie na podgląd", Some("Wprowadź poświadczenia w kroku 2, aby uzyskać podgląd strumienia."), Some("video")),
    ])
}

fn build_wizard_step_profile() -> Component {
    stack_v(vec![
        text("Przypisz profil analityczny do kamery."),
        empty_state("Wybierz profil", Some("Wybierz istniejący profil lub utwórz nowy."), Some("brain")),
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

fn build_profiles_content() -> Component {
    let messages = build_messages_section();
    let category = with_state(|s| s.profiles.category_or_all().to_string());
    let chips = filter_chips(
        vec![
            FilterChipDef { id: "all".into(), label: lit("all"), icon: None, badge: None, count_path: None },
            FilterChipDef { id: "person".into(), label: lit("person"), icon: None, badge: None, count_path: None },
            FilterChipDef { id: "vehicle".into(), label: lit("vehicle"), icon: None, badge: None, count_path: None },
            FilterChipDef { id: "face".into(), label: lit("face"), icon: None, badge: None, count_path: None },
        ],
        &category,
    );
    let toolbar = stack_h(vec![
        heading(2, "Profile analityczne"),
        chips,
        button("Dodaj profil", "profile-add-show", "primary"),
    ]);
    let placeholder_grid = grid(3, vec![
        card(Some("Profil #1"), vec![avatar("P1", "lg"), text("Osoba — pracownik")]),
        card(Some("Profil #2"), vec![avatar("P2", "lg"), text("Pojazd — ADR")]),
        card(Some("Profil #3"), vec![avatar("P3", "lg"), text("Twarz — VIP")]),
    ]);
    stack_v(vec![messages, toolbar, placeholder_grid])
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
