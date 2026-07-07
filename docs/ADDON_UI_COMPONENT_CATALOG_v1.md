# Addon UI Component Catalog v1

> Status: **v1.0 — zaakceptowany przez codex review (7 rund)** (2026-05-21)
> Owner: addon platform team
> Companion: `ADDON_BINARY_PROTOCOL_v1.md` (wire protocol)
> `catalog_version: 1`

## ⚠ Implementation Directives — MUST be followed

Pełne dyrektywy w `ADDON_BINARY_PROTOCOL_v1.md` §"Implementation Directives". Podsumowanie:

1. **Production-ready, NOT MVP.** Każdy commit ready do produkcji.
2. **Zero stubów** — żadnych `todo!()`, `unimplemented!()`, mock responses, fake values, empty bodies, scaffolding.
3. **Zero backward compatibility.** Stare komponenty (`tf-button`, `tf-input`, etc. + obecny `addon-app.js` renderer + `tentaflow-ui-schema` w obecnej formie) są **usuwane** podczas implementacji nowych. Brak aliasów, brak feature flags.
4. **Usuwaj stary kod razem z nowym** — każdy migration commit usuwa stary kod w tym samym commicie.
5. **Single source of truth = `tentaflow-sdk-spec` + canonical manifest.** Brak duplikatów.
6. **Brak parallel-stack scaffolding.** Stary i nowy nie istnieją obok siebie.
7. **Fix dokument przed implementacją** jeśli wymaganie się zmienia.

PRs które naruszają są odrzucane. Twarde reguły, nie sugestie.

---

## 0. Cel dokumentu

Pełna deklaracja wszystkich komponentów UI dostępnych dla addonów TentaFlow: tag space, schemas pól, allowed handlers, accessibility wymagania, walidacja. Każdy komponent ma stabilny **u16 tag** (część ABI wire format). Schema pól jest **deterministycznie sortowana** (integer keys) zgodnie z CBOR Core Deterministic Encoding (patrz protokół §2).

Komponenty są pogrupowane w **8 kategorii** po high-byte taga:

| Range          | Category               | Count   |
|----------------|------------------------|---------|
| 0x0000–0x00FF  | Structured molecules   | 12      |
| 0x0100–0x01FF  | Layout primitives      | 18      |
| 0x0200–0x02FF  | Data display           | 34      |
| 0x0300–0x03FF  | Form                   | 29      |
| 0x0400–0x04FF  | Action                 | 12      |
| 0x0500–0x05FF  | Feedback               | 16      |
| 0x0600–0x06FF  | Specialized            | 17      |
| 0x0700–0x07FF  | Domain-specific        | 10      |
| **Total**      | **v1.0**               | **~151** |

> Autoritative count: `tentaflow-sdk-gen --emit catalog-stats` z `tentaflow-sdk-spec`. Liczby w tabeli są informacyjne, dokładność ±3.

## 1. Common type definitions (referenced by components)

### 1.1 Semantic tokens

```
Tone (enum, tstr):
  "neutral" | "primary" | "success" | "warning" | "critical" | "info" | "muted"

ButtonVariant (enum, tstr):
  "primary" | "secondary" | "tertiary" | "ghost" | "destructive" | "link"

BadgeVariant (enum, tstr):
  "solid" | "soft" | "outline" | "pulse" | "dot"

ChipVariant (enum, tstr):
  "solid" | "soft" | "outline" | "removable" | "selectable" | "toggle"

Density (enum, tstr):
  "compact" | "default" | "comfortable"

Spacing (enum, tstr):                          // mapuje 0/2/4/8/12/16/24/32 px
  "zero" | "xxs" | "xs" | "sm" | "md" | "lg" | "xl" | "xxl"

TextStyle (enum, tstr):
  "display" | "title" | "h1" | "h2" | "h3" | "h4"
  | "body_lg" | "body" | "body_strong" | "caption" | "overline"
  | "code" | "mono" | "quote"

TextAlign (enum, tstr):
  "start" | "center" | "end" | "justify"

TextWrap (enum, tstr):
  "wrap" | "nowrap" | "balance" | "pretty"

RadiusToken (enum, tstr):
  "none" | "xs" | "sm" | "md" | "lg" | "xl" | "pill" | "circle"

ShadowToken (enum, tstr):
  "none" | "subtle" | "medium" | "elevated" | "floating"

BorderToken (discriminated union, always CBOR map z `kind`):
  - { kind: "none" } | { kind: "hairline" } | { kind: "thin" } | { kind: "strong" }
  - { kind: "accent", tone: Tone }

Breakpoint (enum, tstr):                       // 640 / 768 / 1024 / 1280 / 1536 / 1920 px
  "xs" | "sm" | "md" | "lg" | "xl" | "xxl"

IconSize (enum, tstr):                         // 12 / 16 / 20 / 24 / 32 px
  "xs" | "sm" | "md" | "lg" | "xl"

ScrollBehavior (enum, tstr):
  "auto" | "smooth" | "instant"

DrawerSide (enum, tstr):
  "left" | "right" | "top" | "bottom"

NavigateTarget (enum, tstr):
  "new_tab" | "same_tab" | "system_browser"

LiveRegion (enum, tstr):
  "off" | "polite" | "assertive"

CursorToken (enum, tstr):
  "default" | "pointer" | "text" | "move" | "grab" | "grabbing"
  | "not_allowed" | "crosshair" | "col_resize" | "row_resize"
```

### 1.2 IconName (142 named SVG sprites)

Enum tstr, wire form **snake_case** (np. `"arrow_down"`). Renderer mapuje do kebab-prefixed SVG symbol id (`icon-arrow-down`) w `tentaflow-core/www/img/icons.svg`. **Lista autorytatywna** żyje w `tentaflow-sdk-spec/src/protocol/ui/icon_name.rs` (142 wariantów); `icons.svg` MUSI być w sync.

Reprezentatywne (wszystkie obecne w enum):

```
"add" | "alert" | "alarms" | "apps" | "arrow_down" | "arrow_left" | "arrow_right" | "arrow_up"
"audit" | "bar_chart" | "bell" | "bolt" | "brain" | "cameras" | "cancel" | "chart_line"
"chat" | "check" | "chevron_down" | "chevron_left" | "chevron_right" | "chevron_up"
"close" | "code" | "copy" | "cpu" | "danger" | "dashboard" | "database" | "delete"
"document" | "download" | "edit" | "external_link" | "eye" | "eye_off" | "file" | "file_text"
"filter" | "folder" | "globe" | "help" | "home" | "image" | "info" | "key"
"line_chart" | "list" | "lock" | "logout" | "menu" | "mic" | "more" | "network"
"paperclip" | "pause" | "person" | "pin" | "play" | "plus" | "power" | "puzzle"
"question" | "refresh" | "rules" | "save" | "search" | "send" | "settings" | "share"
"shield" | "sparkle" | "speaker" | "star" | "stop" | "success" | "trash" | "trend"
"unlock" | "user" | "users" | "video" | "volume" | "warning" | "x" | "zap"
// pełna lista 142 nazw w tentaflow-sdk-spec/src/protocol/ui/icon_name.rs
```

Unknown icon name → `Error{InvalidIcon}` w validatorze (Krok 4).

### 1.3 ValueFormat (localized display formatting)

```
ValueFormat (discriminated union):
  - { kind: "number", decimals: u8, thousands_sep: bool }
  - { kind: "currency", code: tstr }                                // ISO 4217: "EUR", "PLN", "USD"
  - { kind: "percent", decimals: u8 }
  - { kind: "bytes", base: BytesBase }                              // BytesBase: "1000" | "1024"
  - { kind: "duration", style: DurationStyle }                      // "short" | "long" | "stopwatch"
  - { kind: "date", style: DateStyle }                              // "short" | "medium" | "long" | "full"
  - { kind: "time", style: TimeStyle }                              // "short" | "medium" | "long"
  - { kind: "datetime", style: DateTimeStyle }
  - { kind: "relative" }                                            // "2 minutes ago"
  - { kind: "plain" }                                               // no format
```

### 1.4 StatePath + BindSpec

```
StatePath:
  segments: array<PathSegment>                                      // max 32 segments

PathSegment (enum):
  - { kind: "key", value: tstr }
  - { kind: "index", value: u32 }

BindRef (referencja do state path lub literal):
  - { kind: "literal", value: Value }
  - { kind: "bound", path: StatePath }

BindSpec (deklaracja reactivnego bindingu na komponencie):
  - { kind: "text", path: StatePath, format: ValueFormat or null }
  - { kind: "attr", name: tstr, path: StatePath }
  - { kind: "class_toggle", class_name: tstr, path: StatePath, negate: bool }
  - { kind: "show", path: StatePath, negate: bool }
  - { kind: "list", path: StatePath, item_template_id: tstr, key_field: tstr or null }
  - { kind: "two_way", path: StatePath }                            // form fields only
```

### 1.5 Cross-referenced types

**Optional-field convention** (powtórzenie z protokołu §4): pola oznaczone `or null` w schemach §1.5 są encodowane jako **klucz nieobecny** w CBOR mapie gdy wartość = None. CBOR `null` (0xf6) NIE jest emitowany; decodery odrzucają explicit null. Dla pól `action_id` / `local_action` w `BreadcrumbItem` i `SidebarItem`: wzajemnie wykluczające się — dokładnie jedno (lub żadne) może być obecne; obecność obu naraz → reject na decode.

Następujące types są używane jako pola w wielu komponentach. Dwie kategorie:

**Inline struct types** (proste CBOR maps, NIE Component instances): `IconRef`, `AvatarRef`, `Badge` (jako inline), `Trend`, `Footnote`, `MenuItem`, `SidebarItem`, `SelectOption`, `RadioOption`, `RadioCardOption`, `BreadcrumbItem`, `NavTab`, `TabItem`, `GridChild`, `KvItem`, `StepDef`, `FeatureItem`, `TimelineItem`, `AccordionItem`, `AlarmItem`, `InboxItem`, `DecisionOption`, `PermissionDef`, `RoleDef`, `MapMarker`, `GraphNode`, `GraphEdge`, `TableColumn`, `SegmentOption`, `FilterChipDef`, `HeatmapRow`, `HeatmapColumn`, `GaugeThreshold`, `StackSegment`, `DefItem`, `DatePreset`, `RangePreset`, `FileMeta`, `ValidationRule`, `SliderMark`, oraz wszystkie typy `Chart*`.

**Component-instance refs** (musi być pełen `Component` z `tag`/`id`/`fields`/handlers, używane jako `ComponentRef<X>` w polach katalogu): `Button` (tag 0x0401), `IconButton` (0x0402), `Avatar` (0x020D), `Input` (0x0301), `Select` (0x0303), `SearchBox` (0x0307), `SegmentedControl` (0x0409), `EmptyState` (0x0003), oraz dowolny inny tagged komponent z tego katalogu.

**Dual-form components (Chip + Badge):** mają **dwie formy** w zależności od miejsca użycia:
- **Inline form** (`InlineChip`, `InlineBadge`): prosta struct bez tag/id/handlers, używana w polach takich jak `Header.meta_chips: array<InlineChip>` lub `MenuItem.badge: InlineBadge or null`. Wbudowane formy są przedstawione w sekcji "Uproszczone inline forms" poniżej.
- **Component form** (`ComponentRef<Chip>` z tag 0x020B, `ComponentRef<Badge>` z tag 0x020A): pełen Component z handlerami, używany jako child w layoutach (np. w `Cluster.children`).

Pole katalogu MUSI explicit wybrać jedną z tych form. Validator strict porównuje — wymieszanie odrzucane.

**Uproszczone inline forms (dozwolone w polach inline struct types):**

```
Badge (uproszczona inline form, gdy używana w polu MenuItem.badge / NavTab.badge / etc.):
  variant: BadgeVariant
  tone: Tone
  label: BindRef<tstr> or null
  count: BindRef<u32> or null
  icon: IconRef or null
  pulse: bool
  # NIE ma tag/id/handlers — jest pure inline strukturalny

Chip (uproszczona inline form, gdy używana w polu Header.meta_chips itd.):
  variant: ChipVariant
  tone: Tone
  label: BindRef<tstr>
  icon: IconRef or null
  avatar: AvatarRef or null
  selected: BindRef<bool> or null
  removable: bool
  # NIE ma tag/id/handlers
```

Gdy addon chce **interaktywny Chip/Badge** (z handler na click), MUSI użyć pełnego Component z tag 0x020B/0x020A jako child layoutu (np. w `Cluster.children`), nie inline w polu jak `meta_chips`.

**Inline struct schemas (full definitions):**

