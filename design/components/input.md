# Input

| | |
|---|---|
| Tier | 0 (MVP); `Textarea` — 1 |
| HTML | `<tf-input>` — `tentaflow-core/www/js/components/tf-input.js`, style `controls.css` linie 265–410; `<tf-textarea>` — `tf-textarea.js` (osobny komponent, patrz odstępstwa) |
| Protokół addonów | `0x0301` `Input` (typy text/email/password/url/phone/number/search) i `0x0302` `Textarea` — `docs/ADDON_UI_COMPONENT_CATALOG_v1.md` §5, linie 2001–2075 |
| Natywny (TentaEngine) | `tenta_ui_widgets::TextField` — status: planowany |
| Status dokumentu | draft |

Pojedyncze pole tekstowe (`Input`) lub wieloliniowe (`Textarea`) z etykietą,
podpowiedzią i błędem. Nie używaj do wyboru z listy skończonej (patrz `select.md`)
ani do wyszukiwania pełnoekranowego (`tf-searchbox`, osobny komponent, poza tym
specem).

## Anatomia

```text
┌ Label (caption_strong, uppercase) ─────────────────┐
│ ┌───────────────────────────────────────────────┐ │
│ │ [prefix] [icon] tekst wpisywany [suffix] [icon]│ │  ← .tf-input-wrap
│ └───────────────────────────────────────────────┘ │
│ Hint / Error (caption)                              │
└──────────────────────────────────────────────────────┘
  height: control.height.md (36, kod: 40px — patrz odstępstwa)
  padding-x: spacing.md (12, kod: 14px)   icon: icon_size.sm (16)
```

Części: *label* (opcjonalny, `TextStyle.caption_strong`, uppercase, `text.secondary`
— na focus zmienia kolor na `accent.primary` i skaluje 0.95×), *wrap* (właściciel
obramowania/tła/ringu ogniskowania — dopiero gdy jest ikona/prefix/suffix; samo
`.tf-input` bez wrapu ma własne obramowanie), *leading/trailing icon*, *prefix/suffix*
(tekstowe afiksy typu „$”, „kg”), *hint* (`caption`, `text.muted`), *error*
(`caption`, `semantic.critical`) — hint i error są wzajemnie wykluczające się
(error nadpisuje hint).

## Warianty

| Typ (`InputType`) | HTML `type` | Uwagi |
|---|---|---|
| `text` | `type="text"` (domyślny) | |
| `password` | `type="password"` | brak przełącznika „pokaż hasło” w kodzie |
| `number` | `type="number"` | pass-through `min`/`max`/`step` |
| `search` | `type="search"` | brak przycisku czyszczenia w `tf-input` — to ma osobny `tf-searchbox` |
| `email` / `url` / `phone` | pass-through natywnego `type` | walidacja przeglądarki, brak własnej |
| `InputVariant.outlined` / `ghost` | — | **niezaimplementowane w HTML** — `tf-input` zna tylko jeden, obramowany wygląd |

## Rozmiary

sdk-spec `InputSize` (`sm`/`md`/`lg`) — **niezaimplementowane w HTML**: `tf-input`
nie ma atrybutu `size`, jest tylko jeden rozmiar (~40px wysokości). Docelowo:

| Rozmiar | Wysokość | Typografia | Padding-x |
|---|---|---|---|
| `sm` | `control.height.sm` (28) | `caption` | `spacing.sm` (8) |
| `md` | `control.height.md` (36) | `body` | `spacing.md` (12) |
| `lg` | `control.height.lg` (44) | `body_lg` | `spacing.lg` (16) |

## Stany

| Stan | Tło | Tekst/ikona | Obramowanie | Uwagi |
|---|---|---|---|---|
| default | `bg.input` | `text.primary` | `border.default` | |
| hover | bez zmiany tła | bez zmiany | `border.hover` | tylko `pointer: fine` |
| focus | `bg.card` | ikona → `accent.primary` | `accent.primary` + `glow.accent` (box-shadow) | label flashuje kolor przez 220ms |
| disabled | bez zmiany | bez zmiany | bez zmiany | `opacity: 0.6` na wrapie, `cursor: not-allowed`; natywny `disabled` na `<input>` |
| readonly | bez zmiany | bez zmiany | bez zmiany | wartość zaznaczalna/kopiowalna, w przeciwieństwie do `disabled` |
| error / invalid | bez zmiany | error-text → `semantic.critical` | `semantic.critical` | focus na błędnym polu pokazuje `glow.critical`; **brak `aria-invalid`** (patrz odstępstwa) |
| char counter | — | — | — | **niezaimplementowany** mimo `max_length` w polach protokołu |

## Zachowanie

- Wskaźnik: klik/focus otwiera pole do edycji natywnie; `autofocus` działa tylko
  przy pierwszym montażu (kolejne wymagają ręcznego `.focus()`).
- Klawiatura: natywne zachowanie `<input>`/`<textarea>` — Tab/Shift+Tab, strzałki
  w tekście, Enter (w `<input>` wywołuje `change`, w `tf-textarea` — nowa linia;
  `tf-textarea` re-emituje `keydown` jako `tf-keydown`, żeby moduł mógł zrobić
  Ctrl+Enter = wyślij).
- IME: pole to natywny `<input>`/`<textarea>` bez przechwytywania `keydown` na
  poziomie znaków (poza `tf-tag-input`, patrz `chip.md`) — kompozycja IME
  (japoński/chiński/koreański) działa bez dodatkowej obsługi; `tf-combobox`/
  `tf-multiselect` (patrz `select.md`) nasłuchują `keydown` na Enter/strzałki, co
  **może** przerywać kompozycję IME w edge case'ach (nieprzetestowane — flaga
  ryzyka, nie potwierdzony bug).
