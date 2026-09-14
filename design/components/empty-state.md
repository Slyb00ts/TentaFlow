# Empty state

| | |
|---|---|
| Tier | 0 (MVP) |
| HTML | `<tf-empty-state>` — `www/js/components/tf-empty-state.js`, style `controls.css` linie 3295-3327, 7166-7167 |
| Protokół addonów | `0x0003` `EmptyState` — `docs/ADDON_UI_COMPONENT_CATALOG_v1.md` §2 |
| Natywny (TentaEngine) | `tenta_ui_widgets::EmptyState` — status: planowany |
| Status dokumentu | draft |

Zastępuje pustą listę/tabelę/panel: ikona, tytuł, opcjonalny opis, opcjonalna
akcja. Reguła twarda z `design/README.md` pkt 8 — każda lista ma stan pusty.
Nie używaj go do błędów sieciowych trwałych (patrz `patterns/states.md`,
jeszcze nienapisany) — kod dzisiaj obsługuje błędy przez `toast(...)`, nie
przez wariant tego komponentu (patrz „Znane odstępstwa”).

## Anatomia

```text
┌────────────────────────────┐
│           (icon)           │  ← .tf-empty-state-icon, 48×48 svg, stroke 1.5
│         Tytuł stanu         │  ← .tf-empty-state-title
│   Opis / kontekst pomocy    │  ← .tf-empty-state-message
│      [Akcja podstawowa]     │  ← .tf-empty-state-actions (sweep slotted children)
└────────────────────────────┘
```

`_build()` w `tf-empty-state.js` zbiera dzieci światła DOM na dwa sposoby:
dziecko z `slot="icon"` (np. gotowy `<svg>`/maskotka) zastępuje ikonę ze
sprite'u; wszystkie pozostałe dzieci (typowo `<tf-button>`) trafiają do
`.tf-empty-state-actions`, która chowa się (`display: none`) gdy jest pusta.

## Warianty

Rzeczywisty atrybut `variant` **nie istnieje** w `tf-empty-state.js`
(`observedAttributes` to tylko `icon`, `title`, `message`) — wszystkie
wystąpienia w `www/js/modules/` (np. `tentanas/pools.js:129`) przekazują
tylko `icon`/`title`/`message` + dzieci-akcje. Jedyne warianty w kodzie
pochodzą z renderera protokołu addonów (`EmptyStateVariant`, dodaje klasę
`tf-empty-state--variant-<x>` na wygenerowanym elemencie):

| Wariant | Wygląd | Kiedy używać |
|---|---|---|
| `default` | pełny padding, bez modyfikatora klasy | pusta lista/tabela na całej szerokości panelu |
| `compact` | `padding: spacing.md; gap: spacing.xs` (`controls.css:7166`) | pusty stan w wąskiej karcie/panelu bocznym |
| `illustrated` | `padding: spacing.xl` (`controls.css:7167`) | hero-style pusty stan (np. pierwszy start modułu) |

Te trzy warianty różnią się **tylko gęstością paddingu** — żaden z nich nie
zmienia treści ani ikony w zależności od przyczyny (brak danych / brak
wyników wyszukiwania / błąd / offline).

## Rozmiary

Brak tokenu rozmiaru — ikona jest zawsze 48×48px (stroke 1.5, sprite
`icons.svg`), niezależnie od wariantu; warianty zmieniają tylko `padding`
kontenera (patrz wyżej). Przycisk akcji dziedziczy rozmiar domyślny
`tf-button` (`control.height.md`, 36px) — sam `tf-empty-state` go nie zmienia.

## Stany

| Stan | Tło | Tekst/ikona | Obramowanie | Uwagi |
|---|---|---|---|---|
| default | transparentne | `--tf-text-3` (ikona), `--tf-text` (tytuł) | brak | jedyny stan realnie renderowany |
| hover | — | — | — | nie dotyczy, kontener nie jest interaktywny |
| focus-visible | — | — | — | nie dotyczy komponentu — fokus idzie na przycisk akcji wewnątrz |
| disabled / loading / error / invalid / selected | — | — | — | nie istnieją; komponent nie modeluje przyczyny pustki, tylko wynik |

## Zachowanie

- Interakcja wskaźnikiem: cały ciężar interakcji leży na dziecku-akcji
  (zwykle `<tf-button>`) — `tf-empty-state` sam nic nie obsługuje poza
  pokazywaniem/ukrywaniem sekcji akcji.
- Klawiatura: pośrednio przez przycisk akcji (Tab/Enter/Space jak w
  `Button`).
- Animacje: brak — `attributeChangedCallback` po prostu podmienia
  `textContent`/`display` bez przejść.
