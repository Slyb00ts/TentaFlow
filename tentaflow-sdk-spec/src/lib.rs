// =============================================================================
// File: lib.rs — tentaflow-sdk-spec crate root
// Purpose: single source of truth for TentaFlow addon protocol types, UI catalog
// and codegen annotations. Wire format: CBOR Core Deterministic Encoding
// (RFC 8949 §4.2.1+§4.2.2). See docs/ADDON_BINARY_PROTOCOL_v1.md.
//
// Strict canonical-decode policy (§2.2): typed decoders here enforce a defensive
// subset — bstr fixed lengths, protocol_version == 1, ControlTag whitelist,
// per-variant field whitelisting for ResumeStatus / RejectReason / RateLimitScope,
// indefinite-length reject in Value. The full §2.2 wire validator (reject
// NonCanonicalIntegerWidth / NonCanonicalFloatWidth / NonCanonicalKeyOrder /
// DuplicateMapKey / unknown-keys on every derived map) lives in the host
// dispatch path landing in Krok 4 of Faza 6 (see SYNC_LEDGER_PLAN / addon
// rewrite roadmap). Encoders here already produce canonical output.
// =============================================================================

#![forbid(unsafe_code)]
#![deny(unused_must_use)]

pub mod protocol;

pub use protocol::{
    control::{
        AuthContext, Backpressure, BackpressureSeverity, Capability, CapabilityRejection,
        CapabilityRevoked, CborMap, ControlPayload, ControlTag, CreditBudget, CreditGrant,
        GrantRationale, Heartbeat, ProtocolHello, ProtocolReject, ProtocolWelcome, QueueDepth,
        RateLimitScope, RateLimitUpdate, RejectReason, Resume, ResumeMode, ResumeStatus,
        ServerLimits, SessionEnd, SessionEndCode,
    },
    envelope::{Channel, Envelope, Flags, Priority, ProtocolVersion, PROTOCOL_VERSION},
    ids::{ClientActionId, DeviceId, Hash32, NodeId, SessionId, TraceId},
    stream::{
        StreamAccepted, StreamCancel, StreamChunk, StreamEnd, StreamError, StreamKind,
        StreamOpen, StreamPayload, StreamProgress, StreamRejected, StreamTag,
    },
    ui::{
        a11y::{Accessibility, EventKind, Visibility},
        action::{Action, ActionAck, ActionStatus, FieldError, FormFieldMap, FormFieldValue, ParamEntry},
        bind::{BindRef, BindSpec, PathSegment, StatePath, MAX_STATE_PATH_SEGMENTS},
        command::Command,
        component::{
            Component, FieldMap, HandlerMap, TestId, TestIdError, TEST_ID_MAX_LEN,
        },
        error_code::ErrorCode,
        event::{Event, Topic, TopicSegment},
        handler::{
            FailurePolicy, Handler, HandlerValidationError, LocalAction, DEBOUNCE_MAX_MS,
            HANDLER_MAX_RECURSION_DEPTH, HANDLER_MAX_TOTAL_STEPS, SEQUENCE_MAX_ITEMS,
        },
        icon_name::IconName,
        inline::{
            AccordionItem, AlarmItem, AspectRatio, AvatarRef, BorderToken, BreadcrumbItem,
            ChartAxis, ChartLegend, ChartSeries, ChartTooltip, DatePreset, DatePresetResolve,
            DecisionOption, DefItem, DimensionToken, FeatureItem, FileMeta, FilterChipDef,
            Footnote, GaugeThreshold, GraphEdge, GraphNode, GridChild, GridCol, GridTrack,
            HeatmapBucket, HeatmapColumn, HeatmapRow, HeatmapScale, IconRef, InboxItem,
            InlineBadge, InlineChip, KvItem, MapMarker, MenuItem, NavTab, PermissionDef,
            RadioCardOption, RadioOption, RangePreset, RangePresetRange, RoleDef,
            SegmentOption, SelectGroup, SelectOption, SelectValue, SidebarItem, SliderMark,
            SplitSize, StackSegment, StepDef, TabItem, TableColumn, TableColumnWidth,
            TablePagination, TableSort, TimelineItem, Trend, TrendDirection,
        },
        data::{
            AreaChart, Avatar, AvatarGroup, Badge, BarChart, BulletList, Chip, CodeBlock,
            EmptyCell, Gauge, Heading, Heatmap, KeyValue, LineChart, List, MonoBlock,
            Paragraph, PieChart, RichText, Sparkline, StackedBar, Stat, StatCard, Table,
            Tag, Text, Timeline, Tree,
        },
        layout::{
            Accordion, Breadcrumb, Card, Cluster, Collapsible, Divider, Flex, Grid, NavTabs,
            Pagination, ScrollContainer, SectionCard, Sidebar, Spacer, Split, Stack, Tabs,
            Tooltip,
        },
        molecules::{
            AppShell, EmptyState, ErrorBoundary, Header, Inspector, LoginShell, PageHeader,
            SectionHeader, StatGroup, Toolbar, WelcomeHero, WizardShell,
        },
        panel::{
            CloseReason, PanelClose, PanelError, PanelOpen, PanelOpenContext, PanelReady,
            PanelReset, PanelShell, Viewport,
        },
        patch::{PatchOp, PatchOpKind},
        slot::{
            CachePolicy, SlotDecl, SlotDefault, SlotSemantics, SlotVisibility, StateEntry,
        },
        slot_msg::{SlotClear, SlotContent, SlotHide, SlotShow},
        state::{PatchRejectReason, PatchRejected, StatePatch, StateReset, StateSnapshot},
        tokens::{
            AccordionMode, AreaStacking, AvatarOverlap, AvatarShape, AvatarSize, AvatarStatus,
            BackgroundToken, BadgeVariant, BarStacking, BreadcrumbSeparator, Breakpoint,
            BulletListVariant, ButtonVariant, CardVariant, ChartAxisScale,
            ChartLegendAlign, ChartLegendPosition, ChartOrientation, ChartSeriesStyle,
            ChartZoomMode, ChipVariant, ColorToken, ColumnRender, CursorToken, Density,
            DividerOrientation, DividerVariant, DrawerSide, EmptyCellVariant,
            EmptyStateVariant, FileUploadStatus, FlexAlign, FlexDirection, FlexJustify,
            FlexWrap, GaugeVariant, HeatmapLegendPosition, IconSize, KvLayout, LiveRegion,
            MarkdownBlock, MarkdownMark, NavTabsVariant, NavigateTarget, PaginationVariant,
            PieVariant, RadiusToken, ScrollBehavior, ScrollOrientation, ShadowToken,
            SheetDetent, SortDirection, SpacerAxis, Spacing, SparklineVariant,
            SplitOrientation, StatSize, StepStatus, TableSelectMode, TableVariant,
            TabsVariant, TagSize, TextAlign, TextStyle, TextWrap, TimelineOrientation,
            Tone, TreeVariant,
        },
        typed_field::{decode_from_value, encode_to_value},
        ui_payload::{Batch, BatchMember, UiPayload, UiTag, BATCH_MAX_MEMBERS},
        validation::{StateCondition, ValidationRule},
        value_format::{
            BytesBase, DateStyle, DateTimeStyle, DurationStyle, TimeStyle, ValueFormat,
        },
    },
    value::Value,
};
