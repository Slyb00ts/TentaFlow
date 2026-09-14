# Toast

| | |
|---|---|
| Tier | 0 (MVP) |
| HTML | `<tf-toast>` — `www/js/components/tf-toast.js`, style `controls.css` linie 3525-3594, 7954-7973 |
| Protokół addonów | `0x0504` `Toast` — `docs/ADDON_UI_COMPONENT_CATALOG_v1.md` §7 |
| Natywny (TentaEngine) | `tenta_ui_widgets::Toast` — status: planowany |
| Status dokumentu | draft |

Krótkie powiadomienie efemeryczne, poza flow strony. Do potwierdzeń akcji
(„Zapisano”) i błędów przejściowych. Nie używaj do błędów blokujących akcję
użytkownika — do tego jest `Alert`/`Banner` osadzony w treści.

Uwaga: w repozytorium współistnieją **dwie** ścieżki tworzenia toastów z
odmiennym zachowaniem — statyczna metoda `TfToast.show()` (wołana wprost z
modułów przez lokalny helper `toast(msg, tone)`) i deklaratywny render
protokołu addonów (`Command::Toast` → `<tf-toast persistent>`). Ten dokument
opisuje obie i oznacza różnice.

## Anatomia

```text
┌───────────────────────────────────┐
│ Tytuł                          [×]│  ← .tf-toast-title, .tf-toast-close
│ Treść wiadomości                   │  ← .tf-toast-message
└───────────────────────────────────┘
   border-left: 3px solid <tone>       padding: 14px 36px 14px 16px
```

Kontener stosu: `#tf-toast-container` (`.tf-toast-container`), tworzony
leniwie przy pierwszym `TfToast.show()`. `slot="icon"`/`slot="action"` (dzieci
z tym atrybutem, przed `title`/po `message`) są zachowywane przez `_build()`.

## Warianty

| `tone` (HTML `TfToast.show`) | `Tone` (protokół `0x0504`) | Pasek | Kiedy używać |
|---|---|---|---|
| `success` | `success` | `--tf-success` | operacja się powiodła |
| `danger` | `critical` | `--tf-danger` | błąd | 
| `warning` | `warning` | `--tf-warning` | ostrzeżenie, degradacja |
| `info` (domyślny) | `info` | `--tf-info` | informacja neutralna |
| — | `primary`/`neutral`/`muted` | (klasy `.tf-toast--tone-*` istnieją w CSS 7969-7973, ale `TfToast.show()` nie akceptuje tych wartości) | tylko przez protokół |

Nazwy tonów **nie są identyczne** między obiema ścieżkami: HTML używa
`danger`, protokół (i `Tone` z `tokens.rs`) używa `critical` dla tego samego
koloru — renderer protokołu mapuje `critical → danger` przy ustawianiu
atrybutu `tone` na `<tf-toast>` (`TONE_TO_ALERT`), więc atrybut finalny w DOM
jest zawsze jedną z czterech wartości HTML.

## Rozmiary

Brak tokenu rozmiaru — toast ma stały `min-width: 260px`, `max-width: 380px`
(na poziomie kontenera). Nie ma wariantów `sm`/`md`/`lg`.

## Stany

| Stan | Tło | Tekst/ikona | Obramowanie | Uwagi |
|---|---|---|---|---|
| default | `--tf-bg-card` | `--tf-text` (tytuł), `--tf-text-2` (wiadomość) | `--tf-border` + pasek 3px z tonu | `box-shadow: --tf-shadow-lg` |
| wejście | — | — | — | `tf-toast-in` 0.3s `--tf-spring-smooth`, translateX 100px → 0 |
| wyjście | — | — | — | klasa `.tf-toast-out`, `tf-toast-out` 0.25s ease-in, usuwa element po `animationend` |
| hover (przycisk zamknij) | — | `--tf-text` (z `--tf-text-3`) | — | `.tf-toast-close:hover` |
| persistent | — | — | — | atrybut boolean wyłącza `_startDismiss()` — brak auto-dismiss w ogóle |

Nie ma stanu „pauza na hover” jako osobnego stanu — patrz odstępstwa.

## Zachowanie

- Interakcja wskaźnikiem: przycisk `×` (`.tf-toast-close`) wywołuje
  `_dismiss()` natychmiast. Kliknięcie gdziekolwiek indziej na toaście nic
  nie robi. Slot `action` (protokół, `Toast.action_label`/`action_id`)
  renderuje `<tf-button variant="ghost">` emitujący `toast_action` z
  `detail.action_id` — to jedyny mechanizm zbliżony do „akcji” w toaście,
  nie ma jednak dedykowanego zachowania „undo z 10s”, patrz odstępstwa.
- Klawiatura: przycisk zamknięcia jest zwykłym `<button>`, więc dostaje
  natywny fokus/Enter/Space. Brak dedykowanej obsługi Esc na poziomie
  komponentu (Esc nie zamyka toastu).
- Animacje: `tf-toast-in`/`tf-toast-out`, `--tf-spring-smooth` na wejściu,
  `ease-in` na wyjściu (niespójność z `motion.easing` z `tokens.json`, gdzie
  wyjścia mają własny token `easing.exit`).