```
IconRef (discriminated union):
  - { kind: "named", name: IconName, size: IconSize or null, tone: Tone or null }
  - { kind: "asset", ref: tstr, size_px: u16 or null, alt: tstr or null }   // ref = signed_url_ref

AvatarRef (discriminated union):
  - { kind: "image", ref: tstr }                                            // signed_url_ref
  - { kind: "initials", initials: tstr }                                    // 1-3 chars
  - { kind: "icon", icon: IconRef }

AvatarSource = AvatarRef                                                    // alias

ColorToken (enum, tstr):
  "background_default" | "background_subtle" | "background_muted"
  | "surface_default" | "surface_raised" | "surface_overlay"
  | "border_default" | "border_strong" | "border_subtle"
  | "text_default" | "text_muted" | "text_inverse"
  | "accent_primary" | "accent_secondary"
  | "tone_neutral" | "tone_success" | "tone_warning" | "tone_critical" | "tone_info"

BackgroundToken (enum, tstr):
  "none" | "subtle" | "muted" | "accent" | "inverse"

DimensionToken (discriminated union, always CBOR map z `kind` keyem):
  - { kind: "auto" } | { kind: "full" } | { kind: "fit_content" }
  - { kind: "px", value: u32 }
  - { kind: "vh", value: u8 }                                               // viewport height %
  - { kind: "vw", value: u8 }                                               // viewport width %
  - { kind: "fr", value: u8 }                                               // grid fractions
  - { kind: "percent", value: u8 }
  - { kind: "spacing", value: Spacing }

AspectRatio (discriminated union, always CBOR map z `kind` keyem):
  - { kind: "1:1" } | { kind: "16:9" } | { kind: "4:3" } | { kind: "21:9" }
  - { kind: "3:2" } | { kind: "2:1" } | { kind: "9:16" } | { kind: "3:4" }
  - { kind: "custom", ratio: f32 }                                          // width/height

BorderColor (enum, tstr):                                                   // semantic → theme CSS var
  "default" | "hover" | "accent" | "success" | "warning" | "danger" | "transparent"

BorderLineStyle (enum, tstr):
  "solid" | "dashed" | "none"

Overflow (enum, tstr):
  "visible" | "hidden" | "auto" | "scroll"

SpaceValue (discriminated union, always CBOR map z `kind`):                 // margins/paddings BoxStyle
  - { kind: "token", value: Spacing }
  - { kind: "px",    value: u16 }

RadiusValue (discriminated union, always CBOR map z `kind`):                // narożniki BoxStyle
  - { kind: "token", value: RadiusToken }
  - { kind: "px",    value: u16 }

BorderSide:                                                                 // jedna krawędź borderu
  0: width_px        u8
  1: color           BorderColor
  2: style           BorderLineStyle                                        // "none" wyłącza krawędź

EdgeValues:                                                                 // per-krawędź; brak klucza = default kontenera
  0: top             SpaceValue or null
  1: right           SpaceValue or null
  2: bottom          SpaceValue or null
  3: left            SpaceValue or null
  // Skróty all/x/y rozwiązują buildery SDK — wire niesie wyłącznie krawędzie.

BorderEdges:
  0: top             BorderSide or null
  1: right           BorderSide or null
  2: bottom          BorderSide or null
  3: left            BorderSide or null

CornerValues:
  0: top_left        RadiusValue or null
  1: top_right       RadiusValue or null
  2: bottom_right    RadiusValue or null
  3: bottom_left     RadiusValue or null

BoxStyle:                                                                   // wspólny styling kontenerów (Flex/Grid/Stack/Box/Card/SectionCard `style`)
  0: margin          EdgeValues or null
  1: padding         EdgeValues or null
  2: border          BorderEdges or null
  3: background      BackgroundToken or null
  4: radius          CornerValues or null
  5: width           DimensionToken or null
  6: height          DimensionToken or null
  7: min_width       DimensionToken or null
  8: min_height      DimensionToken or null
  9: max_width       DimensionToken or null
  10: max_height     DimensionToken or null
  11: overflow_x     Overflow or null
  12: overflow_y     Overflow or null
  // Renderer aplikuje BoxStyle PO tokenowych polach kontenera (inline style
  // nadpisuje klasy tokenowe). Px → inline style; tokeny → var(--tf-*).

Trend:
  direction: TrendDirection                                                 // "up" | "down" | "flat"
  percent: f64                                                              // -100..+inf (finite; Value-roundtrip path requires f64)
  label: BindRef<tstr> or null
  tone: Tone or null                                                        // explicit override; default mapped from direction

Footnote:
  tone: Tone                                                                // default "muted"
  icon: IconRef or null
  content: BindRef<tstr>

ChartSeries:
  id: tstr
  name: BindRef<tstr>
  data_path: StatePath                                                      // array of points: { x: Value, y: f64 }
  tone: Tone or null
  style: ChartSeriesStyle                                                   // "solid" | "dashed" | "dotted"
  show_in_legend: bool

ChartAxis:
  label: BindRef<tstr> or null
  format: ValueFormat or null
  min: f64 or null                                                          // null = auto
  max: f64 or null
  ticks: u8 or null                                                         // suggested tick count
  scale: ChartAxisScale                                                     // "linear" | "log" | "time" | "category"

ChartLegend:
  position: ChartLegendPosition                                             // "top" | "bottom" | "left" | "right" | "none"
  alignment: ChartLegendAlign                                               // "start" | "center" | "end"

ChartTooltip:
  enabled: bool
  format: ValueFormat or null

BreadcrumbItem:
  label: BindRef<tstr>
  icon: IconRef or null
  action_id: tstr or null                                                   // jeśli klikabilny — backend Action
  local_action: LocalAction or null                                         // alternatywa do action_id (jeden z dwóch)
  is_current: bool                                                          // last item, not clickable

NavTab:
  id: tstr
  label: BindRef<tstr>
  icon: IconRef or null
  badge: Badge or null                                                      // notification count
  panel_id: tstr or null                                                    // jeśli przełącza panel (NavTabs)
  locked: bool                                                              // requires unlock action

TabItem:                                                                    // dla Tabs (in-panel content swap)
  id: tstr
  label: BindRef<tstr>
  icon: IconRef or null
  badge: Badge or null
  locked: bool
  content_template_id: tstr or null                                         // alternatywa do content_slot

MenuItem:
  id: tstr
  label: BindRef<tstr>
  icon: IconRef or null
  badge: Badge or null
  shortcut: tstr or null                                                    // "Ctrl+S" display only
  danger: bool                                                              // visual emphasis (critical color)
  disabled: BindRef<bool> or null
  divider_after: bool                                                       // visual separator after

SidebarItem:
  id: tstr
  icon: IconRef or null
  label: BindRef<tstr>
  badge: Badge or null
  active_path: StatePath or null                                            // bind path → bool active
  action_id: tstr or null                                                   // backend Action
  local_action: LocalAction or null
  children: array<SidebarItem> or null                                      // nested items (1 level only)

SelectOption:
  value: SelectValue                                                        // SelectValue = tstr | u32 | i32 | bool
  label: BindRef<tstr>
  icon: IconRef or null
  disabled: bool
  group_id: tstr or null                                                    // reference do SelectGroup
  description: BindRef<tstr> or null                                        // sub-label

SelectGroup:
  id: tstr
  label: BindRef<tstr>

RadioOption:
  value: SelectValue
  label: BindRef<tstr>
  hint: BindRef<tstr> or null
  disabled: bool

RadioCardOption:
  value: SelectValue
  icon: IconRef
  title: BindRef<tstr>
  description: BindRef<tstr> or null
  badge: Badge or null
  disabled: bool

SliderMark:
  value: f64
  label: BindRef<tstr> or null

GridChild:
  component: Component                                                      // pełen Component (z tag/id)
  col_span: u8                                                              // default 1
  row_span: u8                                                              // default 1
  col_start: u8 or null                                                     // explicit positioning
  row_start: u8 or null
  align_self: FlexAlign or null
  justify_self: FlexJustify or null

KvItem:
  label: BindRef<tstr>
  value: BindRef<Value>
  hint: BindRef<tstr> or null
  icon: IconRef or null
  action_id: tstr or null                                                   // trailing action button
  format: ValueFormat or null

StepDef:
  id: tstr
  label: BindRef<tstr>
  optional: bool
  status: BindRef<StepStatus> or null                                       // "pending" | "current" | "complete" | "error" | "skipped"
  description: BindRef<tstr> or null

FeatureItem:
  icon: IconRef
  title: BindRef<tstr>
  description: BindRef<tstr> or null

TimelineItem:
  id: tstr
  ts_ms: i64
  title: BindRef<tstr>
  description: BindRef<tstr> or null
  icon: IconRef or null
  tone: Tone or null
  action_id: tstr or null

AccordionItem:
  id: tstr
  header: Component                                                         // typically Heading or SectionHeader
  body: array<Component>
  default_expanded: bool

AlarmItem:
  id: tstr
  ts_ms: i64
  tone: Tone
  title: BindRef<tstr>
  description: BindRef<tstr> or null
  icon: IconRef or null
  action_id: tstr or null
  acknowledged: bool

InboxItem:
  id: tstr
  ts_ms: i64
  read: bool
  title: BindRef<tstr>
  preview: BindRef<tstr> or null
  avatar: AvatarRef or null
  badge: Badge or null
  action_id: tstr

DecisionOption:
  id: tstr
  icon: IconRef
  title: BindRef<tstr>
  description: BindRef<tstr> or null
  tone: Tone or null
  disabled: bool

PermissionDef:
  id: tstr
  label: BindRef<tstr>
  description: BindRef<tstr> or null
  category: tstr or null

RoleDef:
  id: tstr
  label: BindRef<tstr>
  color: Tone or null                                                       // visual accent
  description: BindRef<tstr> or null

MapMarker:
  id: tstr
  lat: f64
  lng: f64
  icon: IconRef or null
  label: BindRef<tstr> or null
  tone: Tone or null
  popup_content: BindRef<tstr> or null

GraphNode:                                                                  // dla RelationGraph
  id: tstr
  label: BindRef<tstr>
  node_type: tstr                                                           // for styling: "person" | "company" | "event" ...
  icon: IconRef or null
  tone: Tone or null

GraphEdge:
  id: tstr
  source_id: tstr
  target_id: tstr
  label: BindRef<tstr> or null
  weight: f32 or null                                                       // affects rendering thickness
  tone: Tone or null

TableColumn:
  id: tstr
  header: BindRef<tstr>
  field_path: array<PathSegment>                                            // relative do row
  width: TableColumnWidth
  render: ColumnRender
  format: ValueFormat or null
  align: TextAlign or null
  sortable: bool
  hidden_by_default: bool
  sticky_left: bool                                                         // pinned-left

TableColumnWidth (discriminated union, always CBOR map z `kind`):
  - { kind: "auto" } | { kind: "min_content" } | { kind: "max_content" }
  - { kind: "px", value: u32 }
  - { kind: "fr", value: u8 }

ColumnRender (enum, tstr):
  "text" | "number" | "currency" | "percent" | "bytes"
  | "date" | "time" | "datetime" | "relative"
  | "badge" | "chip" | "tag" | "avatar" | "avatar_group"
  | "icon" | "stat" | "trend" | "progress" | "rating"
  | "actions" | "checkbox" | "boolean"
  | "custom_template"                                                       // refers to template_id

TablePagination:
  page_size: u32
  current_page_path: StatePath                                              // bound u32
  show_size_picker: bool

TableSort:
  column_id: tstr
  direction: SortDirection                                                  // "asc" | "desc"

SegmentOption:
  value: SelectValue
  label: BindRef<tstr> or null
  icon: IconRef or null
  badge: Badge or null

FilterChipDef:
  id: tstr
  label: BindRef<tstr>
  icon: IconRef or null
  badge: Badge or null
  count_path: StatePath or null                                             // dynamic count

HeatmapRow:
  id: tstr
  label: BindRef<tstr>

HeatmapColumn:
  id: tstr
  label: BindRef<tstr>

HeatmapScale (discriminated union):
  - { kind: "linear", min: f64, max: f64, color_from: Tone, color_to: Tone }
  - { kind: "logarithmic", min: f64, max: f64, base: f64 }
  - { kind: "categorical", buckets: array<HeatmapBucket> }

HeatmapBucket:
  threshold: f64                                                            // value <= threshold belongs to this bucket
  tone: Tone
  label: BindRef<tstr> or null

GaugeThreshold:
  value: f64                                                                // straight numeric field — f64 dla Value-roundtrip compatibility
  tone: Tone
  label: BindRef<tstr> or null

StackSegment:
  id: tstr
  value: BindRef<f64>
  label: BindRef<tstr> or null
  tone: Tone

DefItem:
  term: BindRef<tstr>
  definition: BindRef<tstr>

DatePreset:
  id: tstr
  label: BindRef<tstr>
  resolve: DatePresetResolve                                                // discriminated union, always CBOR map z `kind`: {kind:"today"} | {kind:"yesterday"} | {kind:"last_7_days"} | {kind:"last_30_days"} | {kind:"this_month"} | {kind:"last_month"} | {kind:"custom", offset_days: i32}

RangePreset:
  id: tstr
  label: BindRef<tstr>
  range: { from_offset_days: i32, to_offset_days: i32 }

FileMeta:                                                                   // FileInput state shape
  id: tstr                                                                  // client-generated UUID
  name: tstr
  size_bytes: u64
  mime: tstr
  ts_ms: i64
  upload_progress: f32                                                      // 0..1
  status: FileUploadStatus                                                  // "queued" | "uploading" | "complete" | "error"
  signed_url_ref: tstr or null                                              // populated after upload complete
  error_message: tstr or null

SheetDetent (enum, tstr):
  "small" | "medium" | "large" | "full"

ValidationRule (discriminated union):
  - { kind: "required" }
  - { kind: "min_length", value: u16 }
  - { kind: "max_length", value: u16 }
  - { kind: "min", value: f64 }                                             // dla number
  - { kind: "max", value: f64 }
  - { kind: "pattern", regex: tstr }                                        // ECMAScript regex
  - { kind: "email" }
  - { kind: "url", schemes: array<tstr> }                                   // np. ["https"]
  - { kind: "iban" }
  - { kind: "phone", region: tstr or null }                                 // ISO 3166-1 alpha-2
  - { kind: "uuid" }
  - { kind: "date_range", min: tstr or null, max: tstr or null }            // "YYYY-MM-DD"
  - { kind: "custom", id: tstr, params: map<tstr, Value> or null }          // addon-defined validator

Transform:                                                                  // dla Canvas2D group
  translate_x: f32
  translate_y: f32
  rotate_rad: f32
  scale_x: f32
  scale_y: f32

ClipPath (discriminated union):
  - { kind: "rect", x, y, w, h }
  - { kind: "circle", cx, cy, r }
  - { kind: "polygon", points: array<{x, y}> }

ImageDef:                                                                   // dla Canvas2D image pool
  id: tstr
  ref: tstr                                                                 // signed_url_ref
  width_px: u16
  height_px: u16

CanvasTool:
  id: tstr
  label: BindRef<tstr>
  icon: IconRef
  cursor: CursorToken or null

ShaderDef:                                                                  // dla WebGLSurface
  id: tstr
  vertex_glsl: tstr                                                         // GLSL ES 3.00 source
  fragment_glsl: tstr
  uniforms_schema: array<UniformDef>

UniformDef:
  name: tstr
  uniform_type: UniformType                                                 // "float" | "vec2" | "vec3" | "vec4" | "mat4" | "int" | "sampler2D" | ...

WGPUPipelineDef:
  id: tstr
  pipeline_type: WGPUPipelineType                                           // "render" | "compute"
  wgsl_source: tstr                                                         // WGSL shader code
  vertex_entry: tstr or null                                                // dla render
  fragment_entry: tstr or null
  compute_entry: tstr or null                                               // dla compute
  bind_group_layouts: array<WGPUBindGroupLayout>

WGPUBindGroupLayout:
  group_index: u8                                                           // 0..3
  entries: array<WGPUBindingDef>

WGPUBindingDef:
  binding: u8
  resource_type: WGPUResourceType                                           // "uniform" | "storage_read" | "storage_rw" | "texture" | "sampler"
  visibility: array<WGPUShaderStage>                                        // ["vertex", "fragment", "compute"]

ChartSeriesStyle (enum, tstr):
  "solid" | "dashed" | "dotted"

ChartAxisScale (enum, tstr):
  "linear" | "log" | "time" | "category"
```

**Konwencja typów w polach komponentów (strict, no shortcuts):**

Każde pole musi explicit deklarować:
- `IconRef`, `AvatarRef`, `BreadcrumbItem`, `MenuItem`, `Badge` (inline), `Chip` (inline), … — **inline struct** (CBOR map bez tag/id)
- `ComponentRef<X>` (gdzie X = nazwa komponentu Component-instance, np. `ComponentRef<Button>`) — pełen Component z odpowiednim `tag` (`Component.tag == tag_of(X)`)

**Brak skrótów** w v1.0. Wszystkie pola w katalogu używają explicit `ComponentRef<X>` lub `InlineX`. Generator `tentaflow-sdk-gen` enforce'uje wymóg — emit'uje błąd jeśli zobaczy bare `Button` / `Chip` / `Select` itd. w polu komponentu (bez `ComponentRef<...>` lub `Inline...` prefiksu).

**Aliasy w v1.0:**
- `InlineBadge` = Badge w inline form (§1.5), `InlineChip` = Chip w inline form (§1.5)
- `ComponentRef<X>` = pełen Component instance z `tag == tag_of(X)`

Validator porównuje strict — jeśli pole oczekuje inline Chip ale dostanie pełen Component z tagiem 0x020B → reject. Odwrotnie też.

### 1.55 Event payloads per EventKind

Każdy event handler invocation niesie strukturalny payload (params w `Action` envelope). Schemy:

