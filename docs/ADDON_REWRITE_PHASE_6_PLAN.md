# Faza 6 — Plan przepisania addon UI/SDK architektury

> Status: w trakcie realizacji (sesja 2026-05-21).
> Branch roboczy: `flow-engine-stage-3d-v1.5`.
> **Plik lokalny, NIE commitować do repo** (explicit user preference 2026-05-21).
> Twarde reguły: patrz `docs/ADDON_BINARY_PROTOCOL_v1.md` §"Implementation Directives" + memory `feedback_addon_rewrite_directives.md`.

## Twarde reguły (PR odrzucany jeśli naruszone)

1. Production-ready, NOT MVP.
2. Zero stubów (`todo!()`, `unimplemented!()`, fake values, scaffolded `Ok(())` bez logiki).
3. Zero backward compatibility — stare SDK/renderer usuwane w miejscu, bez aliasów / feature flag.
4. Stary kod usuwany razem z nowym w tym samym commicie.
5. Single source of truth = `tentaflow-sdk-spec` Rust crate (auto-generated SDK dla C#/Python).
6. Brak parallel-stack scaffolding (nowy obok starego).
7. Fix dokument przed implementacją — najpierw update protocol/catalog docs, dopiero potem kod.

## Workflow per chunk

1. TaskCreate → in_progress, ogłoszenie userowi
2. Implementacja (zgodna z 7 regułami, usuwając stary kod w miejscu)
3. `cargo build --release` + `cargo test` muszą przejść bez warningów
4. **Codex review** `codex exec --sandbox read-only --skip-git-repo-check --color never "..." </dev/null` (zamknij stdin!)
5. Iteracja aż codex zatwierdzi (lub explicit accept że pozostałości są non-blocking)
6. **User accept** przed commitem (NIGDY nie commituj bez explicit zgody)
7. Commit z jednolinijkowym opisem `[type]: opis` (po polsku, bez AI/Co-Authored-By)
8. TaskUpdate completed → następny chunk

## Plan 8 kroków

| # | Krok | Czas (~dni) | Blokowany przez | Status |
|---|------|-------------|-----------------|--------|
| 1 | `tentaflow-sdk-spec` Rust crate — typed definitions + codegen annotations | 5 | — | ✅ done (577 tests) |
| 2 | `tentaflow-sdk-gen` MVP — Rust→Rust self-test + emit `catalog-manifest/v1.cbor` (canonical CBOR) | 4 | 1 stabilny | pending |
| 3 | Frontend reactive store + slot manager + DOM diff engine + per-component renderers (zastąp `addon-app.js`) | 7 | 1 stabilny | ✅ done (3.7 cutover odblokowany) |
| 4 | Host validator + dispatch (CBOR decode + slot ownership + state revisions + rate limit + audit) | 6 | 1 stabilny | ✅ done (9 chunks, 72 tests) |
| 5 | End-to-end smoke test — minimalny realny addon Rust (1 panel, 1 action, 1 state patch) | 2 | 1+2+3+4 | pending |
| 6 | Generator backends C# + Python | 14 | 2 stabilny | pending |
| 7 | WASI hosting C# (.NET 10) + Python (CPython 3.13) | 14 | 4 stabilny | pending |
| 8 | Przepisanie istniejących addonów: TentaVision (14 paneli) → Eureka → Contacts → Company Lookup | 13 | 1+3+4+5 | pending |

**Razem ~65 dni roboczych** (~2–3 miesiące z paralelizacją 3+4 oraz 6+7).

Po stabilizacji kroku 1 równolegle: 2, 3, 4. Krok 5 łączy. Krok 6+7 niezależnie po 4. Krok 8 wymaga 1+3+4+5.

## Krok 1 — breakdown chunków

| Chunk | Scope | Status | Commit |
|-------|-------|--------|--------|
| 1.1 | Control channel CBOR (Envelope, Channel, Flags, ProtocolVersion, bstr-16 IDs + Hash32, Value, CborMap, full §5 control payloads + ControlPayload tagged union) | ✅ done | `78660264` |
| 1.2 | UI common types (semantic tokens §1.1, ValueFormat §1.3, StatePath/BindRef/BindSpec §1.4, Accessibility/Visibility/EventKind §1.6) | ✅ done | `e2b0507d` |
| 1.3 | Handler + LocalAction + ValidationRule + StateCondition + Component envelope (z FieldMap opaque + HandlerMap canonical sort + TestId validation, Handler::validate egzekwuje §10.3 limits) | ✅ done | `5037611d` |
| 1.4 | UI panel/slot/command (ErrorCode §16, SlotDecl + Slot* enums + StateEntry, PanelOpen/Shell/Ready/Error/Close/Reset, SlotContent/Clear/Show/Hide, Command 17 variants z https+filename validation) | ✅ done | `d62b8b32` |
| 1.5 | UI state + action + event + batch + UiPayload assembly (StateSnapshot/Patch/Reset, PatchRejected + PatchRejectReason, Action + FormFieldValue, ActionAck + ActionStatus + FieldError, Event + Topic, Batch + BatchMember, UiPayload nad wszystkimi 19 UI messages) | ✅ done | `a73aa6c7` |
| 1.6 | Stream channel typed wire (§7) + StreamPayload (8 messages: Open/Accepted/Rejected/Chunk/Progress/End/Cancel/Error, StreamKind enum, StreamTag enum) | ✅ done | `83748459` |
| 1.7a | IconName enum (142 z icons.svg) + simple §1.5 inline structs (IconRef, AvatarRef, InlineBadge, Trend, Footnote, SelectValue, BreadcrumbItem, NavTab, TabItem, MenuItem, SidebarItem z 1-level children, SelectOption+SelectGroup; mutual exclusion action_id/local_action enforced na decode) | ✅ done | `e09bde28` |
| 1.7b | Pozostałe §1.5 inline structs (13 enums w tokens.rs + ~30 structs w inline.rs + 5 tagged unions z manual encode/decode: DimensionToken, AspectRatio, TableColumnWidth, HeatmapScale, DatePresetResolve) | ✅ done | `8a8b234c` |
| 1.8a | §2 Structured Molecules (12 typed: Header/PageHeader/EmptyState/SectionHeader/Toolbar/AppShell/LoginShell/ErrorBoundary/WelcomeHero/StatGroup/WizardShell/Inspector) + typed_field helpers (encode_to_value/decode_from_value) + IntoComponentError + ensure_no_duplicate_keys validator + defaults Header.density/AppShell.sidebar_width/StatGroup.columns | ✅ done | `55e8109d` |
| 1.8b | §3 Layout Primitives (18 typed: Flex/Grid/Stack/Cluster/Split/Card/SectionCard/Divider/Spacer/Sidebar/Tabs/NavTabs/Collapsible/Accordion/Tooltip/Breadcrumb/Pagination/ScrollContainer) + 13 token enums + 4 tagged unions (BorderToken/SplitSize/GridCol/GridTrack) + dup-key detection w nowych unionach | ✅ done | `12aaa0bd` |
| 1.8c1 | §4 Data Display A: 16 typed (Text-Timeline) + 11 enums + Trend.percent f32→f64 migracja | ✅ done | `3d0e016e` |
| 1.8c2 | §4 Data Display B: 12 typed (Table/List/Tree/EmptyCell/Sparkline/LineChart/BarChart/AreaChart/PieChart/StackedBar/Heatmap/Gauge) + 12 enums + AreaChart.opacity/Gauge.min/max/GaugeThreshold.value f32→f64 | ✅ done | `3ff3c9bd` |
| 1.8c3 | §4 Data Display C: 10 typed (ProgressBar/RatingDisplay/Diff/Markdown/DataDefinitionList/JsonViewer/CalendarMonth/Image/VisuallyHidden/LiveRegionComponent) + 10 enums + ProgressBar.max f32→f64 + MarkdownFeature explicit | ✅ done | `dd9128c7` |
| 1.8d1 | §5 Form (29 typed: Input/Textarea/Select/MultiSelect/Combobox/Autocomplete/SearchBox/TagInput/MentionInput/Toggle/Checkbox/Radio/RadioGroup/RadioCardGroup/Slider/RangeSlider/SliderRow/NumericInput/CurrencyInput/DatePicker/DateRangePicker/TimePicker/DateTimePicker/FileInput/ColorPicker/FormField/FormGroup/FormSection/Form) + 16 enums + FormValidator tagged union + Combobox.searchable always-true egzekwowane | ✅ done | `7ac45fc9` |
| 1.8d2 | §6 Action (~12 typed: Button/IconButton/ButtonGroup/LinkButton/Link/MenuButton/Menu/ActionBar/SegmentedControl/FilterChips/WizardFooter/Fab) | pending | — |
| 1.8d3 | §7 Feedback (~15 typed: Alert/Banner/Callout/Toast/Hint/Skeleton/...) | pending | — |
| 1.8e | §8 Specialized (VideoStream/Canvas/MapView/PermissionMatrix/...) + codegen annotations | pending | — |

## Decyzje architektoniczne (zamrożone w trakcie kroku 1)

- **Wszystkie struktury CBOR mają integer keys**, konkretne assignmenty żyją w `tentaflow-sdk-spec/src/protocol/*.rs`. Wyjątek: free-form `map<tstr, Value>` (CborMap) → sortowane bytewise canonical.
- **Opcjonalne pola "or null"** — encodowane jako klucz nieobecny w CBOR mapie gdy `None`. CBOR `null` (0xf6) NIE jest emitowany; decodery rzucają na explicit null.
- **Strict §2.2 canonical decode w derived `#[cbor(map)]`** (NonCanonicalIntegerWidth, NonCanonicalFloatWidth, DuplicateMapKey, NonCanonicalKeyOrder, unknown-keys, indefinite-length w derived maps) — **odroczony do Kroku 4** host validator. Encodery z `tentaflow-sdk-spec` już produkują canonical output; manualne decodery odrzucają obce per-variant fields, indefinite-length items, unknown enum/tag values, wrong-length bstr IDs.
- **Handler tree limits (§10.3)** — depth ≤ 8, total steps ≤ 16, Sequence ≤ 8 items + sticky no-nesting (przez Confirm/Debounce/Conditional then/else), Debounce ms ∈ (0, 5000]. Cykle strukturalnie niemożliwe (`Box`-tree).
- **Command security checks**: `NavigateExternal.url` MUST be `https://` (encode + decode). `Download.filename` MUST match `[a-zA-Z0-9._-]+` length 1..=128 (encode + decode).
- **Component.test_id**: opcjonalny, grammar `[a-z0-9_-]+`, length ≤ 64.
- **StateCondition** intentionally non-recursive (no And/Or/Not) — kept flat dla O(1) renderer evaluation.

## Stan na 2026-05-21

- Tests: 264/264 passing w `tentaflow-sdk-spec`
- Build: clean, zero warnings
- Branch: `flow-engine-stage-3d-v1.5`, 15 commitów Fazy 6 zaaplikowanych (12 chunków + refactor)

## Pending cleanup tasks

- Systematyczne backfill duplicate-key detection w tagged-union decoderach z chunków 1.1-1.7 (ValueFormat/ResumeStatus/RejectReason/RateLimitScope/ActionStatus/PathSegment/BindRef/BindSpec/ValidationRule/StateCondition/PatchOpKind/LocalAction/Handler/FailurePolicy/IconRef/AvatarRef/SelectValue/DimensionToken/AspectRatio/TableColumnWidth/HeatmapScale/DatePresetResolve/Topic). Nowe decodery od 1.8b mają detection; stare nie. Codex zaakceptował jako separate task.
- SplitSize::decode key-order dependency (kind-before-value required) — refactor na 2-pass scan.

## Następny krok

Chunk 1.8c — §4 Data Display (~30 typed: Text/Heading/Paragraph/RichText/MonoBlock/CodeBlock/KeyValue/StatCard/Stat/Badge/Chip/Tag/Avatar/AvatarGroup/BulletList/Timeline/Table/List/Tree/EmptyCell/Sparkline/LineChart/BarChart/AreaChart/PieChart/StackedBar/Heatmap/Gauge/ProgressBar/RatingDisplay/Diff/Markdown/DataDefinitionList/JsonViewer/CalendarMonth/VisuallyHidden/LiveRegion).
