# Modal

| | |
|---|---|
| Tier | 1 |
| HTML | `<tf-modal>` — `tentaflow-core/www/js/components/tf-modal.js`, style `controls.css:4808-4900` (backdrop/card/warianty) + `controls.css:8256-8268` (rozmiary). `<tf-window>` — `tf-window.js` (shadow DOM, draggable/resizable okno, `sheet` = mobilny bottom sheet). |
| Protokół addonów | `0x0509` `Modal` + `0x050A` `Drawer` — `docs/ADDON_UI_COMPONENT_CATALOG_v1.md` §2832-2864 |
| Natywny (TentaEngine) | `tenta_ui_widgets::Modal` / `tenta_ui_widgets::Window` — status: planowany |
| Status dokumentu | draft |

Nakładka blokująca resztę interfejsu do czasu decyzji lub zamknięcia. `tf-modal` obsługuje wariant wyśrodkowany (`modal`) i cztery warianty szuflady (`drawer-left/right/top/bottom`); `tf-window` to osobny, cięższy komponent — pełne okno z paskiem tytułu, przeciąganiem, zmianą rozmiaru i wieloma instancjami jednocześnie (np. panele narzędziowe w Code Studio). Nie używaj modala do treści wymagającej stałej widoczności obok reszty ekranu — od tego jest `tf-window` lub panel boczny stały w layoucie.

## Anatomia

`tf-modal` (wariant `modal`, wyśrodkowany):

```text
░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░  ← .tf-modal-backdrop, rgba(5,8,24,.6) + blur(8px)
░  ┌─────────────────────────────────┐  ░
░  │ [icon] Tytuł              [×]   │  ░  ← .tf-modal-header
░  │ Podtytuł (opcjonalny)           │  ░  ← .tf-modal-subtitle
░  ├─────────────────────────────────┤  ░
░  │                                 │  ░  ← .tf-modal-body (slot="body")
░  │                                 │  ░
░  ├─────────────────────────────────┤  ░
░  │              [Anuluj] [Zapisz]  │  ░  ← .tf-modal-footer (slot="footer")
░  └─────────────────────────────────┘  ░
░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░
  domyślna szerokość: max-width 520px · border-radius --tf-radius-lg · box-shadow --tf-shadow-lg
```

Warianty szuflady zamiast wyśrodkowanego okna wjeżdżają z krawędzi (`drawer-right`/`-left`: 380px szerokości; `-bottom`: do 80dvh wysokości) tym samym backdropem.

## Warianty

| Wariant | Wygląd | Kiedy używać |
|---|---|---|
| `modal` (domyślny) | wyśrodkowany, skalowany 0.92→1 przy otwarciu | potwierdzenie, formularz krótki, alert |
| `drawer-right` / `drawer-left` | panel pełnej wysokości wjeżdżający z boku, 380px | filtry, szczegóły rekordu, panel konfiguracji |
| `drawer-bottom` | panel wjeżdżający z dołu, do 80dvh | akcje kontekstowe na mobile, arkusz wyboru |
| `drawer-top` | panel wjeżdżający z góry | rzadkie — powiadomienia systemowe |
| `tf-window` | pełne okno z paskiem tytułu, przeciąganiem, resize | narzędzia równoległe (edytor + terminal), wiele instancji jednocześnie |
| `tf-window[sheet]` | na telefonie (≤640px) dokuje się jako bottom sheet zamiast pływać na środku | ta sama funkcja co `tf-window`, ale odpowiedzialna za telefon |

## Rozmiary

| `size` (`tf-modal`) | Szerokość | Kiedy używać |
|---|---|---|
| `xs` | `min(320px, 90vw)` | potwierdzenie tak/nie |
| `sm` | `min(400px, 90vw)` | krótki formularz |
| `md` | `min(560px, 90vw)` | formularz standardowy |
| `lg` | `min(720px, 90vw)` | formularz wielosekcyjny |
| `xl` | `min(960px, 90vw)` | podgląd/edytor złożony |
| `xxl` | `min(92vw, max(960px, 80vw))` | prawie pełny ekran, zachowuje margines |
| `fullscreen` | `100vw × 100vh`, `border-radius: 0` | edytor pełnoekranowy, tylko jawnie zażądany |
| *(brak `size`)* | `max-width: 520px` (domyślne z `.tf-modal-card--modal`) | — patrz odstępstwa: **nie 480px** |

## Stany

