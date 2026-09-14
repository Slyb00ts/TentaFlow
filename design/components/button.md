# Button

| | |
|---|---|
| Tier | 0 (MVP) |
| HTML | `<tf-button>` — `tentaflow-core/www/js/components/tf-button.js`, style `tentaflow-core/www/css/controls.css` linie 96–260 |
| Protokół addonów | `0x0401` `Button` — `docs/ADDON_UI_COMPONENT_CATALOG_v1.md` §6 (linie 2549–2567); powiązane `0x0402 IconButton`, `0x0403 ButtonGroup` |
| Natywny (TentaEngine) | `tenta_ui_widgets::Button` — status: planowany |
| Status dokumentu | draft |

Główna akcja klikalna w interfejsie — zapisz, usuń, otwórz, potwierdź. Nie używaj do
nawigacji między stronami (użyj `Link`/`LinkButton`, gdy trafi do specu) ani do
przełączania stanu on/off (użyj `Toggle`).

## Anatomia

```text
┌──────────────────────────────┐
│ [leading icon] Etykieta [→]  │  ← ASCII szkic
└──────────────────────────────┘
   height: control.height.md (36)   padding-x: spacing.md (12)
   gap ikona↔tekst: spacing.xs (4→8 w kodzie, patrz odstępstwa)
```

Części: *container* (sam `<button>`, nie ma osobnego wrappera), *leading icon*
(opcjonalna, `icon_size.sm`), *label* (`TextStyle.body_strong`), *trailing icon*
(opcjonalna, strzałka/check). Tekst i ikony to flex-children z jednolitym gapem.

## Warianty

Nazwy wariantów w sdk-spec (`ButtonVariant`) i w HTML **nie pokrywają się 1:1** —
patrz „Znane odstępstwa”. Tabela poniżej podaje docelowe zachowanie z tokenów.

| Wariant (sdk-spec) | Wygląd | Kiedy używać |
|---|---|---|
| `primary` | wypełnienie `gradient.accent`, tekst `text.on_accent`, cień `elevation.subtle` | jedna główna akcja na widoku/formularzu |
| `secondary` | tło `bg.elevated`, obramowanie `border.default` | akcje drugorzędne obok primary |
| `tertiary` | bez tła, tekst `text.secondary`, bez obramowania | akcje trzeciorzędne w gęstym UI (odpowiednik dzisiejszego `ghost`) |
| `ghost` | jak `tertiary`, ale z hover-tłem `bg.elevated` | akcje w paskach narzędzi, obok ikon |
| `destructive` | tekst/obramowanie `semantic.critical`, wypełnienie na hover | usuwanie, nieodwracalne operacje |
| `link` | wygląda jak `Link` (bez obramowania/tła), podkreślenie wg `LinkUnderline` | akcja osadzona w zdaniu/tekście |

## Rozmiary

sdk-spec `ButtonSize` ma 4 wartości (`xs`/`sm`/`md`/`lg`); dziś w kodzie HTML istnieją
tylko dwie (`md` domyślny, `sm`).

| Rozmiar | Wysokość (`control.height`) | Typografia | Ikona (`icon_size`) | Uwagi |
|---|---|---|---|---|
| `xs` | brak w kodzie — docelowo < `control.height.sm` | `caption` | `xs` (12) | tylko sdk-spec, nie zaimplementowany w HTML |
| `sm` | `control.height.sm` (28) | `caption_strong` | `sm` (16) | realny cel dotykowy < 44 px — akceptowalne tylko w gęstych paskach narzędzi obok innych `sm` |
| `md` | `control.height.md` (36) | `body_strong` | `sm` (16) | domyślny |
| `lg` | `control.height.lg` (44) | `body_lg` | `md` (20) | brak w kodzie HTML — docelowy cel dotykowy 44 px „za darmo” |

## Stany

