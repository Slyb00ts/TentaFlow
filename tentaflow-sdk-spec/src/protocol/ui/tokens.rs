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
