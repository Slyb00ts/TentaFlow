# Stat card

| | |
|---|---|
| Tier | 0 (MVP) |
| HTML | `<tf-stat-card>` — `www/js/components/tf-stat-card.js`, style `controls.css` linie 2847-2921, 6930-6939, 8697-8704 |
| Protokół addonów | `0x0208` `StatCard` + `0x0209` `Stat` — `docs/ADDON_UI_COMPONENT_CATALOG_v1.md` §4 |
| Natywny (TentaEngine) | `tenta_ui_widgets::StatCard` — status: planowany |
| Status dokumentu | draft |

Kafelek KPI: etykieta, duża wartość, opcjonalna delta z kierunkiem. Używaj go
w siatce dashboardu (`dashboard.js`, `agents.js` — 4 kafelki w rzędzie). Nie
używaj go jako ogólnej karty z treścią — do tego jest `Card`/`SectionCard`.

## Anatomia

```text
┌──────────────────────────────────┐
│ [icon] Etykieta                  │  ← .tf-stat-card-label, TextStyle caption
│ 128 / 24                         │  ← .tf-stat-card-value, size ~20-24px + suffix
│ ↑ +12% wobec wczoraj             │  ← .tf-stat-card-delta.<up|down|warn|neutral>
└──────────────────────────────────┘
   border-left: 3px solid <accent>   (tylko gdy accent=success|danger|warning|info)
```

Trzy wiersze generowane przez `_update()`: label (z opcjonalną ikoną 14×14 z
`icons.svg`), value (+ `suffix` jako `<span class="suffix">`), delta (strzałka
Unicode + tekst). Wariant kompaktowy (atrybut `size="sm|md|lg"`) porzuca
chrom karty i renderuje `.tf-stat` / `.tf-stat__label` /
`.tf-stat__value-row` / `.tf-stat__value` — de facto inny komponent pod tym
samym tagiem, zobacz „Rozmiary”.

## Warianty

| Pole `accent` (HTML) / `Tone` (protokół) | Wygląd | Kiedy używać |
|---|---|---|
| *(brak)* | zwykła karta, bez paska | domyślny KPI bez oceny |
| `success` | pasek 3px `--tf-success` po lewej | metryka „dobra” |
| `danger` (HTML) / `critical` (protokół) | pasek `--tf-danger` | metryka krytyczna |
| `warning` | pasek `--tf-warning` | metryka ostrzegawcza |
| `info` | pasek `--tf-info` | metryka informacyjna |

`delta-type` (niezależne od `accent`): `up` (strzałka ↑, kolor success),
`down` (↓, danger), `warn` (⚠, warning), `neutral` (bez glifu, kolor
`--tf-text-3` — świadoma decyzja: „3 suites” to sam kontekst, nie trend).

Protokół dodatkowo ma `clickable: bool` (dodaje handler `"click"` i kursor
pointer, klasa `.tf-stat-card--clickable`), którego atrybut HTML
`tf-stat-card.js` **nie eksponuje** — element HTML nie jest nigdy klikalny
sam z siebie.

## Rozmiary

`tf-stat-card.js` ma dwa całkowicie różne tryby renderowania sterowane
obecnością atrybutu `size`:

- **Bez `size`** (domyślny, pełna karta): rozmiar wartości ustalony w CSS na
  ok. `1.5em`/`2em`-owej skali karty (`.tf-stat-card-value`), padding karty
  stały.
- **`size="sm"`**: `.tf-stat__value { font-size: 1rem; font-weight: 600 }`
- **`size="md"`**: `font-size: 1.5rem; font-weight: 600`
- **`size="lg"`**: `font-size: 2rem; font-weight: 700`

Wariant `sm`/`md`/`lg` odpowiada polu protokołu `StatSize` komponentu `Stat`
(0x0209) — czyli **stat-card z rozmiarem = to inny komponent protokołu
(„Stat”, nie „StatCard”)** renderowany przez ten sam tag HTML. Karta pełna
nie ma odpowiednika `sm`/`md`/`lg` — jej rozmiar nie jest tokenizowany.

## Stany

| Stan | Tło | Tekst/ikona | Obramowanie | Uwagi |
|---|---|---|---|---|
| default | `--tf-bg-card` | `--tf-text` | `--tf-border` | |
| hover | `--tf-bg-card-hover` (`.tf-stat-card:hover`) | bez zmian | bez zmian | tylko gdy karta ma `--clickable` z protokołu; HTML bez `size` też ma regułę `:hover`, nawet gdy nieklikalna |
| active / pressed | (`.tf-stat-card:active`) delikatne przyciemnienie | — | — | zdefiniowane w CSS niezależnie od `clickable` |
| focus-visible | brak dedykowanej reguły w controls.css | — | — | karta nie jest fokusowalna (nie ma `tabindex`/`role="button"` nigdzie w `tf-stat-card.js`), nawet gdy protokół oznacza ją `clickable` — luka a11y, patrz niżej |
| disabled | nie dotyczy | — | — | brak stanu disabled w komponencie |
| loading | nie dotyczy | — | — | brak wbudowanego skeletonu; strona sama pokazuje/ukrywa kartę |
| error / invalid | nie dotyczy | — | — | brak |
| selected / checked | nie dotyczy | — | — | brak |

## Zachowanie

