// =============================================================================
// File: protocol/ui/tokens.rs — semantic UI tokens (catalog §1.1)
// Purpose: tstr-wire enums for design tokens addons declare; renderer maps
// them to concrete CSS variables / values. Addon NEVER ships raw HTML/CSS,
// hex colors, font sizes — only these tokens.
// =============================================================================

string_enum! {
    /// Semantic tone applied to a component or token.
    pub enum Tone {
        Neutral = "neutral",
        Primary = "primary",
        Success = "success",
        Warning = "warning",
        Critical = "critical",
        Info = "info",
        Muted = "muted",
    }
}

string_enum! {
    /// Visual variant for action buttons.
    pub enum ButtonVariant {
        Primary = "primary",
        Secondary = "secondary",
        Tertiary = "tertiary",
        Ghost = "ghost",
        Destructive = "destructive",
        Link = "link",
    }
}

string_enum! {
    /// Visual variant for badges.
    pub enum BadgeVariant {
        Solid = "solid",
        Soft = "soft",
        Outline = "outline",
        Pulse = "pulse",
        Dot = "dot",
    }
}

string_enum! {
    /// Visual variant for chips.
    pub enum ChipVariant {
        Solid = "solid",
        Soft = "soft",
        Outline = "outline",
        Removable = "removable",
        Selectable = "selectable",
        Toggle = "toggle",
    }
}

string_enum! {
    /// Density / vertical rhythm of a component or layout.
    pub enum Density {
        Compact = "compact",
        Default = "default",
        Comfortable = "comfortable",
    }
}

string_enum! {
    /// Spacing scale token (maps to 0/2/4/8/12/16/24/32 px in theme CSS).
    pub enum Spacing {
        Zero = "zero",
        Xxs = "xxs",
        Xs = "xs",
        Sm = "sm",
        Md = "md",
        Lg = "lg",
        Xl = "xl",
        Xxl = "xxl",
    }
}

string_enum! {
    /// Typographic style token (maps to type-scale rule in theme CSS).
    pub enum TextStyle {
        Display = "display",
        Title = "title",
        H1 = "h1",
        H2 = "h2",
        H3 = "h3",
        H4 = "h4",
        BodyLg = "body_lg",
        Body = "body",
        BodyStrong = "body_strong",
        Caption = "caption",
        CaptionStrong = "caption_strong",
        Overline = "overline",
        Code = "code",
        Mono = "mono",
        Quote = "quote",
    }
}

string_enum! {
    /// Text horizontal alignment.
    pub enum TextAlign {
        Start = "start",
        Center = "center",
        End = "end",
        Justify = "justify",
    }
}

string_enum! {
    /// CSS `text-wrap` family (wrap/nowrap/balance/pretty).
    pub enum TextWrap {
        Wrap = "wrap",
        Nowrap = "nowrap",
        Balance = "balance",
        Pretty = "pretty",
    }
}

string_enum! {
    /// Border-radius token.
    pub enum RadiusToken {
        None = "none",
        Xs = "xs",
        Sm = "sm",
        Md = "md",
        Lg = "lg",
        Xl = "xl",
        Pill = "pill",
        Circle = "circle",
    }
}

string_enum! {
    /// Elevation / box-shadow token.
    pub enum ShadowToken {
        None = "none",
        Subtle = "subtle",
        Medium = "medium",
        Elevated = "elevated",
        Floating = "floating",
        AccentGlow = "accent_glow",
    }
}

string_enum! {
    /// Split divider rendering (catalog §3 0x0105). `handle` is the default
    /// draggable bar; `line` renders a hairline (still draggable when
    /// `resizable`); `none` hides the divider and separates the panes with a
    /// standard gap instead.
    pub enum SplitDivider {
        Handle = "handle",
        Line = "line",
        None = "none",
    }
}

string_enum! {
    /// Text-field visual variant (catalog §5 0x0301/0x0302). `outlined` is the
    /// default framed field; `ghost` drops border/background/padding so the
    /// control blends into surrounding content (e.g. inline title editing).
    pub enum InputVariant {
        Outlined = "outlined",
        Ghost = "ghost",
    }
}

string_enum! {
    /// Responsive breakpoint label. Maps to 640/768/1024/1280/1536/1920 px in theme CSS.
    pub enum Breakpoint {
        Xs = "xs",
        Sm = "sm",
        Md = "md",
        Lg = "lg",
        Xl = "xl",
        Xxl = "xxl",
    }
}

string_enum! {
    /// Icon size token. Maps to 12/16/20/24/32 px in icon sprite.
    pub enum IconSize {
        Xs = "xs",
        Sm = "sm",
        Md = "md",
        Lg = "lg",
        Xl = "xl",
    }
}

