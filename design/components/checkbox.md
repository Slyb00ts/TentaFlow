# Checkbox / Radio

| | |
|---|---|
| Tier | 0 (MVP) — `Checkbox`; 1 — `Radio`/`RadioGroup` |
| HTML | `<tf-checkbox>` — `tentaflow-core/www/js/components/tf-checkbox.js`, style `controls.css` linie 3392–3459; `<tf-radio>`/`<tf-radio-group>` — `tf-radio.js`, style linie 3360–3391 |
| Protokół addonów | `0x030B` `Checkbox` i `0x030D` `RadioGroup` (+ `0x030C` `Radio`, `0x030E` `RadioCardGroup`) — `docs/ADDON_UI_COMPONENT_CATALOG_v1.md` §5, linie 2217–2267 |
| Natywny (TentaEngine) | `tenta_ui_widgets::Checkbox` / `RadioGroup` — status: planowany |
| Status dokumentu | draft |

Wybór binarny niezależny od innych opcji (`Checkbox`, w tym stan pośredni
„zaznacz wszystko”) lub wybór jednej opcji z zamkniętego zbioru (`RadioGroup`).
Nie używaj `Checkbox` pojedynczo do przełączania trybu on/off widocznego od razu
w interfejsie — do tego jest `Toggle` (`toggle.md`).

## Anatomia

```text
┌ tf-checkbox ─────────────┐   ┌ tf-radio-group (vertical) ─┐
│ [☑] Etykieta              │   │ Label grupy                │
└───────────────────────────┘   │ (○) Opcja A                │
                                 │ (●) Opcja B  — hint text   │
   box: 18×18 (patrz odst.)     └─────────────────────────────┘
   gap box↔label: 10px (kod)      box: 18×18, circle
```

Części: *box* (`role="checkbox"`/`role="radio"`, `<span>` stylowany — nie
natywny `<input>`), *label* (`TextStyle.body`), opcjonalny *hint* pod labelem
(tylko `tf-radio`, atrybut `hint`), *group label* nad listą opcji
(`tf-radio-group`).

## Warianty

| Wariant | Opis | Kiedy używać |
|---|---|---|
| `checkbox` — pojedynczy | zaznaczony/niezaznaczony | zgody, wielokrotny wybór niezależny |
| `checkbox` — `indeterminate` | myślnik zamiast ptaszka | „zaznacz wszystko” gdy część elementów listy jest zaznaczona |
| `radio` — lista pionowa/pozioma | jedna z N opcji | ≤ ~6 opcji wzajemnie wykluczających się |
| `radio` — `card` (na `tf-radio`, z `cards` na grupie) | opcja jako pełna karta z dowolną treścią light-DOM | wybór planu/trybu z ikoną i opisem |

## Rozmiary

sdk-spec `CheckboxSize` (`sm`/`md`/`lg`) — **niezaimplementowane**: box ma stały
rozmiar 18×18px w obu komponentach, niezależny od żadnego tokena
`control.icon_size` czy `control.height`. Cel dotykowy 44px nie jest osiągany
przez sam box — polega się na całym obszarze `<label>` (box + tekst) jako
klikalnym, co w praktyce daje więcej niż 44px szerokości, ale wysokość wiersza
zależy od `line-height` tekstu (13px `body` → ~20px, poniżej 44px).

## Stany

| Stan | Tło | Tekst/ikona | Obramowanie | Uwagi |
|---|---|---|---|---|
| default (unchecked) | `bg.input` | — | `border.default`, 2px | |
| hover | bez zmiany | — | `border.hover` | tylko `pointer: fine` |
| checked | `accent.primary` | biały ptaszek (`::after`, border-trick) | `accent.primary` | + `box-shadow` glow (`0 0 12px accent.primary@0.3`) |
| indeterminate | `accent.primary` | biała kreska (`::after`) | `accent.primary` | wizualnie różni się od `checked` tylko znakiem |
| focus-visible | bez zmiany | — | ring `control.focus_ring` | na `.tf-checkbox-input`/`.tf-radio-input`, nie na `<label>` |
| disabled | bez zmiany | — | bez zmiany | `opacity: 0.4` na całym `<label>`, `tabindex="-1"`, `aria-disabled="true"` |
| selected (radio w grupie) | `accent.primary` fill | — | `accent.primary` | tylko jeden radio w grupie ma `tabindex="0"` (roving tabindex) |

## Zachowanie

- Wskaźnik: klik na `<label>` (obejmuje box + tekst) przełącza stan; `tf-checkbox`
  czyszczy `indeterminate` przy każdym toggle (nie da się ręcznie wrócić do stanu
  pośredniego klikiem — tylko atrybutem).
- Klawiatura `tf-checkbox`: Space/Enter na zafokusowanym boxie przełącza. Tab
  wchodzi na każdy checkbox z osobna (brak grupowania).
- Klawiatura `tf-radio-group`: Space/Enter na zafokusowanym radiu wybiera go.
  **Brak obsługi strzałek** (`ArrowUp/Down/Left/Right`) do przełączania między
  opcjami grupy — odstępstwo od standardowego wzorca WAI-ARIA `radiogroup`
  (porównaj z `tf-segmented`, `toggle.md`, które strzałki mają). Tab wchodzi
  tylko na aktualnie wybrany radio (roving `tabindex`), pozostałe mają `-1`.