```
EventPayload (per EventKind):

"click":               { x?: u16, y?: u16, modifiers: KeyModifiers }
"double_click":        { x?: u16, y?: u16, modifiers: KeyModifiers }
"long_press":          { x?: u16, y?: u16, duration_ms: u16 }
"context_menu":        { x?: u16, y?: u16 }

"change":              { value: Value }                                     // form field new value
"input":               { value: Value, raw: tstr or null }                  // dla inputów raw może różnić od typed value
"submit":              { values: map<tstr, Value>, scope_id: tstr or null } // form scope
"reset":               { scope_id: tstr or null }
"commit":              { value: Value }                                     // dla Slider on release

"focus":               { }
"blur":                { }
"key_down":            { key: tstr, code: tstr, modifiers: KeyModifiers, repeat: bool }
"key_up":              { key: tstr, code: tstr, modifiers: KeyModifiers }
"key_press":           { key: tstr, code: tstr, modifiers: KeyModifiers }
"save_shortcut":       { }                                                  // Ctrl/Cmd+S synthesized

"open":                { }
"close":               { reason: CloseReason or null }                       // dla Modal/Drawer/Popover
"select":              { id: tstr, value: Value or null }                    // tabs, menu, options
"deselect":            { id: tstr }
"dismiss":             { reason: tstr or null }
"confirm":             { }
"cancel":              { }

"drag_start":          { source_id: tstr, data: Value or null }
"drag_end":            { source_id: tstr, destination_id: tstr or null }
"drop":                { source_id: tstr, target_id: tstr, position: DropPosition }

"scroll":              { scroll_x: u32, scroll_y: u32 }
"scroll_end":          { scroll_x: u32, scroll_y: u32, edge: ScrollEdge }    // bottom, top, left, right
"resize":              { width_px: u16, height_px: u16 }
"intersect":           { ratio: f32, entered: bool }

"pointer_down":        { x: f32, y: f32, button: PointerButton, modifiers: KeyModifiers, pointer_type: PointerType }
"pointer_up":          { x: f32, y: f32, button: PointerButton, modifiers: KeyModifiers }
"pointer_move":        { x: f32, y: f32, dx: f32, dy: f32, modifiers: KeyModifiers }
"pointer_cancel":      { }
"wheel":               { dx: f32, dy: f32, modifiers: KeyModifiers }

"play":                { }
"pause":               { }
"ended":               { }
"loaded":              { duration_ms: u32 or null }
"stream_error":        { code: tstr, message: tstr }
"fullscreen":          { active: bool }

"stream_chunk":        { stream_id: tstr, chunk_index: u32, end_of_stream: bool }

"row_click":           { row_id: tstr, column_id: tstr or null }
"row_double_click":    { row_id: tstr, column_id: tstr or null }
"selection_change":    { selected_ids: array<tstr> }
"cell_click":          { row_id: tstr, column_id: tstr, value: Value or null }
"cell_hover":          { row_id: tstr, column_id: tstr }

"item_click":          { id: tstr, index: u32 or null }
"marker_click":        { id: tstr, lat: f64, lng: f64 }
"node_click":          { id: tstr }
"edge_click":          { id: tstr }

"zoom_end":            { zoom: u8 }
"pan_end":             { center_lat: f64, center_lng: f64 }
"point_hover":         { series_id: tstr, x_value: Value, y_value: f64 }
"range_select":        { x_from: Value, x_to: Value }

"files_selected":      { files: array<FileMeta> }
"upload_progress":     { file_id: tstr, percent: f32 }
"upload_complete":     { file_id: tstr, signed_url_ref: tstr }
"upload_error":        { file_id: tstr, error_code: u16, message: tstr }

"step_change":         { from_id: tstr, to_id: tstr }
"step_click":          { id: tstr }
"expand":              { id: tstr }
"collapse":            { id: tstr }

"frame":               { delta_ms: f32, frame_index: u64 }                  // dla WebGL/WGPU

"remove":              { id: tstr or null }                                  // Chip removable
"image_click":         { id: tstr, index: u32 }
"day_click":           { date: tstr }
"slot_click":          { date: tstr, time: tstr or null }
"event_click":         { event_id: tstr }
"event_drop":          { event_id: tstr, new_start_ts: i64 }
"cell_toggle":         { row_key: tstr, column_key: tstr, new_value: Value }
"bulk_apply":          { action: tstr, target_ids: array<tstr> }
"add_rule":            { }
"remove_rule":         { id: tstr }
"approve_rule":        { id: tstr }
"mark_read":           { id: tstr }
"retry":               { }                                                   // OfflineBanner

"field_change":        { field_id: tstr, value: Value, scope_id: tstr or null }   // Form descendants
"scroll_top":          { }                                                   // VirtualizedLog infinite scroll górą
"filter_change":       { active_levels: array<tstr> }                        // VirtualizedLog filter
"cell_change":         { row_key: tstr, column_key: tstr, new_value: Value } // AccessMatrix/PermissionMatrix

# common helpers:
KeyModifiers:
  shift: bool
  ctrl: bool
  alt: bool
  meta: bool                                                                 // Cmd on Mac, Win on Windows

PointerButton (enum, u8):                                                    // 0=primary, 1=middle, 2=secondary, 3=back, 4=forward
PointerType (enum, tstr):                                                    // "mouse" | "pen" | "touch"
DropPosition (enum, tstr):                                                    // "before" | "after" | "inside"
ScrollEdge (enum, tstr):                                                      // "top" | "bottom" | "left" | "right"
CloseReason (enum, tstr):                                                     // "user_dismissed" | "backdrop_click" | "escape" | "programmatic"
```

Validator wymusza że `Action.params` przy danym EventKind ma keys zgodne ze schema. Nadmiarowe keys → reject. Brakujące optional keys → OK.

**Component-specific payload extensions (whitelist):** niektóre komponenty extend base EventKind payload dodatkowymi polami. Te rozszerzenia są **explicit declared** w schemie komponentu (poniżej w katalogu) oraz w canonical manifest. Validator zna pełną listę extensions per (ComponentTag, EventKind) i akceptuje dodatkowe pola tylko z tej whitelist.

Przykłady extensions:
- `Pagination` + `change` → adds `{ page: u32 }` (zamiast `value`)
- `NavTabs` + `select` → adds `{ panel_id: tstr }`
- `MapView` + `click` → adds `{ lat: f64, lng: f64 }` (poza standardowymi x,y,modifiers)
- `Tabs` + `select` → adds `{ tab_id: tstr }`
- `Tree` + `select` → adds `{ node_id: tstr, path: array<tstr> }`

Konkretne extensions per komponent są wymienione w sekcji komponentu (notka "Event payload extension:"). Manifest generator emituje tabelę `(tag, event_kind) → extension_schema` jako część catalog manifest.

### 1.6 Common Component envelope (recap z protokołu §10)

```
Component (CBOR map z integer keys):
  0: tag           u16                                              // stable discriminant
  1: id            tstr                                             // unique within panel
  2: fields        map<u8, Value>                                   // per-component schema (see below)
  3: handlers      map<EventKind, Handler> or absent
  4: bind          BindSpec or absent
  5: a11y          Accessibility or absent
  6: visibility    Visibility or absent
  7: test_id       tstr or absent                                   // ≤ 64 chars [a-z0-9_-]
```

```
EventKind (canonical enum, tstr — definicja używana przez protokół i katalog):
  # podstawowe interakcje
  "click" | "double_click" | "long_press" | "context_menu"
  # form
  | "change" | "input" | "submit" | "reset" | "commit"
  # focus/keyboard
  | "focus" | "blur" | "key_down" | "key_up" | "key_press" | "save_shortcut"
  # state transitions
  | "open" | "close" | "select" | "deselect" | "dismiss" | "confirm" | "cancel"
  # drag-drop
  | "drag_start" | "drag_end" | "drop"
  # scroll/resize/intersection
  | "scroll" | "scroll_end" | "resize" | "intersect"
  # pointer (touch + mouse + pen unified)
  | "pointer_down" | "pointer_up" | "pointer_move" | "pointer_cancel" | "wheel"
  # media
  | "play" | "pause" | "ended" | "loaded" | "stream_error" | "fullscreen"
  # streaming
  | "stream_chunk"
  # custom data display
  | "row_click" | "row_double_click" | "selection_change" | "cell_click" | "cell_hover"
  | "item_click" | "marker_click" | "node_click" | "edge_click"
  | "zoom_end" | "pan_end" | "point_hover" | "range_select"
  # form file upload lifecycle
  | "files_selected" | "upload_progress" | "upload_complete" | "upload_error"
  # carousel/step/expand
  | "step_change" | "step_click" | "expand" | "collapse"
  # frame (WebGL/WGPU loop)
  | "frame"
  # event-specific
  | "remove" | "image_click" | "day_click" | "slot_click" | "event_click" | "event_drop"
  | "cell_toggle" | "cell_change" | "bulk_apply" | "add_rule" | "remove_rule" | "approve_rule"
  | "mark_read"
  # form/scroll/filter/retry
  | "field_change" | "scroll_top" | "filter_change" | "retry"
```

```
Accessibility:
  role: tstr or null                                                // ARIA role override
  label: BindRef or null
  label_for: tstr or null
  described_by: tstr or null
  live: LiveRegion or null
  expanded: BindRef or null                                          // bool
  disabled: BindRef or null                                          // bool
  required: BindRef or null
  invalid: BindRef or null
  pressed: BindRef or null
  selected: BindRef or null

Visibility:
  visible: BindRef or null                                           // default true
  display_above_breakpoint: Breakpoint or null
  display_below_breakpoint: Breakpoint or null
  hidden_for_assistive: bool                                         // aria-hidden
```

---

## 2. Structured Molecules (0x0000–0x00FF)

Sztywny layout, addon wypełnia sloty + parametry. Te komponenty narzucają wygląd całkowicie.

### 0x0001 — `Header`

Top-of-page identifier addona/sekcji (jak na mockup TentaVision).

```
Fields:
  0: icon            IconRef                        // { kind: "named", name: IconName, tone?: Tone, size?: IconSize }
                                                    //   or { kind: "asset", ref: tstr }  (signed_url_ref)
  1: title           BindRef<tstr>
  2: status_badge    Badge or null                  // status pill obok title
  3: subtitle        BindRef<tstr> or null
  4: meta_chips      array<InlineChip>                    // max 6
  5: actions         array<ComponentRef<Button>>                  // max 4 button refs
  6: density         Density                        // default "default"
Handlers: none
```

### 0x0002 — `PageHeader`

Generic page-level header (większy niż Header).

```
Fields:
  0: title           BindRef<tstr>
  1: subtitle        BindRef<tstr> or null
  2: breadcrumbs     array<BreadcrumbItem> or null
  3: actions         array<ComponentRef<Button>>                  // max 4
  4: tabs            array<NavTab> or null          // optional sub-nav
Handlers: none
```

### 0x0003 — `EmptyState`

Brak danych / pierwsze użycie ekran.

```
Fields:
  0: icon            IconRef
  1: heading         BindRef<tstr>
  2: message         BindRef<tstr> or null
  3: primary_action  ComponentRef<Button> or null
  4: secondary_action ComponentRef<Button> or null
  5: variant         EmptyStateVariant              // "default" | "compact" | "illustrated"
Handlers: none
```

### 0x0004 — `SectionHeader`

Nagłówek wewnątrz panelu/karty.

```
Fields:
  0: title           BindRef<tstr>
  1: subtitle        BindRef<tstr> or null
  2: actions         array<ComponentRef<Button>>                  // max 3
  3: divider         bool                           // bottom divider line
Handlers: none
```

### 0x0005 — `Toolbar`

Bar z search + filters + segmented + actions.

```
Fields:
  0: search          ComponentRef<SearchBox> or null              // referencja do form Input (0x0307)
  1: filters         array<FilterChipDef>           // max 12
  2: view_mode       ComponentRef<SegmentedControl> or null
  3: sort_control    ComponentRef<Select> or null
  4: trailing_actions array<ComponentRef<Button>>                 // max 4
  5: density         Density
Handlers: none
```

### 0x0006 — `AppShell`

Top-level layout dla addon application panels (sidebar + content).

```
Fields:
  0: sidebar_slot    tstr                           // slot id dla menu po lewej
  1: content_slot    tstr                           // slot id dla głównego content
  2: header_slot     tstr or null
  3: sidebar_width   Spacing                        // default "xl" (256px equivalent)
  4: collapsible_sidebar bool
Handlers: none
```

### 0x0007 — `LoginShell`

Centred container dla login/auth flows.

```
Fields:
  0: logo            IconRef
  1: title           BindRef<tstr>
  2: subtitle        BindRef<tstr> or null
  3: content_slot    tstr
  4: footer_slot     tstr or null
Handlers: none
```

### 0x0008 — `ErrorBoundary`

Standardized error display.

```
Fields:
  0: error_code      BindRef<tstr> or null
  1: title           BindRef<tstr>
  2: message         BindRef<tstr> or null
  3: actions         array<ComponentRef<Button>>                  // typically retry, contact support
  4: technical_details BindRef<tstr> or null        // collapsed by default
Handlers: none
```

### 0x0009 — `WelcomeHero`

Onboarding/welcome screen z dużą grafiką + CTA.

```
Fields:
  0: illustration    IconRef                        // duża ikona/illustracja
  1: title           BindRef<tstr>
  2: subtitle        BindRef<tstr>
  3: features        array<FeatureItem>             // max 5 — { icon, title, description }
  4: primary_action  ComponentRef<Button>
  5: secondary_action ComponentRef<Button> or null
```

### 0x000A — `StatGroup`

Grid 2-6 StatCard'ów z synced spacing.

```
Fields:
  0: stats           array<StatCard>                // min 2, max 6
  1: columns         u8                             // 2 | 3 | 4 | 6; default = stats.len()
  2: density         Density
Handlers: none
```

### 0x000B — `WizardShell`

Multi-step wizard layout.

```
Fields:
  0: steps           array<StepDef>                 // { id, label, optional?, status? }
  1: current_step_id BindRef<tstr>
  2: content_slot    tstr
  3: footer_slot     tstr                           // typically WizardFooter
  4: cancellable     bool
Handlers:
  "step_change":   Handler                         // emitted gdy current_step_id się zmienia
```

### 0x000C — `Inspector`

Right-rail detail panel (jak Figma right panel).

```
Fields:
  0: title           BindRef<tstr>
  1: content_slot    tstr
  2: actions         array<ComponentRef<Button>>                  // top actions
  3: tabs            array<NavTab> or null          // optional tab nav inside
  4: collapsible     bool
Handlers: none
```

---

## 3. Layout Primitives (0x0100–0x01FF)

Free-form composition. Addon układa kompozycje, każdy primitive ma sztywne style.

### 0x0101 — `Flex`

Flex container z explicit flex spec.

```
Fields:
  0: direction       FlexDirection                  // "row" | "row_reverse" | "column" | "column_reverse"
  1: gap             Spacing                        // default "md"
  2: justify         FlexJustify                    // "start" | "end" | "center" | "space_between" | "space_around" | "space_evenly"
  3: align           FlexAlign                      // "start" | "end" | "center" | "baseline" | "stretch"
  4: wrap            FlexWrap                       // "no_wrap" | "wrap" | "wrap_reverse"
  5: children        array<Component>               // max see §9 limits
  6: padding         Spacing or null
  7: background      BackgroundToken or null        // "none" | "subtle" | "muted" | "accent"
  8: radius          RadiusToken or null
  9: style           BoxStyle or null               // §1.5 — nadpisuje pola tokenowe
Handlers: none
```

### 0x0102 — `Grid`

CSS Grid container.

```
Fields:
  0: columns         GridTrack                      // schema below
  1: gap             Spacing
  2: row_gap         Spacing or null                // default = gap
  3: column_gap      Spacing or null                // default = gap
  4: children        array<GridChild>               // { component, col_span?, row_span?, col_start? }
  5: padding         Spacing or null
  6: align_items     FlexAlign or null
  7: style           BoxStyle or null               // §1.5

GridTrack (discriminated union, always CBOR map z `kind`):
  - { kind: "equal",    count: u8 }                                 // N equal-width columns
  - { kind: "explicit", cols: array<GridCol> }                      // per-column track sizing

GridCol (discriminated union, always CBOR map z `kind`):
  - { kind: "auto" } | { kind: "fill" } | { kind: "min_content" } | { kind: "max_content" }
  - { kind: "fr", value: u8 }
  - { kind: "px", value: u32 }
```

### 0x0103 — `Stack`

Vertical Flex column z wbudowanymi defaults (gap="md", align="stretch").

```
Fields:
  0: gap             Spacing
  1: align           FlexAlign
  2: children        array<Component>
  3: padding         Spacing or null
  4: justify         FlexJustify or null            // rozkład na głównej (pionowej) osi
  5: style           BoxStyle or null               // §1.5
```