string_enum! {
    /// Scroll behavior hint for Scroll commands and ScrollContainer.
    pub enum ScrollBehavior {
        Auto = "auto",
        Smooth = "smooth",
        Instant = "instant",
    }
}

string_enum! {
    /// Side a Drawer slot opens from.
    pub enum DrawerSide {
        Left = "left",
        Right = "right",
        Top = "top",
        Bottom = "bottom",
    }
}

string_enum! {
    /// Target window for `Command::NavigateExternal`.
    pub enum NavigateTarget {
        NewTab = "new_tab",
        SameTab = "same_tab",
        SystemBrowser = "system_browser",
    }
}

string_enum! {
    /// ARIA live-region politeness.
    pub enum LiveRegion {
        Off = "off",
        Polite = "polite",
        Assertive = "assertive",
    }
}

string_enum! {
    /// Cursor token; renderer maps to CSS `cursor:` values.
    pub enum CursorToken {
        Default = "default",
        Pointer = "pointer",
        Text = "text",
        Move = "move",
        Grab = "grab",
        Grabbing = "grabbing",
        NotAllowed = "not_allowed",
        Crosshair = "crosshair",
        ColResize = "col_resize",
        RowResize = "row_resize",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip<T>(v: T) -> T
    where
        T: minicbor::Encode<()> + for<'b> minicbor::Decode<'b, ()> + PartialEq + core::fmt::Debug,
    {
        let mut buf = Vec::new();
        minicbor::encode(&v, &mut buf).unwrap();
        let decoded: T = minicbor::decode(&buf).unwrap();
        assert_eq!(decoded, v);
        v
    }

    #[test]
    fn tone_roundtrip_all_variants() {
        for v in [
            Tone::Neutral,
            Tone::Primary,
            Tone::Success,
            Tone::Warning,
            Tone::Critical,
            Tone::Info,
            Tone::Muted,
        ] {
            roundtrip(v);
        }
    }

    #[test]
    fn unknown_variant_rejected() {
        let mut buf = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut buf);
        enc.str("not_a_tone").unwrap();
        let res: Result<Tone, _> = minicbor::decode(&buf);
        assert!(res.is_err());
    }

    #[test]
    fn spacing_wire_strings_match_doc() {
        assert_eq!(Spacing::Zero.as_str(), "zero");
        assert_eq!(Spacing::Xxs.as_str(), "xxs");
        assert_eq!(Spacing::Xxl.as_str(), "xxl");
    }

    #[test]
    fn navigate_target_wire_strings_match_doc() {
        assert_eq!(NavigateTarget::NewTab.as_str(), "new_tab");
        assert_eq!(NavigateTarget::SystemBrowser.as_str(), "system_browser");
    }

    #[test]
    fn shadow_token_roundtrip_all_variants() {
        for v in [
            ShadowToken::None,
            ShadowToken::Subtle,
            ShadowToken::Medium,
            ShadowToken::Elevated,
            ShadowToken::Floating,
            ShadowToken::AccentGlow,
        ] {
            roundtrip(v);
        }
        assert_eq!(ShadowToken::AccentGlow.as_str(), "accent_glow");
    }
}

string_enum! {
    /// Semantic color token (catalog §1.5).
    pub enum ColorToken {
        BackgroundDefault = "background_default",
        BackgroundSubtle = "background_subtle",
        BackgroundMuted = "background_muted",
        SurfaceDefault = "surface_default",
        SurfaceRaised = "surface_raised",
        SurfaceOverlay = "surface_overlay",
        BorderDefault = "border_default",
        BorderStrong = "border_strong",
        BorderSubtle = "border_subtle",
        TextDefault = "text_default",
        TextMuted = "text_muted",
        TextInverse = "text_inverse",
        AccentPrimary = "accent_primary",
        AccentSecondary = "accent_secondary",
        ToneNeutral = "tone_neutral",
        ToneSuccess = "tone_success",
        ToneWarning = "tone_warning",
        ToneCritical = "tone_critical",
        ToneInfo = "tone_info",
    }
}

string_enum! {
    /// Background fill token (catalog §1.5).
    pub enum BackgroundToken {
        None = "none",
        Subtle = "subtle",
        Muted = "muted",
        Accent = "accent",
        Inverse = "inverse",
    }
}

string_enum! {
    /// Flexbox cross-axis alignment (catalog §1.5 / §3).
    pub enum FlexAlign {
        Start = "start",
        End = "end",
        Center = "center",
        Baseline = "baseline",
        Stretch = "stretch",
    }
}