- Animacje: obramowanie/tło/cień na `motion.duration_ms.fast`–`normal` (kod:
  0.15–0.25s, zbliżone do `fast`/`normal`), label-scale na `easing.standard`
  (kod: `--tf-spring-smooth`).
- Zdarzenia/API: atrybuty `label`, `placeholder`, `value`, `hint`, `error`, `type`,
  `icon`, `trailing-icon`, `prefix`, `suffix`, `disabled`, `readonly`,
  `autocomplete`, `autofocus`, `required`, `name`, `autocapitalize`, `autocorrect`,
  `spellcheck`, `inputmode`, `minlength`, `maxlength`, `pattern`, `multiline`,
  `rows`, `min`, `max`, `step`. Property `.value` (get/set). Eventy `input`/`change`
  (custom, `detail.value`, `stopPropagation` na natywnym evencie, żeby konsument
  nie dostał dwóch eventów). W protokole: handlery `input`/`change`/`submit`/
  `focus`/`blur`.
- Zaznaczanie tekstu: dozwolone (to podstawowa funkcja pola).

## Dostępność

Rola: natywna (`textbox`) przez `<input>`/`<textarea>`. Etykieta: wizualny
`<span class="tf-label">` **nie jest** powiązany atrybutem `for`/`id` ani
`aria-labelledby` z polem — to odstępstwo od wzorca `label` (patrz niżej).
Błąd: tekst błędu renderowany wizualnie, ale bez `aria-invalid="true"` ani
`aria-describedby` wskazującego na `.tf-error-text` — czytnik ekranu nie dowie
się o błędzie przy nawigacji do pola. Kontrast: `text.primary` na `bg.input`
15.4:1 (AA), `text.muted` (hint) na `bg.input` — sprawdzić realnie, `text.muted`
jest zastrzeżone jako „nigdy nie tekst główny” w `tokens.json`, hint jest bliski
granicy. `prefers-reduced-motion`: brak jawnej reguły.

## Responsywność i platformy

Pełna szerokość kontenera domyślnie (`tf-input { display: block; width: 100% }`
w `controls.css:72-77`). Na ESP32-P4: 40px wysokości mieści się w minimalnym celu
dotykowym (44px) z marginesem — warto rozważyć `lg` po wdrożeniu skali rozmiarów.
Klawiatura ekranowa: `inputmode`/`autocapitalize`/`autocorrect`/`spellcheck` są
przekazywane 1:1 do natywnego pola — poprawne wsparcie na mobile/WebView.

## Tokeny użyte

`color.themes.dark.bg.input`, `color.themes.dark.bg.card`,
`color.themes.dark.text.primary`, `color.themes.dark.text.secondary`,
`color.themes.dark.text.muted`, `color.themes.dark.border.default`,
`color.themes.dark.border.hover`, `color.themes.dark.border.focus`,
`color.themes.dark.semantic.critical`, `typography.scale.caption`,
`typography.scale.caption_strong`, `typography.scale.body`, `spacing.sm`,
`spacing.md`, `radius.md`, `control.height.sm/md/lg`, `control.icon_size.sm`,
`control.focus_ring`, `motion.duration_ms.fast/normal`.

## Znane odstępstwa w kodzie (2026-09-14)

- **Brak licznika znaków.** Ani `tf-input.js`, ani `tf-textarea.js` nie renderują
  licznika mimo pass-through `maxlength`/`minlength` — pole protokołu
  `max_length` (§5 0x0301/0x0302) nie ma odpowiednika wizualnego w HTML.
- **Dwa niezależne komponenty dla wieloliniowego pola.** `tf-input[multiline]`
  (przez `<textarea>` w tym samym elemencie, `tf-input.js:91`) **i** osobny
  `<tf-textarea>` (`tf-textarea.js`) współistnieją z różnym zestawem atrybutów
  (`tf-textarea` ma `autogrow`, `tf-input[multiline]` nie ma auto-grow wcale) —
  do zunifikowania.
- **Brak `size` (`sm`/`md`/`lg`).** `tf-input.js` `observedAttributes`
  (linia 13) nie zawiera `size` — jeden rozmiar na sztywno, `min-height: 40px`
  (`controls.css:294,346`), który nie odpowiada żadnemu `control.height`.
- **Brak `aria-invalid`/`aria-describedby` dla błędu** — `_update()` w
  `tf-input.js` (linie 219–258) ustawia tylko klasy `.tf-input-error`/
  `.tf-input-wrap-error`, żadnych atrybutów ARIA.
- **Label nie jest programowo powiązany z polem** — brak `for`/`id` lub
  `aria-labelledby` między `.tf-label` a `<input>` w `tf-input.js` (w
  przeciwieństwie do `tf-combobox.js`, który to robi poprawnie, linie 159–168).
- **`InputVariant.ghost`** (bezobramowane pole) nie istnieje w HTML.

## Przykłady

```html
<tf-input label="Email" type="email" icon="mail" placeholder="jan@firma.pl"
          hint="Używany do logowania"></tf-input>
<tf-input label="Hasło" type="password" error="Za krótkie hasło"></tf-input>
<tf-textarea label="Opis" rows="4" autogrow maxlength="500"></tf-textarea>
```

```rust
// natywnie (tenta-ui-widgets, API docelowe)
TextField::new()
    .label("Email")
    .input_type(InputType::Email)
    .leading_icon(Icon::Mail)
    .hint("Używany do logowania")
    .on_change(Msg::EmailChanged)

TextField::multiline()
    .label("Opis")
    .rows(4)
    .max_length(500)
    .on_change(Msg::DescriptionChanged)
```