### 0x0104 — `Cluster`

Horizontal flow z auto-wrap (chips, badges grouped naturally).

```
Fields:
  0: gap             Spacing
  1: align           FlexAlign
  2: justify         FlexJustify
  3: children        array<Component>
```

### 0x0105 — `Split`

2-column split z resizable divider.

```
Fields:
  0: orientation     SplitOrientation               // "horizontal" | "vertical"
  1: primary_size    SplitSize                      // discriminated union (schema below)
  2: min_primary     u32                            // px
  3: max_primary     u32                            // px
  4: resizable       bool
  5: primary_slot    tstr
  6: secondary_slot  tstr

SplitSize (discriminated union, always CBOR map z `kind`):
  - { kind: "auto" }
  - { kind: "px",      value: u32 }
  - { kind: "percent", value: f64 }                                  // 0.0..=100.0 (finite, non-NaN)
```

### 0x0106 — `Card`

Generic container z paddingiem, border-radius, optional shadow.

```
Fields:
  0: variant         CardVariant                    // "filled" | "outlined" | "elevated" | "ghost"
  1: padding         Spacing                        // default "lg"
  2: gap              Spacing                       // default "md" (gap między children)
  3: radius          RadiusToken                    // default "lg"
  4: shadow          ShadowToken                    // default "none" for filled, "subtle" for elevated
  5: border          BorderToken
  6: background      BackgroundToken
  7: accent          Tone or null                   // left-edge accent bar
  8: children        array<Component>
  9: interactive     bool                           // hover/focus visuals
  10: clickable      bool                           // emits "click" event
  11: style          BoxStyle or null               // §1.5
Handlers:
  "click":           Handler (if clickable=true)
```

### 0x0107 — `SectionCard`

Card z built-in SectionHeader.

```
Fields:
  0: title           BindRef<tstr>
  1: subtitle        BindRef<tstr> or null
  2: header_actions  array<ComponentRef<Button>>     // max 3
  3: header_divider  bool
  4: body            array<Component>
  5: footer          array<Component> or null
  6: padding         Spacing                         // default "lg"
  7: gap             Spacing                         // default "md"
  8: variant         CardVariant                     // "filled" | "outlined" | "elevated" | "ghost"
  9: radius          RadiusToken                     // default "lg"
  10: shadow         ShadowToken                     // default "subtle"
  11: border         BorderToken
  12: background     BackgroundToken
  13: accent         Tone or null                    // left-edge accent bar
  14: style          BoxStyle or null                // §1.5
Handlers: none
```

### 0x0108 — `Divider`

Horizontal or vertical line.

```
Fields:
  0: orientation     DividerOrientation             // "horizontal" | "vertical"
  1: variant         DividerVariant                 // "default" | "subtle" | "strong" | "dashed"
  2: spacing         Spacing                        // around-margin
  3: label           BindRef<tstr> or null          // text in middle
```

### 0x0109 — `Spacer`

Empty space (preferred dla layout adjustments zamiast inline padding).

```
Fields:
  0: size            Spacing
  1: axis            SpacerAxis                     // "x" | "y" | "both"
```

### 0x010A — `Sidebar`

Vertical nav container (typowo dla AppShell.sidebar_slot).

```
Fields:
  0: header_slot     tstr or null
  1: items           array<SidebarItem>             // { id, icon?, label, badge?, on_click, active? }
  2: footer_slot     tstr or null
  3: collapsed       BindRef<bool> or null
Handlers: none
```

### 0x010B — `Tabs`

Horizontal tabs z content area.

```
Fields:
  0: variant         TabsVariant                    // "default" | "pills" | "underlined" | "boxed"
  1: items           array<TabItem>                 // { id, label, icon?, badge?, locked? }
  2: active_id       BindRef<tstr>
  3: content_slot    tstr                           // slot rendered for active tab
  4: density         Density
Handlers:
  "select":          Handler                         // emitted z params { id: tstr }
```

### 0x010C — `NavTabs`

Page-level navigation tabs (różni się od Tabs że pełni rolę routingu, nie content swapping).

```
Fields:
  0: items           array<NavTab>                  // { id, label, icon?, badge?, panel_id, locked? }
  1: active_id       BindRef<tstr>
  2: variant         NavTabsVariant                 // "default" | "underlined" | "pills"
  3: scroll_overflow bool
Handlers:
  "select":          Handler                         // emitted z params { id, panel_id }
```

### 0x010D — `Collapsible`

Expandable/collapsible section.

```
Fields:
  0: header          Component                       // typically SectionHeader or Heading
  1: body            array<Component>
  2: expanded        BindRef<bool>
  3: animated        bool
Handlers:
  "open":            Handler
  "close":           Handler
```

### 0x010E — `Accordion`

Wielokrotny Collapsible z mutex/multi-open behavior.

```
Fields:
  0: items           array<AccordionItem>           // { id, header, body, default_expanded? }
  1: mode            AccordionMode                  // "single" | "multiple"
  2: expanded_ids    BindRef<array<tstr>>
```

### 0x010F — `Tooltip`

Hover/focus popup z krótkim opisem.

```
Fields:
  0: child           Component
  1: content         BindRef<tstr>
  2: side            DrawerSide
  3: max_width_px    u16
```

### 0x0110 — `Breadcrumb`

Path navigation.

```
Fields:
  0: items           array<BreadcrumbItem>          // { label, href? or action_id?, icon?, is_current? }
  1: separator       BreadcrumbSeparator            // "chevron" | "slash" | "dot"
  2: max_items       u8                             // collapse middle if exceeds (default 5)
```

### 0x0111 — `Pagination`

Page selector.

```
Fields:
  0: current_page    BindRef<u32>
  1: total_pages     BindRef<u32>
  2: variant         PaginationVariant              // "compact" | "full" | "input"
  3: show_summary    bool                           // "Page 3 of 10"
Handlers:
  "change":          Handler                         // params { page: u32 }
```

### 0x0112 — `ScrollContainer`

Scrollable area z opcjonalnymi sticky headers.

```
Fields:
  0: orientation     ScrollOrientation              // "vertical" | "horizontal" | "both"
  1: height          DimensionToken                 // schema in §1.5 — always CBOR map z `kind`
  2: max_height      DimensionToken or null
  3: children        array<Component>
  4: sticky_header_slot tstr or null
  5: virtualize      bool                            // virtual scroll dla long lists
Handlers:
  "scroll_end":      Handler                         // infinite scroll trigger
```

### 0x0115 — `Box`

