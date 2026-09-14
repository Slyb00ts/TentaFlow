# Chip

| | |
|---|---|
| Tier | 0 (MVP) — `Chip`; 1 — `FilterChips`, `TagInput` |
| HTML | `<tf-chip>` — `tentaflow-core/www/js/components/tf-chip.js`, style `controls.css` linie 1554–1660 i 10038–10043; `<tf-filter-chips>` — `tf-filter-chips.js` (bez dedykowanego bloku CSS znalezionego w tym audycie, klasy `.tf-filter-chip*`); `<tf-tag-input>` — `tf-tag-input.js`, style linie 6100–6126 |
| Protokół addonów | `0x020B` `Chip` — `docs/ADDON_UI_COMPONENT_CATALOG_v1.md` §4, linie 1563–1581; `0x040A` `FilterChips` — §6, linie 2682–2694; brak osobnego tagu dla `TagInput` (patrz `0x0309 MentionInput` jako najbliższy krewny, poza zakresem) |
| Natywny (TentaEngine) | `tenta_ui_widgets::Chip` — status: planowany |
| Status dokumentu | draft |

`Chip` — status/etykieta większa niż `Badge` (`badge.md`), może być klikalna,
usuwalna albo zaznaczalna. `FilterChips` — pasek przełączników filtra (single
lub multi). `TagInput` — pole wielowartościowe, gdzie zatwierdzone wartości
stają się usuwalnymi chipami. Nie używaj `Chip` do liczników (to `Badge`) ani do
głównej akcji (to `Button`).

## Anatomia

```text
┌ tf-chip ────────────┐   ┌ tf-filter-chips (scroll) ──────────────┐
│ ● Online         [×]│   │ [Wszystkie] [Otwarte (4)] [Zamknięte]  │→
└──────────────────────┘   └───────────────────────────────────────┘
  dot 6px + gap 5px          data-overflow: none|start|end|both

┌ tf-tag-input ────────────────────────────┐
│ [tag1 ×][tag2 ×] wpisz i Enter...        │
└────────────────────────────────────────────┘
```

Części *chip*: *dot* (opcjonalna, kolor niezależny od tła przez `dot-tone`),
*leading icon* (12px), *lead slot* (np. avatar), *label*, *remove button*
(`×`, tylko z `removable`). Części *filter-chips*: rząd `<button>` z opcjonalną
ikoną i licznikiem (`.tf-filter-chip-count`), opcjonalny przycisk „wyczyść”.
Części *tag-input*: host chipów (`tf-chip[variant=tag]` w pętli) + pole
tekstowe wpisywania nowej wartości.

## Warianty

sdk-spec `ChipVariant` (`solid`/`soft`/`outline`/`removable`/`selectable`/
`toggle`) **miesza dwie różne osie** (wygląd vs zachowanie) w jednym enumie;
`tf-chip.js` odzwierciedla to jako zestaw niezależnych atrybutów, nie jeden
`variant`:

| Oś | Atrybut HTML | Wartości |
|---|---|---|
| wygląd | `status` | `ok/warn/err/info/accent/neutral/online/offline/pending/recording/scope-*` — koloruje tło+tekst (≈ `soft`) |
| wygląd | `variant="outline"` | tinted border 1px zamiast wypełnienia |
| wygląd | `variant="tag"` | statyczna etykieta bez interakcji (reużywa `.tf-tag`, patrz `Tag` `0x020C`) |
| wygląd | `mono` | czcionka `typography.family.mono`, bez uppercase/tracking — dla identyfikatorów (gałąź, hash) |
| zachowanie | `clickable` + `active` | `role="button"`, `tabindex="0"` — realizuje `selectable`/`toggle` z sdk-spec ręcznie |
| zachowanie | `removable` | dodaje przycisk `×`, emituje `remove` |

Nie ma pojedynczego atrybutu `variant="solid"` — najbliższy odpowiednik to
`status` bez `variant="outline"` (zawsze translucent tło, nigdy pełne
wypełnienie — patrz odstępstwa).

## Rozmiary

Brak `ChipSize` w sdk-spec i brak atrybutu rozmiaru w HTML — jeden rozmiar
(`padding: 3px 9px`, `font-size: 10px` domyślnie / `10.5px` w `outline`/`mono`,
patrz odstępstwa).

