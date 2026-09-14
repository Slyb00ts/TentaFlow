# Form group

| | |
|---|---|
| Tier | 0 (MVP) |
| HTML | brak samodzielnego `<tf-form-group>` — renderowane jako `.tf-form-field`/`.tf-form-group` przez `sdk-runtime/form-wrappers-renderer.js`, style `controls.css` linie 6627-6757 |
| Protokół addonów | `0x031A` `FormField` + `0x031B` `FormGroup` — `docs/ADDON_UI_COMPONENT_CATALOG_v1.md` §5 |
| Natywny (TentaEngine) | `tenta_ui_widgets::FormField` / `FormGroup` — status: planowany |
| Status dokumentu | draft |

**Uwaga terminologiczna:** klasa CSS `.form-group` (jak w Bootstrap) **nie
istnieje** w tym repozytorium — jedyne trafienie jest `.cluster-routing-grid
.form-group-actions` w `style.css:3619`, ad hoc na jednej stronie. Struktura
„label + control + help + error” jest realnie zaimplementowana jako
`FormField` (0x031A, klasy `.tf-form-field*`) opakowywana opcjonalnie w
`FormGroup` (0x031B, klasy `.tf-form-group*`) — te dwa komponenty są
przedmiotem tego dokumentu. `tf-option-row` (wiersz listy wyboru, osobny
komponent — patrz `www/js/components/tf-option-row.js`) **nie jest** częścią
formularzy w kodzie; jest tu wymieniony bo pojawia się w tym samym pliku co
kontrolki formularzowe, ale jego użycie to listy/comboboxy (`aria-current`,
event `option-select`), nie label+input+help+error.

## Anatomia

```text
┌──────────────────────────────────┐
│ Etykieta *                       │  ← .tf-form-field__label (+ __required-mark)
│ [    input dziecka    ]          │  ← .tf-form-field__child (dowolny Component)
│ Podpowiedź pomocnicza             │  ← .tf-form-field__hint (opcjonalny)
│ Komunikat błędu                   │  ← .tf-form-field__error, role="alert"
└──────────────────────────────────┘
```

`FormGroup` (`.tf-form-group`) to nagłówek (`<header>`, opcjonalny tytuł +
opis, opcjonalnie zwijalny przyciskiem `▸`/`▾`) + `.tf-form-group__body`
(flex column) zawierający dowolną liczbę `FormField`. `FormSection`
(0x031C, poza zakresem tej nazwy ale sąsiaduje w tym samym pliku) to
cięższa wersja z `<h2>` i domyślnym `divider_top: true`.

## Warianty

`FormField.layout` (`FormFieldLayout`):

| Wariant | Wygląd | Kiedy używać |
|---|---|---|
| `stacked` (domyślny) | label nad kontrolką, `flex-direction: column` | większość pól |
| `horizontal` | label i kontrolka w rzędzie, label `flex: 0 0 12em` | ustawienia/formularze gęste, etykieta krótka |

`Form.layout` (`FormLayout`, kontener nadrzędny `<form>`): `stacked`
(domyślny), `horizontal` (`flex-direction: row; flex-wrap: wrap`),
`compact` (mniejszy `gap: spacing.sm`).

## Rozmiary

Brak tokenu `sm`/`md`/`lg` na poziomie `FormField`/`FormGroup` — rozmiar
kontrolki dziecka (np. `tf-input size="md"`) jest niezależny. Cel dotykowy
44px pochodzi z samej kontrolki, nie z wrappera.

## Stany

| Stan | Tło | Tekst/ikona | Obramowanie | Uwagi |
|---|---|---|---|---|
| default | — | label `--tf-text` | — | |
| required | — | gwiazdka `--tf-danger` po labelu | — | `.tf-form-field--required` (klasa dodana, ale bez dedykowanego stylu poza `__required-mark`) |
| invalid | — | label `--tf-danger` | — | `.tf-form-field--invalid`, ustawiane reaktywnie gdy `error` BindRef ma wartość niepustą |
| disabled (przez `Form`) | opacity 0.5 na całym `<form>` | — | — | `.tf-form[data-disabled]`, `pointer-events: none` — blokuje **cały formularz naraz**, nie pojedyncze pole |
| collapsed (`FormGroup`) | — | — | — | `bodyEl.hidden = true`, przycisk `aria-expanded="false"` |
| expanded (`FormGroup`) | — | — | — | `.tf-form-group--expanded`, chevron obrócony 90° |

## Zachowanie

- Interakcja wskaźnikiem: przycisk zwijania `FormGroup` (`.tf-form-group__toggle`)
  przełącza `bodyEl.hidden`. Jeśli `expanded` jest powiązane `BindRef`,
  kliknięcie **nie** zmienia stanu lokalnie — emituje event `toggle`
  (`detail: { value, kind: 'bool', bind }`) i czeka, aż host zapisze nową
  wartość z powrotem (write-back przez warstwę dispatch); bez `BindRef`
  przełącza stan lokalnie od razu.
- Klawiatura: przycisk zwijania jest zwykłym `<button type="button">` — Tab,
  Enter/Space działają natywnie. Pola formularza dziedziczą klawiaturę ze
  swoich kontrolek (`Input`, `Select`, …).
- Animacje: **brak** — zwinięcie/rozwinięcie to natychmiastowe `hidden`
  toggle, bez `transition`/`max-height` animacji. Jedyna animacja w tym
  obszarze to obrót chevronu (`transform: rotate(90deg)`, `transition:
  transform 0.15s ease` — nie token `motion.duration_ms`).
