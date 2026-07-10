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
pub mod markdown;
pub mod progress;
pub mod specialised;
pub mod stat;
pub mod tables;
pub mod text;

pub use avatar::{Avatar, AvatarGroup};
pub use charts::{AreaChart, BarChart, LineChart, PieChart, Sparkline, StackedBar};
pub use gauge::{Gauge, Heatmap};
pub use labels::{Badge, Chip, Tag};
pub use lists::{BulletList, Timeline};
pub use markdown::{DataDefinitionList, JsonViewer, Markdown};
pub use progress::{Diff, ProgressBar, RatingDisplay};
pub use specialised::{CalendarMonth, Image, LiveRegionComponent, VisuallyHidden};
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
        ChipVariant, Density, KvLayout, MarkdownBlock, MarkdownMark, StatSize, TagSize, TextAlign,
        TextStyle, TextWrap, TimelineOrientation, Tone,
    };
    use crate::protocol::value::Value;

    fn lit(s: &str) -> BindRef {
        BindRef::Literal(Value::Text(s.into()))
    }

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

    fn rt<T>(
        make: T,
        into: impl Fn(T) -> Component,
        from: impl Fn(&Component) -> Result<T, minicbor::decode::Error>,
    ) where
        T: PartialEq + std::fmt::Debug + Clone,
    {
        let c = into(make.clone());
        assert_eq!(from(&c).unwrap(), make);
    }

    #[test]
    fn text_roundtrip() {
        let t = Text {
            content: lit("hello"),
            style: TextStyle::Body,
            tone: Some(Tone::Primary),
            align: Some(TextAlign::Start),
            wrap: Some(TextWrap::Wrap),
            max_lines: Some(3),
            format: None,
        };
        rt(
            t,
            |m| m.into_component("t").unwrap(),
            Text::try_from_component,
        );
    }

    #[test]
    fn heading_roundtrip() {
        let h = Heading {
            content: lit("Title"),
            level: 1,
            tone: None,
            align: None,
        };
        rt(
            h,
            |m| m.into_component("h").unwrap(),
            Heading::try_from_component,
        );
    }

    #[test]
    fn paragraph_full_roundtrip() {
        let p = Paragraph {
            content: lit("Hello"),
            style: TextStyle::H3,
            allowed_marks: vec![MarkdownMark::Bold, MarkdownMark::Code],
            allow_links: true,
            max_lines: Some(4),
        };
        rt(
            p,
            |m| m.into_component("p").unwrap(),
            Paragraph::try_from_component,
        );
    }

    #[test]
    fn paragraph_style_default_on_absent() {
        let p = Paragraph {
            content: lit("Hello"),
            style: TextStyle::Body,
            allowed_marks: vec![],
            allow_links: true,
            max_lines: None,
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
        rt(
            r,
            |m| m.into_component("r").unwrap(),
            RichText::try_from_component,
        );
    }

    #[test]
    fn mono_block_roundtrip() {
        let m = MonoBlock {
            content: lit("plain text"),
            max_height_px: None,
            word_wrap: false,
            copyable: true,
        };
        rt(
            m,
            |x| x.into_component("m").unwrap(),
            MonoBlock::try_from_component,
        );
    }

    #[test]
    fn code_block_roundtrip() {
        let cb = CodeBlock {
            content: lit("fn main() {}"),
            language: "rust".into(),
            show_line_numbers: true,
            copyable: true,
            max_height_px: Some(300),
            highlight_lines: vec![1, 2],
        };
        rt(
            cb,
            |x| x.into_component("cb").unwrap(),
            CodeBlock::try_from_component,
        );
    }

    #[test]
    fn key_value_roundtrip() {
        let kv = KeyValue {
            items: vec![],
            density: Density::Default,
            layout: KvLayout::Stacked,
            label_width: None,
        };
        rt(
            kv,
            |m| m.into_component("kv").unwrap(),
            KeyValue::try_from_component,
        );
    }

    #[test]
    fn stat_card_roundtrip_with_trend() {
        let sc = StatCard {
            label: lit("Active cameras"),
            icon: None,
            value: BindRef::Literal(Value::U64(42)),
            value_suffix: Some(lit("/50")),
            format: None,
            trend: Some(Trend {
                direction: TrendDirection::Up,
                percent: 5.0,
                label: None,
                tone: None,
            }),
            footnote: None,
            accent: Some(Tone::Success),
            clickable: false,
        };
        rt(
            sc,
            |m| m.into_component("sc").unwrap(),
            StatCard::try_from_component,
        );
    }

    #[test]
    fn stat_roundtrip() {
        let s = Stat {
            label: lit("Total"),
            value: BindRef::Literal(Value::U64(100)),
            format: None,
            trend: None,
            size: StatSize::Md,
        };
        rt(
            s,
            |m| m.into_component("s").unwrap(),
            Stat::try_from_component,
        );
    }

    #[test]
    fn badge_roundtrip() {
        let b = Badge {
            variant: BadgeVariant::Solid,
            tone: Tone::Success,
            label: lit("OK"),
            icon: None,
            count: None,
            max: 99,
            pulse: false,
        };
        rt(
            b,
            |m| m.into_component("b").unwrap(),
            Badge::try_from_component,
        );
    }

    #[test]
    fn chip_roundtrip() {
        let ch = Chip {
            variant: ChipVariant::Removable,
            tone: Tone::Info,
            label: lit("filter1"),
            icon: None,
            avatar: None,
            selected: None,
            removable: true,
        };
        rt(
            ch,
            |m| m.into_component("ch").unwrap(),
            Chip::try_from_component,
        );
    }

    #[test]
    fn tag_roundtrip() {
        let t = Tag {
            tone: Tone::Neutral,
            label: lit("v1.0"),
            size: TagSize::Sm,
        };
        rt(
            t,
            |m| m.into_component("t").unwrap(),
            Tag::try_from_component,
        );
    }

    #[test]
    fn avatar_roundtrip() {
        let a = Avatar {
            source: AvatarRef::Initials {
                initials: "PJ".into(),
            },
            size: AvatarSize::Md,
            shape: AvatarShape::Circle,
            status: Some(AvatarStatus::Online),
            tone: None,
        };
        rt(
            a,
            |m| m.into_component("a").unwrap(),
            Avatar::try_from_component,
        );
    }

    #[test]
    fn avatar_group_roundtrip() {
        let ag = AvatarGroup {
            avatars: vec![dummy(Avatar::TAG)],
            max_visible: 5,
            overlap: AvatarOverlap::Default,
            size: AvatarSize::Sm,
        };
        rt(
            ag,
            |m| m.into_component("ag").unwrap(),
            AvatarGroup::try_from_component,
        );
    }

    #[test]
    fn bullet_list_roundtrip() {
        let bl = BulletList {
            items: vec![lit("a"), lit("b")],
            variant: BulletListVariant::Numbered,
            tone: None,
            density: Density::Compact,
        };
        rt(
            bl,
            |m| m.into_component("bl").unwrap(),
            BulletList::try_from_component,
        );
    }

    #[test]
    fn timeline_roundtrip() {
        let t = Timeline {
            items: vec![],
            orientation: TimelineOrientation::Vertical,
            density: Density::Default,
            show_dates: true,
            group_by_day: false,
        };
        rt(
            t,
            |m| m.into_component("tl").unwrap(),
            Timeline::try_from_component,
        );
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

    fn non_button(id: &str) -> Component {
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
    fn table_rejects_non_button_row_action() {
        use crate::protocol::ui::bind::{PathSegment, StatePath};
        use crate::protocol::ui::tokens::{TableSelectMode, TableVariant};
        let bad = Table {
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
            row_actions: vec![non_button("bad")],
            bulk_actions: vec![],
            virtualize: false,
            row_expandable: false,
            expanded_row_template_id: None,
        };
        assert!(bad.into_component("tbl").is_err());
    }

    #[test]
    fn table_rejects_non_button_bulk_action() {
        use crate::protocol::ui::bind::{PathSegment, StatePath};
        use crate::protocol::ui::tokens::{TableSelectMode, TableVariant};
        let bad = Table {
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
            bulk_actions: vec![non_button("bad")],
            virtualize: false,
            row_expandable: false,
            expanded_row_template_id: None,
        };
        assert!(bad.into_component("tbl").is_err());
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
        let ec = EmptyCell {
            variant: EmptyCellVariant::EmDash,
        };
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
            label: None,
            format: None,
            min: None,
            max: None,
            ticks: None,
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
        crate::protocol::ui::inline::ChartTooltip {
            enabled: true,
            format: None,
        }
    }

    #[test]
    fn line_chart_roundtrip() {
        use crate::protocol::ui::tokens::ChartZoomMode;
        let lc = LineChart {
            series: vec![],
            x_axis: axis(),
            y_axis: axis(),
            legend: legend(),
            tooltip: tooltip(),
            zoom: ChartZoomMode::X,
            brush: false,
            height_px: 200,
        };
        let c = lc.clone().into_component("lc").unwrap();
        assert_eq!(LineChart::try_from_component(&c).unwrap(), lc);
    }

    #[test]
    fn bar_chart_roundtrip() {
        use crate::protocol::ui::tokens::{BarStacking, ChartOrientation};
        let bc = BarChart {
            series: vec![],
            x_axis: axis(),
            y_axis: axis(),
            orientation: ChartOrientation::Vertical,
            stacking: BarStacking::Stacked,
            legend: legend(),
            height_px: 200,
        };
        let c = bc.clone().into_component("bc").unwrap();
        assert_eq!(BarChart::try_from_component(&c).unwrap(), bc);
    }

    #[test]
    fn area_chart_full_roundtrip_non_default_opacity() {
        use crate::protocol::ui::tokens::{AreaStacking, ChartZoomMode};
        let ac = AreaChart {
            series: vec![],
            x_axis: axis(),
            y_axis: axis(),
            legend: legend(),
            tooltip: tooltip(),
            zoom: ChartZoomMode::Both,
            brush: true,
            height_px: 400,
            stacking: AreaStacking::Percent,
            opacity: 0.75,
        };
        let c = ac.clone().into_component("ac").unwrap();
        assert_eq!(c.tag, AreaChart::TAG);
        assert_eq!(AreaChart::try_from_component(&c).unwrap(), ac);
    }

    #[test]
    fn area_chart_default_opacity_on_absent() {
        use crate::protocol::ui::tokens::{AreaStacking, ChartZoomMode};
        let ac = AreaChart {
            series: vec![],
            x_axis: axis(),
            y_axis: axis(),
            legend: legend(),
            tooltip: tooltip(),
            zoom: ChartZoomMode::None,
            brush: false,
            height_px: 200,
            stacking: AreaStacking::None,
            opacity: 0.4,
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
            show_labels: true,
            show_legend: true,
            max_segments: 5,
            height_px: 240,
        };
        let c = p.clone().into_component("pc").unwrap();
        assert_eq!(PieChart::try_from_component(&c).unwrap(), p);
    }

    #[test]
    fn stacked_bar_roundtrip() {
        let sb = StackedBar {
            segments: vec![],
            total: BindRef::Literal(Value::U64(100)),
            show_legend: false,
            show_percentages: true,
            height_px: 30,
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
            rows: vec![],
            columns: vec![],
            cells_path: StatePath::new(vec![PathSegment::Key("cells".into())]),
            scale: HeatmapScale::Linear {
                min: 0.0,
                max: 100.0,
                color_from: Tone::Info,
                color_to: Tone::Critical,
            },
            legend_position: HeatmapLegendPosition::Bottom,
            cell_size_px: 24,
            tooltip: true,
        };
        let c = h.clone().into_component("hm").unwrap();
        assert_eq!(Heatmap::try_from_component(&c).unwrap(), h);
    }

    #[test]
    fn gauge_roundtrip() {
        use crate::protocol::ui::tokens::GaugeVariant;
        let g = Gauge {
            value: BindRef::Literal(Value::U64(42)),
            min: 0.0,
            max: 100.0,
            thresholds: vec![],
            variant: GaugeVariant::Arc,
            label: None,
            format: None,
            size_px: 120,
        };
        let c = g.clone().into_component("gg").unwrap();
        assert_eq!(Gauge::try_from_component(&c).unwrap(), g);
    }

    // --- 1.8c3: Progress / Markdown / Specialised ------------------------

    #[test]
    fn progress_bar_full_roundtrip() {
        use crate::protocol::ui::tokens::{ProgressSize, ProgressVariant};
        let p = ProgressBar {
            value: BindRef::Literal(Value::U64(50)),
            max: 200.0,
            variant: ProgressVariant::Striped,
            tone: Tone::Warning,
            show_label: true,
            label: Some(lit("50%")),
            size: ProgressSize::Lg,
        };
        let c = p.clone().into_component("pb").unwrap();
        assert_eq!(ProgressBar::try_from_component(&c).unwrap(), p);
    }

    #[test]
    fn progress_bar_default_max_on_absent() {
        use crate::protocol::ui::tokens::{ProgressSize, ProgressVariant};
        let p = ProgressBar {
            value: BindRef::Literal(Value::U64(50)),
            max: 1.0,
            variant: ProgressVariant::Default,
            tone: Tone::Primary,
            show_label: true,
            label: None,
            size: ProgressSize::Md,
        };
        let mut c = p.into_component("pb").unwrap();
        c.fields.0.retain(|(k, _)| *k != 1);
        let back = ProgressBar::try_from_component(&c).unwrap();
        assert_eq!(back.max, 1.0);
    }

    #[test]
    fn rating_display_full_roundtrip() {
        use crate::protocol::ui::tokens::{RatingPrecision, RatingVariant};
        let r = RatingDisplay {
            value: BindRef::Literal(Value::U64(7)),
            max: 10,
            variant: RatingVariant::Hearts,
            show_value: true,
            precision: RatingPrecision::Decimal,
        };
        let c = r.clone().into_component("rd").unwrap();
        assert_eq!(RatingDisplay::try_from_component(&c).unwrap(), r);
    }

    #[test]
    fn rating_display_default_max_on_absent() {
        use crate::protocol::ui::tokens::{RatingPrecision, RatingVariant};
        let r = RatingDisplay {
            value: BindRef::Literal(Value::U64(4)),
            max: 5,
            variant: RatingVariant::Stars,
            show_value: true,
            precision: RatingPrecision::Half,
        };
        let mut c = r.into_component("rd").unwrap();
        c.fields.0.retain(|(k, _)| *k != 1);
        let back = RatingDisplay::try_from_component(&c).unwrap();
        assert_eq!(back.max, 5);
    }

    #[test]
    fn diff_roundtrip() {
        use crate::protocol::ui::bind::{PathSegment, StatePath};
        use crate::protocol::ui::tokens::DiffVariant;
        let d = Diff {
            before_path: StatePath::new(vec![PathSegment::Key("before".into())]),
            after_path: StatePath::new(vec![PathSegment::Key("after".into())]),
            variant: DiffVariant::Split,
            language: Some("rust".into()),
            word_wrap: false,
            show_line_numbers: true,
        };
        let c = d.clone().into_component("diff").unwrap();
        assert_eq!(Diff::try_from_component(&c).unwrap(), d);
    }

    #[test]
    fn markdown_roundtrip() {
        use crate::protocol::ui::tokens::{LinkTarget, MarkdownFeature};
        let m = Markdown {
            content: lit("# Hello\n\n- item"),
            allowed_features: vec![MarkdownFeature::Heading, MarkdownFeature::List],
            max_height_px: Some(800),
            link_target: LinkTarget::BlankViaCommand,
        };
        let c = m.clone().into_component("md").unwrap();
        assert_eq!(Markdown::try_from_component(&c).unwrap(), m);
    }

    #[test]
    fn data_definition_list_roundtrip() {
        use crate::protocol::ui::inline::DefItem;
        use crate::protocol::ui::tokens::DlLayout;
        let dl = DataDefinitionList {
            items: vec![DefItem {
                term: lit("Term"),
                definition: lit("Definition"),
            }],
            layout: DlLayout::TwoColumn,
        };
        let c = dl.clone().into_component("dl").unwrap();
        assert_eq!(DataDefinitionList::try_from_component(&c).unwrap(), dl);
    }

    #[test]
    fn json_viewer_full_roundtrip() {
        use crate::protocol::ui::bind::{PathSegment, StatePath};
        let j = JsonViewer {
            value_path: StatePath::new(vec![PathSegment::Key("data".into())]),
            collapsed_depth: 4,
            max_height_px: 600,
            searchable: false,
        };
        let c = j.clone().into_component("jv").unwrap();
        assert_eq!(JsonViewer::try_from_component(&c).unwrap(), j);
    }

    #[test]
    fn json_viewer_default_collapsed_depth_on_absent() {
        use crate::protocol::ui::bind::{PathSegment, StatePath};
        let j = JsonViewer {
            value_path: StatePath::new(vec![PathSegment::Key("data".into())]),
            collapsed_depth: 2,
            max_height_px: 400,
            searchable: true,
        };
        let mut c = j.into_component("jv").unwrap();
        c.fields.0.retain(|(k, _)| *k != 1);
        let back = JsonViewer::try_from_component(&c).unwrap();
        assert_eq!(back.collapsed_depth, 2);
    }

    #[test]
    fn calendar_month_roundtrip() {
        use crate::protocol::ui::tokens::DayOfWeek;
        let cm = CalendarMonth {
            month: lit("2026-05"),
            events_path: None,
            show_week_numbers: false,
            first_day_of_week: DayOfWeek::Monday,
        };
        let c = cm.clone().into_component("cm").unwrap();
        assert_eq!(CalendarMonth::try_from_component(&c).unwrap(), cm);
    }

    #[test]
    fn image_roundtrip() {
        use crate::protocol::ui::inline::DimensionToken;
        use crate::protocol::ui::tokens::{ImageFit, RadiusToken};
        let im = Image {
            src_ref: lit("ref-1"),
            alt: "Test".into(),
            width: Some(DimensionToken::Px { value: 200 }),
            height: None,
            fit: ImageFit::Cover,
            aspect_ratio: None,
            radius: Some(RadiusToken::Md),
            clickable: false,
            lazy_load: true,
        };
        let c = im.clone().into_component("im").unwrap();
        assert_eq!(Image::try_from_component(&c).unwrap(), im);
    }

    #[test]
    fn visually_hidden_roundtrip() {
        use crate::protocol::ui::tokens::LiveRegion as Pol;
        let vh = VisuallyHidden {
            content: lit("Loading"),
            as_live: Some(Pol::Polite),
        };
        let c = vh.clone().into_component("vh").unwrap();
        assert_eq!(VisuallyHidden::try_from_component(&c).unwrap(), vh);
    }

    #[test]
    fn live_region_component_roundtrip() {
        use crate::protocol::ui::tokens::LiveRegion as Pol;
        let lr = LiveRegionComponent {
            politeness: Pol::Assertive,
            content: lit("Saved camera C-25"),
            visible: false,
            tone: None,
            icon: None,
            clear_after_ms: Some(3000),
        };
        let c = lr.clone().into_component("lr").unwrap();
        assert_eq!(LiveRegionComponent::try_from_component(&c).unwrap(), lr);
    }
}
