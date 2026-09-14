# Sparkline

| | |
|---|---|
| Tier | 0 (MVP) |
| HTML | `<tf-sparkline>` — `www/js/components/tf-sparkline.js`, style `controls.css` linie 7170-7207 |
| Protokół addonów | `0x0215` `Sparkline` — `docs/ADDON_UI_COMPONENT_CATALOG_v1.md` §4 |
| Natywny (TentaEngine) | `tenta_ui_widgets::Sparkline` — status: planowany |
| Status dokumentu | draft |

Mikro-wykres trendu bez osi i legendy, do wstrzyknięcia w wiersz statystyki
(`stat-card.md`) albo komórkę tabeli (`ColumnRender::Trend`). Nie używaj go
tam, gdzie użytkownik ma czytać dokładne wartości — do tego jest
`line-chart`/`area-chart` z osiami.

## Anatomia

Dwie niezależne implementacje renderują ten sam koncept inną technologią:

- **Element HTML** (`tf-sparkline.js`) rysuje na `<canvas>` w skali
  `devicePixelRatio`, bez SVG i bez klas BEM z tabeli poniżej.
- **Renderer protokołu addonów** (`0x0215`, chunk 3.3d-7) rysuje inline SVG z
  klasami `.tf-sparkline__*` — to one mają wpisy w `controls.css`.

```text
┌──────────────────────────────────────────┐
│  ⟋‾⟍_⟋⟍___⟋‾‾⟍_⟋                          │  ← linia, 60×20 typowo w stat-card,
└──────────────────────────────────────────┘     ale szerokość = clientWidth (min 60px)
   height: dowolny atrybut `height` (canvas) / SVG viewBox z danych (protokół)
   stroke: 1.5px (canvas default `lineWidth`) / var(--tf-accent-1) (SVG .__line)
```

Elementy SVG (protokół): `.tf-sparkline` (kontener inline), `.tf-sparkline__svg`,
`.tf-sparkline__line` (stroke, `vector-effect: non-scaling-stroke`),
`.tf-sparkline__area` (fill pod linią, tylko `variant="area"`),
`.tf-sparkline__bar` (słupki, `variant="bar"`), opcjonalnie
`.tf-sparkline__stats` + `.tf-sparkline__sep` — para wartość/separator obok
wykresu (np. „12 / 18”), nieudokumentowana w `_TEMPLATE.md`, ale realna w CSS.

## Warianty

| Wariant (`SparklineVariant`) | Wygląd | Kiedy używać |
|---|---|---|
| `line` (domyślny) | pojedyncza łamana/krzywa | trend liczbowy w czasie |
| `area` | linia + wypełnienie pod nią (15% alfa) | podkreślenie wolumenu/skali |
| `bar` | słupki, jeden na punkt | dyskretne okresy (dni/commity), bez interpolacji |

Ton koloru (tylko implementacja protokołu, klasy `.tf-sparkline--tone-*`):
`neutral`, `primary` (domyślny), `info`, `success`, `critical`, `warning`,
`muted` — mapowane na `color.tone.<tone>.fg` z `tokens.json`. Element HTML
(`tf-sparkline.js`) ma **własną**, mniejszą listę: `color` = `primary` |
`success` | `warning` | `danger` | `info` | `accent`, mapowaną ręcznie na
`--tf-accent-1` / `--tf-success` / `--tf-warning` / `--tf-danger` /
`--tf-info` / `--tf-accent-2` — nazwy `danger` (HTML) i `critical` (protokół)
oznaczają ten sam token i różnią się tylko etykietą.

## Rozmiary

Brak tokenu `sm`/`md`/`lg` — sparkline nie jest celem dotykowym (czysto
dekoracyjny/informacyjny SVG-equivalent, `aria-hidden` w praktyce, patrz
Dostępność). Rozmiar to swobodne `width`/`height`:

- Element HTML: `height` (property, domyślnie 32px), `width` = `clientWidth`
  kontenera (minimum wymuszone w kodzie: 60px). Canvas renderuje się w
  `devicePixelRatio` na backing store, żeby nie rozmazać się na ekranach retina.
- Protokół: wymiary wynikają z layoutu rodzica (typowo `stat-card` albo
  komórka tabeli); w opisie zadania „60×20” to rozmiar orientacyjny w
  `stat-card`, nie twardy limit w kodzie.

`lineWidth` (HTML, domyślnie 1.5) i `.tf-sparkline__line { stroke-width: 1.5 }`
(protokół) są zgodne.

## Stany

| Stan | Opis |
|---|---|
| default | linia/area/bar w tonie z atrybutu |
| brak punktów | element nic nie rysuje (HTML: `_render()` wraca wcześnie dla `< 2` punktów w `line`/`area`, `< 1` w `bar`); protokół nie definiuje jawnego stanu pustego — pusta tablica `points` renderuje pustą ścieżkę |
| `smooth` | (tylko HTML) krzywa quadratic przez środki odcinków zamiast łamanej |
| `showDots` | (tylko HTML) kropka 2px promienia na każdym punkcie |
| `fill` | (tylko HTML) obszar pod linią, alfa 0.15 — odpowiednik `variant="area"` w protokole, ale jako osobny boolean, nie wariant |

