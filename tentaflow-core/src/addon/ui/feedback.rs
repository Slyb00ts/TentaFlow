// === File: addon/ui/feedback.rs — feedback primitives (Alert/Banner/Callout/Toast/Spinner/ProgressBar/Skeleton/Hint/GateScreen) ===

use serde::{Deserialize, Serialize};

use super::theme::IconName;
use super::UiComponent;

// =============================================================================
// FeedbackComponent — sub-enum for status/notification/loading primitives
// =============================================================================

/// Feedback primitives covering inline status messages (Alert/Banner/Callout/Hint),
/// loading state (Spinner/ProgressBar/Skeleton), transient notifications (Toast)
/// and gating UX (GateScreen).
///
/// JSON tag `progress_v2` shadows pre-2.1 `Legacy::Progress`. The other tags
/// (`alert`, `banner`, `callout`, `toast`, `spinner`, `skeleton`, `hint`,
/// `gate_screen`) do not collide with Legacy and use their natural names.
///
/// Embedding rules: variants holding `UiComponent` (`Alert.actions`,
/// `Banner.actions` via `action_label`, `Callout.children`, `GateScreen.actions`)
/// reject overlay-kind containers through the central recursive validator.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FeedbackComponent {
    /// Inline alert — sticky within its section. Not auto-dismissed.
    Alert {
        tone: FeedbackTone,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        icon: Option<IconName>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        actions: Vec<UiComponent>,
        #[serde(default)]
        dismissible: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        on_dismiss: Option<String>,
    },
    /// Full-width strip at the top of a view — global announcement
    /// ("system maintenance", "trial ending in 3 days").
    Banner {
        tone: FeedbackTone,
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        icon: Option<IconName>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        action_label: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        on_action: Option<String>,
        #[serde(default)]
        dismissible: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        on_dismiss: Option<String>,
    },
    /// Stronger explanatory block — wizard "what happens next" info cards.
    /// Larger than `Alert`; renders rich body content via `children`.
    Callout {
        tone: FeedbackTone,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        icon: Option<IconName>,
        children: Vec<UiComponent>,
    },
    /// Transient popup. The schema here is declarative (e.g. seeded toasts
    /// in initial panel state). Runtime toasts will be emitted by the
    /// `ui_toast` host fn — same payload shape.
    Toast {
        id: String,
        tone: FeedbackTone,
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        #[serde(default = "default_toast_duration_ms")]
        duration_ms: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        icon: Option<IconName>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        action_label: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        on_action: Option<String>,
    },
    /// Loading spinner.
    Spinner {
        #[serde(default)]
        size: SpinnerSize,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
    /// Linear progress bar — collides with `Legacy::Progress` so JSON tag is
    /// `progress_v2`.
    #[serde(rename = "progress_v2")]
    ProgressBar {
        value: f64,
        #[serde(default = "default_progress_max")]
        max: f64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        #[serde(default)]
        tone: FeedbackTone,
        #[serde(default)]
        indeterminate: bool,
        #[serde(default)]
        size: ProgressBarSize,
        #[serde(default)]
        show_percent: bool,
    },
    /// Grey placeholder boxes shown while content loads.
    Skeleton {
        #[serde(default = "default_skeleton_lines")]
        lines: u8,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        width_percent: Option<u8>,
        #[serde(default)]
        shape: SkeletonShape,
    },
    /// Compact helper text under form fields or section headers — smaller
    /// than `Alert`, closer to a tooltip-style hint.
    Hint {
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        icon: Option<IconName>,
        #[serde(default)]
        tone: FeedbackTone,
    },
    /// Full-screen "missing permission / unmet prerequisite" gate (M07).
    /// Blocks content until the user satisfies the listed requirements.
    GateScreen {
        title: String,
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        icon: Option<IconName>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        requirements: Vec<GateRequirement>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        actions: Vec<UiComponent>,
    },
}

// =============================================================================
// Supporting enums and structs
// =============================================================================

