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
    camera::{
        CameraAddInput, CameraAddOutput, CameraAnalysisFlowOut, CameraAnalysisFlowsOut,
        CameraCredentialsRotateInput, CameraCredentialsRotateOut, CameraDiscoverOut,
        CameraGrantInfo, CameraGrantInput, CameraGrantListInput, CameraGrantListOut, CameraGrantOut,
        CameraHealthOut, CameraIdInput, CameraInfoOut, CameraListOut, CameraRemoveOut,
        CameraRevokeInput, CameraSnapshotOut, CameraTestConnectionInput, CameraTestConnectionOut,
        CameraUpdateInput, DiscoveredCameraOut, LocalCameraDeviceOut, LocalCameraDevicesOut,
    },
    camera_metadata::{
        MetadataFrameOut, MetadataItemOut, MetadataPollInput, MetadataPollOutput,
        MetadataSubscribeInput, MetadataSubscribeOutput, MetadataUnsubscribeInput,
        MetadataUnsubscribeOutput,
    },
    canonical::{validate_canonical, CanonicalError, CanonicalErrorKind},
    control::{
        AuthContext, Backpressure, BackpressureSeverity, Capability, CapabilityRejection,
        CapabilityRevoked, CborMap, ControlPayload, ControlTag, CreditBudget, CreditGrant,
        GrantRationale, Heartbeat, ProtocolHello, ProtocolReject, ProtocolWelcome, QueueDepth,
        RateLimitScope, RateLimitUpdate, RejectReason, Resume, ResumeMode, ResumeStatus,
        ServerLimits, SessionEnd, SessionEndCode,
    },
    doc_parse::{DocBlock, DocParseInput, DocParseOutput},
    document::{
        DocumentDeleteInput, DocumentDeleteOutput, DocumentGetInput, DocumentGetMeta,
        DocumentListInput, DocumentListOutput, DocumentMeta, DocumentPutInput, DocumentPutOutput,
    },
    envelope::{Channel, Envelope, Flags, Priority, ProtocolVersion, PROTOCOL_VERSION},
    flow::{FlowCancelOutput, FlowInvocationIdInput, FlowInvocationOutput, FlowInvokeInput},
    gate::{GateCheckInput, GateCheckOutput, GateSignerOut},
    // NOTE: graph's `GraphNode` is intentionally NOT re-exported here — it would
    // collide with `ui::inline::GraphNode`. Reach it via `protocol::graph::GraphNode`.
    graph::{
        GraphDeleteInput, GraphDeleteOutput, GraphDeleteTarget, GraphDirection, GraphNeighbor,
        GraphNeighborsInput, GraphNeighborsOutput, GraphPagerankInput, GraphPagerankOutput,
        GraphPprInput, GraphPprOutput, GraphProp,
        GraphRankedNode, GraphSeed, GraphUpsertEdgeInput, GraphUpsertEdgeOutput,
        GraphUpsertNodeInput, GraphUpsertNodeOutput, Provenance,
    },
    ids::{ClientActionId, DeviceId, Hash32, NodeId, SessionId, TraceId},
    recording::{
        FrameUrlInput, GetStreamOut, PurgeOut, RecordingGetUrlInput, RecordingRefInput,
        RecordingSaveSegmentInput, RecordingSaveSnapshotInput, RecordingStatsInput,
        SaveRecordingOut, StatsOut, StatsPerCamera, StatsTotals, UrlOut,
    },
    robot::{RobotActionWire, RobotControlResponseWire, RobotDispatchInput},
    services::{
        GpuOut, NodeResourcesInput, NodeResourcesOut, ServiceInfoOut, ServiceListInput,
        ServiceListOutput,
    },
    state::{
        StateEntryMeta, StateListOutput, StateSetInput, STATE_TIER_DURABLE, STATE_TIER_EPHEMERAL,
    },
    stream::{
        StreamAccepted, StreamCancel, StreamChunk, StreamEnd, StreamError, StreamKind,
        StreamOpen, StreamPayload, StreamProgress, StreamRejected, StreamTag,
    },
    streaming::{
        StreamCloseInput, StreamCloseOutput, StreamNextInput, StreamNextOutput,
        StreamSubscribeFilter, StreamSubscribeInput, StreamSubscribeOutput,
    },
    webrtc::{
        WebRtcCloseInput, WebRtcConnectInput, WebRtcConnectOutput, WebRtcDrainInput,
        WebRtcDrainOutput, WebRtcDrainOutputRef, WebRtcMessage, WebRtcRegisterCameraInput,
        WebRtcRegisterCameraOutput,
        WebRtcSendInput, WebRtcSetAnswerInput, WebRtcStateInput, WebRtcStateOutput,
        WebRtcStatusOutput,
    },
    ui::{
        a11y::{Accessibility, EventKind, Visibility},
        action::{Action, ActionAck, ActionStatus, FieldError, FormFieldMap, FormFieldValue, ParamEntry},
        actions::{
            ActionBar, Button, ButtonGroup, Fab, FilterChips, IconButton, Link, LinkButton,
            Menu, MenuButton, SegmentedControl, WizardFooter,
        },
        bind::{BindRef, BindSpec, PathSegment, StatePath, MAX_STATE_PATH_SEGMENTS},
        command::Command,
        component::{
            Component, FieldMap, HandlerMap, TestId, TestIdError, TEST_ID_MAX_LEN,
        },
        error_code::ErrorCode,
        event::{Event, Topic, TopicSegment},
        feedback::{
            Alert, Banner, Callout, ConfirmationDialog, Drawer, GateScreen, Hint, LoadingBar,
            Modal, OfflineBanner, Popover, Sheet, Skeleton, Spinner, Toast,
        },
        form::{
            Autocomplete, Checkbox, ColorPicker, Combobox, CurrencyInput, DatePicker,
            DateRangePicker, DateTimePicker, FileInput, Form, FormField, FormGroup, FormSection,
            FormValidator, Input, MentionInput, MultiSelect, NumericInput, Radio, RadioCardGroup,
            RadioGroup, RangeSlider, SearchBox, Select, Slider, SliderRow, TagInput, Textarea,
            TimePicker, Toggle,
        },
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
            InlineBadge, InlineChip, KvItem, LogEvent, MapMarker, MenuItem, NavTab, PermissionDef,
            RadioCardOption, RadioOption, RangePreset, RangePresetRange, RoleDef,
            SegmentOption, SelectGroup, SelectOption, SelectValue, SidebarItem, SliderMark,
            SplitSize, StackSegment, StepDef, TabItem, TableColumn, TableColumnWidth,
            TablePagination, TableSort, TimelineItem, Trend, TrendDirection,
        },
        data::{
            AreaChart, Avatar, AvatarGroup, Badge, BarChart, BulletList, CalendarMonth, Chip,
            CodeBlock, DataDefinitionList, Diff, EmptyCell, Gauge, Heading, Heatmap, Image,
            JsonViewer, KeyValue, LineChart, List, LiveRegionComponent, Markdown, MonoBlock,
            Paragraph, PieChart, ProgressBar, RatingDisplay, RichText, Sparkline, StackedBar,
            Stat, StatCard, Table, Tag, Text, Timeline, Tree, VisuallyHidden,
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
        schema::{
            section as schema_section, ComponentMeta, EnumMeta, FieldMeta, InlineMeta, UnionMeta,
            VariantMeta, ALL_COMPONENTS, ALL_ENUMS, ALL_INLINE_STRUCTS, ALL_TAGGED_UNIONS,
        },
        patch::{PatchOp, PatchOpKind},
        slot::{
            CachePolicy, SlotDecl, SlotDefault, SlotSemantics, SlotVisibility, StateEntry,
        },
        slot_msg::{SlotClear, SlotContent, SlotHide, SlotShow},
        specialized::{
            Audio, Carousel, CodeEditor, FpsCounter, IFrame, ImageGallery, LiveCameraTile,
            MapView, PdfViewer, StepProgress, Stopwatch, Terminal, VideoStream, VirtualizedLog,
        },
        state::{PatchRejectReason, PatchRejected, StatePatch, StateReset, StateSnapshot},
        tokens::{
            AccordionMode, AlertVariant, AreaStacking, AudioControls, AudioVariant, AutocompleteHint,
            AvatarOverlap, AvatarShape, AvatarSize, AvatarStatus, BackgroundToken, BadgeVariant,
            BannerPosition, BarStacking, BreadcrumbSeparator, Breakpoint, BulletListVariant,
            ButtonGroupOrientation, ButtonSize, ButtonVariant, CameraStatus, CardVariant,
            CarouselGestures, CheckboxSize, ChartAxisScale, CodeEditorTheme, ColorPickerVariant,
            DrawerSize, FabPosition, FabSize, FileCapture, FilterChipsMode, FormFieldLayout,
            FormLayout, FpsVariant, GateVariant, IFrameReferrerPolicy, IFrameSandbox, InputMode,
            InputSize, InputType, LinkUnderline, LogLevel, LogVariant, MenuPlacement, ModalSize, PdfZoomMode,
            PopoverPlacement, RadioCardVariant, RadioGroupOrientation, SearchVariant, SegmentSize,
            SkeletonVariant, SliderRowLayout, SpinnerSize, SpinnerVariant, StepProgressVariant,
            StopwatchVariant, TerminalTheme, TileProvider, TimePrecision, ToggleSize, TogglePosition,
            VideoControls,
            ChartLegendAlign, ChartLegendPosition, ChartOrientation, ChartSeriesStyle,
            ChartZoomMode, ChipVariant, ColorToken, ColumnRender, CursorToken, DayOfWeek,
            Density, DiffVariant, DividerOrientation, DividerVariant, DlLayout, DrawerSide,
            EmptyCellVariant, EmptyStateVariant, FileUploadStatus, FlexAlign, FlexDirection,
            FlexJustify, FlexWrap, GaugeVariant, HeatmapLegendPosition, IconSize, ImageFit,
            KvLayout, LinkTarget, LiveRegion, MarkdownBlock, MarkdownFeature, MarkdownMark,
            NavTabsVariant, NavigateTarget, PaginationVariant, PieVariant, ProgressSize,
            ProgressVariant, RadiusToken, RatingPrecision, RatingVariant, ScrollBehavior,
            ScrollOrientation, ShadowToken, SheetDetent, SortDirection, SpacerAxis, Spacing,
            SparklineVariant, SplitOrientation, StatSize, StepStatus, TableSelectMode,
            TableVariant, TabsVariant, TagSize, TextAlign, TextStyle, TextWrap,
            TimelineOrientation, Tone, TreeVariant,
        },
        typed_field::{decode_from_value, encode_to_value},
        ui_payload::{Batch, BatchMember, UiPayload, UiTag, BATCH_MAX_MEMBERS},
        validation::{StateCondition, ValidationRule},
        value_format::{
            BytesBase, DateStyle, DateTimeStyle, DurationStyle, TimeStyle, ValueFormat,
        },
    },
    value::Value,
    vector::{
        VectorDeleteInput, VectorDeleteOutput, VectorHybridSearchInput, VectorSearchHit,
        VectorSearchInput, VectorSearchOutput, VectorUpsertInput, VectorUpsertOutput,
    },
    vector_query::{Field, FieldSpec, FieldType, FieldValue, Filter, Fusion, SparseVector},
};
