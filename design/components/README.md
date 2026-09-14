# Katalog komponentów

Jeden plik = jedna kontrolka. Każdy spec opisuje *docelowe* zachowanie wyprowadzone
z tokenów (`../tokens/tokens.json`) i konfrontuje je z tym, co dziś realnie robi
`tentaflow-core/www/js/components/tf-*.js` + `www/css/controls.css` (implementacja
HTML) oraz `docs/ADDON_UI_COMPONENT_CATALOG_v1.md` (protokół UI dla addonów, drugi,
częściowo niezależny system — kontekst w `../MIGRATION.md`). Rozjazdy między
specem a żywym kodem są spisane w sekcji „Znane odstępstwa” każdego pliku, nie
zamiatane pod dywan.

Zanim zaczniesz nową stronę, sprawdź [`../patterns/new-page-checklist.md`](../patterns/new-page-checklist.md)
— kontrolki poniżej to budulec, nie gotowe strony. Szablon do kopiowania:
[`_TEMPLATE.md`](_TEMPLATE.md).

Kolumny w tabelach:
- **Spec** — link do pliku w tym katalogu, albo „—” gdy jeszcze nie napisany.
- **HTML** — nazwa custom elementu `tf-*` w `tentaflow-core/www/js/components/`,
  albo „—” gdy komponent nie ma dziś dedykowanego elementu (np. renderowany
  bezpośrednio jako markup strony).
- **Protokół** — tag `0x0…` z `docs/ADDON_UI_COMPONENT_CATALOG_v1.md`, albo „—”
  gdy komponent istnieje tylko po stronie HTML (jeszcze nieopisany w protokole
  addonów) albo jest prymitywem bez własnego tagu (np. ikona).
- **Natywny** — status w `tentaflow-ui-native`/`tenta_ui_widgets`. Dziś zawsze
  „planowany” — natywny silnik jeszcze nie istnieje (`docs/TENTAENGINE_INTEGRATION_PLAN.md`).

## Tier 0 — MVP

Komplet potrzebny, by złożyć pięć reprezentatywnych stron z audytu UI (lista+
gauge+wizard, formularz z tabami, czat, dashboard, widok 3D/kamera).

| Komponent | Spec | HTML | Protokół | Natywny | Uwagi |
|---|---|---|---|---|---|
| Button | [button.md](button.md) | `tf-button` | `0x0401` | planowany | + `tf-icon-button`/`0x0402` (icon-only) |
| Input (text) | [input.md](input.md) | `tf-input` | `0x0301` | planowany | multiline też tu, patrz Tier 1 `Textarea` |
| Select | [select.md](select.md) | `tf-select` | `0x0303` | planowany | + combobox/multiselect, Tier 1 |
| Checkbox | [checkbox.md](checkbox.md) | `tf-checkbox` | `0x030B` | planowany | + radio/radio-group, Tier 1 |
| Toggle / switch | [toggle.md](toggle.md) | `tf-toggle` | `0x030A` | planowany | + segmented control, Tier 1 |
| Badge | [badge.md](badge.md) | `tf-badge` | `0x020A` | planowany | + status-pill, Tier 1 |
| Chip | [chip.md](chip.md) | `tf-chip` | `0x020B` | planowany | + filter-chips/tag-input, Tier 1 |
| Tooltip | [tooltip.md](tooltip.md) | `tf-tooltip` | `0x010F` | planowany | |
| Avatar | [avatar.md](avatar.md) | `tf-avatar` | `0x020D` | planowany | |
| Icon | [icon.md](icon.md) | — (sprite `www/img/icons.svg`) | — | planowany | prymityw `IconRef`, nie osobny tag komponentu |
| Card / panel | [card.md](card.md) | `tf-section-card` | `0x0106`/`0x0107` | planowany | `Card` (surowy) + `SectionCard` (z nagłówkiem) |
| Tabs | [tabs.md](tabs.md) | `tf-tabs` | `0x010B` | planowany | |
| Modal / dialog | [modal.md](modal.md) | `tf-modal` (+ `tf-window`) | `0x0509` | planowany | dwa komponenty HTML, jeden spec |
| Table | [table.md](table.md) | `tf-table` | `0x0211` | planowany | sortowanie, paginacja, `hide-below` |
| List | [list.md](list.md) | `tf-list` | `0x0212` | planowany | |
| Gauge | [gauge.md](gauge.md) | `tf-gauge` | `0x021C` | planowany | circular/arc/semi |
| Sparkline | [sparkline.md](sparkline.md) | `tf-sparkline` | `0x0215` | planowany | |
| Stat card | [stat-card.md](stat-card.md) | `tf-stat-card` | `0x0208`/`0x0209` | planowany | `StatCard` + prymityw `Stat` |
| Empty state | [empty-state.md](empty-state.md) | `tf-empty-state` | `0x0003` | planowany | |
| Toast | [toast.md](toast.md) | `tf-toast` | `0x0504` | planowany | |
| Form group | [form-group.md](form-group.md) | — (wzorzec `tf-input`/`tf-textarea` + label/hint/error) | `0x031A` (`FormField`) | planowany | brak dedykowanego custom elementu w HTML, patrz odstępstwa w spec |
| Breadcrumb | [breadcrumb.md](breadcrumb.md) | `tf-breadcrumb` | `0x0110` | planowany | |
| Chat bubble | [chat-bubble.md](chat-bubble.md) | `tf-chat-bubble` | — | planowany | funkcja hosta (`chat.js`), nie protokołu addonów |
| Chat composer | [chat-composer.md](chat-composer.md) | `tf-chat-composer` | — | planowany | jw. |