## Stany

| Stan | Tło | Tekst/ikona | Obramowanie | Uwagi |
|---|---|---|---|---|
| domyślny (`status`) | kolor @ 15–18% | kolor statusu | brak | np. `ok`→`success`, `err`→`critical` |
| `variant="outline"` | `bg.elevated` (neutralny) lub kolor @ 8% (ze `status`) | kolor statusu | 1px tinted | sentence-case, bez uppercase |
| `clickable` hover | `scale(1.06)` + `brightness(1.15)` | — | — | tylko `pointer: fine` |
| `active` (toggle zaznaczony) | `accent.glow` (tylko gdy `[data-sdk-chip]`, patrz odstępstwa) | `accent.primary_hover` | — | poza kontekstem SDK `active` nie ma własnego stylu w odczytanym zakresie |
| `removable` | jak wariant bazowy | + `×` button | — | `×` ma `aria-label="Remove"` na sztywno (nie i18n, patrz odstępstwa) |
| disabled | **brak stanu w kodzie** | — | — | `tf-chip` nie ma atrybutu `disabled` w ogóle |
| filter-chip aktywny | `.active` klasa | — | — | tryb `single`: tylko jeden aktywny; `multi`: niezależne przełączanie |

## Zachowanie

- **`tf-chip`**: klik na `×` (gdy `removable`) emituje `remove` i zatrzymuje
  propagację (nie wywołuje kliknięcia całego chipa); `clickable` + Space/Enter
  na zafokusowanym chipie wywołuje `.click()` (natywne zdarzenie DOM, bez
  custom eventu — konsument nasłuchuje `click` na hoście).
- **`tf-filter-chips`**: klik na chip przełącza `active` (tryb `single` czyści
  pozostałe, `multi` przełącza niezależnie); `scroll` (atrybut) trzyma chipy w
  jednej linii z poziomym przewijaniem i publikuje `data-overflow`
  (`none/start/end/both`) na kontenerze dla fade'u brzegowego w CSS; `clearable`
  dodaje przycisk czyszczenia wszystkich filtrów. **Brak obsługi klawiatury poza
  natywnym Tab między `<button>`** — brak strzałek mimo że to funkcjonalnie
  bardzo bliskie `tf-segmented` (`toggle.md`), które strzałki ma.
- **`tf-tag-input`**: Enter lub znak-separator (domyślnie `,`, konfigurowalne
  przez property `.separators`) commit wpisanej wartości jako nowy tag;
  Backspace na pustym polu usuwa ostatni tag; blur z niepustym polem też
  commit'uje (zapobiega utracie wpisanej wartości); `max-tags` blokuje dodawanie
  po limicie; `dedupe` blokuje duplikaty.
- Animacje: `tf-chip` hover — `transform`/`filter` na `motion.duration_ms.fast`
  (0.18s); kropki statusowe (`online`/`pending`/`recording`) pulsują różnymi
  okresami (2s/1.2s/1s) — `recording` najszybsza, sygnalizuje aktywne nagrywanie.
- Zdarzenia/API: `tf-chip` — atrybuty jak w tabeli wariantów wyżej + `label`
  (nadpisuje slot tekstowy), event `remove`. `tf-filter-chips` — property
  `.filters` (array `{id,label,icon?,count?,active}`), atrybuty `mode`
  (`single`/`multi`), `clearable`, `scroll`; eventy `change`
  (`detail.{id,active,filters}`), `clear`. `tf-tag-input` — property `.tags`
  (array stringów), `.separators`; atrybuty `placeholder`, `disabled`,
  `max-tags`, `dedupe`; eventy `add` (`detail.tag`), `remove`
  (`detail.{tag,index}`), `change` (`detail.tags`).
- Zaznaczanie tekstu: dozwolone na labelu chipa (nie blokowane jawnie), pole
  `tf-tag-input` — standardowe dla `<input>`.

## Dostępność

