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