string_enum! {
    /// Flexbox main-axis distribution (catalog §1.5 / §3).
    pub enum FlexJustify {
        Start = "start",
        End = "end",
        Center = "center",
        SpaceBetween = "space_between",
        SpaceAround = "space_around",
        SpaceEvenly = "space_evenly",
    }
}

string_enum! {
    /// Sort direction for Table columns (catalog §1.5 TableSort).
    pub enum SortDirection {
        Asc = "asc",
        Desc = "desc",
    }
}

string_enum! {
    /// File upload lifecycle (catalog §1.5 FileMeta).
    pub enum FileUploadStatus {
        Queued = "queued",
        Uploading = "uploading",
        Complete = "complete",
        Error = "error",
    }
}

string_enum! {
    /// Step status in WizardShell / StepDef (catalog §1.5).
    pub enum StepStatus {
        Pending = "pending",
        Current = "current",
        Complete = "complete",
        Error = "error",
        Skipped = "skipped",
    }
}

string_enum! {
    /// BottomSheet detent (catalog §1.5 SheetDetent).
    pub enum SheetDetent {
        Small = "small",
        Medium = "medium",
        Large = "large",
        Full = "full",
    }
}

string_enum! {
    /// Chart series line style (catalog §1.5 ChartSeriesStyle).
    pub enum ChartSeriesStyle {
        Solid = "solid",
        Dashed = "dashed",
        Dotted = "dotted",
    }
}

string_enum! {
    /// Chart axis scale (catalog §1.5 ChartAxisScale).
    pub enum ChartAxisScale {
        Linear = "linear",
        Log = "log",
        Time = "time",
        Category = "category",
    }
}

string_enum! {
    /// Chart legend position (catalog §1.5 ChartLegend).
    pub enum ChartLegendPosition {
        Top = "top",
        Bottom = "bottom",
        Left = "left",
        Right = "right",
        None = "none",
    }
}

string_enum! {
    /// Chart legend item alignment (catalog §1.5 ChartLegend).
    pub enum ChartLegendAlign {
        Start = "start",
        Center = "center",
        End = "end",
    }
}

string_enum! {
    /// Table column rendering hint (catalog §1.5 ColumnRender).
    pub enum ColumnRender {
        Text = "text",
        Number = "number",
        Currency = "currency",
        Percent = "percent",
        Bytes = "bytes",
        Date = "date",
        Time = "time",
        Datetime = "datetime",
        Relative = "relative",
        Badge = "badge",
        Chip = "chip",
        Tag = "tag",
        Avatar = "avatar",
        AvatarGroup = "avatar_group",
        // Image: cell value is an image URL rendered as a fixed-size `<img>`
        // (object-fit contain); empty value → a muted em-dash placeholder.
        Image = "image",
        Icon = "icon",
        Stat = "stat",
        Trend = "trend",
        Progress = "progress",
        Rating = "rating",
        Actions = "actions",
        Checkbox = "checkbox",
        Boolean = "boolean",
        CustomTemplate = "custom_template",
    }
}

string_enum! {
    /// Variant token used by `EmptyState` component (catalog §2 0x0003).
    pub enum EmptyStateVariant {
        Default = "default",
        Compact = "compact",
        Illustrated = "illustrated",
    }
}

string_enum! {
    /// Flex container direction (catalog §3 0x0101 Flex).
    pub enum FlexDirection {
        Row = "row",
        RowReverse = "row_reverse",
        Column = "column",
        ColumnReverse = "column_reverse",
    }
}

string_enum! {
    /// Flex wrap mode (catalog §3 0x0101 Flex).
    pub enum FlexWrap {
        NoWrap = "no_wrap",
        Wrap = "wrap",
        WrapReverse = "wrap_reverse",
    }
}

string_enum! {
    /// Split orientation (catalog §3 0x0105 Split).
    pub enum SplitOrientation {
        Horizontal = "horizontal",
        Vertical = "vertical",
    }
}

string_enum! {
    /// Card variant (catalog §3 0x0106 Card / 0x0107 SectionCard).
    pub enum CardVariant {
        Filled = "filled",
        Outlined = "outlined",
        Elevated = "elevated",
        Ghost = "ghost",
    }
}

string_enum! {
    /// Divider orientation (catalog §3 0x0108 Divider).
    pub enum DividerOrientation {
        Horizontal = "horizontal",
        Vertical = "vertical",
    }
}