/// Semantic tone applied across feedback primitives. Renderer maps each tone
/// to the corresponding palette (info=blue, success=green, warning=amber,
/// danger=red, neutral=grey). `Info` is the default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackTone {
    #[default]
    Info,
    Success,
    Warning,
    Danger,
    Neutral,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SpinnerSize {
    Sm,
    #[default]
    Md,
    Lg,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ProgressBarSize {
    Sm,
    #[default]
    Md,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SkeletonShape {
    #[default]
    Block,
    Avatar,
    Image,
}

/// One item in a `GateScreen.requirements` list. `satisfied=true` shows a
/// success check, `false` shows the unmet/blocked state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GateRequirement {
    pub label: String,
    pub satisfied: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

fn default_toast_duration_ms() -> u32 {
    5000
}

fn default_progress_max() -> f64 {
    1.0
}

fn default_skeleton_lines() -> u8 {
    3
}

// =============================================================================
// Validation
// =============================================================================

const TOAST_DURATION_MIN_MS: u32 = 500;
const TOAST_DURATION_MAX_MS: u32 = 60_000;
const SKELETON_LINES_MIN: u8 = 1;
const SKELETON_LINES_MAX: u8 = 20;
const SKELETON_WIDTH_MIN: u8 = 1;
const SKELETON_WIDTH_MAX: u8 = 100;

/// Validate a single feedback component, recursing into embedded
/// `UiComponent` children. Error strings are static and never echo
/// addon-controlled input.
pub fn validate_and_normalize(component: &mut FeedbackComponent) -> Result<(), &'static str> {
    use FeedbackComponent::*;
    match component {
        Alert { actions, .. } => {
            validate_children(actions, "alert_actions_invalid")?;
            Ok(())
        }
        Banner { .. } => Ok(()),
        Callout { children, .. } => {
            validate_children(children, "callout_children_invalid")?;
            Ok(())
        }
        Toast { duration_ms, .. } => {
            if *duration_ms < TOAST_DURATION_MIN_MS || *duration_ms > TOAST_DURATION_MAX_MS {
                return Err("toast_duration_out_of_range");
            }
            Ok(())
        }
        Spinner { .. } | Hint { .. } => Ok(()),
        ProgressBar {
            value,
            max,
            indeterminate,
            ..
        } => {
            if !(*max > 0.0) {
                return Err("progress_max_must_be_positive");
            }
            if !*indeterminate && (*value < 0.0 || *value > *max) {
                return Err("progress_value_out_of_range");
            }
            Ok(())
        }
        Skeleton {
            lines,
            width_percent,
            ..
        } => {
            if !(SKELETON_LINES_MIN..=SKELETON_LINES_MAX).contains(lines) {
                return Err("skeleton_lines_out_of_range");
            }
            if let Some(w) = width_percent {
                if !(SKELETON_WIDTH_MIN..=SKELETON_WIDTH_MAX).contains(w) {
                    return Err("skeleton_width_out_of_range");
                }
            }
            Ok(())
        }
        GateScreen { actions, .. } => {
            validate_children(actions, "gate_screen_actions_invalid")?;
            Ok(())
        }
    }
}

fn validate_children(
    children: &mut [UiComponent],
    err_tag: &'static str,
) -> Result<(), &'static str> {
    for c in children.iter_mut() {
        super::reject_overlay_kind_in_root(c).map_err(|_| err_tag)?;
        super::validate_and_normalize_component(c).map_err(|_| err_tag)?;
    }
    Ok(())
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::addon::ui::container::{ContainerComponent, WindowSize};
    use crate::addon::ui::legacy::LegacyComponent;

    fn legacy_text(s: &str) -> UiComponent {
        UiComponent::Legacy(LegacyComponent::Text {
            content: s.to_string(),
            style: None,
        })
    }

    fn window_overlay() -> UiComponent {
        UiComponent::Container(ContainerComponent::Window {
            title: "x".to_string(),
            size: WindowSize::Md,
            dismissable: true,
            on_close: None,
            children: vec![],
            footer: vec![],
        })
    }

    fn round_trip(c: &FeedbackComponent) -> FeedbackComponent {
        let j = serde_json::to_value(c).expect("serialize");
        serde_json::from_value(j).expect("deserialize")
    }

    #[test]
    fn alert_round_trip() {
        let c = FeedbackComponent::Alert {
            tone: FeedbackTone::Warning,
            title: Some("Heads up".into()),
            message: "Disk almost full".into(),
            icon: Some(IconName::Warning),
            actions: vec![legacy_text("ok")],
            dismissible: true,
            on_dismiss: Some("dismiss".into()),
        };
        assert_eq!(round_trip(&c), c);
    }

    #[test]
    fn banner_round_trip() {
        let c = FeedbackComponent::Banner {
            tone: FeedbackTone::Info,
            message: "Maintenance window 02:00 UTC".into(),
            icon: Some(IconName::Info),
            action_label: Some("Details".into()),
            on_action: Some("show_details".into()),
            dismissible: false,
            on_dismiss: None,
        };
        let j = serde_json::to_value(&c).expect("ser");
        assert_eq!(j["type"], serde_json::json!("banner"));
        let back: FeedbackComponent = serde_json::from_value(j).expect("de");
        assert_eq!(back, c);
    }

    #[test]
    fn callout_round_trip() {
        let c = FeedbackComponent::Callout {
            tone: FeedbackTone::Success,
            title: Some("Ready".into()),
            icon: Some(IconName::Check),
            children: vec![legacy_text("body")],
        };
        assert_eq!(round_trip(&c), c);
    }

    #[test]
    fn toast_round_trip_with_default_duration() {
        let j = serde_json::json!({
            "type": "toast",
            "id": "t1",
            "tone": "success",
            "message": "Saved"
        });
        let c: FeedbackComponent = serde_json::from_value(j).expect("de");
        if let FeedbackComponent::Toast { duration_ms, .. } = &c {
            assert_eq!(*duration_ms, 5000);
        } else {
            panic!("not toast");
        }
    }

    #[test]
    fn spinner_round_trip_default_size() {
        let j = serde_json::json!({ "type": "spinner" });
        let c: FeedbackComponent = serde_json::from_value(j).expect("de");
        if let FeedbackComponent::Spinner { size, .. } = c {
            assert_eq!(size, SpinnerSize::Md);
        } else {
            panic!("not spinner");
        }
    }

    #[test]
    fn progress_bar_type_tag_is_progress_v2() {
        let c = FeedbackComponent::ProgressBar {
            value: 0.4,
            max: 1.0,
            label: Some("Uploading".into()),
            tone: FeedbackTone::Info,
            indeterminate: false,
            size: ProgressBarSize::Md,
            show_percent: true,
        };
        let j = serde_json::to_value(&c).expect("ser");
        assert_eq!(j["type"], serde_json::json!("progress_v2"));
        let back: FeedbackComponent = serde_json::from_value(j).expect("de");
        assert_eq!(back, c);
    }

    #[test]
    fn skeleton_round_trip_with_defaults() {
        let j = serde_json::json!({ "type": "skeleton" });
        let c: FeedbackComponent = serde_json::from_value(j).expect("de");
        if let FeedbackComponent::Skeleton { lines, shape, .. } = c {
            assert_eq!(lines, 3);
            assert_eq!(shape, SkeletonShape::Block);
        } else {
            panic!("not skeleton");
        }
    }

    #[test]
    fn hint_round_trip() {
        let c = FeedbackComponent::Hint {
            message: "Min 8 chars".into(),
            icon: Some(IconName::Info),
            tone: FeedbackTone::Info,
        };
        assert_eq!(round_trip(&c), c);
    }

    #[test]
    fn gate_screen_round_trip() {
        let c = FeedbackComponent::GateScreen {
            title: "Access required".into(),
            message: "Enable ReID model before opening this view".into(),
            icon: Some(IconName::Locked),
            requirements: vec![
                GateRequirement {
                    label: "ReID model loaded".into(),
                    satisfied: false,
                    description: Some("Download from Models".into()),
                },
                GateRequirement {
                    label: "Camera assigned".into(),
                    satisfied: true,
                    description: None,
                },
            ],
            actions: vec![legacy_text("go to models")],
        };
        assert_eq!(round_trip(&c), c);
    }

    #[test]
    fn ui_component_feedback_round_trip_through_sum() {
        let c = UiComponent::Feedback(FeedbackComponent::Hint {
            message: "x".into(),
            icon: None,
            tone: FeedbackTone::Neutral,
        });
        let j = serde_json::to_value(&c).expect("ser");
        let back: UiComponent = serde_json::from_value(j).expect("de");
        assert_eq!(back, c);
    }

    // ---- validation rejection cases ----

    #[test]
    fn toast_duration_below_min_is_rejected() {
        let mut c = FeedbackComponent::Toast {
            id: "t".into(),
            tone: FeedbackTone::Info,
            message: "x".into(),
            title: None,
            duration_ms: 100,
            icon: None,
            action_label: None,
            on_action: None,
        };
        let err = validate_and_normalize(&mut c).expect_err("must reject");
        assert_eq!(err, "toast_duration_out_of_range");
    }

    #[test]
    fn toast_duration_above_max_is_rejected() {
        let mut c = FeedbackComponent::Toast {
            id: "t".into(),
            tone: FeedbackTone::Info,
            message: "x".into(),
            title: None,
            duration_ms: 120_000,
            icon: None,
            action_label: None,
            on_action: None,
        };
        let err = validate_and_normalize(&mut c).expect_err("must reject");
        assert_eq!(err, "toast_duration_out_of_range");
    }

    #[test]
    fn progress_bar_negative_value_is_rejected() {
        let mut c = FeedbackComponent::ProgressBar {
            value: -0.1,
            max: 1.0,
            label: None,
            tone: FeedbackTone::Info,
            indeterminate: false,
            size: ProgressBarSize::Md,
            show_percent: false,
        };
        let err = validate_and_normalize(&mut c).expect_err("must reject");
        assert_eq!(err, "progress_value_out_of_range");
    }

    #[test]
    fn progress_bar_value_above_max_is_rejected() {
        let mut c = FeedbackComponent::ProgressBar {
            value: 1.5,
            max: 1.0,
            label: None,
            tone: FeedbackTone::Info,
            indeterminate: false,
            size: ProgressBarSize::Md,
            show_percent: false,
        };
        let err = validate_and_normalize(&mut c).expect_err("must reject");
        assert_eq!(err, "progress_value_out_of_range");
    }

    #[test]
    fn progress_bar_indeterminate_skips_value_check() {
        let mut c = FeedbackComponent::ProgressBar {
            value: 99.0,
            max: 1.0,
            label: None,
            tone: FeedbackTone::Info,
            indeterminate: true,
            size: ProgressBarSize::Md,
            show_percent: false,
        };
        assert!(validate_and_normalize(&mut c).is_ok());
    }

    #[test]
    fn progress_bar_zero_max_is_rejected() {
        let mut c = FeedbackComponent::ProgressBar {
            value: 0.0,
            max: 0.0,
            label: None,
            tone: FeedbackTone::Info,
            indeterminate: false,
            size: ProgressBarSize::Md,
            show_percent: false,
        };
        let err = validate_and_normalize(&mut c).expect_err("must reject");
        assert_eq!(err, "progress_max_must_be_positive");
    }

    #[test]
    fn skeleton_zero_lines_is_rejected() {
        let mut c = FeedbackComponent::Skeleton {
            lines: 0,
            width_percent: None,
            shape: SkeletonShape::Block,
        };
        let err = validate_and_normalize(&mut c).expect_err("must reject");
        assert_eq!(err, "skeleton_lines_out_of_range");
    }

    #[test]
    fn skeleton_too_many_lines_is_rejected() {
        let mut c = FeedbackComponent::Skeleton {
            lines: 100,
            width_percent: None,
            shape: SkeletonShape::Block,
        };
        let err = validate_and_normalize(&mut c).expect_err("must reject");
        assert_eq!(err, "skeleton_lines_out_of_range");
    }

    #[test]
    fn skeleton_width_percent_out_of_range_is_rejected() {
        let mut c = FeedbackComponent::Skeleton {
            lines: 3,
            width_percent: Some(150),
            shape: SkeletonShape::Block,
        };
        let err = validate_and_normalize(&mut c).expect_err("must reject");
        assert_eq!(err, "skeleton_width_out_of_range");
    }

    #[test]
    fn skeleton_width_percent_zero_is_rejected() {
        let mut c = FeedbackComponent::Skeleton {
            lines: 3,
            width_percent: Some(0),
            shape: SkeletonShape::Block,
        };
        let err = validate_and_normalize(&mut c).expect_err("must reject");
        assert_eq!(err, "skeleton_width_out_of_range");
    }

    #[test]
    fn alert_with_window_in_actions_is_rejected() {
        let mut c = FeedbackComponent::Alert {
            tone: FeedbackTone::Warning,
            title: None,
            message: "x".into(),
            icon: None,
            actions: vec![window_overlay()],
            dismissible: false,
            on_dismiss: None,
        };
        let err = validate_and_normalize(&mut c).expect_err("must reject");
        assert_eq!(err, "alert_actions_invalid");
    }

    #[test]
    fn callout_with_window_child_is_rejected() {
        let mut c = FeedbackComponent::Callout {
            tone: FeedbackTone::Info,
            title: None,
            icon: None,
            children: vec![window_overlay()],
        };
        let err = validate_and_normalize(&mut c).expect_err("must reject");
        assert_eq!(err, "callout_children_invalid");
    }

    #[test]
    fn gate_screen_with_window_action_is_rejected() {
        let mut c = FeedbackComponent::GateScreen {
            title: "x".into(),
            message: "y".into(),
            icon: None,
            requirements: vec![],
            actions: vec![window_overlay()],
        };
        let err = validate_and_normalize(&mut c).expect_err("must reject");
        assert_eq!(err, "gate_screen_actions_invalid");
    }
}
