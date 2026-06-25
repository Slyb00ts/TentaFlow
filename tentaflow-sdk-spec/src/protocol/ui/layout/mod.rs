// =============================================================================
// File: protocol/ui/layout/mod.rs — §3 Layout Primitives (0x0100-0x01FF)
// Purpose: typed structs for 18 layout components split per-group:
//   - containers.rs: Flex, Grid, Stack, Cluster, Split, ScrollContainer, Box
//   - cards.rs:      Card, SectionCard, Collapsible, Accordion
//   - atomic.rs:     Divider, Spacer, Tooltip
//   - nav.rs:        Sidebar, Tabs, NavTabs, Breadcrumb, Pagination
// =============================================================================

pub mod atomic;
pub mod cards;
pub mod containers;
pub mod nav;

pub use atomic::{Divider, Spacer, Tooltip};
pub use cards::{Accordion, Card, Collapsible, SectionCard};
pub use containers::{Box, Cluster, Flex, Grid, ScrollContainer, Split, Stack};
pub use nav::{Breadcrumb, NavTabs, Pagination, Sidebar, Tabs};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::ui::bind::BindRef;
    use crate::protocol::ui::component::{Component, FieldMap};
    use crate::protocol::ui::inline::{BorderToken, DimensionToken, GridCol, GridTrack, SplitSize};
    use crate::protocol::ui::tokens::{
        AccordionMode, BackgroundToken, BreadcrumbSeparator, CardVariant, Density,
        DividerOrientation, DividerVariant, FlexAlign, FlexDirection, FlexJustify, FlexWrap,
        NavTabsVariant, PaginationVariant, RadiusToken, ScrollOrientation, ShadowToken,
        SpacerAxis, SplitOrientation, Spacing, TabsVariant, Tone,
    };
    use crate::protocol::value::Value;

    fn dummy(tag: u16) -> Component {
        Component {
            tag,
            id: "x".into(),
            fields: FieldMap::default(),
            handlers: None,
            bind: None,
            a11y: None,
            visibility: None,
            test_id: None,
        }
    }

    fn lit(s: &str) -> BindRef {
        BindRef::Literal(Value::Text(s.into()))
    }

    #[test]
    fn flex_roundtrip() {
        let f = Flex {
            direction: FlexDirection::Row,
            gap: Spacing::Md,
            justify: FlexJustify::SpaceBetween,
            align: FlexAlign::Center,
            wrap: FlexWrap::Wrap,
            children: vec![],
            padding: Some(Spacing::Sm),
            background: Some(BackgroundToken::Subtle),
            radius: Some(RadiusToken::Md),
        };
        let c = f.clone().into_component("f").unwrap();
        assert_eq!(c.tag, Flex::TAG);
        assert_eq!(Flex::try_from_component(&c).unwrap(), f);
    }

    #[test]
    fn grid_roundtrip() {
        let g = Grid {
            columns: GridTrack::Explicit {
                cols: vec![GridCol::Fr { value: 1 }, GridCol::Auto],
            },
            gap: Spacing::Md,
            row_gap: None,
            column_gap: None,
            children: vec![],
            padding: None,
            align_items: None,
        };
        let c = g.clone().into_component("g").unwrap();
        assert_eq!(Grid::try_from_component(&c).unwrap(), g);
    }

    #[test]
    fn stack_defaults_on_absent() {
        let s = Stack {
            gap: Spacing::Lg,
            align: FlexAlign::Stretch,
            children: vec![],
            padding: None,
            justify: None,
        };
        let mut c = s.into_component("s").unwrap();
        c.fields.0.retain(|(k, _)| *k != 0 && *k != 1);
        let back = Stack::try_from_component(&c).unwrap();
        assert_eq!(back.gap, Spacing::Md);
        assert_eq!(back.align, FlexAlign::Stretch);
    }

    #[test]
    fn cluster_roundtrip() {
        let cl = Cluster {
            gap: Spacing::Sm,
            align: FlexAlign::Center,
            justify: FlexJustify::Start,
            children: vec![dummy(0x0001)],
            wrap: None,
        };
        let c = cl.clone().into_component("cl").unwrap();
        assert_eq!(Cluster::try_from_component(&c).unwrap(), cl);
    }

    #[test]
    fn split_roundtrip() {
        let s = Split {
            orientation: SplitOrientation::Horizontal,
            primary_size: SplitSize::Percent { value: 30.0 },
            min_primary: 200,
            max_primary: 600,
            resizable: true,
            primary_slot: "left".into(),
            secondary_slot: "right".into(),
        };
        let c = s.clone().into_component("sp").unwrap();
        assert_eq!(Split::try_from_component(&c).unwrap(), s);
    }

    #[test]
    fn card_roundtrip_with_defaults() {
        let card = Card {
            variant: CardVariant::Filled,
            padding: Spacing::Lg,
            gap: Spacing::Md,
            radius: RadiusToken::Lg,
            shadow: ShadowToken::None,
            border: BorderToken::None,
            background: BackgroundToken::None,
            accent: Some(Tone::Primary),
            children: vec![],
            interactive: false,
            clickable: false,
        };
        let mut c = card.clone().into_component("card").unwrap();
        // Drop default padding(1), gap(2), radius(3) — try_from must fill defaults.
        c.fields.0.retain(|(k, _)| *k != 1 && *k != 2 && *k != 3);
        let back = Card::try_from_component(&c).unwrap();
        assert_eq!(back, card);
    }

    #[test]
    fn section_card_rejects_non_button_header_action() {
        // SectionCard.header_actions is array<ComponentRef<Button>>.
        let bad = SectionCard {
            title: lit("t"), subtitle: None,
            header_actions: vec![dummy(0x040C)], // Fab tag, not Button
            header_divider: false,
            body: vec![], footer: None,
            padding: Spacing::Lg, gap: Spacing::Md,
            variant: CardVariant::Filled,
            radius: RadiusToken::Lg, shadow: ShadowToken::Subtle,
            border: BorderToken::None, background: BackgroundToken::None,
            accent: None,
        };
        assert!(bad.into_component("sc").is_err());
    }

    #[test]
    fn divider_roundtrip() {
        let d = Divider {
            orientation: DividerOrientation::Horizontal,
            variant: DividerVariant::Subtle,
            spacing: Spacing::Md,
            label: Some(lit("OR")),
        };
        let c = d.clone().into_component("d").unwrap();
        assert_eq!(Divider::try_from_component(&c).unwrap(), d);
    }

    #[test]
    fn spacer_roundtrip() {
        let s = Spacer { size: Spacing::Lg, axis: SpacerAxis::Both };
        let c = s.into_component("sp").unwrap();
        assert_eq!(Spacer::try_from_component(&c).unwrap(), s);
    }

    #[test]
    fn sidebar_roundtrip() {
        let s = Sidebar {
            header_slot: None,
            items: vec![],
            footer_slot: None,
            collapsed: None,
        };
        let c = s.clone().into_component("sb").unwrap();
        assert_eq!(Sidebar::try_from_component(&c).unwrap(), s);
    }

    #[test]
    fn tabs_roundtrip() {
        let t = Tabs {
            variant: TabsVariant::Pills,
            items: vec![],
            active_id: lit("t1"),
            content_slot: "tab_content".into(),
            density: Density::Default,
        };
        let c = t.clone().into_component("tb").unwrap();
        assert_eq!(Tabs::try_from_component(&c).unwrap(), t);
    }

    #[test]
    fn nav_tabs_roundtrip() {
        let nt = NavTabs {
            items: vec![],
            active_id: lit("nt1"),
            variant: NavTabsVariant::Underlined,
            scroll_overflow: true,
        };
        let c = nt.clone().into_component("ntb").unwrap();
        assert_eq!(NavTabs::try_from_component(&c).unwrap(), nt);
    }

    #[test]
    fn collapsible_roundtrip() {
        let col = Collapsible {
            header: dummy(0x0004),
            body: vec![],
            expanded: lit("expanded"),
            animated: true,
        };
        let c = col.clone().into_component("co").unwrap();
        assert_eq!(Collapsible::try_from_component(&c).unwrap(), col);
    }

    #[test]
    fn accordion_roundtrip() {
        let a = Accordion {
            items: vec![],
            mode: AccordionMode::Single,
            expanded_ids: lit("ids"),
        };
        let c = a.clone().into_component("ac").unwrap();
        assert_eq!(Accordion::try_from_component(&c).unwrap(), a);
    }

    #[test]
    fn tooltip_roundtrip() {
        let t = Tooltip {
            child: dummy(0x0201),
            content: lit("Hint"),
            side: crate::protocol::ui::tokens::DrawerSide::Top,
            max_width_px: 300,
        };
        let c = t.clone().into_component("tt").unwrap();
        assert_eq!(Tooltip::try_from_component(&c).unwrap(), t);
    }

    #[test]
    fn breadcrumb_roundtrip_with_default_max_items() {
        let b = Breadcrumb {
            items: vec![],
            separator: BreadcrumbSeparator::Chevron,
            max_items: 5,
        };
        let mut c = b.clone().into_component("br").unwrap();
        c.fields.0.retain(|(k, _)| *k != 2);
        let back = Breadcrumb::try_from_component(&c).unwrap();
        assert_eq!(back.max_items, 5);
    }

    #[test]
    fn pagination_roundtrip() {
        let p = Pagination {
            current_page: lit("page"),
            total_pages: lit("total"),
            variant: PaginationVariant::Compact,
            show_summary: false,
        };
        let c = p.clone().into_component("pg").unwrap();
        assert_eq!(Pagination::try_from_component(&c).unwrap(), p);
    }

    #[test]
    fn scroll_container_default_height() {
        let s = ScrollContainer {
            orientation: ScrollOrientation::Vertical,
            height: DimensionToken::Px { value: 400 },
            max_height: None,
            children: vec![],
            sticky_header_slot: None,
            virtualize: false,
            gap: None,
        };
        let mut c = s.into_component("sc").unwrap();
        c.fields.0.retain(|(k, _)| *k != 1);
        let back = ScrollContainer::try_from_component(&c).unwrap();
        assert_eq!(back.height, DimensionToken::Full);
    }
}