- Interakcja wskaźnikiem: karta z `accent` reaguje wizualnie na hover/active
  (`:hover`/`:active` w CSS), ale **żaden handler kliknięcia nie jest
  dodawany przez `tf-stat-card.js`** — strony, które chcą klikalności, muszą
  same owinąć element albo dodać listener na zewnątrz. Wersja protokołu z
  `clickable: true` dodaje `.tf-stat-card--clickable` (kursor pointer,
  `border-color` na hover) i emituje handler `"click"` przez warstwę
  dispatch.
- Klawiatura: brak — karta nigdy nie jest w kolejności Tab, ani w wersji
  HTML, ani w wersji protokołu (mimo `clickable`).
- Animacje: brak deklarowanych `@keyframes`/`transition` poza standardowym
  `transition: color 0.15s` odziedziczonym z ogólnych reguł linków.
- Zdarzenia/API: atrybuty obserwowane (`observedAttributes`): `label`,
  `value`, `suffix`, `delta`, `delta-type`, `icon`, `accent`, `size`. Wartości
  są escapowane (`escapeHtml`) przed wstrzyknięciem do `innerHTML`. Istniejące
  dzieci światła DOM (dodane przed podłączeniem elementu) są zachowywane po
  wygenerowanej treści — pozwala to dopisać własny footnote/SDK-owy element
  bez utraty go przy re-renderze atrybutów.
- Zaznaczanie tekstu: dozwolone (wartość i etykieta to zwykły tekst, bez
  `user-select: none`).

## Dostępność

Karta renderuje się jako zwykły `<div>` — brak `role`, brak `aria-label`.
Wartość liczbowa i delta są zwykłym tekstem czytanym przez czytnik ekranu w
kolejności DOM (label → value → delta), co jest sensowną kolejnością
semantyczną, ale nie ma explicit `aria-live` — zmiana wartości (np. w
realtime dashboardzie) nie jest ogłaszana automatycznie. Ikona w labelu ma
`aria-hidden="true"` (poprawnie, bo etykieta tekstowa obok jest wystarczająca).
Brak fokusu klawiaturowego nawet dla `clickable: true` w protokole to
naruszenie zasady „klawiatura wszędzie” z `design/README.md` — patrz
odstępstwa.

## Responsywność i platformy

`.tf-stat-group` (kontener siatki kart komponentu protokołu `StatGroup`
0x000A, `controls.css:8697-8705`) ma warianty gęstości
`--density-compact`/`--density-comfortable` mapowane na `spacing.sm`/
`spacing.lg`, i przechodzi na `grid-template-columns: 1fr !important` poniżej
639px — jeden pixel pod tokenem `layout.breakpoints_px.xs` (640), nie
dokładnie na nim. Strony budowane ręcznie w HTML (np. `agents.js`) nie używają
`tf-stat-group` wcale — wstawiają karty bezpośrednio do własnego kontenera
CSS Grid strony, więc ta reguła dotyczy tylko ścieżki addonów. Na ESP32-P4
karty tracą `box-shadow` zgodnie z
`platform.esp32p4_tab5.shadows: "flat"` — border pozostaje jedynym separatorem.

## Tokeny użyte

- `color.themes.dark.bg.card` / `card_hover` — tło karty i hover
- `color.themes.dark.border.default` — obramowanie
- `color.themes.dark.semantic.success/warning/critical/info` — akcenty i delty
- `color.themes.dark.text.muted` — delta neutralna
- `typography.scale.caption` — etykieta
- `spacing.sm` / `spacing.lg` — gęstość `tf-stat-group`
- `radius.lg` (`--tf-radius`, karty) — promień karty

## Znane odstępstwa w kodzie (2026-09-14)

- **Brak sparkline/gauge w StatCard.** Ani schemat protokołu `0x0208`
  (`controls.css` sekcja StatCard, `docs/ADDON_UI_COMPONENT_CATALOG_v1.md`
  §4 pola 0-8), ani `tf-stat-card.js` nie mają pola/property na osadzenie
  `tf-sparkline`/`tf-gauge`. Rzeczywiste użycie w kodzie (`agents.js:2741-2753`,
  `dashboard.js` hero) to zawsze goły tekst + delta, nigdy wykres w kafelku.
  Jeśli dashboard ma wykres przy KPI, jest to osobny element obok karty, nie
  wewnątrz niej. Ten dokument opisuje to jako **planowane rozszerzenie**, nie
  obecne zachowanie.
- **`clickable` bez fokusu klawiatury** — protokół dodaje handler kliknięcia
  bez `tabindex="0"`/`role="button"`, więc karta klikalna myszą jest
  niedostępna z klawiatury. Do naprawienia w `data-stat-labels-renderer.js`
  przed uznaniem `clickable` za stabilne.
- **Dwa różne komponenty pod jednym tagiem.** `<tf-stat-card size="...">`
  (klasy `.tf-stat*`) i `<tf-stat-card>` bez `size` (klasy `.tf-stat-card*`)
  mają rozłączne drzewa DOM i style — de facto renderują `StatCard` (0x0208)
  i `Stat` (0x0209) przez jeden custom element. Warto rozdzielić je na dwa
  tagi HTML (`tf-stat-card` / `tf-stat`) przy kolejnej iteracji.

## Przykłady

```html
<tf-stat-card label="Runs" value="128" delta-type="up" delta="+12 dziś" accent="success"></tf-stat-card>
<tf-stat-card label="Aktywne" value="3" size="md"></tf-stat-card>
```

```rust
// natywnie (tenta-ui-widgets, API docelowe)
StatCard::new("Runs", Value::Number(128.0))
    .trend(Trend::up(12.0).label("dziś"))
    .accent(Tone::Success)
    .on_click(Msg::OpenRuns)
```
