// =============================================================================
// File: protocol/ui/molecules/mod.rs — §2 Structured Molecules (0x0000-0x00FF)
// Purpose: typed structs for the 12 structured molecules split per-group:
//   - page.rs:     Header, PageHeader, SectionHeader
//   - shell.rs:    AppShell, LoginShell, WizardShell
//   - empty.rs:    EmptyState, ErrorBoundary, WelcomeHero
//   - sections.rs: Toolbar, StatGroup, Inspector
// Common conversion pattern: into_component(id) -> Result<Component,
// IntoComponentError> (in typed_field.rs); try_from_component(&Component) ->
// Result<Self, minicbor::decode::Error>. ComponentRef<X> fields stay typed as
// `Component` with runtime tag validation deferred to host validator (Krok 4).
// =============================================================================

pub mod empty;
pub mod page;
pub mod sections;
pub mod shell;

pub use empty::{EmptyState, ErrorBoundary, WelcomeHero};
pub use page::{Header, PageHeader, SectionHeader};
pub use sections::{Inspector, StatGroup, Toolbar};
pub use shell::{AppShell, LoginShell, WizardShell};

/// `IntoComponentError` previously lived in this module; it has been moved to
/// `super::typed_field::IntoComponentError` and is re-exported here so the
/// pre-refactor path `ui::molecules::IntoComponentError` keeps working.
pub use super::typed_field::IntoComponentError;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::ui::bind::BindRef;
    use crate::protocol::ui::component::{Component, FieldMap};
    use crate::protocol::ui::icon_name::IconName;
    use crate::protocol::ui::inline::{IconRef, InlineBadge, InlineChip};
    use crate::protocol::ui::tokens::{
        BadgeVariant, ChipVariant, Density, EmptyStateVariant, Spacing, Tone,
    };
    use crate::protocol::value::Value;

    fn dummy_button(id: &str) -> Component {
        // §6 0x0401 Button — chunk 1.8d will provide a typed Button; here we
        // just emit a Component with the Button tag for testing.
        Component {
            tag: 0x0401,
            id: id.into(),
            fields: FieldMap::default(),
            handlers: None,
            bind: None,
            a11y: None,
            visibility: None,
            test_id: None,
        }
    }

    fn icon(name: IconName) -> IconRef {
        IconRef::Named {
            name,
            size: None,
            tone: None,
        }
    }

    fn lit(s: &str) -> BindRef {
        BindRef::Literal(Value::Text(s.into()))
    }

    fn rt_molecule<F, M>(
        make: F,
        tag: u16,
        into: impl Fn(M) -> Component,
        from: impl Fn(&Component) -> Result<M, minicbor::decode::Error>,
    ) where
        F: Fn() -> M,
        M: PartialEq + std::fmt::Debug + Clone,
    {
        let m = make();
        let c = into(m.clone());
        assert_eq!(c.tag, tag);
        let back = from(&c).unwrap();
        assert_eq!(back, m);
    }

    #[test]
    fn header_roundtrip() {
        let make = || Header {
            icon: icon(IconName::Brain),
            title: lit("TentaVision"),
            status_badge: Some(InlineBadge {
                variant: crate::protocol::ui::tokens::BadgeVariant::Solid,
                tone: Tone::Success,
                label: Some(lit("Live")),
                count: None,
                icon: None,
                pulse: false,
            }),
            subtitle: Some(lit("Camera platform")),
            meta_chips: vec![InlineChip {
                variant: ChipVariant::Soft,
                tone: Tone::Info,
                label: lit("v1.0"),
                icon: None,
                avatar: None,
                selected: None,
                removable: false,
            }],
            actions: vec![dummy_button("btn_add")],
            density: Density::Default,
        };
        rt_molecule(
            make,
            Header::TAG,
            |m| m.into_component("h1").unwrap(),
            Header::try_from_component,
        );
    }

    #[test]
    fn page_header_roundtrip() {
        let make = || PageHeader {
            title: lit("Settings"),
            subtitle: None,
            breadcrumbs: None,
            actions: vec![dummy_button("save")],
            tabs: None,
        };
        rt_molecule(
            make,
            PageHeader::TAG,
            |m| m.into_component("ph").unwrap(),
            PageHeader::try_from_component,
        );
    }

    #[test]
    fn empty_state_roundtrip() {
        let make = || EmptyState {
            icon: icon(IconName::Search),
            heading: lit("Nothing here"),
            message: Some(lit("Add cameras to get started")),
            primary_action: Some(dummy_button("add_cam")),
            secondary_action: None,
            variant: EmptyStateVariant::Illustrated,
        };
        rt_molecule(
            make,
            EmptyState::TAG,
            |m| m.into_component("es").unwrap(),
            EmptyState::try_from_component,
        );
    }

    #[test]
    fn section_header_roundtrip() {
        let make = || SectionHeader {
            title: lit("Cameras"),
            subtitle: Some(lit("Manage")),
            actions: vec![],
            divider: true,
        };
        rt_molecule(
            make,
            SectionHeader::TAG,
            |m| m.into_component("sh").unwrap(),
            SectionHeader::try_from_component,
        );
    }

    #[test]
    fn toolbar_roundtrip() {
        let make = || Toolbar {
            search: None,
            filters: vec![],
            view_mode: None,
            sort_control: None,
            trailing_actions: vec![],
            density: Density::Compact,
        };
        rt_molecule(
            make,
            Toolbar::TAG,
            |m| m.into_component("tb").unwrap(),
            Toolbar::try_from_component,
        );
    }

    #[test]
    fn app_shell_roundtrip() {
        let make = || AppShell {
            sidebar_slot: "sidebar".into(),
            content_slot: "main".into(),
            header_slot: Some("top".into()),
            sidebar_width: Spacing::Xl,
            collapsible_sidebar: true,
        };
        rt_molecule(
            make,
            AppShell::TAG,
            |m| m.into_component("shell").unwrap(),
            AppShell::try_from_component,
        );
    }

    #[test]
    fn login_shell_roundtrip() {
        let make = || LoginShell {
            logo: icon(IconName::Shield),
            title: lit("Sign in"),
            subtitle: None,
            content_slot: "form".into(),
            footer_slot: None,
        };
        rt_molecule(
            make,
            LoginShell::TAG,
            |m| m.into_component("login").unwrap(),
            LoginShell::try_from_component,
        );
    }

    #[test]
    fn error_boundary_roundtrip() {
        let make = || ErrorBoundary {
            error_code: Some(lit("E_TIMEOUT")),
            title: lit("Connection lost"),
            message: Some(lit("Try again")),
            actions: vec![dummy_button("retry")],
            technical_details: None,
        };
        rt_molecule(
            make,
            ErrorBoundary::TAG,
            |m| m.into_component("err").unwrap(),
            ErrorBoundary::try_from_component,
        );
    }

    #[test]
    fn welcome_hero_roundtrip() {
        let make = || WelcomeHero {
            illustration: icon(IconName::Sparkle),
            title: lit("Welcome"),
            subtitle: lit("Get started"),
            features: vec![],
            primary_action: dummy_button("start"),
            secondary_action: None,
        };
        rt_molecule(
            make,
            WelcomeHero::TAG,
            |m| m.into_component("wh").unwrap(),
            WelcomeHero::try_from_component,
        );
    }

    #[test]
    fn stat_group_roundtrip() {
        let make = || StatGroup {
            stats: vec![],
            columns: 4,
            density: Density::Default,
        };
        rt_molecule(
            make,
            StatGroup::TAG,
            |m| m.into_component("sg").unwrap(),
            StatGroup::try_from_component,
        );
    }

    #[test]
    fn wizard_shell_roundtrip() {
        let make = || WizardShell {
            steps: vec![],
            current_step_id: lit("step1"),
            content_slot: "wizard_content".into(),
            footer_slot: "wizard_footer".into(),
            cancellable: true,
        };
        rt_molecule(
            make,
            WizardShell::TAG,
            |m| m.into_component("wz").unwrap(),
            WizardShell::try_from_component,
        );
    }

    #[test]
    fn inspector_roundtrip() {
        let make = || Inspector {
            title: lit("Details"),
            content_slot: "ins_content".into(),
            actions: vec![],
            tabs: None,
            collapsible: true,
        };
        rt_molecule(
            make,
            Inspector::TAG,
            |m| m.into_component("ins").unwrap(),
            Inspector::try_from_component,
        );
    }

    #[test]
    fn tag_mismatch_rejected() {
        let mut c = dummy_button("x"); // tag 0x0401
        c.tag = 0x9999;
        assert!(Header::try_from_component(&c).is_err());
    }

    fn non_button(id: &str) -> Component {
        // 0x040C Fab carrying the wrong tag for ComponentRef<Button>.
        Component {
            tag: 0x040C,
            id: id.into(),
            fields: FieldMap::default(),
            handlers: None,
            bind: None,
            a11y: None,
            visibility: None,
            test_id: None,
        }
    }

    #[test]
    fn header_rejects_non_button_action() {
        let bad = Header {
            icon: icon(IconName::Brain),
            title: lit("T"),
            status_badge: None,
            subtitle: None,
            meta_chips: vec![],
            actions: vec![non_button("bad")],
            density: Density::Default,
        };
        assert!(bad.into_component("h").is_err());
    }

    #[test]
    fn empty_state_rejects_non_button_primary_action() {
        let bad = EmptyState {
            icon: icon(IconName::Brain),
            heading: lit("h"),
            message: None,
            primary_action: Some(non_button("bad")),
            secondary_action: None,
            variant: EmptyStateVariant::Default,
        };
        assert!(bad.into_component("e").is_err());
    }

    #[test]
    fn welcome_hero_rejects_non_button_primary_action() {
        let bad = WelcomeHero {
            illustration: icon(IconName::Brain),
            title: lit("t"),
            subtitle: lit("s"),
            features: vec![],
            primary_action: non_button("bad"),
            secondary_action: None,
        };
        assert!(bad.into_component("w").is_err());
    }

    #[test]
    fn toolbar_rejects_non_button_trailing_action() {
        let bad = Toolbar {
            search: None,
            filters: vec![],
            view_mode: None,
            sort_control: None,
            trailing_actions: vec![non_button("bad")],
            density: Density::Default,
        };
        assert!(bad.into_component("tb").is_err());
    }

    #[test]
    fn toolbar_rejects_non_searchbox_in_search_slot() {
        // ComponentRef<SearchBox> (0x0307) — provide a Fab (0x040C) instead.
        let bad = Toolbar {
            search: Some(non_button("nope")), // 0x040C
            filters: vec![],
            view_mode: None,
            sort_control: None,
            trailing_actions: vec![],
            density: Density::Default,
        };
        assert!(bad.into_component("tb").is_err());
    }

    #[test]
    fn stat_group_rejects_non_statcard() {
        // ComponentRef<StatCard> (0x0208) — provide a Fab instead.
        let bad = StatGroup {
            stats: vec![non_button("nope")],
            columns: 2,
            density: Density::Default,
        };
        assert!(bad.into_component("sg").is_err());
    }

    #[test]
    fn inspector_rejects_non_button_action() {
        let bad = Inspector {
            title: lit("t"),
            content_slot: "x".into(),
            actions: vec![non_button("bad")],
            tabs: None,
            collapsible: false,
        };
        assert!(bad.into_component("i").is_err());
    }

    #[test]
    fn error_boundary_rejects_non_button_action() {
        let bad = ErrorBoundary {
            error_code: None,
            title: lit("err"),
            message: None,
            actions: vec![non_button("bad")],
            technical_details: None,
        };
        assert!(bad.into_component("eb").is_err());
    }

    #[test]
    fn duplicate_field_key_rejected() {
        let mut c = Header {
            icon: icon(IconName::Brain),
            title: lit("T"),
            status_badge: None,
            subtitle: None,
            meta_chips: vec![],
            actions: vec![],
            density: Density::Default,
        }
        .into_component("h")
        .unwrap();
        // Duplicate key 1 (title appears twice).
        let title_val = c.fields.0[1].1.clone();
        c.fields.0.push((1, title_val));
        let err = Header::try_from_component(&c).unwrap_err();
        assert!(format!("{err}").contains("duplicate"));
    }

    #[test]
    fn header_density_absent_defaults_to_default() {
        let mut c = Header {
            icon: icon(IconName::Brain),
            title: lit("T"),
            status_badge: None,
            subtitle: None,
            meta_chips: vec![],
            actions: vec![],
            density: Density::Compact,
        }
        .into_component("h")
        .unwrap();
        // Strip the density entry (key 6).
        c.fields.0.retain(|(k, _)| *k != 6);
        let back = Header::try_from_component(&c).unwrap();
        assert_eq!(back.density, Density::Default);
    }

    #[test]
    fn app_shell_sidebar_width_absent_defaults_to_xl() {
        let mut c = AppShell {
            sidebar_slot: "s".into(),
            content_slot: "m".into(),
            header_slot: None,
            sidebar_width: Spacing::Sm,
            collapsible_sidebar: false,
        }
        .into_component("shell")
        .unwrap();
        c.fields.0.retain(|(k, _)| *k != 3);
        let back = AppShell::try_from_component(&c).unwrap();
        assert_eq!(back.sidebar_width, Spacing::Xl);
    }

    #[test]
    fn stat_group_columns_default_equals_stats_len() {
        let mut c = StatGroup {
            stats: vec![],
            columns: 4,
            density: Density::Default,
        }
        .into_component("sg")
        .unwrap();
        // Drop columns key (1).
        c.fields.0.retain(|(k, _)| *k != 1);
        let back = StatGroup::try_from_component(&c).unwrap();
        assert_eq!(back.columns, back.stats.len() as u8);
    }

    #[test]
    fn unknown_field_key_rejected() {
        let mut c = Header {
            icon: icon(IconName::Brain),
            title: lit("T"),
            status_badge: None,
            subtitle: None,
            meta_chips: vec![],
            actions: vec![],
            density: Density::Default,
        }
        .into_component("h")
        .unwrap();
        c.fields.0.push((99, Value::U64(1)));
        assert!(Header::try_from_component(&c).is_err());
    }
}