`tf-chip` z `clickable`: `role="button"`, `tabindex="0"`, focus-visible ring na
`> .tf-chip` (`controls.css:1641-1644`, selektor celuje w wewnętrzny span, nie
w host). `tf-filter-chips`: natywne `<button>` — dostępność „za darmo” (focus,
Enter/Space, czytnik ekranu). `tf-tag-input`: pole ma `role="textbox"` (zbędne —
`<input type="text">` już ma tę rolę domyślnie), usuwanie tagów przez chip `×`
dziedziczy `aria-label="Remove"` z `tf-chip` — **stały angielski tekst**, nie
przechodzi przez i18n (5 języków aplikacji, patrz `foundations/accessibility.md`).

## Responsywność i platformy

`tf-filter-chips[scroll]` implementuje świadomą strategię mobile: jedna linia
przewijalna pozioma zamiast zawijania na wiele rzędów (uzasadnienie w
komentarzu pliku: „5 filtrów w 3 rzędach kosztuje zbyt dużo pionowej
przestrzeni na telefonie”) — dobry wzorzec do skopiowania w innych paskach
filtrów. `ResizeObserver` synchronizuje `data-overflow` przy zmianie szerokości
(np. zwinięcie sidebar). Na ESP32-P4: kropki pulsujące (`online`/`recording`)
— jak w `badge.md`, koszt CPU rendererze do zweryfikowania.

## Tokeny użyte

`color.themes.dark.semantic.success/warning/critical/info`,
`color.themes.dark.accent.glow`, `color.themes.dark.accent.primary_hover`,
`color.themes.dark.bg.elevated`, `color.themes.dark.text.secondary`,
`typography.scale.overline`, `typography.family.mono`, `spacing.xxs`,
`spacing.xs`, `radius.pill`, `radius.xs` (`usage.xs` obejmuje „tags, code
chips”), `motion.duration_ms.fast`.

## Znane odstępstwa w kodzie (2026-09-14)

- **`ChipVariant` (solid/soft/outline/removable/selectable/toggle) nie istnieje
  jako jeden atrybut** — kod rozbija te sześć wartości na niezależne, częściowo
  nakładające się atrybuty (`status`+`variant="outline"`+`clickable`+`active`+
  `removable`), bez wariantu `solid` w ogóle (zawsze translucent tło).
- **`font-size: 10.5px`** w `.tf-chip--outline` (`controls.css:1570`) i
  `.tf-chip--mono` (`controls.css:10040`) — literał off-token, zakazany przez
  `tokens.json → typography.rules.half_pixel_sizes`.
- **`aria-label="Remove"` na stałe po angielsku** (`tf-chip.js:152`), niezależnie
  od aktywnego języka aplikacji — pomija warstwę i18n, w przeciwieństwie do
  `tf-filter-chips.js:97` (`aria-label="Wyczyść filtry"`, po polsku na stałe —
  odwrotny, ale równie realny problem: hard-coded string zamiast klucza i18n).
- **`.active` na `tf-chip` ma styl tylko w kontekście `[data-sdk-chip]`**
  (`controls.css:1585`) — poza rendererem protokołu addonów `clickable`+`active`
  nie ma żadnego wizualnego stylu „zaznaczony” w odczytanym zakresie CSS,
  mimo że atrybut `active` jest dodawany do klasy zawsze
  (`tf-chip.js:127`).
- **`tf-filter-chips` nie ma nawigacji strzałkami**, mimo bliskiego
  pokrewieństwa funkcjonalnego z `tf-segmented` (`toggle.md`), które ją ma.

## Przykłady

```html
<tf-chip status="online" dot>Online</tf-chip>
<tf-chip mono icon="branch">cs/piotr/9f2a1c4b</tf-chip>
<tf-chip status="info" removable label="beta"></tf-chip>

<tf-filter-chips mode="single" clearable></tf-filter-chips>
<tf-tag-input placeholder="Dodaj tag..." max-tags="10" dedupe></tf-tag-input>
```

```rust
// natywnie (tenta-ui-widgets, API docelowe)
Chip::new("Online").tone(Tone::Success).dot(true)
Chip::new("beta").tone(Tone::Info).removable().on_remove(Msg::RemoveTag)

FilterChips::new(filters).mode(FilterChipsMode::Single).on_change(Msg::FilterChanged)
TagInput::new().max_tags(10).dedupe(true).on_change(Msg::TagsChanged)
```