string_enum! {
    /// Divider variant (catalog §3 0x0108 Divider).
    pub enum DividerVariant {
        Default = "default",
        Subtle = "subtle",
        Strong = "strong",
        Dashed = "dashed",
    }
}

string_enum! {
    /// Spacer axis (catalog §3 0x0109 Spacer).
    pub enum SpacerAxis {
        X = "x",
        Y = "y",
        Both = "both",
    }
}

string_enum! {
    /// Tabs visual variant (catalog §3 0x010B Tabs).
    pub enum TabsVariant {
        Default = "default",
        Pills = "pills",
        Underlined = "underlined",
        Boxed = "boxed",
    }
}

string_enum! {
    /// NavTabs visual variant (catalog §3 0x010C NavTabs).
    pub enum NavTabsVariant {
        Default = "default",
        Underlined = "underlined",
        Pills = "pills",
    }
}

string_enum! {
    /// Accordion mutex mode (catalog §3 0x010E Accordion).
    pub enum AccordionMode {
        Single = "single",
        Multiple = "multiple",
    }
}

string_enum! {
    /// Breadcrumb separator style (catalog §3 0x0110 Breadcrumb).
    pub enum BreadcrumbSeparator {
        Chevron = "chevron",
        Slash = "slash",
        Dot = "dot",
    }
}

string_enum! {
    /// Pagination presentation style (catalog §3 0x0111 Pagination).
    pub enum PaginationVariant {
        Compact = "compact",
        Full = "full",
        Input = "input",
    }
}

string_enum! {
    /// ScrollContainer orientation (catalog §3 0x0112 ScrollContainer).
    pub enum ScrollOrientation {
        Vertical = "vertical",
        Horizontal = "horizontal",
        Both = "both",
    }
}

string_enum! {
    /// Markdown inline mark allowed in Paragraph/RichText (catalog §4 0x0203/0x0204).
    pub enum MarkdownMark {
        Bold = "bold",
        Italic = "italic",
        Code = "code",
        Link = "link",
    }
}

string_enum! {
    /// Markdown block element allowed in RichText/Markdown (catalog §4 0x0204/0x0220).
    pub enum MarkdownBlock {
        Heading = "heading",
        List = "list",
        CodeBlock = "code_block",
        Blockquote = "blockquote",
        Table = "table",
    }
}

string_enum! {
    /// `KeyValue` layout style (catalog §4 0x0207).
    pub enum KvLayout {
        Stacked = "stacked",
        Horizontal = "horizontal",
        Grid = "grid",
    }
}

string_enum! {
    /// `Stat` size token (catalog §4 0x0209).
    pub enum StatSize {
        Sm = "sm",
        Md = "md",
        Lg = "lg",
    }
}

string_enum! {
    /// `Tag` size token (catalog §4 0x020C).
    pub enum TagSize {
        Xs = "xs",
        Sm = "sm",
        Md = "md",
    }
}

string_enum! {
    /// `Avatar` size token (catalog §4 0x020D).
    pub enum AvatarSize {
        Xs = "xs",
        Sm = "sm",
        Md = "md",
        Lg = "lg",
        Xl = "xl",
    }
}

string_enum! {
    /// `Avatar` outline shape (catalog §4 0x020D).
    pub enum AvatarShape {
        Circle = "circle",
        Rounded = "rounded",
        Square = "square",
    }
}

string_enum! {
    /// Online/presence indicator for `Avatar` (catalog §4 0x020D).
    pub enum AvatarStatus {
        Online = "online",
        Offline = "offline",
        Busy = "busy",
        Away = "away",
    }
}

string_enum! {
    /// Visual spacing between stacked avatars in `AvatarGroup` (catalog §4 0x020E).
    pub enum AvatarOverlap {
        Tight = "tight",
        Default = "default",
        Loose = "loose",
    }
}

string_enum! {
    /// `BulletList` bullet style (catalog §4 0x020F).
    pub enum BulletListVariant {
        Bullet = "bullet",
        Numbered = "numbered",
        Check = "check",
        Icon = "icon",
    }
}

string_enum! {
    /// `Timeline` orientation (catalog §4 0x0210).
    pub enum TimelineOrientation {
        Vertical = "vertical",
        Horizontal = "horizontal",
    }
}

string_enum! {
    /// `Table` visual variant (catalog §4 0x0211).
    pub enum TableVariant {
        Default = "default",
        Striped = "striped",
        Borderless = "borderless",
        Compact = "compact",
    }
}

string_enum! {
    /// `Table` row-selection mode (catalog §4 0x0211).
    pub enum TableSelectMode {
        None = "none",
        Single = "single",
        Multi = "multi",
    }
}