| Stan | Tło | Tekst/ikona | Obramowanie | Uwagi |
|---|---|---|---|---|
| default | wg wariantu | wg wariantu | wg wariantu | |
| hover | `primary`: jaśniejszy gradient + `elevation.medium`; `secondary`/`ghost`: `bg.card_hover` | bez zmian | `border.hover` gdzie dotyczy | tylko `pointer: fine`; unosi się o 1px (`translateY(-1px)`) |
| active / pressed | bez zmiany tła | bez zmiany | bez zmiany | `scale(0.97)`, bez cienia dodatkowego |
| focus-visible | bez zmiany tła | bez zmiany | ring `control.focus_ring` (2px, offset 2px, `border.focus`) | tylko po klawiaturze (`:focus-visible`) |
| disabled | bez zmiany koloru, `opacity: 0.4` | bez zmiany | bez zmiany | `aria-disabled="true"`, `pointer-events: none`, transform/filter zablokowane |
| loading | **brak w kodzie** | **brak w kodzie** | **brak w kodzie** | sdk-spec ma pole `loading: BindRef<bool>`; HTML `tf-button` go nie obsługuje w ogóle — patrz odstępstwa |
| icon-only | tło/tekst jak wariant | — | — | wymaga `aria-label` (patrz Dostępność); klasa `.tf-btn-icon`, kwadrat |

## Zachowanie

- Wskaźnik: klik wywołuje handler `click`; podczas `disabled` klik jest tłumiony
  (`preventDefault` + `stopImmediatePropagation`) zarówno dla myszy, jak i automatyzacji
  klikającej programowo. Dźwięk UI (`Sfx.play('ui-click')`) gra tylko dla wariantów
  primary/secondary/danger*/success — `ghost`/`outline` są ciche.
- Klawiatura: natywny `<button>`, więc Enter/Space aktywują klik bez dodatkowego kodu.
- Animacje: hover — `transform`/`box-shadow`/`filter` na `motion.duration_ms.fast`
  (120 ms w tokenach; kod używa 150 ms, patrz odstępstwa), `easing.overshoot`
  (`--tf-spring-smooth` w kodzie ≠ nazwa tokena, patrz niżej) na kliknięciu.
  Brak jawnej obsługi `prefers-reduced-motion` w regule `.tf-btn` — dziedziczy
  globalne wyłączenie animacji, jeśli istnieje na poziomie `<html>`.
- Zdarzenia/API HTML: atrybuty `variant`, `tone`, `size`, `icon`, `trailing-icon`,
  `disabled`, `type`, `label`, `full-width`; event natywny `click` (bubblingowy,
  bez custom detail). W protokole: handler `"click"` (`Handler`, zwykle
  `Backend` lub `Both`). Natywnie: `Msg` przez `.on_press(Msg::…)`.
- Zaznaczanie tekstu: zablokowane (`user-select: none`) — przycisk nie jest polem
  tekstowym.

## Dostępność

Rola: natywny element `<button>` (rola `button` domyślna, bez potrzeby `role=`).
Icon-only (`icon` ustawiony, brak tekstu) **musi** dostać `aria-label` na hoście
`<tf-button>` — dzisiejszy komponent tego nie wymusza ani nie ostrzega, gdy brak
(deviation, patrz niżej). Kontrast: `primary` (biały na `accent.primary`) 4.7:1 (AA
dla tekstu ≥ 14px, graniczne dla drobniejszego — `sm` ma 11px, sprawdzić realnie).
`secondary`/`ghost` tekst `text.secondary`/`text` na `bg.elevated`/`bg.card_hover` —
mieści się w AA (patrz `foundations/colors.md`). `prefers-reduced-motion`: brak
jawnej reguły w `.tf-btn` — do naprawienia (patrz `design/MIGRATION.md`).

## Responsywność i platformy

