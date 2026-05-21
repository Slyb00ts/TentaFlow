// =============================================================================
// File: protocol/ui/data/mod.rs — §4 Data Display (0x0200-0x02FF), part 1
// Purpose: typed structs for 16 data-display components split per-group:
//   - text.rs:   Text, Heading, Paragraph, RichText, MonoBlock, CodeBlock
//   - stat.rs:   KeyValue, StatCard, Stat
//   - labels.rs: Badge, Chip, Tag
//   - avatar.rs: Avatar, AvatarGroup
//   - lists.rs:  BulletList, Timeline
// (1.8c2/1.8c3 will append further data files: table/charts/gauge/...)
// =============================================================================

pub mod avatar;
pub mod charts;
pub mod gauge;
pub mod labels;
pub mod lists;
pub mod stat;
pub mod tables;
pub mod text;

pub use avatar::{Avatar, AvatarGroup};
pub use charts::{AreaChart, BarChart, LineChart, PieChart, Sparkline, StackedBar};
pub use gauge::{Gauge, Heatmap};
pub use labels::{Badge, Chip, Tag};
pub use lists::{BulletList, Timeline};
pub use stat::{KeyValue, Stat, StatCard};
pub use tables::{EmptyCell, List, Table, Tree};
pub use text::{CodeBlock, Heading, MonoBlock, Paragraph, RichText, Text};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::ui::bind::BindRef;
    use crate::protocol::ui::component::{Component, FieldMap};
    use crate::protocol::ui::icon_name::IconName;
    use crate::protocol::ui::inline::{AvatarRef, IconRef, Trend, TrendDirection};
    use crate::protocol::ui::tokens::{
        AvatarOverlap, AvatarShape, AvatarSize, AvatarStatus, BadgeVariant, BulletListVariant,
        ChipVariant, Density, KvLayout, MarkdownBlock, MarkdownMark, StatSize, TagSize,
        TextAlign, TextStyle, TextWrap, TimelineOrientation, Tone,
    };
    use crate::protocol::value::Value;

    fn lit(s: &str) -> BindRef {
        BindRef::Literal(Value::Text(s.into()))
    }

    fn dummy(tag: u16) -> Component {
        Component {
            tag, id: "x".into(), fields: FieldMap::default(),
            handlers: None, bind: None, a11y: None, visibility: None, test_id: None,
        }
    }

    fn rt<T>(make: T, into: impl Fn(T) -> Component, from: impl Fn(&Component) -> Result<T, minicbor::decode::Error>)
    where T: PartialEq + std::fmt::Debug + Clone {
        let c = into(make.clone());
        assert_eq!(from(&c).unwrap(), make);
    }

    #[test]
    fn text_roundtrip() {
        let t = Text {
            content: lit("hello"), style: TextStyle::Body,
            tone: Some(Tone::Primary), align: Some(TextAlign::Start),
            wrap: Some(TextWrap::Wrap), max_lines: Some(3), format: None,
        };
        rt(t, |m| m.into_component("t").unwrap(), Text::try_from_component);
    }

    #[test]
    fn heading_roundtrip() {
        let h = Heading {
            content: lit("Title"), level: 1, tone: None, align: None,
        };
        rt(h, |m| m.into_component("h").unwrap(), Heading::try_from_component);
    }

    #[test]
    fn paragraph_full_roundtrip() {
        let p = Paragraph {
            content: lit("Hello"), style: TextStyle::H3,
            allowed_marks: vec![MarkdownMark::Bold, MarkdownMark::Code],
            allow_links: true, max_lines: Some(4),
        };
        rt(p, |m| m.into_component("p").unwrap(), Paragraph::try_from_component);
    }

    #[test]
    fn paragraph_style_default_on_absent() {
        let p = Paragraph {
            content: lit("Hello"), style: TextStyle::Body,
            allowed_marks: vec![],
            allow_links: true, max_lines: None,
        };
        let mut c = p.into_component("p").unwrap();
        c.fields.0.retain(|(k, _)| *k != 1);
        let back = Paragraph::try_from_component(&c).unwrap();
        assert_eq!(back.style, TextStyle::Body);
    }

    #[test]
    fn rich_text_roundtrip() {
        let r = RichText {
            content: lit("# Heading\n\n- item"),
            allowed_blocks: vec![MarkdownBlock::Heading, MarkdownBlock::List],
            allowed_marks: vec![MarkdownMark::Bold],
            max_height_px: Some(400),
        };
        rt(r, |m| m.into_component("r").unwrap(), RichText::try_from_component);
    }

    #[test]
    fn mono_block_roundtrip() {
        let m = MonoBlock {
            content: lit("plain text"), max_height_px: None,
            word_wrap: false, copyable: true,
        };
        rt(m, |x| x.into_component("m").unwrap(), MonoBlock::try_from_component);
    }

    #[test]
    fn code_block_roundtrip() {
        let cb = CodeBlock {
            content: lit("fn main() {}"), language: "rust".into(),
            show_line_numbers: true, copyable: true,
            max_height_px: Some(300), highlight_lines: vec![1, 2],
        };
        rt(cb, |x| x.into_component("cb").unwrap(), CodeBlock::try_from_component);
    }

    #[test]
    fn key_value_roundtrip() {
        let kv = KeyValue {
            items: vec![], density: Density::Default,
            layout: KvLayout::Stacked, label_width: None,
        };
        rt(kv, |m| m.into_component("kv").unwrap(), KeyValue::try_from_component);
    }

    #[test]
    fn stat_card_roundtrip_with_trend() {
        let sc = StatCard {
            label: lit("Active cameras"), icon: None,
            value: BindRef::Literal(Value::U64(42)),
            value_suffix: Some(lit("/50")), format: None,
            trend: Some(Trend {
                direction: TrendDirection::Up,
                percent: 5.0, label: None, tone: None,
            }),
            footnote: None, accent: Some(Tone::Success), clickable: false,
        };
        rt(sc, |m| m.into_component("sc").unwrap(), StatCard::try_from_component);
    }

    #[test]
    fn stat_roundtrip() {
        let s = Stat {
            label: lit("Total"),
            value: BindRef::Literal(Value::U64(100)),
            format: None, trend: None, size: StatSize::Md,
        };
        rt(s, |m| m.into_component("s").unwrap(), Stat::try_from_component);
    }

    #[test]
    fn badge_roundtrip() {
        let b = Badge {
            variant: BadgeVariant::Solid, tone: Tone::Success,
            label: lit("OK"), icon: None, count: None, max: 99, pulse: false,
        };
        rt(b, |m| m.into_component("b").unwrap(), Badge::try_from_component);
    }

    #[test]
    fn chip_roundtrip() {
        let ch = Chip {
            variant: ChipVariant::Removable, tone: Tone::Info,
            label: lit("filter1"), icon: None, avatar: None,
            selected: None, removable: true,
        };
        rt(ch, |m| m.into_component("ch").unwrap(), Chip::try_from_component);
    }

    #[test]
    fn tag_roundtrip() {
        let t = Tag { tone: Tone::Neutral, label: lit("v1.0"), size: TagSize::Sm };
        rt(t, |m| m.into_component("t").unwrap(), Tag::try_from_component);
    }

    #[test]
    fn avatar_roundtrip() {
        let a = Avatar {
            source: AvatarRef::Initials { initials: "PJ".into() },
            size: AvatarSize::Md, shape: AvatarShape::Circle,
            status: Some(AvatarStatus::Online), tone: None,
        };
        rt(a, |m| m.into_component("a").unwrap(), Avatar::try_from_component);
    }

    #[test]
    fn avatar_group_roundtrip() {
        let ag = AvatarGroup {
            avatars: vec![dummy(Avatar::TAG)],
            max_visible: 5, overlap: AvatarOverlap::Default, size: AvatarSize::Sm,
        };
        rt(ag, |m| m.into_component("ag").unwrap(), AvatarGroup::try_from_component);
    }

    #[test]
    fn bullet_list_roundtrip() {
        let bl = BulletList {
            items: vec![lit("a"), lit("b")],
            variant: BulletListVariant::Numbered, tone: None,
            density: Density::Compact,
        };
        rt(bl, |m| m.into_component("bl").unwrap(), BulletList::try_from_component);
    }

    #[test]
    fn timeline_roundtrip() {
        let t = Timeline {
            items: vec![], orientation: TimelineOrientation::Vertical,
            density: Density::Default, show_dates: true, group_by_day: false,
        };
        rt(t, |m| m.into_component("tl").unwrap(), Timeline::try_from_component);
    }

    #[test]
    fn _unused_iconname_smoke() {
        let _ = IconName::Brain;
    }

    // --- 1.8c2: Tables / Charts / Gauge -----------------------------------

    #[test]
    fn table_roundtrip_minimal() {
        use crate::protocol::ui::bind::{PathSegment, StatePath};
        use crate::protocol::ui::tokens::{TableSelectMode, TableVariant};
        let t = Table {
            columns: vec![],
            rows_path: StatePath::new(vec![PathSegment::Key("rows".into())]),
            row_key_field: "id".into(),
            variant: TableVariant::Default,
            density: Density::Default,
            sortable: false,
            sort_by: None,
            selectable: TableSelectMode::None,
            selected_ids: None,
            sticky_header: true,
            sticky_columns: 0,
            pagination: None,
            empty_state: None,
            row_actions: vec![],
            bulk_actions: vec![],
            virtualize: false,
            row_expandable: false,
            expanded_row_template_id: None,
        };
        let c = t.clone().into_component("tbl").unwrap();
        assert_eq!(c.tag, Table::TAG);
        assert_eq!(Table::try_from_component(&c).unwrap(), t);
    }

    #[test]
    fn list_roundtrip() {
        use crate::protocol::ui::bind::{PathSegment, StatePath};
        let l = List {
            items_path: StatePath::new(vec![PathSegment::Key("items".into())]),
            item_template_id: "row".into(),
            divider: true,
            density: Density::Compact,
            virtualize: false,
            empty_state: None,
            max_visible: Some(50),
        };
        let c = l.clone().into_component("list").unwrap();
        assert_eq!(List::try_from_component(&c).unwrap(), l);
    }

    #[test]
    fn tree_roundtrip() {
        use crate::protocol::ui::bind::{PathSegment, StatePath};
        use crate::protocol::ui::tokens::TreeVariant;
        let t = Tree {
            nodes_path: StatePath::new(vec![PathSegment::Key("nodes".into())]),
            expanded_ids: lit("expanded"),
            selected_id: None,
            variant: TreeVariant::WithIcons,
            lazy_load: false,
        };
        let c = t.clone().into_component("tree").unwrap();
        assert_eq!(Tree::try_from_component(&c).unwrap(), t);
    }

    #[test]
    fn empty_cell_roundtrip() {
        use crate::protocol::ui::tokens::EmptyCellVariant;
        let ec = EmptyCell { variant: EmptyCellVariant::EmDash };
        let c = ec.into_component("ec").unwrap();
        assert_eq!(EmptyCell::try_from_component(&c).unwrap(), ec);
    }

    #[test]
    fn sparkline_roundtrip() {
        use crate::protocol::ui::bind::{PathSegment, StatePath};
        use crate::protocol::ui::tokens::SparklineVariant;
        let s = Sparkline {
            data_path: StatePath::new(vec![PathSegment::Key("data".into())]),
            variant: SparklineVariant::Area,
            tone: Tone::Primary,
            width_px: 200,
            height_px: 40,
            show_min_max: true,
        };
        let c = s.clone().into_component("sp").unwrap();
        assert_eq!(Sparkline::try_from_component(&c).unwrap(), s);
    }

    fn axis() -> crate::protocol::ui::inline::ChartAxis {
        use crate::protocol::ui::tokens::ChartAxisScale;
        crate::protocol::ui::inline::ChartAxis {
            label: None, format: None, min: None, max: None, ticks: None,
            scale: ChartAxisScale::Linear,
        }
    }

    fn legend() -> crate::protocol::ui::inline::ChartLegend {
        use crate::protocol::ui::tokens::{ChartLegendAlign, ChartLegendPosition};
        crate::protocol::ui::inline::ChartLegend {
            position: ChartLegendPosition::Bottom,
            alignment: ChartLegendAlign::Center,
        }
    }

    fn tooltip() -> crate::protocol::ui::inline::ChartTooltip {
        crate::protocol::ui::inline::ChartTooltip { enabled: true, format: None }
    }

    #[test]
    fn line_chart_roundtrip() {
        use crate::protocol::ui::tokens::ChartZoomMode;
        let lc = LineChart {
            series: vec![], x_axis: axis(), y_axis: axis(),
            legend: legend(), tooltip: tooltip(),
            zoom: ChartZoomMode::X, brush: false, height_px: 200,
        };
        let c = lc.clone().into_component("lc").unwrap();
        assert_eq!(LineChart::try_from_component(&c).unwrap(), lc);
    }

    #[test]
    fn bar_chart_roundtrip() {
        use crate::protocol::ui::tokens::{BarStacking, ChartOrientation};
        let bc = BarChart {
            series: vec![], x_axis: axis(), y_axis: axis(),
            orientation: ChartOrientation::Vertical,
            stacking: BarStacking::Stacked,
            legend: legend(), height_px: 200,
        };
        let c = bc.clone().into_component("bc").unwrap();
        assert_eq!(BarChart::try_from_component(&c).unwrap(), bc);
    }

    #[test]
    fn area_chart_full_roundtrip_non_default_opacity() {
        use crate::protocol::ui::tokens::{AreaStacking, ChartZoomMode};
        let ac = AreaChart {
            series: vec![], x_axis: axis(), y_axis: axis(),
            legend: legend(), tooltip: tooltip(),
            zoom: ChartZoomMode::Both, brush: true, height_px: 400,
            stacking: AreaStacking::Percent, opacity: 0.75,
        };
        let c = ac.clone().into_component("ac").unwrap();
        assert_eq!(c.tag, AreaChart::TAG);
        assert_eq!(AreaChart::try_from_component(&c).unwrap(), ac);
    }

    #[test]
    fn area_chart_default_opacity_on_absent() {
        use crate::protocol::ui::tokens::{AreaStacking, ChartZoomMode};
        let ac = AreaChart {
            series: vec![], x_axis: axis(), y_axis: axis(),
            legend: legend(), tooltip: tooltip(),
            zoom: ChartZoomMode::None, brush: false, height_px: 200,
            stacking: AreaStacking::None, opacity: 0.4,
        };
        let mut c = ac.into_component("ac").unwrap();
        c.fields.0.retain(|(k, _)| *k != 9);
        let back = AreaChart::try_from_component(&c).unwrap();
        assert_eq!(back.opacity, 0.4);
    }

    #[test]
    fn pie_chart_roundtrip() {
        use crate::protocol::ui::bind::{PathSegment, StatePath};
        use crate::protocol::ui::tokens::PieVariant;
        let p = PieChart {
            data_path: StatePath::new(vec![PathSegment::Key("data".into())]),
            variant: PieVariant::Donut,
            show_labels: true, show_legend: true, max_segments: 5, height_px: 240,
        };
        let c = p.clone().into_component("pc").unwrap();
        assert_eq!(PieChart::try_from_component(&c).unwrap(), p);
    }

    #[test]
    fn stacked_bar_roundtrip() {
        let sb = StackedBar {
            segments: vec![],
            total: BindRef::Literal(Value::U64(100)),
            show_legend: false, show_percentages: true, height_px: 30,
        };
        let c = sb.clone().into_component("sb").unwrap();
        assert_eq!(StackedBar::try_from_component(&c).unwrap(), sb);
    }

    #[test]
    fn heatmap_roundtrip() {
        use crate::protocol::ui::bind::{PathSegment, StatePath};
        use crate::protocol::ui::inline::HeatmapScale;
        use crate::protocol::ui::tokens::HeatmapLegendPosition;
        let h = Heatmap {
            rows: vec![], columns: vec![],
            cells_path: StatePath::new(vec![PathSegment::Key("cells".into())]),
            scale: HeatmapScale::Linear { min: 0.0, max: 100.0, color_from: Tone::Info, color_to: Tone::Critical },
            legend_position: HeatmapLegendPosition::Bottom,
            cell_size_px: 24, tooltip: true,
        };
        let c = h.clone().into_component("hm").unwrap();
        assert_eq!(Heatmap::try_from_component(&c).unwrap(), h);
    }

    #[test]
    fn gauge_roundtrip() {
        use crate::protocol::ui::tokens::GaugeVariant;
        let g = Gauge {
            value: BindRef::Literal(Value::U64(42)),
            min: 0.0, max: 100.0,
            thresholds: vec![],
            variant: GaugeVariant::Arc,
            label: None, format: None, size_px: 120,
        };
        let c = g.clone().into_component("gg").unwrap();
        assert_eq!(Gauge::try_from_component(&c).unwrap(), g);
    }
}