- Zdarzenia/API `Form` (0x031D): przechwytuje natywny `submit` (zawsze
  `novalidate`, nigdy nie robi natywnego POST), emituje `submit_form`
  (`detail: { scope_id, validators }`) i `reset_form` (`detail: { scope_id
  }}`) — oba **nie bąbelkują** (`bubbles: false`), więc słuchacz musi wisieć
  bezpośrednio na `<form>`. `Form.disabled` (`BindRef<bool>`) ustawia
  `disabled` na wszystkich natywnych i `tf-*` kontrolkach potomnych przez
  `querySelectorAll`, oznaczając je `data-tf-form-disabled` — przy wyłączeniu
  czyści tylko te, które sam ustawił, nigdy pola z własnym `disabled`
  ustawionym przez inne wiązanie.
  `FormField` powiązuje `label`↔`child` przez `aria-labelledby` (nie
  `<label for>`, bo dziecko może być dowolnym wrapperem, nie zawsze
  natywnym `<input id=...>`).
- Zaznaczanie tekstu: dozwolone (label/hint/error to tekst).

## Dostępność

`FormField` ustawia `aria-labelledby` na dziecku (id wygenerowane z
`component.id`), `aria-required="true"` gdy `required`, `aria-describedby`
łączące hint i błąd (dynamicznie dopisywane/usuwane z listy id przy zmianie
stanu błędu — kod pilnuje braku duplikatów). Komunikat błędu ma
`role="alert"` (ogłaszany asertywnie przy pojawieniu się). `FormGroup`
zwijalny: `aria-controls` na przycisku wskazuje na `id` ciała, `aria-expanded`
odzwierciedla stan. `Form` wyłączony: `aria-disabled="true"` +
`data-disabled` na `<form>`.

## Responsywność i platformy

**Brak reguł `@media` w `controls.css` dla `.tf-form-field`/`.tf-form-group`/
`.tf-form`** — nie ma przejścia „jedna kolumna poniżej `content.narrow` (800),
dwie kolumny od `lg`”. `FormField.layout="horizontal"` jest statyczny
(zawsze rząd, `label flex: 0 0 12em`) niezależnie od szerokości viewportu —
na wąskim ekranie label 12em może wypychać kontrolkę poza czytelną szerokość,
bo nic nie przełącza layoutu z powrotem na `stacked`.

## Tokeny użyte

- `spacing.xs` (`--tf-space-xs`, gap `.tf-form-field`)
- `spacing.md` (`--tf-space-md`, gap layout horizontal, nagłówek sekcji)
- `spacing.zero`…`spacing.xxl` — pełna ośmiostopniowa skala mapowana wprost
  na `.tf-form-group--spacing-*`/`.tf-form-section--spacing-*` (gap ciała)
- `color.themes.dark.text.primary` — label
- `color.themes.dark.text.muted` (`--tf-text-3`) — hint, opis grupy
- `color.themes.dark.semantic.critical` (`--tf-danger`) — błąd, gwiazdka wymagalności
- `color.themes.dark.border.default` — `divider_top` w `FormSection`
- `layout.content.narrow` (800) — **niewykorzystany dziś w CSS**, patrz odstępstwa

## Znane odstępstwa w kodzie (2026-09-14)

- **Brak klasy `.form-group`.** Nazwa z zadania nie odpowiada niczemu w
  kodzie poza jednym ad hoc selektorem strony (`style.css:3619`). Realny
  odpowiednik to `FormField`/`FormGroup` opisane wyżej.
- **`tf-option-row` nie ma nic wspólnego z formularzami** — to wiersz listy
  wyboru (comboboxy, palety poleceń), z własnym eventem `option-select` i
  atrybutem `selected`/`aria-current`. Wzmianka o nim w opisie zadania jest
  myląca; udokumentowany tu jedynie dla jasności rozróżnienia.
- **Brak responsywnego przełączania 1-kolumnowy/2-kolumnowy.** Ani
  `content.narrow` (800), ani `layout.breakpoints_px.lg` (1280) nie są
  użyte w regułach `.tf-form-field`/`.tf-form-group` — layout horyzontalny
  jest stały niezależnie od szerokości ekranu.
- **Brak paska submit/cancel.** Nie ma klasy `.tf-form-actions`/
  `.form-actions` w `controls.css` — `Form` renderuje tylko dzieci
  przekazane w polu `children`; przyciski submit/cancel to zwykłe `Button`
  wstawione ręcznie przez wywołującego, bez dedykowanego stylu paska akcji.
- **Walidacja „on blur, potem on change” nie istnieje jako mechanizm
  komponentu.** `FormField.error` to zwykły `BindRef<tstr>` — to host/addon
  decyduje, kiedy go ustawić (blur, change, submit); `form-wrappers-renderer.js`
  tylko subskrybuje i renderuje, nie zawiera własnej logiki timing walidacji.

## Przykłady

```html
<!-- wynik renderowania FormField/FormGroup przez sdk-runtime, nie ręczny markup -->
<section class="tf-form-group tf-form-group--spacing-md">
  <header class="tf-form-group__header">
    <h3 class="tf-form-group__title">Dane konta</h3>
  </header>
  <div class="tf-form-group__body">
    <div class="tf-form-field tf-form-field--layout-stacked tf-form-field--required">
      <div class="tf-form-field__label" id="f1-label">Nazwa<span class="tf-form-field__required-mark" aria-hidden="true">*</span></div>
      <tf-input class="tf-form-field__child" aria-labelledby="f1-label" aria-required="true"></tf-input>
      <div class="tf-form-field__hint">Widoczna w panelu zespołu.</div>
    </div>
  </div>
</section>
```

```rust
// natywnie (tenta-ui-widgets, API docelowe)
FormGroup::new("Dane konta")
    .child(
        FormField::new("Nazwa", TextInput::new().bind(Model::name))
            .required(true)
            .hint("Widoczna w panelu zespołu.")
            .error_if(|m| m.name.is_empty(), "Pole wymagane")
    )
```