| Stan | Tło | Tekst/ikona | Obramowanie | Uwagi |
|---|---|---|---|---|
| closed | — | — | — | `opacity: 0; pointer-events: none`, poza DOM interakcji |
| open | `--tf-bg-card` na karcie, backdrop `rgba(5,8,24,.6)` + `blur(8px)` | `--tf-text` | 1px `--tf-border` | `transition: opacity .25s, transform .3s --tf-spring-snappy` |
| no-dismiss | jak open | jak open | jak open | Esc i klik na backdrop nie zamykają; przycisk X nadal działa (chyba że `no-close`) |
| no-close | jak open | jak open | jak open | przycisk X ukryty; zamknięcie tylko programowe lub Esc/backdrop |
| focus-visible (przycisk X) | — | — | outline standardowy | |

## Zachowanie

- Interakcja wskaźnikiem: klik poza kartą (na backdrop) zamyka modal, chyba że `no-dismiss`. Klik X zamyka zawsze, chyba że `no-close` (wtedy X nie istnieje w DOM).
- Klawiatura: `Escape` zamyka (nasłuch na `document`, dodawany tylko gdy modal jest `open`), chyba że `no-dismiss`.
- Zamknięcie zawsze emituje zdarzenie `close` (bąbelkujące) i usuwa atrybut `open` — host odpowiada za faktyczne usunięcie elementu z DOM, jeśli był tworzony programowo.
- API statyczne `TfModal.open({title, body, actions})` zwraca `Promise` — rozwiązuje się wartością klikniętego przycisku akcji (`action.value ?? action.label`) albo `null` gdy zamknięty bez wyboru (Esc/backdrop/X).
- `tf-window`: przeciąganie za pasek tytułu (`pointerdown` na headerze), zmiana rozmiaru za uchwyt w rogu, `z-index` przydzielany rosnąco z globalnego licznika (`_zCounter`, start 1000) przy każdym kliknięciu w okno — najnowsza interakcja zawsze na wierzchu. `close-request` (cancelable) pozwala hostowi przechwycić próbę zamknięcia (np. potwierdzenie utraty zmian) przed faktycznym `close`.
- Zdarzenia/API: `<tf-modal open|title|subtitle|variant|size|no-dismiss|no-close>`, zdarzenie `close`. `<tf-window title|subtitle|icon|buttons|draggable|resizable|transparent|min-width|min-height|initial-x|initial-y|width|height>`, zdarzenia `action` (cancelable), `close-request` (cancelable); metoda `win.close(force)`.

## Dostępność

- `tf-modal-card` ma `role="dialog"` i `aria-modal="true"` ustawione raz przy budowie (`tf-modal.js:72-73`) — **niezależnie od stanu otwarcia**, więc technologie wspomagające widzą `aria-modal="true"` nawet gdy modal jest zamknięty i ukryty przez `opacity`/`pointer-events` (nie przez `hidden`/`display:none`).
- Brak zarządzania fokusem: `tf-modal.js` **nie przenosi fokusu** do karty przy otwarciu, **nie ogranicza Tab** do zawartości modala (brak focus trap) i **nie przywraca fokusu** elementowi wyzwalającemu po zamknięciu. To rozmija się z twardą zasadą dostępności #5 z `design/README.md` („widoczny fokus… obsługa klawiatury”) — patrz odstępstwa.
- `tf-window` ma nasłuch `keydown` na `document` (Esc-close), ale też nie implementuje focus trap ani `role="dialog"`/`aria-modal` na swoim shadow roocie (grep repo-wide nie znajduje żadnego wystąpienia w pliku).
- Kontrast tekstu nagłówka/treści na `--tf-bg-card` dziedziczy te same wartości AA co `color.themes.dark.text.primary`.

## Responsywność i platformy

- `size="fullscreen"` to jedyny sposób uzyskania pełnoekranowego arkusza na telefonie — **nie ma automatycznego przełącznika breakpointu** z wariantu `modal` na pełny ekran poniżej np. 640px; każdy ekran musi jawnie zażądać `fullscreen` albo użyć wariantu `drawer-bottom`.
- `tf-window[sheet]` jest jedynym komponentem z wbudowanym automatycznym zachowaniem „dokuje się na dole telefonu” (czysty CSS, `:host([sheet])`, próg ≤640px) — `tf-modal` tej logiki nie ma.
- Na ESP32-P4 (`platform.esp32p4_tab5`, `shadows: flat`) `box-shadow: --tf-shadow-lg` spłaszcza się do obramowania 1px zgodnie z regułą platformy; `backdrop-filter: blur(8px)` prawdopodobnie nie jest wspierane przez renderer CPU/natywny — wymaga zamiennika (np. stałe przyciemnienie bez rozmycia).
- `z_index` modala w żywym CSS to stałe liczby `9990`/`9991` (`controls.css:4814/4826`), niezależne od `layout.z_index.modal` (1100) w tokenach — patrz odstępstwa.

