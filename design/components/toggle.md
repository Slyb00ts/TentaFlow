# Toggle / Segmented control

| | |
|---|---|
| Tier | 0 (MVP) — `Toggle`; 1 — `SegmentedControl` |
| HTML | `<tf-toggle>` — `tentaflow-core/www/js/components/tf-toggle.js`, style `controls.css` linie 558–618; `<tf-segmented>` — `tf-segmented.js`, style linie 689–760 |
| Protokół addonów | `0x030A` `Toggle` — `docs/ADDON_UI_COMPONENT_CATALOG_v1.md` §5, linie 2200–2215; `0x0409` `SegmentedControl` — §6, linie 2668–2680 |
| Natywny (TentaEngine) | `tenta_ui_widgets::Toggle` / `SegmentedControl` — status: planowany |
| Status dokumentu | draft |

`Toggle` — przełącznik on/off, którego efekt jest widoczny natychmiast (bez
przycisku „Zapisz”). `SegmentedControl` — wybór jednej z 2–5 krótkich opcji
wyświetlonych jako spójny pasek. Nie używaj `Toggle` dla akcji wymagającej
potwierdzenia (to `Checkbox` + `Button`) ani `SegmentedControl` dla > 5 opcji
(to `RadioGroup` albo `Select`, patrz `select.md`).

## Anatomia

```text
┌ tf-toggle ──┐              ┌ tf-segmented ──────────────────┐
│ ( )═══●     │  ← off       │ [Auto] [ Zezwól ] [ Odmów ]    │
│ ●═══( )     │  ← on        └──────────────────────────────────┘
└──────────────┘                każda opcja: role=radio w role=radiogroup
  44×24 (default) / 28×16 (sm)
```

Części *toggle*: *track* (`<span class="tf-toggle">`, `role="switch"`), *thumb*
(`::after`, krąg przesuwający się `translateX`). Części *segmented*: *container*
(`role="radiogroup"`, tło `bg.elevated`, `radius.pill`), *segment* (`<button>`,
`role="radio"`, opcjonalna wiodąca ikona 14px), kolor aktywnego segmentu zależy
od `data-variant` opcji (`ok`/`warn`/`err`/`accent`/`info`/`neutral`).

## Warianty

| Komponent | Wariant | Opis |
|---|---|---|
| `tf-toggle` | jedyny | brak wariantów wizualnych poza rozmiarem — kolor `on` zawsze `gradient.accent` |
| `tf-segmented` | domyślny (`size` bez `md`) | pigułka z paddingiem 3px, aktywna opcja koloruje się wg `variant` opcji (`ok`=success, `warn`=warning, `err`=critical, `accent`=primary, `info`=info, `neutral`=domyślny fiolet) |
| `tf-segmented` | `size="md"` | pasek „sentence-case”, kwadratowe segmenty, aktywna opcja zawsze `accent.primary` niezależnie od `data-variant` |

## Rozmiary

| Rozmiar | `tf-toggle` (track) | `tf-segmented` |
|---|---|---|
| `sm` | 28×16, thumb 10px | `.tf-segmented-sm`: padding 4px/10px, font 10px |
| `md` (domyślny) | 44×24, thumb 18px | `.tf-segmented-md`: padding 5px/10px, font **11.5px** (patrz odstępstwa) |
| `lg` | **brak w kodzie** | **brak w kodzie** |

sdk-spec `ToggleSize`/`SegmentSize` mają `sm`/`md`/`lg` — `lg` nie jest
zaimplementowany w żadnym z dwóch komponentów HTML. Cel dotykowy: track
44×24px `tf-toggle` domyślny ma szerokość zgodną z `control.touch_target_min`
(44), ale wysokość 24px jest poniżej — powiększ obszar klikalny paddingiem w
layoutach dotykowych.

## Stany

| Stan | Tło | Tekst/ikona | Obramowanie | Uwagi |
|---|---|---|---|---|
| off | `bg.elevated` | thumb `text.primary` | `border.default` | |
| on | `gradient.accent` | thumb biały | `accent.primary` | + glow (`0 0 18px accent.primary@0.35`) |
| hover | bez zmiany | — | — | brak jawnej reguły hover w CSS dla `tf-toggle`/`tf-segmented` poza `.active` |
| focus-visible | — | — | ring `control.focus_ring` | na `.tf-seg-opt` dla segmented; `tf-toggle` — sprawdzić (nie znaleziono jawnej reguły `:focus-visible` w zakresie odczytanym) |
| disabled | bez zmiany | — | — | `tf-toggle`: `opacity: 0.4` przez `[aria-disabled="true"]`; `tf-segmented`: `.tf-segmented-disabled { opacity: 0.5; pointer-events: none }` |
| selected (segment aktywny) | wg `data-variant` | biały | — | `translateY(-0.5px)` w wariancie domyślnym, brak przesunięcia w `md` |

## Zachowanie

- **`tf-toggle`**: klik lub Space/Enter na zafokusowanym elemencie przełącza;
  po przełączeniu odtwarza dźwięk (`Sfx.play('toggle')`) i dodaje klasę
  `.tf-ripple` na 500ms (efekt fali).
