# Tooltip

| | |
|---|---|
| Tier | 0 (MVP) |
| HTML | `<tf-tooltip>` — `tentaflow-core/www/js/components/tf-tooltip.js`, style `controls.css` linie 4235–4278 (plus druga, równoległa implementacja `.tf-tooltip` dla renderera protokołu, linie 5220–5234, patrz odstępstwa) |
| Protokół addonów | `0x010F` `Tooltip` — `docs/ADDON_UI_COMPONENT_CATALOG_v1.md` §3, linie 1344–1354 |
| Natywny (TentaEngine) | `tenta_ui_widgets::Tooltip` — status: planowany |
| Status dokumentu | draft |

Krótka podpowiedź pokazywana po najechaniu/zafokusowaniu elementu — dopowiedzenie
etykiety ikony, skrót klawiszowy, dodatkowy kontekst. **Nigdy nie umieszczaj w
tooltipie informacji koniecznej do wykonania zadania** — treść musi być
dostępna też bez najeżdżania (np. przez widoczny tekst, `aria-label`, komunikat
błędu). Nie używaj do dłuższych wyjaśnień (to `Popover`, poza tym specem) ani do
potwierdzania akcji (to `Modal`/`Alert`).

## Anatomia

```text
        ┌────────────────────┐
        │ Zapisz zmiany       │  ← .tf-tooltip-bubble, role="tooltip"
        └──────────┬─────────┘
                    ▼
              [ Ikona 💾 ]         ← .tf-tooltip-wrap (dowolne dziecko)
```

Części: *wrap* (owija dowolną zawartość light-DOM bez zmiany jej semantyki),
*bubble* (`role="tooltip"`, tło `bg.card`, obramowanie `border.default`, cień
`elevation.subtle`, tekst `caption`). Strzałka wskazująca element **nie jest
zaimplementowana** — sama pozycja (odsunięcie 6px) sugeruje kierunek bez grota.

## Warianty

Brak wariantów wizualnych w sdk-spec (`Tooltip` ma tylko `side`/`max_width_px`)
— jeden wygląd, cztery pozycje:

| `side` (HTML) / `DrawerSide` (sdk-spec) | Pozycja bąbla |
|---|---|
| `top` (domyślny) | nad elementem, wyśrodkowany poziomo |
| `bottom` | pod elementem, wyśrodkowany poziomo |
| `left` | po lewej, wyśrodkowany pionowo |
| `right` | po prawej, wyśrodkowany pionowo |

## Rozmiary

Brak skali rozmiarów — `max_width_px` w sdk-spec (pole 3, `0x010F`) sugeruje
limit szerokości, którego `tf-tooltip.js` nie realizuje (`white-space: nowrap`
w CSS, więc bąbel rośnie z długością tekstu bez zawijania — ryzyko wyjścia poza
viewport przy długim tekście).

## Stany

| Stan | Tło | Tekst/ikona | Obramowanie | Uwagi |
|---|---|---|---|---|
| ukryty | — | — | — | `opacity: 0`, `pointer-events: none` (nigdy nie przechwytuje kliknięć) |
| widoczny (hover) | `bg.card` | `text.primary` | `border.default` | pokazuje się **natychmiast** na `:hover`, niezależnie od atrybutu `delay` (patrz odstępstwa) |
| widoczny (focus) | jw. | jw. | jw. | pokazuje się po `delay` ms (domyślnie 400) — jedyna ścieżka, gdzie `delay` faktycznie działa |
| widoczny (dotyk) | **brak stanu** | — | — | brak jakiejkolwiek obsługi touch/long-press w kodzie (patrz odstępstwa) |

## Zachowanie

- Wskaźnik (mysz): `mouseenter` uruchamia timer `delay` ms, ale CSS
  `:hover` pokazuje bąbel przez `opacity: 1` **od razu**, bez czekania na klasę
  `.show` dodawaną przez timer — więc na myszy tooltip pojawia się natychmiast
  (0.15s tranzycja opacity), a `delay` jest efektywnie martwym kodem dla tej
  ścieżki. `mouseleave` chowa natychmiast (czyści timer, usuwa `.show`, ale to
  i tak nie miało znaczenia dla `:hover`).
- Wskaźnik (dotyk): **brak obsługi**. Nie ma `touchstart`/long-press — na
  urządzeniu dotykowym (`pointer: coarse`, brak `:hover` realnego) tooltip
  praktycznie nigdy się nie pokaże, bo nie ma zdarzenia, które by go wywołało
  poza focusem (a elementy niefokusowalne, np. ikona bez `tabindex`, nie dają
  żadnej drogi dostępu do treści tooltipa na dotyku). To sprzeczne z zasadą
  „nigdy nie chowaj koniecznej informacji w tooltipie” — na dotyku ta
  informacja jest praktycznie nieosiągalna.
- Klawiatura: `focusin` na dowolnym fokusowalnym dziecku pokazuje bąbel po
  `delay` ms, `focusout` chowa natychmiast. **Brak obsługi `Escape`** do
  ręcznego zamknięcia przed `focusout`.
- Animacje: `opacity` 0→1 na `motion.duration_ms.fast` (kod: 0.15s). Brak
  animacji pozycji/skali (tylko fade).