## Tier 1 — szeroki zestaw

Kolejny krok po MVP — pokrywa formularze, listy danych, wykresy kartezjańskie,
edytory tekstu/kodu i komunikację realtime.

| Komponent | Spec | HTML | Protokół | Natywny | Uwagi |
|---|---|---|---|---|---|
| Textarea | [input.md](input.md) | `tf-textarea` | `0x0302` | planowany | patrz odstępstwa: dubluje się z `tf-input[multiline]` |
| Combobox | [select.md](select.md) | `tf-combobox` | — | planowany | protokół ma tylko `Select` z polem `searchable` |
| Multiselect | [select.md](select.md) | `tf-multiselect` | — | planowany | jw. |
| Radio / RadioGroup | [checkbox.md](checkbox.md) | `tf-radio` / `tf-radio-group` | `0x030C`/`0x030D` | planowany | + `RadioCardGroup` `0x030E` |
| Segmented control | [toggle.md](toggle.md) | `tf-segmented` | `0x0409` | planowany | |
| Slider | — | `tf-slider` | `0x030F` | planowany | |
| Color input | — | `tf-color-input` | `0x0319` (`ColorPicker`) | planowany | |
| Pin input | — | `tf-pin-input` | — | planowany | |
| Searchbox | — | `tf-searchbox` | `0x0307` | planowany | |
| File input | — | `tf-file-input` | `0x0318` | planowany | |
| Tag input | [chip.md](chip.md) | `tf-tag-input` | — | planowany | najbliższy krewny w protokole: `MentionInput` `0x0309` |
| Mention input | — | `tf-mention-input` | `0x0309` | planowany | |
| Filter chips | [chip.md](chip.md) | `tf-filter-chips` | `0x040A` | planowany | |
| Menu | — | `tf-menu` | `0x0407` | planowany | + `MenuButton` `0x0406` |
| Command palette | — | `tf-command-palette` | — | planowany | |
| Datepicker | — | `tf-datepicker` | `0x0314` | planowany | |
| Calendar | — | `tf-calendar` | `0x0223` (`CalendarMonth`) | planowany | |
| Kanban | — | `tf-kanban` | — | planowany | |
| Tree | — | `tf-tree` | `0x0213` | planowany | |
| Key-value | — | `tf-key-value` | `0x0207` | planowany | |
| Accordion / section card | [card.md](card.md) | `tf-section-card` (collapsed mode) | `0x010E` (`Accordion`) | planowany | |
| Choice card | — | `tf-choice-card` | `0x030E` (`RadioCardGroup`, najbliższy) | planowany | |
| Skeleton | — | `tf-skeleton` | `0x0506` | planowany | |
| Spinner | — | `tf-spinner` | `0x0507` | planowany | |
| Progress bar | — | `tf-progress-bar` | `0x021D` | planowany | |
| Alert | — | `tf-alert` | `0x0501` | planowany | |
| Status pill | [badge.md](badge.md) | `tf-status-pill` | — | planowany | patrz odstępstwa w `badge.md` |
| Detail header | — | `tf-detail-header` | — | planowany | |
| Window / dialog chrome | [modal.md](modal.md) | `tf-window` | `0x0509` (`Modal`, dzielony z Modal) | planowany | `TfWindow.open`/`.confirm()` helpery |
| Line/Bar/Area/Pie chart | — | `tf-line-chart`/`tf-bar-chart`/`tf-area-chart`/`tf-pie-chart` | `0x0217`/`0x0218`/`0x0219` (+ LineChart bez osobnego numeru w tym audycie) | planowany | wspólna baza `TfCartesianChart` |
| Heatmap | — | `tf-heatmap` | `0x021B` | planowany | |
| Density plot | — | `tf-density-plot` | — | planowany | |
| Histogram | — | `tf-shot-histogram` | — | planowany | domenowo quantum, patrz Tier 2 |
| Stream chart (realtime) | — | `tf-stream-chart` | — | planowany | |
| Timeline / state-timeline / run-timeline | — | `tf-timeline`/`tf-state-timeline`/`tf-run-timeline` | `0x0210` (`Timeline`) | planowany | trzy warianty HTML, jeden tag protokołu |
| Diff viewer | — | `tf-diff` | `0x021F` | planowany | |
| Code editor | — | `tf-code-editor` | `0x0607` | planowany | własne tokenizery |
| Terminal emulator | — | `tf-terminal` | `0x0608` | planowany | |
| Mime output viewer | — | `tf-mime-output` | — | planowany | |
| Relation / flow graph editor | — | `tf-relation-graph` (+ `flows-builder/canvas.js`) | `0x0703` (`RelationGraph`) | planowany | |
| Video stream / camera tile | — | `tf-video-stream` / `tf-live-camera-tile` | `0x0604`/`0x0605` | planowany | |