- **`tf-segmented`**: klik na segmencie wybiera go natychmiast; strzałki
  `ArrowLeft`/`ArrowRight` przesuwają wybór o jedną opcję (bez zawijania na
  końcach — `Math.max`/`Math.min`, nie modulo), `Home`/`End` skaczą na
  pierwszą/ostatnią opcję; wybrana opcja przejmuje focus programowo
  (`btns[nextIdx].focus()`).
- Animacje: `tf-toggle` thumb — `motion.spring` (kod: `--tf-spring-snappy`,
  0.25s); `tf-segmented` — `transform`/`background`/`color` na
  `motion.duration_ms.fast` (0.15s) z `scale(0.92)` na `:active`.
- Zdarzenia/API: `tf-toggle` — atrybuty `checked`, `disabled`; property
  `.checked`; event `change` (`detail.checked`). `tf-segmented` — dzieci
  `<option value="…" variant="…" icon="…">` konsumowane przy budowie; atrybuty
  `value`, `size`, `disabled`; property `.value`; event `change`
  (`detail.value`).
- Zaznaczanie tekstu: zablokowane pośrednio (elementy interaktywne, nie
  tekstowe).

## Dostępność

`tf-toggle`: `role="switch"`, `aria-checked`, `tabindex="0"` (lub `-1` gdy
disabled + `aria-disabled`). `tf-segmented`: `role="radiogroup"` na
kontenerze, każdy segment `role="radio"` + `aria-checked`, roving `tabindex`
(tylko aktywny segment ma `0`) — **ta implementacja ma prawidłową obsługę
strzałek**, w przeciwieństwie do `tf-radio-group` (`checkbox.md`), mimo tej
samej roli ARIA — niespójność do ujednolicenia w warstwie natywnej. Kontrast:
sprawdzić `warn`/`info` (tekst ciemny `#0a0d24` na tle `warning`/`info`) —
zamierzone dla czytelności na jasnym tle, zgodne z zasadą „tekst ciemny na
jasnym akcencie”.

## Responsywność i platformy

Brak specjalnej logiki responsywnej w obu komponentach. Na ESP32-P4: `tf-toggle`
44×24 mieści cel dotykowy szerokością, nie wysokością — jak w `checkbox.md`,
dodaj padding w layoutach dotykowych. `tf-segmented` z wieloma opcjami może nie
mieścić się na wąskich ekranach (720px panel) — brak logiki zawijania/przewijania
w odczytanym zakresie CSS, w przeciwieństwie do `tf-filter-chips`
(`chip.md`), który ma jawny scroll z `data-overflow`.

## Tokeny użyte

`color.themes.dark.bg.elevated`, `color.themes.dark.gradient.accent`,
`color.themes.dark.accent.primary`, `color.themes.dark.border.default`,
`color.themes.dark.semantic.success/warning/critical/info`,
`color.themes.dark.text.primary`, `radius.pill`, `radius.sm`,
`control.focus_ring`, `control.touch_target_min`, `motion.spring.snappy`,
`motion.duration_ms.fast`.

## Znane odstępstwa w kodzie (2026-09-14)

- **Brak rozmiaru `lg`** w `tf-toggle` i `tf-segmented` mimo `ToggleSize`/
  `SegmentSize` w sdk-spec zakładających `sm`/`md`/`lg`.
- **`font-size: 11.5px`** w `.tf-segmented-md .tf-seg-opt`
  (`controls.css:744`) — literał off-token; najbliższy token to
  `typography.scale.caption` (11px) lub `body` (13px), żaden nie jest 11.5.
  Zakazane przez `tokens.json → typography.rules.half_pixel_sizes`
  („forbidden — migration debt”).
- **`tf-toggle` i `tf-radio-group`/`tf-segmented` różnią się obsługą klawiatury**
  mimo pokrewnej semantyki wyboru — `tf-segmented` ma strzałki, `tf-toggle` (jako
  `switch`, poprawnie) nie potrzebuje ich, ale `tf-radio-group` (patrz
  `checkbox.md`) też powinien je mieć i nie ma.
- **Kolor aktywnego segmentu w wariancie `md` ignoruje `data-variant`** opcji —
  zawsze `accent.primary`, podczas gdy wariant domyślny koloruje wg
  `ok`/`warn`/`err`/`info`/`neutral` — niespójność wizualna między dwoma
  rozmiarami tego samego komponentu.

## Przykłady

```html
<tf-toggle checked></tf-toggle>
<tf-toggle size="sm" disabled></tf-toggle>

<tf-segmented value="auto" size="sm">
  <option value="auto" variant="neutral">Auto</option>
  <option value="allow" variant="ok">Zezwól</option>
  <option value="deny" variant="err">Odmów</option>
</tf-segmented>
```

```rust
// natywnie (tenta-ui-widgets, API docelowe)
Toggle::new()
    .checked(state.notifications_enabled)
    .on_change(Msg::ToggleNotifications)

SegmentedControl::new()
    .options([
        SegmentOption::new("auto", "Auto"),
        SegmentOption::new("allow", "Zezwól").tone(Tone::Success),
        SegmentOption::new("deny", "Odmów").tone(Tone::Critical),
    ])
    .value(state.mode)
    .on_change(Msg::ModeChanged)
```
