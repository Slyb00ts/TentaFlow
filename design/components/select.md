# Select

| | |
|---|---|
| Tier | 0 (MVP) — `Select`; 1 — `Combobox`, `Multiselect` |
| HTML | `<tf-select>` — `tentaflow-core/www/js/components/tf-select.js`, style `controls.css` linie 504–552; `<tf-combobox>` — `tf-combobox.js`, style linie 4435–4534; `<tf-multiselect>` — `tf-multiselect.js`, style linie 4544–4660 |
| Protokół addonów | `0x0303` `Select` — `docs/ADDON_UI_COMPONENT_CATALOG_v1.md` §5, linie 2077–2095; brak osobnych tagów dla combobox/multiselect w katalogu (patrz odstępstwa) |
| Natywny (TentaEngine) | `tenta_ui_widgets::Select` / `Combobox` / `MultiSelect` — status: planowany |
| Status dokumentu | draft |

Wybór jednej wartości z listy (`Select`), wybór z podpowiedziami/wyszukiwaniem
(`Combobox`), wybór wielu wartości jako chipy (`Multiselect`). Nie używaj `Select`
gdy opcji jest ≤ 5 i mieszczą się poziomo — wtedy `Segmented control`
(`toggle.md`) lub `RadioGroup` (`checkbox.md`) są czytelniejsze.

## Anatomia

**`tf-select`** — owija natywny `<select>`, dziedziczy wygląd `tf-input`:

```text
┌ Label ───────────────────────────────┐
│ ┌───────────────────────────────┐▾  │  ← natywny <select>, custom strzałka
│ └───────────────────────────────┘   │
└────────────────────────────────────────┘
```

**`tf-combobox`** — input + popover listbox:

```text
┌───────────────────────────────┐ [×]
│ wpisany tekst / placeholder    │
└───────────────────────────────┘
┌ popover (role=listbox) ────────┐
│ [ikon] Etykieta   opis         │  ← .tf-combobox-option, aria-activedescendant
│ [ikon] Etykieta 2              │
└─────────────────────────────────┘
```

**`tf-multiselect`** — trigger z chipami + popover z checkboxami:

```text
┌ [chip ×][chip ×] wolne miejsce      ▾│ [×]
└────────────────────────────────────────┘
┌ popover ─────────────────────────────┐
│ [wyszukiwarka]                       │
│ [Zaznacz wszystko / Wyczyść]         │
│ [✓] Opcja 1     [ ] Opcja 2          │
└────────────────────────────────────────┘
```

## Warianty

| Komponent | Wygląd | Kiedy używać |
|---|---|---|
| `tf-select` | natywny `<select>` w ramce `tf-input` | krótkie listy (≤ ~15), brak potrzeby wyszukiwania, wsparcie klawiatury systemowej „za darmo” |
| `tf-combobox` | input z filtrowanym popoverem, opcja `free-input` (commit dowolnego tekstu) | długie listy, potrzebne wyszukiwanie, jedna wartość |
| `tf-multiselect` | trigger-chipy + popover z checkboxami, opcjonalna wyszukiwarka (`no-search` wyłącza) | wiele wartości naraz, z opcjonalnym `select-all` i `max-selections` |

## Rozmiary

sdk-spec `InputSize` (`sm`/`md`/`lg`) — **niezaimplementowane** w żadnym z trzech
komponentów HTML. Jeden rozmiar na sztywno (`min-height` 40px `tf-select`/
`tf-multiselect`; `tf-combobox` dziedziczy padding `tf-input`). Docelowo skala
identyczna jak w `input.md`.

## Stany

| Stan | Tło | Tekst/ikona | Obramowanie | Uwagi |
|---|---|---|---|---|
| default | `bg.input` | `text.primary` | `border.default` | |
| hover | bez zmiany | bez zmiany | `border.hover` | |
| focus / open | `bg.card` | — | `accent.primary` + `glow.accent` | `tf-combobox`/`tf-multiselect`: `aria-expanded="true"`, popover widoczny |
| disabled | bez zmiany (`tf-select`: `opacity 0.5`) | bez zmiany | bez zmiany | `tf-multiselect` usuwa `tabindex`, dodaje `aria-disabled` |
| option hover/active | `accent.glow` (active) / `bg.elevated` (hover) | `text.primary` | — | `.active` = aktywna klawiaturą (`aria-activedescendant`), nie to samo co `:hover` |
| selected (multiselect) | checkbox `✓`, opcja `aria-selected="true"` | — | — | chip w trigerze z przyciskiem usuwania |
| max-selections osiągnięte | — | — | — | dalsze `_toggle()` w `tf-multiselect.js` no-op (linia 468), brak komunikatu w UI |

## Zachowanie

- **`tf-select`**: klik/Enter/Space otwiera natywną listę systemową (poza
  kontrolą CSS), strzałki poruszają wybór — zachowanie przeglądarki, nie
  reimplementowane.
- **`tf-combobox`**: pisanie filtruje opcje (`label`/`description` case-insensitive
  `includes`); `ArrowDown`/`ArrowUp` otwierają popover lub przesuwają aktywną
  opcję (cyklicznie); `Home`/`End` skaczą na pierwszą/ostatnią widoczną opcję;
  `Enter` commit aktywnej opcji lub — z atrybutem `free-input` — wpisanego
  tekstu; `Escape` zamyka; `Tab` zamyka bez commitu; klik poza komponentem
  zamyka (`document` click listener). `min-chars` blokuje otwarcie poniżej progu
  znaków.
