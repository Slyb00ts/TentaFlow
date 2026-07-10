// =============================================================================
// File: protocol/ui/feedback/mod.rs — §7 Feedback components (0x0501-0x050F)
// 15 typed: Alert/Banner/Callout/Toast/Hint/Skeleton/Spinner/LoadingBar/Modal/
// Drawer/Popover/Sheet/GateScreen/ConfirmationDialog/OfflineBanner.
// =============================================================================

pub mod gates;
pub mod inline;
pub mod loading;
pub mod overlays;

pub use gates::{ConfirmationDialog, GateScreen};
pub use inline::{Alert, Banner, Callout, Hint, OfflineBanner, Toast};
pub use loading::{LoadingBar, Skeleton, Spinner};
pub use overlays::{Drawer, Modal, Popover, Sheet};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::ui::actions::{Button, Fab};
    use crate::protocol::ui::bind::BindRef;
    use crate::protocol::ui::component::{Component, FieldMap};
    use crate::protocol::ui::icon_name::IconName;
    use crate::protocol::ui::inline::{DimensionToken, IconRef};
    use crate::protocol::ui::tokens::{
        AlertVariant, BannerPosition, ButtonSize, ButtonVariant, Density, DrawerSide, DrawerSize,
        FabPosition, FabSize, GateVariant, ModalSize, PopoverPlacement, SheetDetent,
        SkeletonVariant, SpinnerSize, SpinnerVariant, Tone,
    };
    use crate::protocol::ui::typed_field::encode_to_value;
    use crate::protocol::value::Value;

    fn lit(s: &str) -> BindRef {
        BindRef::Literal(Value::Text(s.into()))
    }
    fn icon() -> IconRef {
        IconRef::Named {
            name: IconName::Check,
            size: None,
            tone: None,
        }
    }
    fn non_button() -> Component {
        Fab {
            icon: icon(),
            tone: Tone::Primary,
            size: FabSize::Md,
            position: FabPosition::Inline,
            label: None,
        }
        .into_component("not-btn")
        .unwrap()
    }
    fn sample_button() -> Component {
        Button {
            variant: ButtonVariant::Primary,
            tone: Tone::Primary,
            label: lit("OK"),
            icon_leading: None,
            icon_trailing: None,
            size: ButtonSize::Md,
            full_width: false,
            disabled: None,
            loading: None,
            density: Density::Comfortable,
        }
        .into_component("b")
        .unwrap()
    }

    fn rt<T: PartialEq + std::fmt::Debug + Clone>(
        make: T,
        into: impl Fn(T) -> Component,
        from: impl Fn(&Component) -> Result<T, minicbor::decode::Error>,
    ) {
        let c = into(make.clone());
        assert_eq!(from(&c).unwrap(), make);
    }

    #[test]
    fn alert_roundtrip() {
        let v = Alert {
            tone: Tone::Warning,
            variant: AlertVariant::Soft,
            icon: Some(icon()),
            title: Some(lit("Heads up")),
            message: lit("Saved"),
            actions: Some(vec![sample_button()]),
            dismissible: true,
        };
        rt(
            v,
            |m| m.into_component("a").unwrap(),
            Alert::try_from_component,
        );
    }

    #[test]
    fn alert_rejects_non_button_action_on_encode() {
        let v = Alert {
            tone: Tone::Critical,
            variant: AlertVariant::Filled,
            icon: None,
            title: None,
            message: lit("x"),
            actions: Some(vec![non_button()]),
            dismissible: false,
        };
        assert!(v.into_component("a").is_err());
    }

    #[test]
    fn alert_rejects_non_button_action_on_decode() {
        // Encode a well-formed Alert, then swap the actions array to contain a Fab.
        let good = Alert {
            tone: Tone::Info,
            variant: AlertVariant::Default,
            icon: None,
            title: None,
            message: lit("ok"),
            actions: Some(vec![sample_button()]),
            dismissible: false,
        }
        .into_component("a")
        .unwrap();
        let mut tampered = good;
        let bad_actions: Vec<Component> = vec![non_button()];
        for (k, v) in tampered.fields.0.iter_mut() {
            if *k == 5 {
                *v = encode_to_value(&bad_actions).unwrap();
            }
        }
        assert!(Alert::try_from_component(&tampered).is_err());
    }

    #[test]
    fn banner_rejects_non_button_action_on_encode() {
        let v = Banner {
            tone: Tone::Info,
            icon: None,
            message: lit("x"),
            action: Some(non_button()),
            dismissible: false,
            position: BannerPosition::Top,
        };
        assert!(v.into_component("bn").is_err());
    }

    #[test]
    fn banner_rejects_non_button_action_on_decode() {
        let good = Banner {
            tone: Tone::Info,
            icon: None,
            message: lit("ok"),
            action: Some(sample_button()),
            dismissible: false,
            position: BannerPosition::Top,
        }
        .into_component("bn")
        .unwrap();
        let mut tampered = good;
        for (k, v) in tampered.fields.0.iter_mut() {
            if *k == 3 {
                *v = encode_to_value(&non_button()).unwrap();
            }
        }
        assert!(Banner::try_from_component(&tampered).is_err());
    }

    #[test]
    fn gate_screen_rejects_non_button_on_decode() {
        let good = GateScreen {
            icon: icon(),
            title: lit("t"),
            message: lit("m"),
            actions: vec![sample_button()],
            variant: GateVariant::AuthRequired,
        }
        .into_component("gs")
        .unwrap();
        let mut tampered = good;
        let bad_actions: Vec<Component> = vec![non_button()];
        for (k, v) in tampered.fields.0.iter_mut() {
            if *k == 3 {
                *v = encode_to_value(&bad_actions).unwrap();
            }
        }
        assert!(GateScreen::try_from_component(&tampered).is_err());
    }

    #[test]
    fn banner_roundtrip() {
        let v = Banner {
            tone: Tone::Info,
            icon: None,
            message: lit("Welcome"),
            action: Some(sample_button()),
            dismissible: true,
            position: BannerPosition::Top,
        };
        rt(
            v,
            |m| m.into_component("bn").unwrap(),
            Banner::try_from_component,
        );
    }

    #[test]
    fn callout_roundtrip() {
        let v = Callout {
            tone: Tone::Neutral,
            icon: None,
            title: Some(lit("Note")),
            content: vec![],
        };
        rt(
            v,
            |m| m.into_component("co").unwrap(),
            Callout::try_from_component,
        );
    }

    #[test]
    fn toast_roundtrip() {
        let v = Toast {
            tone: Tone::Success,
            title: lit("Saved"),
            body: Some(lit("All changes stored")),
            icon: None,
            action_label: Some("Undo".into()),
            action_id: Some("undoSave".into()),
        };
        rt(
            v,
            |m| m.into_component("t").unwrap(),
            Toast::try_from_component,
        );
    }

    #[test]
    fn hint_roundtrip() {
        let v = Hint {
            content: lit("Try /search"),
            icon: None,
            tone: Tone::Neutral,
        };
        rt(
            v,
            |m| m.into_component("h").unwrap(),
            Hint::try_from_component,
        );
    }

    #[test]
    fn skeleton_roundtrip() {
        let v = Skeleton {
            variant: SkeletonVariant::Text,
            width: Some(DimensionToken::Auto),
            height: None,
            animate: true,
            lines: 3,
        };
        rt(
            v,
            |m| m.into_component("sk").unwrap(),
            Skeleton::try_from_component,
        );
    }

    #[test]
    fn spinner_roundtrip() {
        let v = Spinner {
            size: SpinnerSize::Md,
            tone: Tone::Primary,
            label: Some(lit("Loading")),
            variant: SpinnerVariant::Ring,
        };
        rt(
            v,
            |m| m.into_component("sp").unwrap(),
            Spinner::try_from_component,
        );
    }

    #[test]
    fn loading_bar_roundtrip() {
        let v = LoadingBar {
            visible: BindRef::Literal(Value::Bool(true)),
            progress: Some(BindRef::Literal(Value::F64(0.42))),
            tone: Tone::Primary,
        };
        rt(
            v,
            |m| m.into_component("lb").unwrap(),
            LoadingBar::try_from_component,
        );
    }

    #[test]
    fn modal_roundtrip() {
        let v = Modal {
            title: lit("Edit"),
            subtitle: None,
            body_slot: "body".into(),
            footer_slot: Some("footer".into()),
            size: ModalSize::Md,
            dismissible: true,
            prevent_scroll: true,
            closable: true,
        };
        rt(
            v,
            |m| m.into_component("m").unwrap(),
            Modal::try_from_component,
        );
    }

    #[test]
    fn drawer_roundtrip() {
        let v = Drawer {
            side: DrawerSide::Right,
            size: DrawerSize::Lg,
            title: Some(lit("Filters")),
            body_slot: "body".into(),
            footer_slot: None,
            dismissible: true,
        };
        rt(
            v,
            |m| m.into_component("d").unwrap(),
            Drawer::try_from_component,
        );
    }

    #[test]
    fn popover_roundtrip() {
        let v = Popover {
            anchor_id: "btn1".into(),
            body_slot: "p-body".into(),
            placement: PopoverPlacement::BottomStart,
            dismissible: true,
            arrow: true,
        };
        rt(
            v,
            |m| m.into_component("po").unwrap(),
            Popover::try_from_component,
        );
    }

    #[test]
    fn sheet_roundtrip() {
        let v = Sheet {
            title: None,
            body_slot: "s-body".into(),
            footer_slot: None,
            detents: vec![SheetDetent::Small, SheetDetent::Medium, SheetDetent::Large],
            current_detent: Some(lit("medium")),
            dismissible: true,
        };
        rt(
            v,
            |m| m.into_component("sh").unwrap(),
            Sheet::try_from_component,
        );
    }

    #[test]
    fn gate_screen_roundtrip() {
        let v = GateScreen {
            icon: icon(),
            title: lit("Permission denied"),
            message: lit("You don't have access"),
            actions: vec![sample_button()],
            variant: GateVariant::PermissionDenied,
        };
        rt(
            v,
            |m| m.into_component("gs").unwrap(),
            GateScreen::try_from_component,
        );
    }

    #[test]
    fn gate_screen_rejects_non_button_on_encode() {
        let v = GateScreen {
            icon: icon(),
            title: lit("x"),
            message: lit("y"),
            actions: vec![non_button()],
            variant: GateVariant::AuthRequired,
        };
        assert!(v.into_component("gs").is_err());
    }

    #[test]
    fn confirmation_dialog_roundtrip() {
        let v = ConfirmationDialog {
            title: lit("Delete?"),
            message: lit("This cannot be undone"),
            icon: Some(icon()),
            tone: Tone::Critical,
            confirm_label: lit("Delete"),
            cancel_label: lit("Cancel"),
            destructive: true,
            require_typing: Some("DELETE".into()),
        };
        rt(
            v,
            |m| m.into_component("cd").unwrap(),
            ConfirmationDialog::try_from_component,
        );
    }

    #[test]
    fn offline_banner_roundtrip() {
        let v = OfflineBanner {
            message: lit("Offline"),
            action_label: Some(lit("Retry")),
            reconnecting: BindRef::Literal(Value::Bool(false)),
        };
        rt(
            v,
            |m| m.into_component("ob").unwrap(),
            OfflineBanner::try_from_component,
        );
    }

    #[test]
    fn tag_mismatch_rejected() {
        let bogus = Component {
            tag: 0x9999,
            id: "x".into(),
            fields: FieldMap::default(),
            handlers: None,
            bind: None,
            a11y: None,
            visibility: None,
            test_id: None,
        };
        assert!(Alert::try_from_component(&bogus).is_err());
        assert!(Modal::try_from_component(&bogus).is_err());
        assert!(ConfirmationDialog::try_from_component(&bogus).is_err());
    }
}