Uniwersalny, przezroczysty kontener („div"): kontrola dziecka wewnątrz
Flex/Cluster (grow/align_self/width/margin), pełne stylowanie pudełka przez
`BoxStyle` (§1.5) i opcjonalne proste zachowanie flex dla własnych dzieci.
Wszystkie pola opcjonalne — pusty Box renderuje się jako goły `div.tf-box`.

```
Fields:
  0: width           DimensionToken or null
  1: grow            bool or null                   // flex-grow:1 wewnątrz rodzica flex
  2: align_self      FlexAlign or null
  3: padding         Spacing or null                // token na wszystkie krawędzie
  4: margin          Spacing or null                // token na wszystkie krawędzie
  5: children        array<Component>
  6: style           BoxStyle or null               // §1.5 — margin/padding/border/bg/radius/wymiary/overflow
  7: direction       FlexDirection or null          // dowolne z 7-10 włącza display:flex
  8: gap             Spacing or null
  9: align           FlexAlign or null
  10: justify        FlexJustify or null
Handlers: none
```

---

## 4. Data Display (0x0200–0x02FF)

### 0x0201 — `Text`

Pojedynczy string text.

```
Fields:
  0: content         BindRef<tstr>
  1: style           TextStyle
  2: tone            Tone or null
  3: align           TextAlign or null
  4: wrap            TextWrap or null
  5: max_lines       u8 or null                     // truncate z ellipsis
  6: format          ValueFormat or null            // jeśli content jest non-string (number/date)
Handlers: none
```

### 0x0202 — `Heading`

Semantic heading (h1-h6).

```
Fields:
  0: content         BindRef<tstr>
  1: level           u8                              // 1-6
  2: tone            Tone or null
  3: align           TextAlign or null
```

### 0x0203 — `Paragraph`

Multi-line text z markdown-light support (bold, italic, code inline, links — sanitized).

```
Fields:
  0: content         BindRef<tstr>                  // markdown source
  1: style           TextStyle                      // default "body"
  2: allowed_marks   array<MarkdownMark>            // { "bold", "italic", "code", "link" } subset
  3: allow_links     bool                            // jeśli false → strip
  4: max_lines       u8 or null
Handlers: none
```

### 0x0204 — `RichText`

Bardziej ograniczony niż Paragraph — sanitized HTML subset.

```
Fields:
  0: content         BindRef<tstr>                  // markdown
  1: allowed_blocks  array<MarkdownBlock>           // { "heading", "list", "code_block", "blockquote", "table" }
  2: allowed_marks   array<MarkdownMark>
  3: max_height_px   u16 or null                    // overflow scroll if exceeds
```

### 0x0205 — `MonoBlock`

Preformatted text (no syntax highlighting).

```
Fields:
  0: content         BindRef<tstr>
  1: max_height_px   u16 or null
  2: word_wrap       bool
  3: copyable        bool                            // top-right copy button
```

### 0x0206 — `CodeBlock`

Z syntax highlighting.

```
Fields:
  0: content         BindRef<tstr>
  1: language        tstr                            // "rust" | "javascript" | "json" | "toml" | "sql" | ...
  2: show_line_numbers bool
  3: copyable        bool
  4: max_height_px   u16 or null
  5: highlight_lines array<u32>
```

### 0x0207 — `KeyValue`

2-column display (label : value list).

```
Fields:
  0: items           array<KvItem>                  // { label, value: BindRef, hint?, action? }
  1: density         Density
  2: layout          KvLayout                        // "stacked" | "horizontal" | "grid"
  3: label_width     Spacing or null                 // dla "horizontal"
```

### 0x0208 — `StatCard`

Big-number metric card.

```
Fields:
  0: label           BindRef<tstr>
  1: icon            IconRef or null
  2: value           BindRef<Value>                  // typowo number albo string
  3: value_suffix    BindRef<tstr> or null           // np. "/ 24"
  4: format          ValueFormat or null
  5: trend           Trend or null                   // schema w §1.5 (percent: f64)
  6: footnote        Footnote or null                // { tone: Tone, content: BindRef<tstr> }
  7: accent          Tone or null                    // left-edge bar
  8: clickable       bool
Handlers:
  "click":           Handler (if clickable)
```

### 0x0209 — `Stat`

Smaller stat (jako pojedyncza wartość bez ramki).

```
Fields:
  0: label           BindRef<tstr>
  1: value           BindRef<Value>
  2: format          ValueFormat or null
  3: trend           Trend or null
  4: size            StatSize                        // "sm" | "md" | "lg"
```

### 0x020A — `Badge`

Pill z text (status, count).

```
Fields:
  0: variant         BadgeVariant
  1: tone            Tone
  2: label           BindRef<tstr>
  3: icon            IconRef or null                 // leading icon
  4: count           BindRef<u32> or null             // overrides label jako count
  5: max             u32                              // cap dla count display (np. "99+")
  6: pulse           bool                              // animation
```

### 0x020B — `Chip`

Tag/filter chip (większy niż Badge, może być interactive).

```
Fields:
  0: variant         ChipVariant
  1: tone            Tone
  2: label           BindRef<tstr>
  3: icon            IconRef or null
  4: avatar          AvatarRef or null
  5: selected        BindRef<bool> or null            // dla "selectable" variant
  6: removable       bool                             // adds X button
Handlers:
  "click":           Handler (jeśli selectable/toggle)
  "remove":          Handler (jeśli removable)
```

### 0x020C — `Tag`

Static tag (read-only label, mniejszy niż Chip).

```
Fields:
  0: tone            Tone
  1: label           BindRef<tstr>
  2: size            TagSize                          // "xs" | "sm" | "md"
```

### 0x020D — `Avatar`

User avatar (image, initials, or icon).

```
Fields:
  0: source          AvatarSource                    // { kind: "image", ref: tstr } | { kind: "initials", initials: tstr } | { kind: "icon", icon: IconRef }
  1: size            AvatarSize                       // "xs" | "sm" | "md" | "lg" | "xl"
  2: shape           AvatarShape                      // "circle" | "rounded" | "square"
  3: status          AvatarStatus or null             // "online" | "offline" | "busy" | "away" — small indicator
  4: tone            Tone or null                     // background dla initials/icon variants
```

### 0x020E — `AvatarGroup`

Stack of avatars with overflow.

```
Fields:
  0: avatars         array<ComponentRef<Avatar>>                    // max 8 shown
  1: max_visible     u8                                // overflow → "+N"
  2: overlap         AvatarOverlap                     // "tight" | "default" | "loose"
  3: size            AvatarSize
```

### 0x020F — `BulletList`

Simple bullet/numbered list.

```
Fields:
  0: items           array<BindRef<tstr>>             // max 100
  1: variant         BulletListVariant                 // "bullet" | "numbered" | "check" | "icon"
  2: tone            Tone or null
  3: density         Density
```

### 0x0210 — `Timeline`

Chronological events display.

```
Fields:
  0: items           array<TimelineItem>              // { id, ts_ms, title, description?, icon?, tone?, action? }
  1: orientation     TimelineOrientation              // "vertical" | "horizontal"
  2: density         Density
  3: show_dates      bool
  4: group_by_day    bool
Handlers:
  "item_click":      Handler                           // params { id }
```

### 0x0211 — `Table`

Powerful data table.

```
Fields:
  0: columns         array<TableColumn>               // schema + width + render
  1: rows_path       StatePath                        // bound to state array
  2: row_key_field   tstr                             // for stable identity
  3: variant         TableVariant                     // "default" | "striped" | "borderless" | "compact"
  4: density         Density
  5: sortable        bool
  6: sort_by         BindRef<TableSort> or null       // { column_id, direction: "asc"|"desc" }
  7: selectable      TableSelectMode                  // "none" | "single" | "multi"
  8: selected_ids    BindRef<array<tstr>> or null
  9: sticky_header   bool
  10: sticky_columns u8                               // left-pinned count
  11: pagination     TablePagination or null          // { page_size, current_page_path }
  12: empty_state    ComponentRef<EmptyState> or null
  13: row_actions    array<ComponentRef<Button>>                    // per-row action menu
  14: bulk_actions   array<ComponentRef<Button>>                    // shown when rows selected
  15: virtualize     bool                             // virtual scroll for large datasets
  16: row_expandable bool                              // shows expand chevron
  17: expanded_row_template_id tstr or null
Handlers:
  "row_click":       Handler
  "row_double_click": Handler
  "selection_change": Handler

TableColumn:
  id: tstr
  header: BindRef<tstr>
  field_path: array<PathSegment>                       // relative to row
  width: TableColumnWidth                              // schema w §1.5 — always CBOR map z `kind`
  render: ColumnRender                                 // "text" | "number" | "currency" | "badge" | "chip" | "avatar" | "icon" | "stat" | "actions" | "custom_component"
  format: ValueFormat or null
  align: TextAlign or null
  sortable: bool
  hidden_by_default: bool
```

### 0x0212 — `List`

Wirtualna/non-virtualna lista (lżejsza niż Table).

```
Fields:
  0: items_path      StatePath
  1: item_template_id tstr                            // referencja do item template w panel templates
  2: divider         bool
  3: density         Density
  4: virtualize      bool
  5: empty_state     ComponentRef<EmptyState> or null
  6: max_visible     u32 or null                      // show only first N, rest behind "Show more"
Handlers:
  "item_click":      Handler                           // params { id, index }
```

### 0x0213 — `Tree`

Hierarchical data tree.

```
Fields:
  0: nodes_path      StatePath                        // tree structure: array<{ id, label, children[]? }>
  1: expanded_ids    BindRef<array<tstr>>
  2: selected_id     BindRef<tstr> or null
  3: variant         TreeVariant                      // "default" | "compact" | "with_icons"
  4: lazy_load       bool                             // node may be loaded on expand
Handlers:
  "expand":          Handler
  "collapse":        Handler
  "select":          Handler
```

### 0x0214 — `EmptyCell`

Placeholder dla nullish values w listach/tabelach.

```
Fields:
  0: variant         EmptyCellVariant                 // "dash" | "em_dash" | "n_a" | "none" | "loading"
```

### 0x0215 — `Sparkline`

Inline mini chart.

```
Fields:
  0: data_path       StatePath                        // array<f32>
  1: variant         SparklineVariant                  // "line" | "area" | "bar"
  2: tone            Tone
  3: width_px        u16
  4: height_px       u16
  5: show_min_max    bool
```

### 0x0216 — `LineChart`

Full line chart.

```
Fields:
  0: series          array<ChartSeries>               // { id, name, data_path, tone, style }
  1: x_axis          ChartAxis
  2: y_axis          ChartAxis
  3: legend          ChartLegend                       // { position: "top"|"bottom"|"left"|"right"|"none" }
  4: tooltip         ChartTooltip
  5: zoom            ChartZoomMode                     // "none" | "x" | "y" | "both"
  6: brush           bool                              // range selector
  7: height_px       u16
Handlers:
  "point_hover":     Handler
  "range_select":    Handler                           // emit gdy zoom changes
```

### 0x0217 — `BarChart`

```
Fields:
  0: series          array<ChartSeries>
  1: x_axis          ChartAxis
  2: y_axis          ChartAxis
  3: orientation     ChartOrientation                 // "vertical" | "horizontal"
  4: stacking        BarStacking                       // "none" | "stacked" | "percent"
  5: legend          ChartLegend
  6: height_px       u16
```

### 0x0218 — `AreaChart`

Like LineChart ale z filled area below.

```
Fields:
  0: series          array<ChartSeries>
  1: x_axis          ChartAxis
  2: y_axis          ChartAxis
  3: legend          ChartLegend
  4: tooltip         ChartTooltip
  5: zoom            ChartZoomMode                    // "none" | "x" | "y" | "both"
  6: brush           bool
  7: height_px       u16
  8: stacking        AreaStacking                      // "none" | "stacked" | "percent"
  9: opacity         f64                              // 0.0..=1.0 fill opacity, default 0.4 (naked f64 dla Value-roundtrip)
Handlers:
  "point_hover":     Handler
  "range_select":    Handler
```

### 0x0219 — `PieChart`

```
Fields:
  0: data_path       StatePath                        // array<{ label, value, tone? }>
  1: variant         PieVariant                        // "pie" | "donut"
  2: show_labels     bool
  3: show_legend     bool
  4: max_segments    u8                                // group rest into "Other"
  5: height_px       u16
```

### 0x021A — `StackedBar`

Horizontal stacked bar (used for capacity displays).

```
Fields:
  0: segments        array<StackSegment>              // { id, value, label?, tone }
  1: total           BindRef<f64>
  2: show_legend     bool
  3: show_percentages bool
  4: height_px       u16
```

### 0x021B — `Heatmap`

Grid colored by value (mockup #39 z kamerami/godzinami).

```
Fields:
  0: rows            array<HeatmapRow>                // { id, label }
  1: columns         array<HeatmapColumn>             // { id, label }
  2: cells_path      StatePath                        // array<{ row_id, col_id, value, tone? }>
  3: scale           HeatmapScale                     // schema w §1.5 — linear { min, max, color_from, color_to } | logarithmic { min, max, base } | categorical { buckets: array<HeatmapBucket{threshold, tone, label?}> }
  4: legend_position HeatmapLegendPosition            // "top_right" | "bottom" | "none"
  5: cell_size_px    u16
  6: tooltip         bool
Handlers:
  "cell_click":      Handler                           // params { row_id, col_id, value }
  "cell_hover":      Handler
```

### 0x021C — `Gauge`

Circular/arc gauge.

```
Fields:
  0: value           BindRef<f64>
  1: min             f64                              // naked numeric — f64 dla Value-roundtrip
  2: max             f64                              // naked numeric — f64
  3: thresholds      array<GaugeThreshold>            // { value: f64, tone, label? }
  4: variant         GaugeVariant                     // "circular" | "arc" | "semi"
  5: label           BindRef<tstr> or null
  6: format          ValueFormat or null
  7: size_px         u16
```

### 0x021D — `ProgressBar`

Linear progress (different from Gauge).

```
Fields:
  0: value           BindRef<f64>                     // 0..1 or 0..max
  1: max             f64                              // default 1.0 (naked numeric — f64 dla Value-roundtrip)
  2: variant         ProgressVariant                   // "default" | "striped" | "indeterminate"
  3: tone            Tone
  4: show_label      bool
  5: label           BindRef<tstr> or null            // override default percent
  6: size            ProgressSize                      // "xs" | "sm" | "md" | "lg"
```

### 0x021E — `RatingDisplay`

Star/heart/numeric rating display.

```
Fields:
  0: value           BindRef<f32>
  1: max             u8                               // default 5
  2: variant         RatingVariant                     // "stars" | "hearts" | "circles" | "numeric"
  3: show_value      bool
  4: precision       RatingPrecision                   // "full" | "half" | "decimal"
```

### 0x021F — `Diff`

Text diff display.

```
Fields:
  0: before_path     StatePath
  1: after_path      StatePath
  2: variant         DiffVariant                      // "split" | "inline" | "unified"
  3: language        tstr or null                    // syntax highlighting
  4: word_wrap       bool
  5: show_line_numbers bool
```

### 0x0220 — `Markdown`

Trusted markdown source render (różni się od Paragraph że pozwala na headings, lists, tables, code blocks).

```
Fields:
  0: content         BindRef<tstr>                    // markdown source
  1: allowed_features array<MarkdownFeature>          // controlled subset (no raw HTML)
                                                       // MarkdownFeature: "heading" | "list" | "code_block" |
                                                       //                  "blockquote" | "table" | "link" |
                                                       //                  "image" | "emphasis" | "strong" |
                                                       //                  "code_inline"
  2: max_height_px   u16 or null
  3: link_target     LinkTarget                       // "self" | "blank_via_command"
```

### 0x0221 — `DataDefinitionList`

`<dl>` semantic list (term/definition pairs).

```
Fields:
  0: items           array<DefItem>                   // { term, definition }
  1: layout          DlLayout                         // "stacked" | "two_column"
```

### 0x0222 — `JsonViewer`

Read-only JSON tree explorer.

```
Fields:
  0: value_path      StatePath
  1: collapsed_depth u8                                // default 2
  2: max_height_px   u16
  3: searchable      bool
```

### 0x0223 — `CalendarMonth`

Static month view (no editing — for date_picker use form component).

```
Fields:
  0: month           BindRef<tstr>                    // "YYYY-MM"
  1: events_path     StatePath or null                // array<{ date: "YYYY-MM-DD", count, tone? }>
  2: show_week_numbers bool
  3: first_day_of_week DayOfWeek                       // "sunday" | "monday"
Handlers:
  "day_click":       Handler                           // params { date }
```

### 0x0225 — `VisuallyHidden`

A11y-only text/content (screen-reader-only). Wizualnie ukryte, ale obecne w accessibility tree.

```
Fields:
  0: content         BindRef<tstr>
  1: as_live         LiveRegion or null               // jeśli set → aria-live region
Handlers: none
```

### 0x0226 — `LiveRegion` (Rust type: `LiveRegionComponent`)

Stand-alone live region dla status announcements (różni się od inline ARIA live na komponencie). Typed Rust struct nazwany `LiveRegionComponent` żeby uniknąć kolizji z `LiveRegion` token enum (§1.1).

```
Fields:
  0: politeness      LiveRegion                       // "polite" | "assertive"
  1: content         BindRef<tstr>
  2: visible         bool                              // jeśli false → tylko a11y announcement, no visual
  3: tone            Tone or null                     // jeśli visible
  4: icon            IconRef or null
  5: clear_after_ms  u32 or null                      // auto-clear hint
Handlers: none
```

Use case: addon chce announce'ować "Zapisano kamerę C-25" do screen readerów po backend action — bez tworzenia visible toast.

### 0x0224 — `Image`

Inline image z signed_url_ref.

```
Fields:
  0: src_ref         BindRef<tstr>                    // signed_url_ref
  1: alt             tstr
  2: width           DimensionToken or null
  3: height          DimensionToken or null
  4: fit             ImageFit                         // "cover" | "contain" | "fill" | "none"
  5: aspect_ratio    AspectRatio or null              // schema in §1.5 — always CBOR map z `kind`
  6: radius          RadiusToken or null
  7: clickable       bool
  8: lazy_load       bool
Handlers:
  "click":           Handler
```

---

## 5. Form (0x0300–0x03FF)

### 0x0301 — `Input`

Text input (single line).

```
Fields:
  0: type            InputType                        // "text" | "email" | "password" | "url" | "phone" | "number" | "search"
  1: bind_path       StatePath                        // two-way bind do __draft
  2: placeholder     BindRef<tstr> or null
  3: label           BindRef<tstr> or null
  4: hint            BindRef<tstr> or null
  5: leading_icon    IconRef or null
  6: trailing_icon   IconRef or null
  7: prefix          BindRef<tstr> or null            // np. "$", "@"
  8: suffix          BindRef<tstr> or null
  9: validators      array<ValidationRule>
  10: max_length     u16 or null
  11: min_length     u16 or null
  12: pattern        tstr or null                    // regex (frontend AND backend)
  13: autocomplete   AutocompleteHint or null         // "name" | "email" | "off" | ...
  14: input_mode     InputMode or null                // mobile virtual keyboard hint
  15: disabled       BindRef<bool> or null
  16: readonly       BindRef<bool> or null
  17: error          BindRef<tstr> or null            // shown when not null
  18: size           InputSize                        // "sm" | "md" | "lg"
Handlers:
  "input":           Handler                           // emitted on each keystroke (debounce-able)
  "change":          Handler                           // emitted on blur z committed value
  "submit":          Handler                           // emitted on Enter
  "focus":           Handler
  "blur":            Handler

ValidationRule (enum):
  - { kind: "required" }
  - { kind: "min_length", value: u16 }
  - { kind: "max_length", value: u16 }
  - { kind: "min", value: f64 }                        // dla number
  - { kind: "max", value: f64 }
  - { kind: "pattern", regex: tstr }
  - { kind: "email" }
  - { kind: "url" }
  - { kind: "custom", id: tstr }                       // addon defines custom validator
```

### 0x0302 — `Textarea`

Multi-line input.

```
Fields:
  0: bind_path       StatePath                        // two-way bind do __draft
  1: placeholder     BindRef<tstr> or null
  2: label           BindRef<tstr> or null
  3: hint            BindRef<tstr> or null
  4: validators      array<ValidationRule>
  5: max_length      u16 or null
  6: min_length      u16 or null
  7: disabled        BindRef<bool> or null
  8: readonly        BindRef<bool> or null
  9: error           BindRef<tstr> or null
  10: size           InputSize                        // "sm" | "md" | "lg"
  11: rows           u8                               // initial visible rows (default 3)
  12: autoresize     bool                             // grow with content
  13: max_rows       u8 or null                      // cap autoresize
  14: monospace      bool                             // dla code-style input
Handlers:
  "input":           Handler
  "change":          Handler
  "focus":           Handler
  "blur":            Handler
```

### 0x0303 — `Select`

Single-value dropdown.

```
Fields:
  0: bind_path       StatePath
  1: options         array<SelectOption>              // { value: tstr | u32, label, icon?, disabled?, group? }
  2: placeholder     BindRef<tstr> or null
  3: label           BindRef<tstr> or null
  4: searchable      bool
  5: clearable       bool                              // X button to clear
  6: virtualize      bool                              // for large lists
  7: disabled        BindRef<bool> or null
  8: size            InputSize
  9: groups          array<SelectGroup> or null       // { id, label }
Handlers:
  "change":          Handler                          // params { value }
```

### 0x0304 — `MultiSelect`

Multi-value chip-based select.

```
Fields:
  0: selected_path   StatePath                        // array of SelectValue
  1: options         array<SelectOption>
  2: placeholder     BindRef<tstr> or null
  3: label           BindRef<tstr> or null
  4: searchable      bool
  5: clearable       bool
  6: virtualize      bool
  7: disabled        BindRef<bool> or null
  8: size            InputSize
  9: groups          array<SelectGroup> or null
  10: max_selections u32 or null
  11: show_select_all bool
Handlers:
  "change":          Handler                          // params { selected: array<SelectValue> }
```

### 0x0305 — `Combobox`

Filterable input z autocomplete.

```
Fields:
  0: bind_path       StatePath
  1: options         array<SelectOption>
  2: placeholder     BindRef<tstr> or null
  3: label           BindRef<tstr> or null
  4: searchable      bool                             // always true for Combobox
  5: clearable       bool
  6: virtualize      bool
  7: disabled        BindRef<bool> or null
  8: size            InputSize
  9: groups          array<SelectGroup> or null
  10: free_input     bool                             // allow non-listed values
  11: min_search_chars u8                             // trigger filter after N chars
  12: remote_search  bool                             // backend search via Action
  13: remote_action_id tstr or null                   // if remote_search=true
Handlers:
  "change":          Handler
  "input":           Handler                          // emitted on each filter keystroke
```

### 0x0306 — `Autocomplete`

Like Combobox ale z free_input always true i remote search default.

```
Fields:
  0: bind_path       StatePath
  1: remote_action_id tstr
  2: result_template_id tstr or null
  3: min_search_chars u8
  4: debounce_ms     u16
  5: placeholder     BindRef<tstr> or null
  6: label           BindRef<tstr> or null
```

### 0x0307 — `SearchBox`

Specialized search input (for toolbars).

```
Fields:
  0: bind_path       StatePath
  1: placeholder     BindRef<tstr>
  2: debounce_ms     u16                              // default 300
  3: variant         SearchVariant                    // "default" | "subtle" | "prominent"
  4: shortcut_hint   tstr or null                    // "Ctrl+K" display
  5: on_search_action_id tstr or null                // backend action
```

### 0x0308 — `TagInput`

Multiple values displayed as chips inside input.

```
Fields:
  0: values_path     StatePath                        // array<tstr>
  1: placeholder     BindRef<tstr> or null
  2: validators      array<ValidationRule>            // for each tag
  3: max_tags        u32 or null
  4: separator       array<tstr>                      // ["Enter", ",", " "]
  5: dedupe          bool
```

### 0x0309 — `MentionInput`

Like Textarea ale z @ trigger dla mention autocomplete.

```
Fields:
  0: bind_path       StatePath
  1: mentions_path   StatePath                        // array of selected mentions
  2: trigger_chars   array<tstr>                      // ["@", "#"]
  3: mention_action_id tstr                           // backend resolve
  4: placeholder     BindRef<tstr> or null
```

### 0x030A — `Toggle`

Switch (on/off).

```
Fields:
  0: bind_path       StatePath                        // bool
  1: label           BindRef<tstr> or null
  2: hint            BindRef<tstr> or null
  3: size            ToggleSize                       // "sm" | "md" | "lg"
  4: tone            Tone                             // "primary" (default)
  5: disabled        BindRef<bool> or null
  6: label_position  TogglePosition                   // "leading" | "trailing"
Handlers:
  "change":          Handler                          // params { value: bool }
```

### 0x030B — `Checkbox`

Standard checkbox z label.

```
Fields:
  0: bind_path       StatePath
  1: label           BindRef<tstr> or null
  2: hint            BindRef<tstr> or null
  3: indeterminate   BindRef<bool> or null
  4: disabled        BindRef<bool> or null
  5: size            CheckboxSize
```

### 0x030C — `Radio`

Single radio button.

```
Fields:
  0: bind_path       StatePath                        // shared z group
  1: value           tstr | u32                       // ten radio's value
  2: label           BindRef<tstr>
  3: hint            BindRef<tstr> or null
  4: disabled        BindRef<bool> or null
```

### 0x030D — `RadioGroup`

Group of Radios z shared state.

```
Fields:
  0: bind_path       StatePath
  1: options         array<RadioOption>              // { value, label, hint?, disabled? }
  2: orientation     RadioGroupOrientation            // "horizontal" | "vertical"
  3: label           BindRef<tstr> or null
  4: density         Density
```

### 0x030E — `RadioCardGroup`

Like RadioGroup ale options są pełne karty (with icon + title + description).

```
Fields:
  0: bind_path       StatePath
  1: options         array<RadioCardOption>          // { value, icon, title, description?, badge? }
  2: columns         u8                              // 1-4
  3: variant         RadioCardVariant                // "default" | "compact" | "feature"
```

### 0x030F — `Slider`

Single-handle slider.

```
Fields:
  0: bind_path       StatePath                        // f64
  1: min             f64
  2: max             f64
  3: step            f64
  4: label           BindRef<tstr> or null
  5: show_value      bool
  6: format          ValueFormat or null
  7: marks           array<SliderMark> or null       // { value, label }
  8: tone            Tone
Handlers:
  "change":          Handler                         // continuous
  "commit":          Handler                         // on release
```

### 0x0310 — `RangeSlider`

Two-handle slider (min/max).

```
Fields:
  0: bind_path_min   StatePath                       // f64 lower handle
  1: bind_path_max   StatePath                       // f64 upper handle
  2: min             f64
  3: max             f64
  4: step            f64
  5: label           BindRef<tstr> or null
  6: show_value      bool
  7: format          ValueFormat or null
  8: marks           array<SliderMark> or null
  9: tone            Tone
  10: min_separation f64                             // minimum gap między handles
Handlers:
  "change":          Handler
  "commit":          Handler
```

### 0x0311 — `SliderRow`

Inline slider z label/value display po prawej.

```
Fields:
  0: bind_path       StatePath
  1: min             f64
  2: max             f64
  3: step            f64
  4: label           BindRef<tstr>                    // wymagany (różni się od Slider)
  5: format          ValueFormat or null
  6: marks           array<SliderMark> or null
  7: tone            Tone
  8: layout          SliderRowLayout                  // "horizontal" | "compact"
Handlers:
  "change":          Handler
  "commit":          Handler
```

### 0x0312 — `NumericInput`

Number input with up/down spinners.

```
Fields:
  0: bind_path       StatePath                        // f64
  1: min             f64 or null
  2: max             f64 or null
  3: step            f64
  4: precision       u8                               // decimal places
  5: format          ValueFormat or null              // display formatting
  6: label           BindRef<tstr> or null
  7: hint            BindRef<tstr> or null
  8: size            InputSize
  9: locale_aware    bool                             // decimal separator from locale
```

### 0x0313 — `CurrencyInput`

Specialized NumericInput dla currency.

```
Fields:
  0: bind_path       StatePath                       // f64 (storage in major units; addon decides)
  1: currency_code   tstr                            // ISO 4217 ("EUR", "PLN", "USD")
  2: min             f64 or null
  3: max             f64 or null
  4: step            f64                              // default 0.01
  5: precision       u8                              // decimal places, default 2
  6: label           BindRef<tstr> or null
  7: hint            BindRef<tstr> or null
  8: size            InputSize
  9: show_symbol     bool                             // prefix/suffix z currency symbol
  10: locale_aware   bool                             // decimal separator from locale
Handlers:
  "change":          Handler
```

### 0x0314 — `DatePicker`

Single date select.

```
Fields:
  0: bind_path       StatePath                        // "YYYY-MM-DD"
  1: label           BindRef<tstr> or null
  2: min_date        tstr or null
  3: max_date        tstr or null
  4: locale          tstr or null
  5: format          DateStyle
  6: first_day_of_week DayOfWeek
  7: disabled_dates  array<tstr> or null
  8: presets         array<DatePreset> or null       // "today", "yesterday", "last_7_days"...
  9: placeholder     BindRef<tstr> or null
```

### 0x0315 — `DateRangePicker`

Two dates (from/to).

```
Fields:
  0: from_path       StatePath                       // "YYYY-MM-DD"
  1: to_path         StatePath
  2: label           BindRef<tstr> or null
  3: min_date        tstr or null
  4: max_date        tstr or null
  5: locale          tstr or null
  6: format          DateStyle
  7: first_day_of_week DayOfWeek
  8: disabled_dates  array<tstr> or null
  9: presets         array<RangePreset> or null
  10: placeholder_from BindRef<tstr> or null
  11: placeholder_to BindRef<tstr> or null
  12: max_range_days u16 or null                     // np. 365
Handlers:
  "change":          Handler
```

### 0x0316 — `TimePicker`

```
Fields:
  0: bind_path       StatePath                        // "HH:MM" or "HH:MM:SS"
  1: precision       TimePrecision                    // "minute" | "second"
  2: format          TimeStyle                        // 12h / 24h
  3: step_minutes    u16
  4: label           BindRef<tstr> or null
```

### 0x0317 — `DateTimePicker`

Combined date + time.

```
Fields:
  0: bind_path       StatePath                       // "YYYY-MM-DDTHH:MM[:SS]"
  1: label           BindRef<tstr> or null
  2: min_datetime    tstr or null
  3: max_datetime    tstr or null
  4: date_format     DateStyle
  5: time_format     TimeStyle
  6: time_precision  TimePrecision                    // "minute" | "second"
  7: step_minutes    u16
  8: locale          tstr or null
  9: first_day_of_week DayOfWeek
  10: placeholder    BindRef<tstr> or null
  11: timezone       tstr or null                    // IANA tz name; null = local
Handlers:
  "change":          Handler
```

### 0x0318 — `FileInput`

File picker (single or multiple).

```
Fields:
  0: bind_path       StatePath                        // array<FileMeta>
  1: accept          array<tstr>                      // MIME or extensions: "image/*", ".pdf"
  2: max_size_bytes  u64
  3: max_files       u8
  4: multiple        bool
  5: drag_and_drop   bool
  6: capture         FileCapture or null              // mobile: "user" | "environment"
  7: upload_action_id tstr                            // backend handler — receives Stream
  8: label           BindRef<tstr> or null
  9: hint            BindRef<tstr> or null
Handlers:
  "files_selected":  Handler                          // before upload
  "upload_progress": Handler                          // params { file_id, percent }
  "upload_complete": Handler
  "upload_error":    Handler
```

### 0x0319 — `ColorPicker`

```
Fields:
  0: bind_path       StatePath                        // hex string
  1: variant         ColorPickerVariant               // "swatch" | "wheel" | "compact" | "tokens_only"
  2: allowed_tokens  array<ColorToken> or null        // jeśli "tokens_only"
  3: show_alpha      bool
  4: label           BindRef<tstr> or null
```

### 0x031A — `FormField`

Wrapper for any form input z labelem/hintem/error w jednolitej strukturze.

```
Fields:
  0: label           BindRef<tstr>
  1: hint            BindRef<tstr> or null
  2: error           BindRef<tstr> or null
  3: required        bool
  4: child           Component                        // actual input
  5: layout          FormFieldLayout                  // "stacked" | "horizontal"
```

### 0x031B — `FormGroup`

Group of FormFields z optional collapsible section header.

```
Fields:
  0: title           BindRef<tstr> or null
  1: description     BindRef<tstr> or null
  2: collapsible     bool
  3: expanded        BindRef<bool> or null
  4: children        array<Component>                 // FormFields
  5: spacing         Spacing
```

### 0x031C — `FormSection`

Like FormGroup but heavier (z divider + bigger heading).

```
Fields:
  0: title           BindRef<tstr>
  1: description     BindRef<tstr> or null
  2: children        array<Component>
  3: spacing         Spacing                          // default "lg"
  4: divider_top     bool                              // default true
```

### 0x031D — `Form`

Explicit form container z submit scope. Zbiera fields ze swoich descendants i emituje atomic submit event.

```
Fields:
  0: children        array<Component>                 // form fields + sections + actions
  1: scope_id        tstr                              // unique within panel — fields w innych Forms są ignored
  2: validators      array<FormValidator>             // form-level (cross-field) validators
  3: prevent_default_submit bool                       // block Enter submit (force explicit button click)
  4: layout          FormLayout                        // "stacked" | "horizontal" | "compact"
  5: disabled        BindRef<bool> or null              // disables all child fields
Handlers:
  "submit":          Handler                           // backend Action; params { values: map<tstr, Value>, scope_id }
  "reset":           Handler                           // local reset to initial values
  "field_change":    Handler                           // emitted on any descendant field change; params { field_id, value }

FormValidator (discriminated union):
  - { kind: "all_required", field_ids: array<tstr> }
  - { kind: "any_required", field_ids: array<tstr>, error_message: BindRef<tstr> }
  - { kind: "match", field_a: tstr, field_b: tstr }   // np. password confirmation
  - { kind: "custom", id: tstr, params: map<tstr, Value> or null }
```

Fields w obrębie `<Form>` mają access do form scope (umożliwia odwołanie po field_id w form-level validators). Submit aggreguje values po form fields w descendant tree, **wykluczając** fields z nested `Form` (sub-forms są oddzielnym scope).

---

## 6. Action (0x0400–0x04FF)

### 0x0401 — `Button`

Standard button (referenced as `Button` w innych komponentach).

```
Fields:
  0: variant         ButtonVariant
  1: tone            Tone                             // dla destructive/critical
  2: label           BindRef<tstr>
  3: icon_leading    IconRef or null
  4: icon_trailing   IconRef or null
  5: size            ButtonSize                       // "xs" | "sm" | "md" | "lg"
  6: full_width      bool
  7: disabled        BindRef<bool> or null
  8: loading         BindRef<bool> or null
  9: density         Density
Handlers:
  "click":           Handler                          // typically Backend or Both
```

### 0x0402 — `IconButton`

Button bez label, tylko icon.

```
Fields:
  0: icon            IconRef
  1: variant         ButtonVariant
  2: tone            Tone
  3: size            ButtonSize
  4: aria_label      tstr                             // required dla a11y
  5: disabled        BindRef<bool> or null
  6: loading         BindRef<bool> or null
Handlers:
  "click":           Handler
```

### 0x0403 — `ButtonGroup`

Grouped buttons z shared style.

```
Fields:
  0: buttons         array<ComponentRef<Button>>                    // each is full Button definition
  1: orientation     ButtonGroupOrientation           // "horizontal" | "vertical"
  2: attached        bool                             // visually joined
```

### 0x0404 — `LinkButton`

Looks like a link, behaves like a button.

```
Fields:
  0: label           BindRef<tstr>
  1: icon_leading    IconRef or null
  2: icon_trailing   IconRef or null
  3: tone            Tone
  4: underline       LinkUnderline                    // "always" | "hover" | "never"
Handlers:
  "click":           Handler
```

### 0x0405 — `Link`

Standard text link. **NIE ma raw `href`** — link wykorzystuje handlers + Command pipeline (zgodne z security model). Renderowany jako `<a>` z `href="#"` + click handler która emituje backend Action lub local Navigate.

```
Fields:
  0: label           BindRef<tstr>
  1: underline       LinkUnderline                    // "always" | "hover" | "never"
  2: tone            Tone
  3: leading_icon    IconRef or null
  4: trailing_icon   IconRef or null
Handlers:
  "click":           Handler                          // Backend(action_id) → addon zwraca Command::NavigateExternal (validated przez core URL pipeline) lub LocalAction::Navigate dla intra-addon
```

External URLs są **zawsze** validated przez `Command::NavigateExternal` pipeline (protokół §13.2). Brak shortcut'u w komponencie Link.

### 0x0406 — `MenuButton`

Button z dropdown menu.

```
Fields:
  0: trigger_label   BindRef<tstr> or null            // null → use icon
  1: trigger_icon    IconRef or null
  2: trigger_variant ButtonVariant
  3: items           array<MenuItem>                  // { id, label, icon?, badge?, danger?, disabled?, divider? }
  4: placement       MenuPlacement                    // "bottom_start" | "bottom_end" | ...
Handlers:
  "select":          Handler                          // params { id }
```

### 0x0407 — `Menu`

Standalone menu (typically inside Popover).

```
Fields:
  0: items           array<MenuItem>
  1: search          bool                             // searchable items
Handlers:
  "select":          Handler
```

### 0x0408 — `ActionBar`

Bar of actions (right-aligned typically).

```
Fields:
  0: leading_actions array<ComponentRef<Button>>                    // typically left
  1: trailing_actions array<ComponentRef<Button>>                   // right
  2: divider_between bool
  3: sticky          bool                             // sticks to bottom of container
```

### 0x0409 — `SegmentedControl`

Toggle-like multi-option selector.

```
Fields:
  0: bind_path       StatePath
  1: options         array<SegmentOption>             // { value, label, icon? }
  2: size            SegmentSize
  3: full_width      bool
Handlers:
  "change":          Handler
```

### 0x040A — `FilterChips`

Row of selectable chips (search filter).

```
Fields:
  0: chips           array<FilterChipDef>             // { id, label, icon?, badge?, count? }
  1: selected_ids    StatePath                        // array<tstr>
  2: mode            FilterChipsMode                  // "single" | "multi"
  3: clearable       bool
Handlers:
  "change":          Handler                          // params { selected_ids }
```

### 0x040B — `WizardFooter`

Navigation footer dla wizardów (prev/next/cancel).

```
Fields:
  0: back_action     ComponentRef<Button> or null
  1: next_action     ComponentRef<Button> or null
  2: cancel_action   ComponentRef<Button> or null
  3: skip_action     ComponentRef<Button> or null
  4: extra_actions   array<ComponentRef<Button>>                    // left side
```

### 0x040C — `Fab`

Floating action button.

```
Fields:
  0: icon            IconRef
  1: tone            Tone
  2: size            FabSize                          // "sm" | "md" | "lg"
  3: position        FabPosition                      // "bottom_right" | "bottom_left" | "inline"
  4: label           BindRef<tstr> or null            // extended FAB if set
Handlers:
  "click":           Handler
```

---

## 7. Feedback (0x0500–0x05FF)

### 0x0501 — `Alert`

Inline alert message.

```
Fields:
  0: tone            Tone
  1: variant         AlertVariant                     // "default" | "filled" | "outlined" | "soft"
  2: icon            IconRef or null
  3: title           BindRef<tstr> or null
  4: message         BindRef<tstr>
  5: actions         array<ComponentRef<Button>> or null            // inline actions
  6: dismissible     bool
Handlers:
  "dismiss":         Handler
```

### 0x0502 — `Banner`

Full-width attention bar (typowo na górze strony).

```
Fields:
  0: tone            Tone
  1: icon            IconRef or null
  2: message         BindRef<tstr>
  3: action          ComponentRef<Button> or null
  4: dismissible     bool
  5: position        BannerPosition                   // "top" | "inline"
```

### 0x0503 — `Callout`

Inline note (lighter than Alert).

```
Fields:
  0: tone            Tone
  1: icon            IconRef or null
  2: title           BindRef<tstr> or null
  3: content         array<Component>                 // body — paragraphs, lists
```

### 0x0504 — `Toast`

(Generated via Command::Toast, ale jako embedded inline display:)

```
Fields:
  0: tone            Tone
  1: title           BindRef<tstr>
  2: body            BindRef<tstr> or null
  3: icon            IconRef or null
  4: action_label    tstr or null
  5: action_id       tstr or null
```

### 0x0505 — `Hint`

Subtle help text.

```
Fields:
  0: content         BindRef<tstr>
  1: icon            IconRef or null
  2: tone            Tone                             // typowo "muted"
```

### 0x0506 — `Skeleton`

Loading placeholder.

```
Fields:
  0: variant         SkeletonVariant                  // "text" | "circle" | "rectangle" | "card" | "table_row"
  1: width           DimensionToken or null
  2: height          DimensionToken or null
  3: animate         bool
  4: lines           u8                               // dla "text" variant
```

### 0x0507 — `Spinner`

Loading spinner.

```
Fields:
  0: size            SpinnerSize                      // "xs" | "sm" | "md" | "lg" | "xl"
  1: tone            Tone
  2: label           BindRef<tstr> or null
  3: variant         SpinnerVariant                   // "default" | "ring" | "dots" | "bars"
```

### 0x0508 — `LoadingBar`

Top-of-page progress indicator.

```
Fields:
  0: visible         BindRef<bool>
  1: progress        BindRef<f32> or null             // null → indeterminate
  2: tone            Tone
```

### 0x0509 — `Modal`

Modal dialog (always w overlay slot).

```
Fields:
  0: title           BindRef<tstr>
  1: subtitle        BindRef<tstr> or null
  2: body_slot       tstr                             // gdzie addon wstawi content
  3: footer_slot     tstr or null
  4: size            ModalSize                        // "xs" | "sm" | "md" | "lg" | "xl" | "fullscreen"
  5: dismissible     bool                             // close on backdrop click / Esc
  6: prevent_scroll  bool                             // body scroll lock
  7: closable        bool                             // X button
Handlers:
  "close":           Handler
```

### 0x050A — `Drawer`

Side panel (slides in from edge).

```
Fields:
  0: side            DrawerSide
  1: size            DrawerSize                       // "xs" | "sm" | "md" | "lg" | "xl"
  2: title           BindRef<tstr> or null
  3: body_slot       tstr
  4: footer_slot     tstr or null
  5: dismissible     bool
Handlers:
  "close":           Handler
```

### 0x050B — `Popover`

Floating panel anchored to component.

```
Fields:
  0: anchor_id       tstr                             // component id this popover anchors to
  1: body_slot       tstr
  2: placement       PopoverPlacement                 // 12 placements
  3: dismissible     bool                             // close on outside click
  4: arrow           bool
```

### 0x050C — `Sheet`

Bottom sheet (mobile-style, mounted in overlay).

```
Fields:
  0: title           BindRef<tstr> or null
  1: body_slot       tstr
  2: footer_slot     tstr or null
  3: detents         array<SheetDetent>               // ["small", "medium", "large", "full"]
  4: current_detent  BindRef<tstr> or null
  5: dismissible     bool
```

### 0x050D — `GateScreen`

Full-screen permission/auth gate.

```
Fields:
  0: icon            IconRef
  1: title           BindRef<tstr>
  2: message         BindRef<tstr>
  3: actions         array<ComponentRef<Button>>
  4: variant         GateVariant                      // "auth_required" | "permission_denied" | "rate_limited" | "maintenance"
```

### 0x050F — `OfflineBanner`

Specialized banner shown when connection lost.

```
Fields:
  0: message         BindRef<tstr>                    // default localized "Brak połączenia"
  1: action_label    BindRef<tstr> or null            // "Spróbuj ponownie"
  2: reconnecting    BindRef<bool>                    // shows spinner if true
Handlers:
  "retry":           Handler
```

### 0x050E — `ConfirmationDialog`

Specialized Modal dla destructive confirmations.

```
Fields:
  0: title           BindRef<tstr>
  1: message         BindRef<tstr>
  2: icon            IconRef or null
  3: tone            Tone                             // typowo "critical"
  4: confirm_label   BindRef<tstr>
  5: cancel_label    BindRef<tstr>
  6: destructive     bool                              // visual emphasis
  7: require_typing  tstr or null                     // user must type this string to confirm
Handlers:
  "confirm":         Handler
  "cancel":          Handler
```

---

## 8. Specialized (0x0600–0x06FF)

Komponenty wymagające custom rendering (canvas, video, 3D, maps, code editor).

### 0x0601 — `Canvas2D`

Generic Canvas2D rendering surface z draw commands.

```
Fields:
  0: width_px        u16                              // pixel buffer width
  1: height_px       u16                              // pixel buffer height
  2: density         f32                              // devicePixelRatio override or "auto"
  3: background_token BackgroundToken
  4: cursor          CursorToken
  5: commands_path   StatePath                        // array<DrawCommand>
  6: image_pool      array<ImageDef>                  // { id, ref }  signed_url_refs preloaded
  7: interactive     bool
  8: tools           array<CanvasTool>                // { id, label, icon, cursor? }
  9: active_tool_path StatePath or null               // bound tool id
  10: hit_test_mode  HitTestMode                      // "exact" | "bbox" | "none"
Handlers:
  "pointer_down":    Handler                          // params { x, y, button, modifiers }
  "pointer_move":    Handler
  "pointer_up":      Handler
  "wheel":           Handler                          // params { dx, dy }
  "key_down":        Handler                          // when focused

DrawCommand (enum):
  - { kind: "rect", x: f32, y: f32, w: f32, h: f32, fill?: Tone, stroke?: Tone, stroke_width?: f32, radius?: f32 }
  - { kind: "circle", cx: f32, cy: f32, r: f32, fill?: Tone, stroke?: Tone }
  - { kind: "line", x1, y1, x2, y2, stroke: Tone, stroke_width: f32, dash?: array<f32> }
  - { kind: "polyline", points: array<{x,y}>, stroke: Tone, stroke_width: f32, closed: bool }
  - { kind: "text", x, y, content: tstr, style: TextStyle, fill: Tone, align?: TextAlign }
  - { kind: "image", id: tstr, x, y, w, h, opacity?: f32 }
  - { kind: "group", id: tstr, children: array<DrawCommand>, transform?: Transform }
  - { kind: "clip", path: ClipPath, children: array<DrawCommand> }
  - { kind: "gradient", ... }
```

### 0x0602 — `WebGLSurface`

WebGL2 rendering surface dla custom 3D/2D shaders. Ograniczona w v1 — addon dostarcza shaders i geometrię.

```
Fields:
  0: width_px        u16
  1: height_px       u16
  2: density         f32
  3: webgl_version   WebGLVersion                    // "1" | "2"
  4: clear_color     ColorToken
  5: scene_path      StatePath                        // bound scene description
  6: shaders         array<ShaderDef>                // { id, vertex_glsl, fragment_glsl, uniforms_schema }
  7: framerate_target u8                              // 30 | 60 | 120
  8: render_mode     RenderMode                      // "continuous" | "on_demand" | "interaction_only"
  9: interactive     bool
Handlers:
  "pointer_down":    Handler
  "pointer_move":    Handler
  "wheel":           Handler
  "frame":           Handler                          // emitted before each render — addon updates uniforms
```

**Bezpieczeństwo i limity (enforce'owane przez frontend):**
- GLSL shaders walidowane przez ANGLE shader validator (parse + statyczna analiza). Loop counters MUSZĄ mieć compile-time bounded iterations (no unbounded `while(true)`) — wymagane przez WebGL2 spec, dodatkowo wymuszane przez `loop_iteration_limit = 256` per loop.
- Max textures per surface: 16. Max texture dimensions: 4096×4096. Max draw calls per frame: 1024.
- Max buffer size: 32 MB per surface.
- Frame rate cap 60 default; 120 wymaga user permission `webgl.high_framerate`.
- **No readPixels access** dla addon code — frontend wykonuje rendering ale nie eksponuje pixel buffer addonowi (chroni przed canvas fingerprinting cross-content).
- WebGL extensions: tylko whitelisted (`EXT_color_buffer_float`, `OES_texture_float`, `WEBGL_compressed_texture_*`). Inne wymagają per-extension permission.
- GPU watchdog: jeśli render frame > 100 ms → reset context, emituje `stream_error` event.
- WebGL context loss handled gracefully (re-init on demand z `frame` event).
- Brak cross-origin texture loading — wszystkie images muszą być z host `signed_url_ref`.

### 0x0603 — `WGPUSurface`

WebGPU rendering surface — najnowsza GPU API, dla compute + advanced graphics. Ograniczona w v1.

```
Fields:
  0: width_px        u16
  1: height_px       u16
  2: density         f32
  3: clear_color     ColorToken
  4: pipelines       array<WGPUPipelineDef>          // render i compute pipelines
  5: bind_groups_path StatePath                      // bind groups state
  6: framerate_target u8                              // 30 | 60 | 120
  7: render_mode     RenderMode                      // "continuous" | "on_demand" | "interaction_only"
  8: required_features array<WGPUFeature>            // tylko explicit declared
  9: interactive     bool
Handlers:
  "frame":           Handler
  "pointer_down":    Handler
  "pointer_up":      Handler
  "pointer_move":    Handler
  "wheel":           Handler
```

**Bezpieczeństwo i limity:**
- WGSL shaders walidowane przez naga (Rust WGSL validator embedded w frontend). Static analysis: bounded loops, no unbounded recursion, no infinite control flow.
- Max bind groups: 4. Max bind group entries: 8. Max texture dimensions: 4096×4096.
- Max compute dispatch dimensions: 65535 per axis.
- Max storage buffer per pipeline: 16 MB.
- WebGPU adapter request gated user permission `webgpu.use` (per origin).
- **Unsupported features → reject** (no silent fallback). Jeśli adapter nie wspiera deklarowanej feature, surface emituje `stream_error` z code.
- `timestamp-query` feature wymaga osobnej permission `webgpu.timing` — risk side-channel timing leaks. Default off.
- `shader-f16` permission `webgpu.f16`.
- Compute pipelines: max 8 dispatch calls per `frame` event, max dispatch size 65535×65535×64.
- GPU watchdog: 100 ms timeout per frame. Repeated timeouts → context loss + addon notified.
- Brak `mapAsync` z host pamięci do addona — addon nie czyta GPU buffers bezpośrednio.

### 0x0604 — `VideoStream`

MSE-based fMP4 video player.

```
Fields:
  0: stream_id       BindRef<tstr>                    // stream_id z host (StreamHub)
  1: width_px        u16 or null                     // null → fluid
  2: aspect_ratio    AspectRatio
  3: controls        VideoControls                    // "none" | "minimal" | "full"
  4: autoplay        bool
  5: muted           bool
  6: object_fit      ImageFit
  7: poster_ref      tstr or null                    // signed_url_ref
Handlers:
  "play":            Handler
  "pause":           Handler
  "stream_error":    Handler
  "loaded":          Handler
```

### 0x0605 — `LiveCameraTile`

Specialized for surveillance camera live view (z statusem, FPS overlay, fullscreen).

```
Fields:
  0: stream_id       BindRef<tstr>
  1: camera_label    BindRef<tstr>
  2: status          BindRef<CameraStatus>           // "online" | "offline" | "buffering" | "error"
  3: fps             BindRef<f32> or null
  4: show_overlay    bool
  5: show_fullscreen_button bool
  6: aspect_ratio    AspectRatio
Handlers:
  "click":           Handler
  "fullscreen":      Handler
```

### 0x0606 — `MapView`

Geographic map (Leaflet-based abstraction).

```
Fields:
  0: center_path     StatePath                        // { lat: f64, lng: f64 }
  1: zoom_path       StatePath                        // u8 0-22
  2: tile_provider   TileProvider                     // "osm" | "mapbox" | "tile_server"
  3: tile_server_url tstr or null                    // jeśli "tile_server", validated
  4: height          DimensionToken
  5: markers_path    StatePath                        // array<MapMarker>
  6: polygons_path   StatePath or null
  7: heatmap_path    StatePath or null
  8: interactive     bool                             // pan/zoom
  9: show_attribution bool
Handlers:
  "click":           Handler                          // params { lat, lng }
  "marker_click":    Handler                          // params { id }
  "zoom_end":        Handler
  "pan_end":         Handler
```

### 0x0607 — `CodeEditor`

CodeMirror-based code editor.

```
Fields:
  0: bind_path       StatePath
  1: language        tstr                             // "rust" | "javascript" | "json" | "sql" | ...
  2: read_only       bool
  3: line_numbers    bool
  4: word_wrap       bool
  5: theme           CodeEditorTheme                  // "auto" | "light" | "dark"
  6: min_height_px   u16
  7: max_height_px   u16 or null
  8: tab_size        u8                              // default 2
  9: indent_with_tabs bool
  10: bracket_matching bool
  11: autocomplete   bool
  12: linting_action_id tstr or null                  // backend linter
Handlers:
  "change":          Handler
  "blur":            Handler
  "save_shortcut":   Handler                          // Ctrl+S
```

### 0x0608 — `Terminal`

Read-only terminal (xterm.js-based, no input).

```
Fields:
  0: stream_id       BindRef<tstr>                    // stream of text chunks
  1: rows            u16
  2: cols            u16
  3: theme           TerminalTheme                    // "default" | "high_contrast" | "dim"
  4: searchable      bool
  5: copyable        bool
  6: max_buffer_lines u32                             // default 10000
```

### 0x0609 — `Audio`

Audio player.

```
Fields:
  0: src_ref         BindRef<tstr>                    // signed_url_ref
  1: controls        AudioControls                    // "minimal" | "full" | "none"
  2: autoplay        bool
  3: loop            bool
  4: variant         AudioVariant                     // "default" | "compact" | "waveform"
Handlers:
  "play":            Handler
  "pause":           Handler
  "ended":           Handler
```

### 0x060A — `IFrame`

Sandboxed iframe (dla embeddable content). Heavily restricted.

```
Fields:
  0: src             tstr                             // MUST be in addon manifest allowlist
  1: sandbox         array<IFrameSandbox>             // explicit permissions: "scripts", "forms", "popups"... default "none"
  2: width           DimensionToken
  3: height          DimensionToken
  4: title           tstr                             // a11y
  5: referrer_policy IFrameReferrerPolicy
```

**Security (strict):**
- Każdy iframe rejected unless addon manifest deklaruje `[[iframe_allowed]] src_pattern = "..."` AND admin approve'uje (per-origin).
- `src` URL przechodzi **dokładnie ten sam pipeline** co `Command::NavigateExternal` (protokół §13.2): https-only, IDNA canonicalize, no IP literals, no private networks, port policy, allowlist matching, redirect policy.
- Default sandbox = empty (no scripts, no forms). User explicit opt-in dla każdej permission.
- **Forbidden sandbox tokens (NIGDY dozwolone):** `allow-same-origin` (would break addon isolation), `allow-top-navigation` (could escape addon UI), `allow-popups-to-escape-sandbox`.
- **Allowed sandbox tokens (opt-in per addon manifest):** `allow-scripts`, `allow-forms`, `allow-popups` (sandboxed), `allow-modals`.
- Cookies/storage: zawsze partitioned (CHIPS) — iframe nie współdzieli z parent ani z innym iframem.
- CSP: `frame-src` ograniczone do allowlisted hosts. Strict `script-src` w iframe context.
- `postMessage` z iframe do parent: addon **nie** odbiera tych messages bezpośrednio. Jeśli potrzebne — frontend musi mieć explicit handler routing przez host signed message bus.
- Referrer policy: domyślnie `no-referrer`.
- Frame ancestors: iframe nie może embed'ować TentaFlow (anti-clickjacking).
- Permissions Policy: kamera/mikrofon/geolocation default `none`, wymaga per-feature permission.

### 0x060B — `ImageGallery`

Grid of images z lightbox.

```
Fields:
  0: images_path     StatePath                        // array<{ id, ref, alt, caption? }>
  1: columns         u8                              // 1-6
  2: aspect_ratio    AspectRatio
  3: gap             Spacing
  4: lightbox        bool
  5: lazy_load       bool
Handlers:
  "image_click":     Handler                          // params { id }
```

### 0x060C — `Carousel`

Slideshow.

```
Fields:
  0: items_path      StatePath                        // array<{ id, content_template_id }>
  1: current_index_path StatePath                     // u32
  2: autoplay        bool
  3: autoplay_ms     u16
  4: loop            bool
  5: show_indicators bool
  6: show_arrows     bool
  7: gestures        CarouselGestures                 // "swipe" | "arrows_only" | "none"
```

### 0x060D — `PdfViewer`

Inline PDF viewer.

```
Fields:
  0: src_ref         tstr                             // signed_url_ref
  1: page_path       StatePath or null                // current page
  2: height          DimensionToken
  3: zoom_mode       PdfZoomMode                      // "fit_width" | "fit_height" | "actual" | "custom"
  4: searchable      bool
```

### 0x060E — `FpsCounter`

Specialized small component dla telemetry overlay.

```
Fields:
  0: source_path     StatePath                        // f32
  1: variant         FpsVariant                       // "minimal" | "detailed"
  2: history_secs    u8                              // sparkline history
```

### 0x060F — `StepProgress`

Visual stepper for wizards.

```
Fields:
  0: steps           array<StepDef>                   // { id, label, status: "pending"|"current"|"complete"|"error"|"skipped" }
  1: current_id_path StatePath
  2: variant         StepProgressVariant              // "horizontal" | "vertical" | "compact"
  3: clickable_completed bool
Handlers:
  "step_click":      Handler
```

### 0x0611 — `VirtualizedLog`

Long-stream structured event log (różni się od Terminal — typed events, filterable, color-coded by level).

```
Fields:
  0: events_path     StatePath                        // array<LogEvent>
  1: variant         LogVariant                        // "compact" | "default" | "expanded"
  2: max_buffer_events u32                             // default 10000, FIFO trim
  3: auto_scroll     bool                              // tail mode
  4: searchable      bool
  5: filter_levels   array<LogLevel>                  // visible levels
  6: show_timestamps bool
  7: show_source     bool                              // origin column
  8: copyable        bool                              // line copy
  9: height          DimensionToken                    // schema w §1.5; default {kind:"full"}
  10: max_height     DimensionToken or null
  11: density        Density
Handlers:
  "event_click":     Handler                           // params { event_id }
  "scroll_top":      Handler                           // dla load-older infinite scroll
  "filter_change":   Handler

LogEvent (inline struct):
  id: tstr
  ts_ms: i64
  level: LogLevel                                      // "trace" | "debug" | "info" | "warn" | "error" | "fatal"
  source: tstr or null
  message: BindRef<tstr>
  details: map<tstr, Value> or null                    // expandable extra data
  trace_id: tstr or null

LogLevel (enum, tstr):
  "trace" | "debug" | "info" | "warn" | "error" | "fatal"
```

Performance: virtualized scrolling (rendered only visible rows + ~10 buffer), efficient append, copy-on-write event buffer.

### 0x0610 — `Stopwatch`

Live timer display.

```
Fields:
  0: started_at_path StatePath                        // ts_ms or null
  1: variant         StopwatchVariant                 // "seconds" | "minutes" | "hours" | "full"
  2: tone            Tone
```

### 0x0612 — `AudioCapture`

Mikrofon w panelu addona. Nagrywa wypowiedź użytkownika (push-to-talk lub VAD),
uploaduje gotowy WAV przez kanał document-upload i emituje `action_id` z
referencją nagrania (bajty audio NIGDY nie jadą w evencie). Host wymaga
uprawnienia `audio.capture` — addon bez grantu nie dostaje audio (fail-closed).

```
Fields:
  0: action_id       tstr                             // backend action z params {doc_ref, mime, sample_rate, duration_ms, size, language_hint?}
  1: mode            AudioCaptureMode                 // "push_to_talk" | "vad"
  2: silence_ms      u16 or null                      // VAD: cisza kończąca wypowiedź (default 800)
  3: min_speech_ms   u16 or null                      // VAD: min. długość mowy (default 200)
  4: language_hint   tstr or null                     // ISO-639-1, przekazywane w params
  5: recording_path  StatePath or null                // renderer zapisuje tu bool `recording`
  6: disabled        BindRef or null
```

---

## 9. Domain-specific (0x0700–0x07FF)

Komponenty specyficzne dla TentaFlow lub klas addonów. Tag space available dla future expansion.

### 0x0701 — `PermissionMatrix`

Grid permissions × roles.

```
Fields:
  0: permissions     array<PermissionDef>             // { id, label, description }
  1: roles           array<RoleDef>                   // { id, label, color? }
  2: grants_path     StatePath                        // array<{ permission_id, role_id, granted }>
  3: editable        bool
  4: bulk_actions    array<ComponentRef<Button>>
Handlers:
  "cell_toggle":     Handler                          // params { permission_id, role_id }
  "bulk_apply":      Handler
```

### 0x0702 — `NetworkRuleEditor`

Specjalizowany dla manifestowych network rules.

```
Fields:
  0: rules_path      StatePath
  1: editable        bool
  2: show_approval_status bool
Handlers:
  "add_rule":        Handler
  "remove_rule":     Handler
  "approve_rule":    Handler
```

### 0x0703 — `RelationGraph`

Graph visualization (D3-based) dla Contacts/CRM.

```
Fields:
  0: nodes_path      StatePath                        // array<{ id, label, type, icon? }>
  1: edges_path      StatePath                        // array<{ source_id, target_id, label?, weight? }>
  2: layout          GraphLayout                      // "force_directed" | "hierarchical" | "radial" | "manual"
  3: interactive     bool
  4: max_nodes       u32                              // cap dla performance
Handlers:
  "node_click":      Handler
  "edge_click":      Handler
```

### 0x0704 — `AlarmFeed`

Real-time alarm/event feed.

```
Fields:
  0: items_path      StatePath                        // array<AlarmItem>
  1: max_visible     u16
  2: auto_scroll     bool
  3: filterable      bool
  4: filter_tones    array<Tone>                      // selected filters
```

### 0x0705 — `WeeklyScheduleGrid`

Calendar week view z events.

```
Fields:
  0: week_path       StatePath                        // start date "YYYY-MM-DD"
  1: events_path     StatePath                        // array<{ id, start_ts, end_ts, title, tone?, all_day? }>
  2: time_range      ScheduleTimeRange                 // start/end hour (default 7-22)
  3: time_step_min   u16                              // grid resolution
  4: editable        bool
Handlers:
  "event_click":     Handler
  "slot_click":      Handler                          // params { date, time }
  "event_drop":      Handler                          // drag-drop reschedule
```

### 0x0706 — `AccessMatrix`

Cells of users × resources access grants.

```
Fields:
  0: resources       array<ResourceDef>               // { id, label, category? }
  1: subjects        array<SubjectDef>                // { id, label, kind: "user"|"role"|"group", avatar? }
  2: grants_path     StatePath                        // array<{ resource_id, subject_id, level: AccessLevel }>
  3: levels          array<AccessLevel>               // "read"|"write"|"admin"|"none" (per addon definition)
  4: editable        bool
  5: bulk_actions    array<ComponentRef<Button>>
Handlers:
  "cell_change":     Handler                          // params { resource_id, subject_id, level }
  "bulk_apply":      Handler
```

### 0x0707 — `ReqCard`

Specialized card dla "requirement" displays (mockup #58 area).

```
Fields:
  0: title           BindRef<tstr>
  1: status          BindRef<ReqStatus>                // "pending" | "in_progress" | "blocked" | "complete"
  2: assignee        AvatarRef or null
  3: due_date        BindRef<tstr> or null
  4: priority        BindRef<ReqPriority>             // "low" | "medium" | "high" | "critical"
  5: tags            array<InlineChip>
  6: progress        BindRef<f32> or null
Handlers:
  "click":           Handler
```

### 0x0708 — `DecisionRow`

Vertical row z multiple option cards.

```
Fields:
  0: prompt          BindRef<tstr>
  1: options         array<DecisionOption>            // { id, icon, title, description, tone? }
  2: bind_path       StatePath                        // selected option id
  3: layout          DecisionLayout                   // "cards" | "list" | "compact"
Handlers:
  "select":          Handler
```

### 0x0709 — `Inbox`

Notification/message inbox feed.

```
Fields:
  0: items_path      StatePath                        // array<InboxItem>
  1: unread_count_path StatePath
  2: groupable       bool                             // group by date
  3: item_template_id tstr
Handlers:
  "item_click":      Handler
  "mark_read":       Handler
```

### 0x070A — `RuntimeStatusGrid`

Dashboard tile grid dla runtime stats (mockup).

```
Fields:
  0: items_path      StatePath                        // array<{ label, value, format?, tone?, link? }>
  1: columns         u8
  2: variant         StatusGridVariant                 // "compact" | "default" | "comfortable"
```

---

## 10. Walidacja per komponent

Każdy komponent jest validated po stronie core'a (decoded payload) i frontend (defensive). Validation rules:

1. **Tag known:** unknown tag → `UnknownPayloadTag`
2. **Required fields present:** missing required field → `MissingRequiredField` z lista pól
3. **Field types match:** wrong type → `TypeMismatch`
4. **Enum values valid:** unknown enum value → `InvalidToneVariant` / `InvalidIcon` / etc.
5. **Range checks:** numeric fields out of declared range (np. column count) → `Validation` error
6. **String length:** max 16384 bytes (z server_limits)
7. **Array length:** max declared per field (np. Header.meta_chips ≤ 6, BulletList.items ≤ 100)
8. **Nested depth:** rekursywne komponenty (Card.children) liczone do `max_component_depth`
9. **Handler eligibility:** EventKind nie deklarowany przez ten component tag → reject
10. **Local capability:** każdy LocalAction w handler tree sprawdzany przeciw `declared_local_capabilities`
11. **Bind paths valid:** StatePath w bind/handler referencing root key out of allowed set (per §10.3.1 protokołu) → reject

## 11. Catalog versioning

`catalog_version: u32` jest częścią protocol handshake (`Capability`). v1 catalog ma `catalog_version = 1`. Future additions:

- Dodanie nowego komponentu → bump catalog_version, fields existing components bez zmian
- Dodanie nowego pola do existing component → bump catalog_version, field MUST be optional
- **Usunięcie komponentu lub zmiana semantyki** → bump major (catalog_version 2) — wymaga protokół_version bump też
- **Enum variant addition** → bump catalog_version, addons referring to old enums unaffected

Wszystkie SDK MUSZĄ implementować catalog_version compatibility check przy handshake.

## 12. Component catalog hash

Catalog ma deterministyczny **wire-equivalent representation** który jest źródłem prawdy dla hashu (nie sama treść markdown, która może zawierać prozę, komentarze, formatowanie).

**Canonical catalog manifest** (osobny artefakt generowany przez `tentaflow-sdk-gen`):

- Format: **CBOR Core Deterministic Encoding** (RFC 8949 §4.2.1, ten sam profil co protokół §2.1)
- Zawartość: pełna lista komponentów z tag, name, field schema (typed), allowed handler events, accessibility constraints — wszystko jako structured data
- Excluded: markdown prose, comments, examples
- Field ordering w manifeście: integer keys per komponent sorted ascending, components sorted by tag

**Hash computation:**
- Sender encode'uje catalog manifest CBOR
- `catalog_hash = SHA-256(canonical_cbor_bytes)`
- `catalog_hash` jest `bstr 32` (raw bytes, nie hex)

**Handshake usage:**

```
Capability {
  name: "ui_v1",
  version: 1,                                            // catalog_version
  hash: bstr 32,                                         // SHA-256 catalog manifest
  params: null
}
```

Wysyłany w `ProtocolHello.capabilities_requested[]`. Core porównuje z własnym (server-known) hash dla `catalog_version`. Mismatch → `ProtocolWelcome.capabilities_rejected[] = [{ capability: "ui_v1", reason: "catalog_hash_mismatch" }]`. Klient kończy lub fallback (jeśli accept downgrade).

**Source of canonical manifest:** generated z `tentaflow-sdk-spec` Rust crate przez `tentaflow-sdk-gen --emit catalog-manifest`. To ten sam artefakt który consume'ują wszystkie language SDKs. Manifest jest commitowany do repo (`tentaflow-sdk-spec/catalog-manifest/v1.cbor`) i versioned.

## 13. Residual risks (accepted, do egzekwowania w implementacji)

Wszystkie blockers ABI z poprzednich rund codex review **rozwiązane w v0.3**:

- ✅ Cross-component types — explicit common types w §1.5
- ✅ Brakujące komponenty: `Form` (0x031D), `VisuallyHidden` (0x0225), `LiveRegion` (0x0226), `VirtualizedLog` (0x0611), `OfflineBanner` (0x050F) dodane
- ✅ Field-level ValidationRule (§1.5) explicit
- ✅ EventPayload per EventKind (§1.55) explicit
- ✅ Schema "(jak X)" shortcuts rozpisane do pełnych field lists
- ✅ ComponentRef<X> convention precyzyjna
- ✅ Catalog hash mechanism (§12) precyzyjny

Pozostałe residual risks (jawne, do egzekwowania przez codegen i validator, nie blokujące implementacji):

1. **Canonical manifest jest źródłem prawdy.** Ten dokument (markdown) jest **specyfikacją czytelną dla człowieka** — wartość binding ma `tentaflow-sdk-spec` Rust crate + wygenerowany przez `tentaflow-sdk-gen` `catalog-manifest/v1.cbor`. W każdym konflikcie dokument vs manifest → **manifest wygrywa**. Manifest jest commitowany do repo i versioned.

   **Stan w repo (2026-05-21):** stub directory `tentaflow-sdk-spec/` (z `README.md` + pusty `catalog-manifest/`) został utworzony. Pełny crate + wygenerowany `v1.cbor` powstaje w pierwszej iteracji implementacji Fazy 6 (krok 3 `Następne kroki`). Markdown spec (ten dokument) jest autoritative do momentu gdy manifest zostanie wygenerowany — wtedy autoritative source jest manifest.
2. **WebGL/WGPU/Canvas2D enforcement po stronie frontend:** Core nie ma pełnego GPU stacku do walidacji shader limits — frontend (jako trusted execution environment dla rendering, ale untrusted dla user data input) enforce'uje. Mitygacja: shadery są validated po obu stronach (core robi parsing/static analysis przez naga/ANGLE bindings przed dispatch, frontend egzekwuje runtime limits). User-supplied data idąca przez addon→shader uniforms jest tylko czytana przez addon, nie pisana po frontend boundary.
3. **DNS rebinding TOCTOU** dla `NavigateExternal` — accepted residual z opcjonalnym `proxy_mode = "always"` w manifeście jako mitygacja. Default `proxy_mode = "off"` (browser-side resolution).
4. **Component count w tabeli §0.** Tabela podaje liczby — autoritative count jest w canonical manifest (`tentaflow-sdk-gen --emit catalog-stats`). Markdown tabela jest informational i może drift'ować +/-3 — implementator referuje do manifestu, nie tabeli.

## 14. Następne kroki

1. Codex review tego dokumentu
2. Iteracja → v1.0
3. Generation z tej spec do `tentaflow-sdk-spec` Rust crate (atrybuty `#[derive(SdkType)]`)
4. Implementacja frontend renderer dla każdego komponentu (mapping tag → DOM render function)
5. Test suite: każdy komponent ma roundtrip test (encode CBOR → validate → decode → assert structure)