string_enum! {
    /// `Tree` visual variant (catalog §4 0x0213).
    pub enum TreeVariant {
        Default = "default",
        Compact = "compact",
        WithIcons = "with_icons",
    }
}

string_enum! {
    /// `EmptyCell` rendering style (catalog §4 0x0214).
    pub enum EmptyCellVariant {
        Dash = "dash",
        EmDash = "em_dash",
        NA = "n_a",
        None = "none",
        Loading = "loading",
    }
}

string_enum! {
    /// `Sparkline` visual variant (catalog §4 0x0215).
    pub enum SparklineVariant {
        Line = "line",
        Area = "area",
        Bar = "bar",
    }
}

string_enum! {
    /// Chart zoom mode (catalog §4 LineChart/AreaChart).
    pub enum ChartZoomMode {
        None = "none",
        X = "x",
        Y = "y",
        Both = "both",
    }
}

string_enum! {
    /// `BarChart` orientation (catalog §4 0x0217).
    pub enum ChartOrientation {
        Vertical = "vertical",
        Horizontal = "horizontal",
    }
}

string_enum! {
    /// `BarChart` stacking mode (catalog §4 0x0217).
    pub enum BarStacking {
        None = "none",
        Stacked = "stacked",
        Percent = "percent",
    }
}

string_enum! {
    /// `AreaChart` stacking mode (catalog §4 0x0218).
    pub enum AreaStacking {
        None = "none",
        Stacked = "stacked",
        Percent = "percent",
    }
}

string_enum! {
    /// `PieChart` variant (catalog §4 0x0219).
    pub enum PieVariant {
        Pie = "pie",
        Donut = "donut",
    }
}

string_enum! {
    /// `Heatmap` legend placement (catalog §4 0x021B).
    pub enum HeatmapLegendPosition {
        TopRight = "top_right",
        Bottom = "bottom",
        None = "none",
    }
}

string_enum! {
    /// `Gauge` shape variant (catalog §4 0x021C).
    pub enum GaugeVariant {
        Circular = "circular",
        Arc = "arc",
        Semi = "semi",
    }
}

string_enum! {
    /// `ProgressBar` visual variant (catalog §4 0x021D).
    pub enum ProgressVariant {
        Default = "default",
        Striped = "striped",
        Indeterminate = "indeterminate",
    }
}

string_enum! {
    /// `ProgressBar` size token (catalog §4 0x021D).
    pub enum ProgressSize {
        Xs = "xs",
        Sm = "sm",
        Md = "md",
        Lg = "lg",
    }
}

string_enum! {
    /// `ProgressBar` fill orientation (catalog §4 0x021D). `Horizontal` is the
    /// default (fill grows left→right); `Vertical` is a narrow column whose
    /// fill grows bottom→top, used by ranked-score columns.
    pub enum ProgressOrientation {
        Horizontal = "horizontal",
        Vertical = "vertical",
    }
}

string_enum! {
    /// `RatingDisplay` visual variant (catalog §4 0x021E).
    pub enum RatingVariant {
        Stars = "stars",
        Hearts = "hearts",
        Circles = "circles",
        Numeric = "numeric",
    }
}

string_enum! {
    /// `RatingDisplay` precision token (catalog §4 0x021E).
    pub enum RatingPrecision {
        Full = "full",
        Half = "half",
        Decimal = "decimal",
    }
}

string_enum! {
    /// `Diff` rendering variant (catalog §4 0x021F).
    pub enum DiffVariant {
        Split = "split",
        Inline = "inline",
        Unified = "unified",
    }
}

string_enum! {
    /// `Markdown` allowed-feature token (catalog §4 0x0220).
    pub enum MarkdownFeature {
        Heading = "heading",
        List = "list",
        CodeBlock = "code_block",
        Blockquote = "blockquote",
        Table = "table",
        Link = "link",
        Image = "image",
        Emphasis = "emphasis",
        Strong = "strong",
        CodeInline = "code_inline",
    }
}

string_enum! {
    /// `Markdown.link_target` policy (catalog §4 0x0220).
    pub enum LinkTarget {
        SelfTarget = "self",
        BlankViaCommand = "blank_via_command",
    }
}

string_enum! {
    /// `DataDefinitionList` layout (catalog §4 0x0221).
    pub enum DlLayout {
        Stacked = "stacked",
        TwoColumn = "two_column",
    }
}

string_enum! {
    /// `CalendarMonth.first_day_of_week` (catalog §4 0x0223).
    pub enum DayOfWeek {
        Sunday = "sunday",
        Monday = "monday",
    }
}