## Tier 2 — domenowe / GPU

Komponenty specyficzne dla TentaQuant (obliczenia kwantowe), robotyki i
diagnostyki GPU — najbardziej wymagające renderowania (3D, voxel, duże zbiory
punktów), właściwy powód budowy natywnego silnika.

| Komponent | Spec | HTML | Protokół | Natywny | Uwagi |
|---|---|---|---|---|---|
| Voxel / 3D viewer | — | WASM (`js/voxel/voxel_glue_bg.wasm`) | — | planowany | jedyny dziś komponent skompilowany do WASM — prior art dla ścieżki GPU |
| Robot view | — | `tf-robot-view` | — | planowany | |
| Bloch sphere | — | `tf-bloch-sphere` | — | planowany | |
| Q-sphere | — | `tf-qsphere` | — | planowany | |
| Entanglement graph | — | `tf-entanglement-graph` | — | planowany | |
| Quantum circuit editor | — | `tf-quantum-circuit` | — | planowany | |
| Shot histogram | — | `tf-shot-histogram` | — | planowany | patrz też Tier 1 „Histogram” |
| GPU topology diagram | — | (moduł `gpu-topology-view.js`, brak dedykowanego `tf-*`) | — | planowany | |
| Flamegraph | — | (moduł `profile-flamegraph.js`, brak dedykowanego `tf-*`) | — | planowany | |
| Face / mascot renderer | — | `tf-face` | — | planowany | |
| Alarm card / alarm feed | — | `tf-alarm-card` / `tf-alarm-feed` | — | planowany | |
| Contrib tag | — | `tf-contrib-tag` | — | planowany | |
| FPS counter (dev/debug overlay) | — | `tf-fps-counter` | `0x060E` | planowany | |

## Cross-cutting

Nie są to komponenty same w sobie, ale infrastruktura, od której zależy każdy
tier — opisana w `../foundations/`, nie tutaj:

- token system — [`../tokens/tokens.json`](../tokens/tokens.json)
- ikonografia (130 ikon stroke, sprite) — [`../foundations/iconography.md`](../foundations/iconography.md)
- i18n (5 języków, dot-path JSON) — poza `design/`, `tentaflow-core/www/i18n/`
- a11y (focus ring, role ARIA, roving tabindex, live regions) — [`../foundations/accessibility.md`](../foundations/accessibility.md)