- Zdarzenia/API: atrybuty `text`, `side`, `delay` (ms, domyślnie 400 — liczba
  własna, nie token `motion.duration_ms`). Brak eventów wychodzących (czysto
  prezentacyjny, konsument nie dowiaduje się o pokazaniu/ukryciu).
- Zaznaczanie tekstu: tooltip ma `pointer-events: none` — nie da się zaznaczyć
  ani skopiować jego treści (zamierzone dla efemerycznego UI, ale problematyczne
  jeśli tooltip kiedykolwiek nosi dane do skopiowania, np. pełny hash).

## Dostępność

`role="tooltip"` na bąblu — poprawne. **Brak `aria-describedby`** łączącego
element wyzwalający z bąblem — czytnik ekranu, który wchodzi fokusem na
element, nie ma programowego związku z treścią tooltipa poza tym, że tekst
bąbla akurat staje się widoczny w DOM (zależnie od czytnika, może być
ogłoszony przypadkowo albo wcale). To główna luka a11y komponentu — do
naprawienia przed uznaniem za `reviewed`. Zasada „tooltip nigdy nie jest
jedynym nośnikiem koniecznej informacji” nie jest dziś wymuszana przez kod
(żaden lint/test nie sprawdza, czy element z `tf-tooltip` ma też `aria-label`
lub widoczny tekst).

## Responsywność i platformy

**Brak strategii dotykowej** — patrz „Zachowanie” wyżej; to największa luka
platformowa komponentu. Docelowo (nie zaimplementowane): long-press ~500ms na
dotyku pokazuje tooltip, tap poza nim chowa, tooltip nigdy nie jest jedyną
drogą do informacji na ESP32-P4 (tam nie ma pojęcia „hover” w ogóle —
`platforms/esp32p4.md`, `pointer: coarse`). Brak logiki `flip`/`shift` gdy
bąbel wychodzi poza viewport (np. `side="top"` blisko górnej krawędzi ekranu).

## Tokeny użyte

`color.themes.dark.bg.card`, `color.themes.dark.text.primary`,
`color.themes.dark.border.default`, `elevation.subtle`,
`typography.scale.caption`, `radius.sm`, `spacing.xs`,
`motion.duration_ms.fast`, `layout.z_index.tooltip` (1300 w tokenach; kod:
`z-index: 10000`, patrz odstępstwa).

## Znane odstępstwa w kodzie (2026-09-14)

- **CSS `:hover` pokazuje tooltip natychmiast, ignorując `delay`.**
  `controls.css:4274-4277` (`.tf-tooltip-wrap:hover .tf-tooltip-bubble { opacity: 1 }`)
  działa niezależnie od klasy `.show`, którą `_show()` w `tf-tooltip.js:87-92`
  dodaje dopiero po `delay` ms. Efekt: atrybut `delay` ma znaczenie tylko dla
  focusu klawiaturą, nie dla myszy — sprzeczne z oczekiwanym zachowaniem
  „pokaż po chwili, nie natychmiast”, i z opisem w pliku (`delay` jako parametr
  ogólny, nie tylko dla focusu).
- **Zero obsługi dotyku (long-press).** Task zakłada „touch behaviour =
  long-press”; w kodzie nie ma żadnego `touchstart`/`touchend`/long-press timera
  — potwierdzona, poważna luka.
- **Brak `aria-describedby`** łączącego wyzwalacz z treścią tooltipa.
- **Druga, równoległa implementacja CSS** dla renderera protokołu addonów
  (`.tf-tooltip`/`.tf-tooltip--side-*`, `controls.css:5220-5234`) używa innych
  nazw klas i innych tokenów kolorów (`--tf-bg-inverse`, którego nie ma w
  `tokens.json` jako osobnego wpisu) niż `tf-tooltip.js`
  (`.tf-tooltip-bubble`/`.side-*`) — dwa niezależne systemy wizualne dla tego
  samego komponentu, potwierdzające rozjazd HTML vs protokół addonów opisany w
  `design/MIGRATION.md`.
- **`z-index: 10000`** (`controls.css:4252`) zamiast `layout.z_index.tooltip`
  (1300) — literał znacznie przewyższający token, działa przypadkiem (zawsze
  na wierzchu), ale nie jest odwołaniem do skali.
- **Brak `max_width_px`** z pola protokołu — `white-space: nowrap` w CSS
  (linia 4248) oznacza, że długi tekst rozciąga bąbel bez ograniczenia.

## Przykłady

```html
<tf-tooltip text="Zapisz zmiany" side="top">
  <tf-button variant="ghost" icon="save" aria-label="Zapisz"></tf-button>
</tf-tooltip>
<tf-tooltip text="Skrót: Ctrl+K" side="bottom" delay="600">
  <tf-chip clickable>Paleta poleceń</tf-chip>
</tf-tooltip>
```

```rust
// natywnie (tenta-ui-widgets, API docelowe)
Tooltip::new("Zapisz zmiany")
    .side(DrawerSide::Top)
    .child(IconButton::new(Icon::Save).aria_label("Zapisz"))
```