- Zdarzenia/API: atrybuty `icon` (nazwa symbolu w `icons.svg`, bez prefiksu
  `i-`), `title`, `message`. Brak customowych eventów — akcja emituje własny
  event/handler (np. klik `<tf-button>`), `tf-empty-state` go nie
  przechwytuje ani nie re-emituje.
  Protokół: pola `icon` (`IconRef`), `heading` (`BindRef<tstr>`, wymagane —
  mapuje się na atrybut `title` w renderowanym elemencie, nie na `heading`),
  `message` (opcjonalne), `primary_action`/`secondary_action`
  (`ComponentRef<Button>`), `variant`. Renderer ustawia `role="status"` na
  wygenerowanym elemencie — ale tylko w tej ścieżce (patrz Dostępność).
- Zaznaczanie tekstu: dozwolone (tytuł/opis to zwykły tekst).

## Dostępność

**W czystym HTML (`tf-empty-state.js` bezpośrednio) element nie ma żadnej
roli ARIA** — renderuje się jako goły `<div>`. Renderer protokołu addonów
dodaje `role="status"` na wygenerowanym `<tf-empty-state>`, co ogłasza jego
pojawienie się czytnikom ekranu (bez `aria-live` jawnego — `role="status"`
niesie domyślnie `aria-live="polite"`). Ikona nie ma jawnego `aria-hidden`
w `tf-empty-state.js` (svg ma `aria-hidden="true"` na elemencie `<svg>`
wygenerowanym w `_update()`, poprawnie). Przycisk akcji dziedziczy a11y z
`tf-button`. Reguła twarda tego dokumentu: **treść nigdy nie może być
gołym „Brak elementów”** — zawsze tytuł + kontekst (np. `pools.empty_msg`:
„Masz N wolnych dysków — utwórz z nich pierwszy pool” zamiast samego „Brak
poola”), zgodnie z realnymi kluczami i18n w `tentanas/pools.js`.

## Responsywność i platformy

Brak reguł `@media` dedykowanych `tf-empty-state` w `controls.css` — layout
jest z natury elastyczny (flex column, wyśrodkowany tekst). Warianty
`compact`/`illustrated` (gęstość paddingu) pełnią funkcję, którą inne
komponenty realizują przez breakpointy — tu decyzję podejmuje strona/addon
w momencie renderu, nie CSS w zależności od szerokości viewportu. Na
ESP32-P4 (`platform.esp32p4_tab5`) czcionki ograniczone do 4 rozmiarów ×
2 wagi — tytuł/opis muszą mieścić się w tym zestawie.

## Tokeny użyte

- `spacing.md` (`compact` variant padding), `spacing.xs` (`compact` gap)
- `spacing.xl` (`illustrated` variant padding)
- `color.themes.dark.text.muted` (`--tf-text-3`) — ikona i tytuł domyślny
- `color.themes.dark.text.primary` (`--tf-text`) — tytuł
- `control.icon_size` — nie używany wprost; ikona ma stały rozmiar 48px, spoza skali `IconSize` (`xl` = 32px, patrz odstępstwa)

## Znane odstępstwa w kodzie (2026-09-14)

- **Brak wariantów treściowych `empty`/`no-results`/`error`/`offline`.**
  Jedyne warianty w kodzie (`EmptyStateVariant`: `default`/`compact`/
  `illustrated`) różnicują **gęstość**, nie **przyczynę** pustego stanu.
  Rzeczywiste błędy i stan offline w aplikacji są dziś sygnalizowane przez
  `toast(message, 'error')` (np. `meeting-live.js:211,245,1124,1143`), nie
  przez `tf-empty-state`. Ten dokument proponuje cztery warianty treściowe
  jako kierunek rozwoju (ikona/copy/CTA różne dla „brak danych” vs „brak
  wyników filtra” vs „błąd ładowania” vs „offline”), ale **żaden moduł dziś
  tego nie robi** — traktuj to jako lukę do zamknięcia, nie opis obecnego
  zachowania.
- **Rozmiar ikony 48px poza skalą `IconSize`** (`tokens.json` →
  `control.icon_size`: xs/sm/md/lg/xl = 12/16/20/24/32px). 48px nie jest
  żadnym z tych tokenów — literał w `tf-empty-state.js:78` i sprite
  `<svg width="48" height="48">`.
- **`role="status"` tylko w ścieżce protokołu.** Bezpośrednie użycie
  `<tf-empty-state>` w modułach HTML (`pools.js`, `code-studio.js`,
  `tentaquant/*`) nie dostaje żadnej roli ARIA — do ujednolicenia.

## Przykłady

```html
<tf-empty-state icon="layers" title="Brak poola dyskowego"
  message="Masz 3 wolne dyski — utwórz z nich pierwszy pool.">
  <tf-button variant="primary" icon="plus">Utwórz pool</tf-button>
</tf-empty-state>
```

```rust
// natywnie (tenta-ui-widgets, API docelowe)
EmptyState::new(Icon::Layers, "Brak poola dyskowego")
    .message("Masz 3 wolne dyski — utwórz z nich pierwszy pool.")
    .primary_action(Button::new("Utwórz pool").on_press(Msg::CreatePool))
    .variant(EmptyStateVariant::Default)
```