- Animacje: przejście tła/obramowania na `motion.duration_ms.fast`
  (kod: 0.15s/0.2s).
- Zdarzenia/API: `tf-checkbox` — atrybuty `checked`, `label`, `disabled`,
  `indeterminate`; property `.checked`/`.indeterminate`; event `change`
  (`detail.checked`). `tf-radio-group` — atrybuty `name`, `value`, `label`,
  `nested` (tryb bez przenoszenia light-DOM, do list z nagłówkami sekcji
  przeplecionymi z opcjami), `cards`, `orientation`; property `.value`; event
  `change` (`detail.value`). `tf-radio` (dziecko) czyta `checked` pośrednio przez
  porównanie `group.value === this.value` — nie ma własnego atrybutu `checked`.
- Zaznaczanie tekstu: dozwolone na labelu (nie blokowane `user-select`).

## Dostępność

`tf-checkbox`: `role="checkbox"` na boxie, `aria-checked` (`"true"`/`"false"`/
`"mixed"` dla indeterminate) — poprawne odwzorowanie stanu pośredniego w ARIA.
`tf-radio-group`: `role="radiogroup"` na wrapie (lub na hoście w trybie
`nested`), każdy `tf-radio` ma `role="radio"` + `aria-checked`. Brak natywnego
`<input type="checkbox">`/`<input type="radio">` oznacza, że komponent **musi**
poprawnie zarządzać `role`/`aria-checked`/`tabindex` ręcznie — co robi, ale bez
wsparcia strzałek (patrz wyżej) zachowanie odbiega od tego, czego czytniki ekranu
uczą użytkowników oczekiwać po `radiogroup`. Kontrast: box `border.default` na
`bg.input` — sprawdzić realnie (obramowanie 2px, nie tekst, więc próg AA dla
tekstu nie ma zastosowania wprost, ale WCAG 1.4.11 non-text contrast ≥ 3:1
dotyczy).

## Responsywność i platformy

Brak specjalnej logiki responsywnej — `tf-radio-group` z atrybutem
`orientation` udostępnia hook CSS (`.tf-radio-group__list`) dla modyfikatorów
SDK, ale sam layout (`flex-direction`) nie jest widoczny w tym pliku (patrz CSS
poza zakresem tego audytu). Na ESP32-P4/mobile: 18px box + brak wymuszonego
44px celu dotykowego na samym boxie — polegaj na całym wierszu `<label>` jako
obszarze klikalnym i dodawaj padding w layoutach dotykowych.

## Tokeny użyte

`color.themes.dark.bg.input`, `color.themes.dark.accent.primary`,
`color.themes.dark.border.default`, `color.themes.dark.border.hover`,
`typography.scale.body`, `typography.scale.caption` (hint), `spacing.sm`,
`radius.xs` (box checkboxa, `usage.xs = "checkbox, tags, code chips"`),
`radius.circle` (box radio), `control.focus_ring`, `control.touch_target_min`,
`motion.duration_ms.fast`.

## Znane odstępstwa w kodzie (2026-09-14)

- **Box 18×18px nie jest powiązany z żadnym tokenem** — nie ma odpowiednika w
  `control.icon_size` (12/16/20/24/32) ani osobnej skali; wartość własna
  powtórzona w `tf-checkbox-input` (`controls.css:3416-3417`) i
  `tf-radio-input` (`controls.css:3371-3372`).
- **Brak `CheckboxSize`/rozmiaru w ogóle** — sdk-spec definiuje `sm`/`md`/`lg`
  dla `Checkbox`, HTML ma jeden stały rozmiar.
- **`tf-radio-group` nie obsługuje strzałek klawiatury** mimo `role="radiogroup"`
  — niezgodność z wzorcem WAI-ARIA Authoring Practices dla radiogroup (Arrow
  keys move selection). `tf-segmented` (funkcjonalnie bardzo podobny, też
  `role="radiogroup"`) tę obsługę ma (`tf-segmented.js:120-136`) — wewnętrzna
  niespójność między dwoma komponentami o tej samej roli ARIA.
- **`indeterminate` nie ma odpowiednika w `tf-radio`** (naturalne — radio nie ma
  stanu pośredniego w ARIA), ale sdk-spec też go nie przewiduje dla `Radio`, więc
  to nie jest odstępstwo, tylko potwierdzenie zgodności.

## Przykłady

```html
<tf-checkbox label="Akceptuję regulamin" checked></tf-checkbox>
<tf-checkbox label="Zaznacz wszystkie" indeterminate></tf-checkbox>

<tf-radio-group name="plan" value="pro" label="Wybierz plan">
  <tf-radio value="free" label="Free" hint="Do 3 projektów"></tf-radio>
  <tf-radio value="pro" label="Pro" hint="Bez limitów"></tf-radio>
</tf-radio-group>
```

```rust
// natywnie (tenta-ui-widgets, API docelowe)
Checkbox::new("Akceptuję regulamin")
    .checked(state.accepted)
    .on_change(Msg::ToggleAccepted)

RadioGroup::new("plan")
    .label("Wybierz plan")
    .options([
        RadioOption::new("free", "Free").hint("Do 3 projektów"),
        RadioOption::new("pro", "Pro").hint("Bez limitów"),
    ])
    .value(state.plan)
    .on_change(Msg::PlanChanged)
```
