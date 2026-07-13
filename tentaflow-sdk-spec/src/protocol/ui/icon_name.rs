// =============================================================================
// File: protocol/ui/icon_name.rs — IconName enum (§1.2)
// Purpose: typed wire identifier for SVG sprites in tentaflow-core/www/img/
// icons.svg. Wire form is snake_case (e.g. "arrow_down"); renderer maps to
// the kebab-cased SVG symbol id ("icon-arrow-down"). 142 variants — list is
// authoritative and MUST match icons.svg symbol ids one-to-one.
// =============================================================================

string_enum! {
    /// Whitelist of known icon names. Unknown decode → `Error{InvalidIcon}`.
    pub enum IconName {
        Add = "add",
        Alarms = "alarms",
        Alert = "alert",
        Apps = "apps",
        Arrow = "arrow",
        ArrowDown = "arrow_down",
        ArrowLeft = "arrow_left",
        ArrowOut = "arrow_out",
        ArrowRight = "arrow_right",
        ArrowUp = "arrow_up",
        Audit = "audit",
        Ban = "ban",
        BarChart = "bar_chart",
        Bell = "bell",
        Bolt = "bolt",
        Brain = "brain",
        Branch = "branch",
        Cameras = "cameras",
        Cancel = "cancel",
        Catalog = "catalog",
        ChartLine = "chart_line",
        Chat = "chat",
        Check = "check",
        ChevronDown = "chevron_down",
        ChevronLeft = "chevron_left",
        ChevronRight = "chevron_right",
        ChevronUp = "chevron_up",
        Chip = "chip",
        Clock = "clock",
        ClockGlance = "clock_glance",
        Close = "close",
        Cloud = "cloud",
        Cluster = "cluster",
        Code = "code",
        Collapse = "collapse",
        Copy = "copy",
        Core = "core",
        Cpu = "cpu",
        Cylinder = "cylinder",
        Danger = "danger",
        Dashboard = "dashboard",
        Database = "database",
        Delete = "delete",
        Desktop = "desktop",
        Docker = "docker",
        Document = "document",
        Download = "download",
        Edit = "edit",
        Evidence = "evidence",
        ExternalLink = "external_link",
        Eye = "eye",
        EyeOff = "eye_off",
        Face = "face",
        File = "file",
        FileText = "file_text",
        Filter = "filter",
        Flow = "flow",
        Folder = "folder",
        Globe = "globe",
        GlobeGrid = "globe_grid",
        Gpu = "gpu",
        GridRows = "grid_rows",
        Grip = "grip",
        Help = "help",
        Home = "home",
        HomeSimple = "home_simple",
        Host = "host",
        IfaceLan = "iface_lan",
        IfaceLoop = "iface_loop",
        IfaceTb = "iface_tb",
        IfaceVirt = "iface_virt",
        IfaceVpn = "iface_vpn",
        IfaceWifi = "iface_wifi",
        Image = "image",
        Info = "info",
        Key = "key",
        LineChart = "line_chart",
        List = "list",
        Lock = "lock",
        Locked = "locked",
        Logout = "logout",
        Management = "management",
        Max = "max",
        Meeting = "meeting",
        Message = "message",
        Mic = "mic",
        Min = "min",
        Model = "model",
        Models = "models",
        More = "more",
        Network = "network",
        NetworkSvg = "network_svg",
        Os = "os",
        Paperclip = "paperclip",
        Pause = "pause",
        Person = "person",
        Pi = "pi",
        Pin = "pin",
        Play = "play",
        Plus = "plus",
        Power = "power",
        Profiles = "profiles",
        Prompt = "prompt",
        Puzzle = "puzzle",
        Question = "question",
        RagDb = "rag_db",
        Ram = "ram",
        Record = "record",
        RecordDot = "record_dot",
        Refresh = "refresh",
        Registry = "registry",
        Rotate = "rotate",
        Rules = "rules",
        Save = "save",
        Search = "search",
        Send = "send",
        Services = "services",
        Settings = "settings",
        Settings2 = "settings2",
        Share = "share",
        Shield = "shield",
        Sparkle = "sparkle",
        Speaker = "speaker",
        SpeakerAlt = "speaker_alt",
        Star = "star",
        Stop = "stop",
        Success = "success",
        Transform = "transform",
        Trash = "trash",
        Trend = "trend",
        Unlock = "unlock",
        Unlocked = "unlocked",
        User = "user",
        Users = "users",
        Vehicle = "vehicle",
        Video = "video",
        Volume = "volume",
        Warning = "warning",
        WorkflowApp = "workflow_app",
        X = "x",
        Zap = "zap",
        Zones = "zones",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_representative_icons() {
        for n in [
            IconName::Add,
            IconName::ArrowDown,
            IconName::ChevronRight,
            IconName::Cameras,
            IconName::Brain,
            IconName::Eye,
            IconName::Trash,
            IconName::Settings,
        ] {
            let mut buf = Vec::new();
            minicbor::encode(&n, &mut buf).unwrap();
            let d: IconName = minicbor::decode(&buf).unwrap();
            assert_eq!(d, n);
        }
    }

    #[test]
    fn unknown_icon_rejected() {
        let mut buf = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut buf);
        enc.str("not_an_icon").unwrap();
        let res: Result<IconName, _> = minicbor::decode(&buf);
        assert!(res.is_err());
    }

    #[test]
    fn snake_case_wire_form_spot_check() {
        assert_eq!(IconName::ArrowDown.as_str(), "arrow_down");
        assert_eq!(IconName::ChevronRight.as_str(), "chevron_right");
        assert_eq!(IconName::ExternalLink.as_str(), "external_link");
    }
}