string_enum! {
    /// `Image.fit` mode (catalog §4 0x0224).
    pub enum ImageFit {
        Cover = "cover",
        Contain = "contain",
        Fill = "fill",
        None = "none",
    }
}

string_enum! {
    /// `Input.type` (catalog §5 0x0301).
    pub enum InputType {
        Text = "text",
        Email = "email",
        Password = "password",
        Url = "url",
        Phone = "phone",
        Number = "number",
        Search = "search",
    }
}

string_enum! {
    /// HTML `autocomplete` hint (catalog §5 0x0301).
    pub enum AutocompleteHint {
        Off = "off",
        On = "on",
        Name = "name",
        Email = "email",
        Username = "username",
        CurrentPassword = "current_password",
        NewPassword = "new_password",
        OneTimeCode = "one_time_code",
        Tel = "tel",
        Url = "url",
        StreetAddress = "street_address",
        PostalCode = "postal_code",
    }
}

string_enum! {
    /// Mobile virtual-keyboard hint (catalog §5 0x0301).
    pub enum InputMode {
        None = "none",
        Text = "text",
        Tel = "tel",
        Url = "url",
        Email = "email",
        Numeric = "numeric",
        Decimal = "decimal",
        Search = "search",
    }
}

string_enum! {
    /// Common input size token (catalog §5 multiple components).
    pub enum InputSize {
        Sm = "sm",
        Md = "md",
        Lg = "lg",
    }
}

string_enum! {
    /// `SearchBox` visual variant (catalog §5 0x0307).
    pub enum SearchVariant {
        Default = "default",
        Subtle = "subtle",
        Prominent = "prominent",
    }
}

string_enum! {
    /// `Toggle` size token (catalog §5 0x030A).
    pub enum ToggleSize {
        Sm = "sm",
        Md = "md",
        Lg = "lg",
    }
}

string_enum! {
    /// `Toggle.label_position` (catalog §5 0x030A).
    pub enum TogglePosition {
        Leading = "leading",
        Trailing = "trailing",
    }
}

string_enum! {
    /// `Checkbox` size token (catalog §5 0x030B).
    pub enum CheckboxSize {
        Sm = "sm",
        Md = "md",
        Lg = "lg",
    }
}

string_enum! {
    /// `RadioGroup` axis (catalog §5 0x030D).
    pub enum RadioGroupOrientation {
        Horizontal = "horizontal",
        Vertical = "vertical",
    }
}

string_enum! {
    /// `RadioCardGroup` visual variant (catalog §5 0x030E).
    pub enum RadioCardVariant {
        Default = "default",
        Compact = "compact",
        Feature = "feature",
    }
}

string_enum! {
    /// `SliderRow.layout` (catalog §5 0x0311).
    pub enum SliderRowLayout {
        Horizontal = "horizontal",
        Compact = "compact",
    }
}

string_enum! {
    /// `TimePicker.precision` (catalog §5 0x0316/0x0317).
    pub enum TimePrecision {
        Minute = "minute",
        Second = "second",
    }
}

string_enum! {
    /// Mobile camera-capture hint for `FileInput` (catalog §5 0x0318).
    pub enum FileCapture {
        User = "user",
        Environment = "environment",
    }
}

string_enum! {
    /// `ColorPicker` variant (catalog §5 0x0319).
    pub enum ColorPickerVariant {
        Swatch = "swatch",
        Wheel = "wheel",
        Compact = "compact",
        TokensOnly = "tokens_only",
    }
}

string_enum! {
    /// `FormField.layout` (catalog §5 0x031A).
    pub enum FormFieldLayout {
        Stacked = "stacked",
        Horizontal = "horizontal",
    }
}

string_enum! {
    /// `Form.layout` (catalog §5 0x031D).
    pub enum FormLayout {
        Stacked = "stacked",
        Horizontal = "horizontal",
        Compact = "compact",
    }
}

string_enum! {
    /// `Button.size` (catalog §6 0x0401).
    pub enum ButtonSize {
        Xs = "xs",
        Sm = "sm",
        Md = "md",
        Lg = "lg",
    }
}

string_enum! {
    /// `ButtonGroup.orientation` (catalog §6 0x0403).
    pub enum ButtonGroupOrientation {
        Horizontal = "horizontal",
        Vertical = "vertical",
    }
}

string_enum! {
    /// `Link.underline` / `LinkButton.underline` (catalog §6 0x0404/0x0405).
    pub enum LinkUnderline {
        Always = "always",
        Hover = "hover",
        Never = "never",
    }
}