- Zdarzenia/API:
  - `TfToast.show({ tone, title, message, duration = 4000 })` (statyczna
    metoda) — tworzy element, dołącza do kontenera, zwraca referencję.
  - Atrybuty obserwowane: `tone`, `title`, `message`, `duration`,
    `persistent`.
  - `_startDismiss()`: `setTimeout(dismiss, duration || 4000)`, chyba że
    `persistent` jest ustawiony — wtedy timer nigdy nie startuje.
  - Renderer protokołu zawsze ustawia `persistent` (cykl życia toastu
    kontroluje stan addonu, nie wewnętrzny timer) — **czasy 4s/10s z tego
    dokumentu dotyczą wyłącznie ścieżki `TfToast.show()`**, nie toastów
    addonowych.
- Zaznaczanie tekstu: dozwolone (tytuł/treść to zwykły tekst).

## Dostępność

**`TfToast.show()` (ścieżka natywna aplikacji) nie ustawia żadnej roli ARIA
ani `aria-live`** — `_build()`/`_update()` w `tf-toast.js` operują wyłącznie
na `className`/`textContent`. Renderer protokołu addonów **ustawia
`role="status"` + `aria-live="polite"`** na wygenerowanym elemencie
(`feedback-inline-renderer.js:256-257`) — czyli tylko toasty pochodzące z
addonów są ogłaszane czytnikom ekranu; toasty z wywołań `toast(...)` w
modułach hosta (setki wystąpień w `js/modules/`) są dla czytnika ekranu
niewidoczne aż do momentu, gdy fokus na nie natrafi przypadkiem. To jest
najpoważniejsza luka a11y w tym komponencie. `prefers-reduced-motion` nie
jest respektowane — `tf-toast-in`/`-out` nie mają guardu w żadnym z dwóch
plików CSS.

## Responsywność i platformy

Kontener `.tf-toast-container` jest **zawsze** `position: fixed; bottom: 20px;
right: 20px` — nie ma reguły `@media` przełączającej go na dół ekranu na
mobile (bo już tam jest) ani żadnej zmiany na szerokość pełną. Stos rośnie w
górę (`flex-direction: column-reverse`) od najstarszego u dołu.
`pointer-events: none` na kontenerze + `auto` na każdym toaście pozwala
klikać przez puste miejsca w stosie.

## Tokeny użyte

- `color.themes.dark.bg.card` — tło
- `color.themes.dark.border.default` — obramowanie
- `color.themes.dark.semantic.success/warning/critical/info` — pasek/tony
- `color.themes.dark.text.primary` / `secondary` / `muted` — tytuł, treść, przycisk zamknij
- `elevation.elevated` (`--tf-shadow-lg`, przybliżenie — patrz odstępstwa) 
- `motion.easing.standard` (`--tf-spring-smooth`) — wejście
- `layout.z_index.toast` (1200) — pozycja w stosie warstw

## Znane odstępstwa w kodzie (2026-09-14)

- **Pozycja stała bottom-right, nie top-right.** `.tf-toast-container`
  (`controls.css:3529-3539`) jest zakotwiczony `bottom: 20px; right: 20px` na
  wszystkich szerokościach — nie top-right jak zakłada ten dokument, i bez
  osobnego zachowania mobile (bo dolna krawędź już tam jest). Do wyrównania:
  albo dokument, albo CSS.
- **Brak auto-dismiss 4s dla toastów addonowych.** Renderer protokołu zawsze
  ustawia `persistent` — timer 4000ms z `TfToast.show()` dotyczy tylko
  wywołań natywnych (`toast()` helper w modułach). Dwie ścieżki, dwa różne
  modele cyklu życia.
- **Brak dedykowanego trybu „undo 10s” dla akcji destrukcyjnych.** Pole
  `action_label`/`action_id` (schemat `0x0504`) daje ogólny przycisk akcji
  (`toast_action` event), ale nie ma w kodzie osobnego, dłuższego czasu
  życia (10s) ani specjalnej semantyki „undo” — to warstwa wyżej (addon)
  musiałaby sama wydłużyć czas i obsłużyć cofnięcie.
- **Brak `role`/`aria-live` w ścieżce `TfToast.show()`.** Patrz „Dostępność”.
- **Brak pauzy na hover.** Ani `mouseenter`/`mouseleave`, ani żaden odpowiednik
  w `tf-toast.js` — najechanie myszką na toast nie wstrzymuje odliczania do
  `_dismiss()`.
- **`ease-in` na wyjściu zamiast `motion.easing.exit`** (`cubic-bezier(0.7, 0,
  0.84, 0)`) — `tf-toast-out` w CSS używa dosłownego `ease-in`, nie tokenu.

## Przykłady

```html
<!-- API JS, nie znacznik pisany ręcznie -->
<script type="module">
  import '/js/components/tf-toast.js';
  TfToast.show({ tone: 'success', title: 'Zapisano', message: 'Zmiany zastosowane.' });
</script>
```

```rust
// natywnie (tenta-ui-widgets, API docelowe)
Toast::info("Zapisano").body("Zmiany zastosowane.")
Toast::critical("Usunięto rozmowę").undo(Msg::Undo).duration_ms(10_000)
```