Sparkline nie ma stanów `hover`/`focus`/`disabled` — nie jest interaktywny w
żadnej z dwóch implementacji.

## Zachowanie

- Interakcja wskaźnikiem: brak — ani `tf-sparkline.js`, ani renderer protokołu
  nie dodają listenerów myszy/dotyku. Tooltip przy najechaniu (typowy dla
  sparkline w innych systemach) **nie istnieje**.
  Klawiatura: nie dotyczy (nie jest fokusowalny).
- Animacje: `tf-sparkline.js` dodaje klasę `sdk-animate-fade-in` przy pierwszym
  `connectedCallback` (delikatne pojawienie się), bez animacji rysowania linii
  — świadoma decyzja w kodzie, bo animacja „kreślenia” wymagałaby własnej
  pętli `requestAnimationFrame` kolidującej z reaktywnymi update'ami.
- Zdarzenia/API: brak eventów. Właściwości HTML: `points` (`Array<number>`,
  filtrowane do skończonych liczb), `color`, `fill`, `showDots`, `height`,
  `variant`, `smooth`, `lineWidth` — wszystkie jako gettery/settery, re-render
  natychmiastowy jeśli element jest podłączony do DOM.
  Pola protokołu (`0x0215`): `points` (`BindRef<array<f64>>`, reaktywne),
  `variant`, `tone` (`Tone`), `show_stats` (bool → `.tf-sparkline__stats`).
- Zaznaczanie tekstu: nie dotyczy (brak tekstu poza opcjonalnym `__stats`).

## Dostępność

Sparkline jest czysto dekoracyjny — nie niesie unikalnej informacji tekstowej
(wartość liczbowa jest zwykle obok, w `tf-stat-card`). Żadna z dwóch
implementacji nie ustawia `aria-hidden="true"` ani `role="img"` +
`aria-label` na elemencie automatycznie — **to jest luka**, patrz „Znane
odstępstwa”. Zalecenie tego dokumentu: rodzic (`stat-card`) powinien albo
oznaczyć sparkline `aria-hidden="true"`, albo dodać `aria-label` z opisem
trendu („wzrost 12% w ostatnich 7 dniach”), nigdy oba naraz.
`prefers-reduced-motion` nie ma znaczenia — nie ma animacji do wyłączenia
poza jednorazowym fade-in (200ms `motion.duration_ms.normal` klasy rzędu).

## Responsywność i platformy

Canvas (HTML) skaluje się do `clientWidth` rodzica przy każdym `_render()` —
działa responsywnie bez media queries, ale wymaga ręcznego wywołania
`_render()` po zmianie layoutu (np. zmiana szerokości panelu bocznego nie
triggeruje automatycznego re-layoutu, bo nie ma `ResizeObserver` w kodzie).
Na ESP32-P4: brak cieni i tak nie dotyczy (sparkline nie ma cienia); stroke
1px+ pozostaje czytelny przy `scale_factor: 1.25`.

## Tokeny użyte

- `color.tone.*.fg` (protokół, per `tone`) — `tokens.json` → `color.themes.dark.tone`
- `color.themes.dark.accent.primary` (`--tf-accent-1`) — domyślny kolor linii
- `color.themes.dark.accent.glow` — wypełnienie `area` w wariancie `primary`
- `color.themes.dark.semantic.success/warning/critical/info` — pozostałe tony
- `color.themes.dark.text.muted` (`--tf-text-3`) — ton `muted`/`neutral`
- `motion.duration_ms.normal` (200ms) — orientacyjny czas `sdk-animate-fade-in`

## Znane odstępstwa w kodzie (2026-09-14)

- **Dwie niekompatybilne implementacje.** `tf-sparkline.js` rysuje na
  `<canvas>` (bez klas `.tf-sparkline__*`), podczas gdy renderer protokołu
  addonów rysuje SVG z tymi klasami. Ten sam tag `<tf-sparkline>` istnieje w
  obu światach, ale ich wewnętrzny DOM i API `color` vs `tone` się różnią —
  dokument zaleca ujednolicenie w stronę wariantu SVG (łatwiejszy do
  stylowania tokenami CSS) jako kierunek dla natywnego silnika.
- **Brak `aria-hidden`/`aria-label` domyślnie** w obu implementacjach —
  `controls.css:7170-7207` i `tf-sparkline.js` nie ustawiają żadnej roli ARIA.
- **`show_stats`** (pole protokołu) nie ma odpowiednika we właściwościach
  `tf-sparkline.js` — funkcja istnieje tylko po stronie addonów.
- Brak `ResizeObserver` w `tf-sparkline.js` — zmiana szerokości kontenera bez
  ponownego wywołania settera nie odświeża canvasu.

## Przykłady

```html
<!-- element HTML (canvas) -->
<tf-sparkline height="20" color="success" fill smooth></tf-sparkline>
<script>
  document.querySelector('tf-sparkline').points = [4, 7, 6, 9, 12, 10, 14];
</script>
```

```rust
// natywnie (tenta-ui-widgets, API docelowe)
Sparkline::new(points)
    .variant(SparklineVariant::Area)
    .tone(Tone::Success)
    .show_stats(true)
```