`full-width` rozciąga przycisk na 100% szerokości kontenera — używane w
formularzach mobilnych i modalach na wąskich ekranach. Gęstość: brak wsparcia
`control.density` w kodzie HTML (jeden rozmiar `md`/`sm` niezależnie od
`compact`/`comfortable`). Na ESP32-P4: brak cieni (`elevation` spłaszczone do
1px obramowania), `hover` nieistotny (`pointer: coarse`) — aktywny wyłącznie stan
`active`/`focus`; `sm` (28px) jest za mały na panelu dotykowym — preferuj `lg`
(44px, dziś niezaimplementowany) lub `md` z dodatkowym marginesem dotykowym.

## Tokeny użyte

`color.themes.dark.accent.primary`, `color.themes.dark.gradient.accent`,
`color.themes.dark.text.on_accent`, `color.themes.dark.bg.elevated`,
`color.themes.dark.bg.card_hover`, `color.themes.dark.border.default`,
`color.themes.dark.border.hover`, `color.themes.dark.semantic.critical`,
`typography.scale.body_strong`, `typography.scale.caption_strong`,
`typography.scale.body_lg`, `spacing.md`, `spacing.xs`, `radius.md`,
`elevation.subtle`, `elevation.medium`, `control.height.sm/md/lg`,
`control.focus_ring`, `control.icon_size.sm/md`, `motion.duration_ms.fast`,
`motion.easing.standard`.

## Znane odstępstwa w kodzie (2026-09-14)

- **Nazewnictwo wariantów nie pokrywa się z `ButtonVariant`.** `tf-button.js`
  (`VARIANT_CLASS`, linie 25–34) zna `primary/secondary/ghost/outline/danger/
  danger-solid/danger-outline/success` — nie ma `tertiary`, `destructive` ani
  `link`; za to ma `outline`, `success` i trzy warianty `danger*`, których w
  sdk-spec nie ma wcale. `tone` (osobny atrybut, `TONE_CLASS`, linie 39–46) tylko
  częściowo odwzorowuje `Tone` (nie ma efektu na `primary`/`secondary`/`danger-solid`
  /`success` — patrz komentarz w kodzie, linia 36–38).
- **Brak stanu `loading`.** `tf-button.js` `observedAttributes` (linia 50) nie
  zawiera `loading`; nie ma spinnera ani `aria-busy`, mimo że katalog protokołu
  (`0x0401` pole 8) i `ButtonSize`/`ButtonVariant` już to zakładają.
- **Brak rozmiaru `lg` i `xs`.** CSS ma tylko `.tf-btn` (domyślny, de facto `md`,
  38px min-height) i `.tf-btn-sm` (30px) — `control.height` (28/36/44) nie jest
  odwzorowany 1:1 (38px i 30px to wartości własne, nie z tokena).
- **Font-size `sm` = 11px** (`controls.css:225`) zamiast `typography.scale.caption`
  (11/16, waga 400) — wagę 600 ma, ale rozmiar 11px pokrywa się przypadkiem, nie
  przez odwołanie do tokena (literał).
- **`min-height: 38px`** (`controls.css:115`) dla wariantu domyślnego nie
  odpowiada żadnemu `control.height` (28/36/44) — własna wartość.
- Sprężyny animacji w CSS (`--tf-spring-smooth`, `--tf-spring-snappy`) mają te
  same wartości cubic-bezier co `motion.easing.standard`/`overshoot` w tokenach,
  ale inne nazwy zmiennych — do zunifikowania przy generatorze (`design/MIGRATION.md`).

## Przykłady

```html
<tf-button variant="primary" size="md">Zapisz</tf-button>
<tf-button variant="secondary" icon="download">Pobierz</tf-button>
<tf-button variant="ghost" icon="settings" aria-label="Ustawienia"></tf-button>
<tf-button variant="danger-solid" disabled>Usuń konto</tf-button>
```

```rust
// natywnie (tenta-ui-widgets, API docelowe)
Button::new("Zapisz")
    .variant(ButtonVariant::Primary)
    .size(ButtonSize::Md)
    .on_press(Msg::Save)

Button::icon_only(Icon::Settings)
    .variant(ButtonVariant::Ghost)
    .aria_label("Ustawienia")
    .on_press(Msg::OpenSettings)
```