string_enum! {
    /// `MenuButton.placement` (catalog §6 0x0406).
    pub enum MenuPlacement {
        BottomStart = "bottom_start",
        BottomEnd = "bottom_end",
        TopStart = "top_start",
        TopEnd = "top_end",
        LeftStart = "left_start",
        LeftEnd = "left_end",
        RightStart = "right_start",
        RightEnd = "right_end",
    }
}

string_enum! {
    /// `SegmentedControl.size` (catalog §6 0x0409).
    pub enum SegmentSize {
        Sm = "sm",
        Md = "md",
        Lg = "lg",
    }
}

string_enum! {
    /// `FilterChips.mode` (catalog §6 0x040A).
    pub enum FilterChipsMode {
        Single = "single",
        Multi = "multi",
    }
}

string_enum! {
    /// `Fab.size` (catalog §6 0x040C).
    pub enum FabSize {
        Sm = "sm",
        Md = "md",
        Lg = "lg",
    }
}

string_enum! {
    /// `Fab.position` (catalog §6 0x040C).
    pub enum FabPosition {
        BottomRight = "bottom_right",
        BottomLeft = "bottom_left",
        Inline = "inline",
    }
}

string_enum! {
    /// `Alert.variant` (catalog §7 0x0501).
    pub enum AlertVariant {
        Default = "default",
        Filled = "filled",
        Outlined = "outlined",
        Soft = "soft",
    }
}

string_enum! {
    /// `Banner.position` (catalog §7 0x0502).
    pub enum BannerPosition {
        Top = "top",
        Inline = "inline",
    }
}

string_enum! {
    /// `Skeleton.variant` (catalog §7 0x0506).
    pub enum SkeletonVariant {
        Text = "text",
        Circle = "circle",
        Rectangle = "rectangle",
        Card = "card",
        TableRow = "table_row",
    }
}

string_enum! {
    /// `Spinner.size` (catalog §7 0x0507).
    pub enum SpinnerSize {
        Xs = "xs",
        Sm = "sm",
        Md = "md",
        Lg = "lg",
        Xl = "xl",
    }
}

string_enum! {
    /// `Spinner.variant` (catalog §7 0x0507).
    pub enum SpinnerVariant {
        Default = "default",
        Ring = "ring",
        Dots = "dots",
        Bars = "bars",
    }
}

string_enum! {
    /// `Modal.size` (catalog §7 0x0509).
    pub enum ModalSize {
        Xs = "xs",
        Sm = "sm",
        Md = "md",
        Lg = "lg",
        Xl = "xl",
        Fullscreen = "fullscreen",
    }
}

string_enum! {
    /// `Drawer.size` (catalog §7 0x050A).
    pub enum DrawerSize {
        Xs = "xs",
        Sm = "sm",
        Md = "md",
        Lg = "lg",
        Xl = "xl",
    }
}

string_enum! {
    /// `Popover.placement` (catalog §7 0x050B).
    pub enum PopoverPlacement {
        Top = "top",
        TopStart = "top_start",
        TopEnd = "top_end",
        Bottom = "bottom",
        BottomStart = "bottom_start",
        BottomEnd = "bottom_end",
        Left = "left",
        LeftStart = "left_start",
        LeftEnd = "left_end",
        Right = "right",
        RightStart = "right_start",
        RightEnd = "right_end",
    }
}

string_enum! {
    /// `GateScreen.variant` (catalog §7 0x050D).
    pub enum GateVariant {
        AuthRequired = "auth_required",
        PermissionDenied = "permission_denied",
        RateLimited = "rate_limited",
        Maintenance = "maintenance",
    }
}

string_enum! {
    /// `VideoStream.controls` (catalog §8 0x0604).
    pub enum VideoControls {
        None = "none",
        Minimal = "minimal",
        Full = "full",
    }
}

string_enum! {
    /// `LiveCameraTile.status` (catalog §8 0x0605).
    pub enum CameraStatus {
        Online = "online",
        Offline = "offline",
        Buffering = "buffering",
        Error = "error",
    }
}

string_enum! {
    /// `MapView.tile_provider` (catalog §8 0x0606).
    pub enum TileProvider {
        Osm = "osm",
        Mapbox = "mapbox",
        TileServer = "tile_server",
    }
}

string_enum! {
    /// `CodeEditor.theme` (catalog §8 0x0607).
    pub enum CodeEditorTheme {
        Auto = "auto",
        Light = "light",
        Dark = "dark",
    }
}

string_enum! {
    /// `Terminal.theme` (catalog §8 0x0608).
    pub enum TerminalTheme {
        Default = "default",
        HighContrast = "high_contrast",
        Dim = "dim",
    }
}