- **`tf-multiselect`**: trigger ma `tabindex="0"` i własną obsługę
  `ArrowDown/Up/Home/End/Enter/Space/Escape/Tab`; wewnątrz popoveru wpisywanie w
  polu wyszukiwania przekierowuje klawisze nawigacyjne z powrotem do triggera
  (`KeyboardEvent` reemitted, linia 191–198), żeby jedna logika obsługi klawiatury
  wystarczyła. `select-all` przełącza między „Zaznacz wszystko”/„Wyczyść”
  zależnie od stanu i `max-selections`.
- Animacje: brak jawnych — popovery pokazują się/chowają przez `hidden`
  (natychmiastowe), bez tranzycji wejścia/wyjścia (deviation vs `motion.easing.emphasized`
  wymagane dla „entrances” dropdownów).
- Zdarzenia/API: `tf-select` — `change` (`detail.value`), metoda `setOptions(list,
  selected)` do wymiany opcji po fakcie. `tf-combobox` — property `.options`
  (`{value,label,description?,icon?,group?,disabled?}`), eventy `input`
  (`detail.query`) i `change` (`detail.{value,label}`, `free:true` przy wolnym
  tekście, `value:null` przy czyszczeniu). `tf-multiselect` — property `.options`/
  `.value` (array), event `change` (`detail.value` = array). W protokole:
  `0x0303 Select` ma pola `searchable`/`clearable`/`virtualize`, których HTML
  `tf-select` **nie realizuje w ogóle** (patrz odstępstwa).

## Dostępność

`tf-combobox`: `role="combobox"` na `<input>`, `aria-haspopup="listbox"`,
`aria-expanded`, `aria-autocomplete="list"`, `aria-activedescendant` wskazujący
aktywną opcję (`role="option"`), etykieta przez `aria-labelledby` **lub**
`aria-label` (poprawnie zaimplementowane, `tf-combobox.js:156-168`).
`tf-multiselect`: `role="combobox"` na triggerze, popover `role="listbox"
aria-multiselectable="true"`, każda opcja `aria-selected`. `tf-select`: rola
natywna `<select>`, w pełni dostępna z klawiatury i czytnikiem ekranu za darmo —
najbardziej dostępny z trzech wariantów mimo najmniejszej liczby funkcji.
Kontrast: identyczny jak `input.md` (pole dziedziczy `.tf-input`/`bg.input`).

## Responsywność i platformy

Popovery (`tf-combobox-popover`, `tf-multiselect-popover`) mają
`max-height: 260px` na sztywno i `overflow-y: auto` — brak logiki
przestawienia się nad pole (`flip`), gdy brakuje miejsca pod spodem (ryzyko
ucięcia na niskich ekranach/dole strony). Na dotyku: `tf-combobox`/
`tf-multiselect` polegają na `focus`/`click`, brak specjalnej obsługi
`pointer: coarse` — działa, ale bez large-tap-target dla opcji (padding 9px 12px
≈ niżej niż 44px).

## Tokeny użyte

`color.themes.dark.bg.input`, `color.themes.dark.bg.card`,
`color.themes.dark.bg.elevated`, `color.themes.dark.accent.primary`,
`color.themes.dark.accent.glow`, `color.themes.dark.border.default`,
`color.themes.dark.border.hover`, `typography.scale.body`,
`typography.scale.caption`, `spacing.sm`, `spacing.md`, `radius.md`,
`radius.sm`, `control.height.md`, `control.icon_size.sm`,
`layout.z_index.dropdown`, `motion.easing.emphasized` (docelowo, brak w kodzie).

## Znane odstępstwa w kodzie (2026-09-14)

- **`tf-select` nie realizuje `searchable`/`clearable`/`virtualize`** z pól
  `0x0303 Select` — to goły `<select>` bez atrybutu `size`, `hint`, `error`
  (`tf-select.js` `observedAttributes`, linia 10: tylko `value/disabled/name/label`).
- **Brak animowanego wejścia/wyjścia popoveru** w `tf-combobox`/`tf-multiselect`
  — `hidden` przełącza się natychmiast, wbrew `motion.easing.emphasized` dla
  „entrances” zadeklarowanemu w `tokens.json`.
- **Brak `size` (`InputSize`) w żadnym z trzech komponentów.**
- **Katalog protokołu nie ma osobnych tagów dla `Combobox`/`Multiselect`** —
  tylko `0x0303 Select` z polami `searchable`/`clearable` sugerującymi, że jeden
  tag protokołu miał pokryć oba warianty; HTML poszedł inną drogą (trzy oddzielne
  komponenty). Do wyjaśnienia, który system jest kanoniczny (patrz
  `design/MIGRATION.md`, punkt o dwóch systemach komponentów).
- **Brak flip/collision detection** dla popoverów (`max-height: 260px` na sztywno,
  zawsze otwiera się w dół).

## Przykłady

```html
<tf-select label="Region" value="eu">
  <option value="eu">Europa</option>
  <option value="us">USA</option>
</tf-select>

<tf-combobox label="Model" placeholder="Wybierz model..." clearable></tf-combobox>
<script>
  document.querySelector('tf-combobox').options = [
    { value: 'gpt', label: 'GPT-4', group: 'OpenAI' },
    { value: 'claude', label: 'Claude', group: 'Anthropic' },
  ];
</script>

<tf-multiselect label="Tagi" select-all max-selections="5"></tf-multiselect>
```

```rust
// natywnie (tenta-ui-widgets, API docelowe)
Select::new()
    .label("Region")
    .options([("eu", "Europa"), ("us", "USA")])
    .value("eu")
    .on_change(Msg::RegionChanged)

Combobox::new()
    .label("Model")
    .options(models)
    .searchable(true)
    .on_change(Msg::ModelChanged)

MultiSelect::new()
    .label("Tagi")
    .options(tags)
    .max_selections(5)
    .on_change(Msg::TagsChanged)
```