## Tokeny użyte

- `color.themes.dark.bg.overlay` — oczekiwany kolor scrimu (`rgba(0,0,0,0.80)` + blur 8px wg tokena); żywy kod używa innej wartości, patrz odstępstwa.
- `color.themes.dark.bg.card`, `color.themes.dark.border.default` — tło/obramowanie karty.
- `radius.lg` (`--tf-radius-lg`) — promień karty modala/drawer (zgodne).
- `elevation.elevated` (rola „modals, drawers”) — realizowane przez `--tf-shadow-lg` (nie ma bezpośredniego mapowania nazwy zmiennej na klucz tokena, ale rola się zgadza).
- `layout.z_index.modal` (1100) — oczekiwana warstwa; żywy kod ma inną wartość, patrz odstępstwa.
- `motion.spring.snappy` (`--tf-spring-snappy`) — animacja wejścia karty.
- `motion.duration_ms.normal` (200) — zbliżone do `0.2-0.3s` użytych w przejściach.

## Znane odstępstwa w kodzie (2026-09-14)

1. **Domyślna szerokość to 520px, nie 480px.** `.tf-modal-card--modal` (`controls.css:4850`) ustawia `max-width: 520px` gdy nie podano `size`. Jeśli 480px ma być kanonicznym domyślnym rozmiarem systemu, wymaga to zmiany w CSS albo aktualizacji tego zapisu.
2. **Kolor i przezroczystość scrimu nie zgadzają się z tokenem.** `tokens.json → color.themes.dark.bg.overlay` = `rgba(0, 0, 0, 0.80)`; żywy `.tf-modal-backdrop` (`controls.css:4811`) to `rgba(5, 8, 24, 0.6)` — inny kolor bazowy (odcień `--bg` zamiast czystej czerni) i mniejsza nieprzezroczystość (60% zamiast 80%).
3. **`z-index` jest zahardkodowany, nie tokenizowany.** `controls.css:4814` (`9990`) i `:4826` (`9991`) ignorują `layout.z_index.modal` (1100) z `tokens.json`. `tf-window` ma osobny, rosnący licznik od 1000 (`tf-window.js:38`) — trzeci, niezależny schemat warstwowania w tym samym systemie.
4. **Brak focus trap w obu implementacjach.** Ani `tf-modal.js`, ani `tf-window.js` nie przechwytują `Tab`/`Shift+Tab` wewnątrz okna, nie ustawiają fokusu początkowego przy otwarciu i nie przywracają go elementowi wyzwalającemu po zamknięciu — mimo że `role="dialog"`/`aria-modal="true"` sugerują odbiorcy technologii wspomagającej, że taka izolacja istnieje.
5. **Mobile full-screen sheet jest opt-in, nie automatyczny.** W przeciwieństwie do `tf-window[sheet]` (czysty CSS, próg 640px), `tf-modal` wymaga jawnego `size="fullscreen"` lub `variant="drawer-bottom"` — nie ma jednej spójnej reguły „modal na telefonie zawsze pełnoekranowy”.

## Przykłady

```html
<tf-modal open title="Usuń zasób" subtitle="Tej operacji nie można cofnąć." size="sm">
  <div slot="body">Czy na pewno chcesz usunąć „prod-cluster-01”?</div>
  <div slot="footer">
    <tf-button variant="secondary">Anuluj</tf-button>
    <tf-button variant="destructive">Usuń</tf-button>
  </div>
</tf-modal>
```

```rust
// natywnie (tenta-ui-widgets, API docelowe)
Modal::new("Usuń zasób")
    .subtitle("Tej operacji nie można cofnąć.")
    .size(ModalSize::Sm)
    .body(vec![text("Czy na pewno chcesz usunąć „prod-cluster-01”?")])
    .footer(vec![
        Button::new("Anuluj").variant(ButtonVariant::Secondary),
        Button::new("Usuń").variant(ButtonVariant::Destructive),
    ])
    .on_close(Msg::CancelDelete);
```