string_enum! {
    /// `Audio.controls` (catalog §8 0x0609).
    pub enum AudioControls {
        None = "none",
        Minimal = "minimal",
        Full = "full",
    }
}

string_enum! {
    /// `Audio.variant` (catalog §8 0x0609).
    pub enum AudioVariant {
        Default = "default",
        Compact = "compact",
        Waveform = "waveform",
    }
}

string_enum! {
    /// `AudioCapture.mode` (catalog §8 0x0612).
    pub enum AudioCaptureMode {
        PushToTalk = "push_to_talk",
        Vad = "vad",
    }
}

string_enum! {
    /// `AudioCapture.variant` (catalog §8 0x0612). `standalone` is the vertical
    /// column (waves framing the mic, label + status below); `docked` is the
    /// horizontal strip for recording docks (mic on the left, one waveform on
    /// the right, no idle label — status only when non-empty).
    pub enum AudioCaptureVariant {
        Standalone = "standalone",
        Docked = "docked",
    }
}

string_enum! {
    /// `FpsCounter.variant` (catalog §8 0x060E).
    pub enum FpsVariant {
        Minimal = "minimal",
        Detailed = "detailed",
    }
}

string_enum! {
    /// `Stopwatch.variant` (catalog §8 0x0610).
    pub enum StopwatchVariant {
        Seconds = "seconds",
        Minutes = "minutes",
        Hours = "hours",
        Full = "full",
    }
}

string_enum! {
    /// `Carousel.gestures` (catalog §8 0x060C).
    pub enum CarouselGestures {
        Swipe = "swipe",
        ArrowsOnly = "arrows_only",
        None = "none",
    }
}

string_enum! {
    /// `PdfViewer.zoom_mode` (catalog §8 0x060D).
    pub enum PdfZoomMode {
        FitWidth = "fit_width",
        FitHeight = "fit_height",
        Actual = "actual",
        Custom = "custom",
    }
}

string_enum! {
    /// `StepProgress.variant` (catalog §8 0x060F).
    pub enum StepProgressVariant {
        Horizontal = "horizontal",
        Vertical = "vertical",
        Compact = "compact",
    }
}

string_enum! {
    /// `IFrame.sandbox` token (catalog §8 0x060A). Allow-listed tokens only;
    /// host validator rejects `allow-same-origin`, `allow-top-navigation`,
    /// `allow-popups-to-escape-sandbox` per spec.
    pub enum IFrameSandbox {
        AllowScripts = "allow-scripts",
        AllowForms = "allow-forms",
        AllowPopups = "allow-popups",
        AllowModals = "allow-modals",
    }
}

string_enum! {
    /// `IFrame.referrer_policy` (catalog §8 0x060A). Default `no-referrer`.
    pub enum IFrameReferrerPolicy {
        NoReferrer = "no-referrer",
        NoReferrerWhenDowngrade = "no-referrer-when-downgrade",
        Origin = "origin",
        OriginWhenCrossOrigin = "origin-when-cross-origin",
        SameOrigin = "same-origin",
        StrictOrigin = "strict-origin",
        StrictOriginWhenCrossOrigin = "strict-origin-when-cross-origin",
        UnsafeUrl = "unsafe-url",
    }
}

string_enum! {
    /// Semantic border color token for `BorderSide` (catalog §1.5 BoxStyle).
    /// Renderer maps to theme CSS vars (`--tf-border`, `--tf-accent-1`, …).
    pub enum BorderColor {
        Default = "default",
        Hover = "hover",
        Accent = "accent",
        Success = "success",
        Warning = "warning",
        Danger = "danger",
        Transparent = "transparent",
    }
}

string_enum! {
    /// Border line style for `BorderSide` (catalog §1.5 BoxStyle).
    pub enum BorderLineStyle {
        Solid = "solid",
        Dashed = "dashed",
        None = "none",
    }
}

string_enum! {
    /// CSS overflow behavior for `BoxStyle` (catalog §1.5).
    pub enum Overflow {
        Visible = "visible",
        Hidden = "hidden",
        Auto = "auto",
        Scroll = "scroll",
    }
}

string_enum! {
    /// `VirtualizedLog.variant` (catalog §8 0x0611).
    pub enum LogVariant {
        Compact = "compact",
        Default = "default",
        Expanded = "expanded",
    }
}

string_enum! {
    /// Log event level (catalog §8 0x0611 LogEvent).
    pub enum LogLevel {
        Trace = "trace",
        Debug = "debug",
        Info = "info",
        Warn = "warn",
        Error = "error",
        Fatal = "fatal",
    }
}
