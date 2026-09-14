# Responsywność

TentaFlow renderuje jeden kod UI na przeglądarkę desktop, WebView mobilny i —
docelowo — natywne okno TentaEngine oraz stały panel ESP32-P4 (zasada 9 w
[`../README.md`](../README.md)). Strona nigdy nie pyta „czy jestem na
telefonie?” — pyta motyw o `Breakpoint`, gęstość i `pointer`. Ten dokument
opisuje breakpointy, strategię per klasa ekranu i realny (zweryfikowany w
`www/css`) stan wdrożenia — który dziś od tej zasady odbiega.

## `Breakpoint` — skala kanoniczna (mobile-first)

Z `tokens.json` → `layout.breakpoints_px`; `min-width`, mobile-first (reguła
działa od podanej szerokości w górę).

| Token | px | Co się zmienia od tej szerokości |
|---|---|---|
| `xs` | 640 | telefon poziomo / mały tablet — siatki 1→2 kolumny, `tf-table` zaczyna pokazywać ukryte kolumny niskiego priorytetu |
| `sm` | 768 | tablet — sidebar może zostać rozwinięty na żądanie, dwukolumnowe formularze |
| `md` | 1024 | próg `sidebar.collapse_below` — sidebar domyślnie rozwinięty zamiast hamburgera; strony admin zaczynają zakładać pełną szerokość |
| `lg` | 1280 | trzykolumnowe layouty dashboardów, wykresy pokazują pełne legendy |
| `xl` | 1536 | `content.max_width` (1440) zaczyna centrować treść zamiast rozciągać |
| `xxl` | 1920 | duże monitory — dodatkowy margines, gęstość `comfortable` nie jest już wymuszana |

Powiązane tokeny layoutu: `layout.sidebar` (`expanded: 240`, `collapsed: 64`,
`collapse_below: "md"`), `layout.topbar.height: 56`, `layout.content`
(`max_width: 1440`, `narrow: 800`), `layout.grid` (`columns: 12`,
`gutter: spacing.lg`).

## Strategia per klasa ekranu

| Klasa ekranu | Strategia | Uzasadnienie |
|---|---|---|
| Strony admin (`clusters`, `users`, `audit`, `settings`, …) | Preferowane `≥ md` (1024), ale muszą degradować się czytelnie poniżej — nie łamać (zasada 6 README) | Gęste tabele/wykresy operacyjne; admin zwykle na desktopie, ale telefon musi dać się użyć w awarii |
| Aplikacje użytkownika (`chat`, `notes`, `my-accounts`, …) | W pełni responsywne od `xs` | To jest podstawowy przypadek użycia na telefonie |
| `devtools` (profiling, flamegraph, code-studio, benchmark) | Tylko desktop — poniżej `md` komunikat zamiast layoutu | Dane (flamegraphy, timeline'y) nie mają sensownej reprezentacji mobilnej; lepiej jawny komunikat niż połamany UI |

## Zwijanie sidebaru

`layout.sidebar.collapse_below: "md"` — poniżej 1024 px sidebar chowa się za
hamburgerem i pokazuje jako nakładka (overlay) nad treścią, nie jako druga
kolumna. Powyżej `md` sidebar jest stałą kolumną (`expanded: 240px`, po
zwinięciu ręcznym przez użytkownika `collapsed: 64px` — tylko ikony).

## Strategia tabel

`tf-table` (`www/js/components/tf-table.js`) ma trzy, niezależne mechanizmy
degradacji — użyj tego, który pasuje do kolumny/tabeli, nie tylko jednego:

| Atrybut | Poziom | Efekt |
|---|---|---|
| `<tf-column hide-below="900">` | kolumna | Kolumna znika poniżej podanej szerokości viewportu (dowolna wartość px, nie tylko breakpointy z tokena — patrz niżej) |
| `<tf-column priority="low">` | kolumna, tylko `variant="flush"` | Kolumna znika na telefonach (`≤ 480px`) niezależnie od `hide-below` |
| `variant="flush"` + `narrow` (atrybut na `<tf-table>`) | cała tabela | Tabela w wąskiej karcie — kolumna `fill` traci własny szablon, wiersze renderują się jak karty |
| `data-label` na `<td>` | mobile `≤ 720px` | Fallback „karta": każda komórka dostaje etykietę kolumny obok wartości zamiast nagłówka tabeli |

`hide-below` jest per-kolumna i dowolne (kod widziano z wartościami
480/640/720/900/1024/1180/1280 w różnych tabelach) — to jest świadomy wybór
projektanta tabeli, nie błąd, ale dokumentuj każdą wartość w spec komponentu
strony, bo poza `tf-table` nic tych liczb nie centralizuje.

## Pointer vs. touch

Zasada: żadna afordancja nie może być dostępna **wyłącznie** przez hover.

| Media query | Użycie w kodzie (`www/css`) | Reguła |
|---|---|---|
| `(hover: none)` | 3 wystąpienia (`style.css`, `tentanas.css`, …) | Ukryj afordancje typu „pokaż przy hover", pokaż je na stałe zamiast |
| `(pointer: coarse)` | 2 wystąpienia (`controls.css:1705,2615`) | Powiększ cele dotykowe / odstępy |

To pokrycie jest bardzo płytkie (5 wystąpień łącznie w ~40 plikach CSS) —
większość komponentów nie rozróżnia dziś pointer/touch explicite i polega na
tym, że `:hover` na dotyku po prostu się nie odpala. Nowy kod powinien
świadomie sprawdzać `(pointer: coarse)`/`(hover: none)`, nie zakładać, że
brak `:hover` wystarczy.

Cel dotykowy: `control.touch_target_min: 44` px — każdy interaktywny element
(przycisk, wiersz listy, ikona klikalna) ma efektywny obszar trafień ≥ 44×44
px na `pointer: coarse`, nawet jeśli wizualnie jest mniejszy (dopełnienie
przez `padding`, nie przez powiększenie ikony). `control.height.lg: 44`
odpowiada temu minimum.

## Logiczne px vs. `scale_factor` (natywny renderer)

Wszystkie wartości w `tokens.json` (rozmiary czcionek, `spacing`, `radius`,
`icon_size`) są w **logicznych px** — jednostce niezależnej od gęstości
ekranu. Przełożenie na piksele urządzenia jest zadaniem platformy, nie
strony:

| Platforma | `platform.*` w `tokens.json` | `scale_factor` |
|---|---|---|
| `web_desktop` | `pointer: fine`, `density: default` | `devicePixelRatio` (przeglądarka) |
| `mobile_phone` | `pointer: coarse`, `density: comfortable`, `min_touch_target: 44` | `os` |
| `tablet` | `pointer: coarse`, `density: default` | `os` |
| `esp32p4_tab5` | panel `720×1280`, `RGB565`, `pointer: coarse`, `density: comfortable`, `shadows: flat` | **1.25** (stały, brak systemu DPI) |
| `esp32p4_jc8012` | panel `800×1280`, `RGB565` | 1.25 |
| `esp32p4_jc4880` | panel `480×800`, `RGB565`, `density: default` | 1.0 |

Renderer tekstu TentaEngine już zaokrągla `size * scale_factor` do pełnego
piksela urządzenia (komentarz `typography.scale._comment` w `tokens.json`) —
docelowo ta sama reguła obowiązuje dla `spacing`/`radius`/`icon_size` w
natywnym motywie. Panele ESP32-P4 mają `scale_factor` **stały i skończony**
(nie ma systemu operacyjnego raportującego DPI), więc te wartości są zapisane
wprost w tokenach, nie odczytywane z systemu w runtime.

## Zasada: kod aplikacji nigdy nie sprawdza platformy

Strona/komponent nie pyta `navigator.userAgent`, nie sprawdza szerokości
panelu ESP32-P4 na sztywno i nie rozgałęzia się po „czy jestem w WebView”.
Zamiast tego czyta z motywu:

- **Breakpoint** bieżącego viewportu (`Breakpoint` enum) — steruje layoutem.
- **Gęstość** (`control.density`: `compact` 0.9 / `default` 1.0 /
  `comfortable` 1.15) — mnożnik odstępów/wysokości kontrolek, ustawiany przez
  platformę (`platform.*.density`), nie przez stronę.
- **`pointer`** (`fine`/`coarse`) — steruje obecnością afordancji hover-only
  i minimalnym celem dotykowym.

Różnice sprzętowe (brak cieni na ESP32-P4 — `platform.esp32p4_tab5.shadows:
"flat"`, stały `scale_factor`, ograniczony zestaw fontów — `"Latin+Polish
subset, 4 sizes × 2 weights"`) są rozwiązywane przez `platform` w `tokens.json`
i implementację motywu natywnego, nigdy przez `if (platform === …)` w kodzie
strony czy komponentu.

## Znany dług: 25+ ad-hoc breakpointów

Zweryfikowane grepem po `css/*.css` (2026-09-14): rzeczywiste `@media` w
kodzie używają **ponad 25 odrębnych wartości** `max-width`, w większości
mobile-last (`max-width`, nie `min-width`) — sprzeczne z zasadą 6 README
(„responsywność mobile-first"). Najczęstsze: `720px` (~28-29×), `640px`
(~28×), `900px` (~21×), `1100px` (~16×), `767px` (~15×), plus kilkanaście
rzadszych (480/980/960/1180/1024/768/520/1023/1000/880/860/820/760/639/420px).
Prawdziwie mobile-first pary (`min-width: 768px`/`min-width: 1024px`) istnieją,
ale są rzadkością (kilka wystąpień w `style.css`/`controls.css`).

**Cel migracji:** nowy kod CSS pisze `@media (min-width: var(--bp-md))`-style
progi z tokena `layout.breakpoints_px`, nie własne `max-width`. Istniejące
strony z ad-hoc breakpointami nie są wymieniane hurtowo — konsolidacja
następuje przy okazji przepisywania danej strony (patrz
`patterns/new-page-checklist.md`, gdy powstanie), z wpisem w
`design/MIGRATION.md`.

## Checklist dla autora strony

- [ ] Layout testowany na `640/768/1024` (minimum) — nie tylko na desktopie.
- [ ] Żaden nowy `@media` nie wprowadza własnej liczby breakpointu — używa
      wartości z `layout.breakpoints_px`.
- [ ] Sidebar zachowuje się poprawnie poniżej `md` (hamburger + overlay, nie
      druga kolumna).
- [ ] Tabele mają jawną strategię degradacji (`hide-below`/`priority`/
      `variant="flush"`) — nie polegają na przewijaniu poziomym jako jedynej
      odpowiedzi.
- [ ] Żadna afordancja nie jest dostępna wyłącznie przez `:hover` —
      sprawdzone pod `(pointer: coarse)`.
- [ ] Cele dotykowe ≥ 44 px (`control.touch_target_min`) na ekranach
      dotykowych.
- [ ] Strona `devtools`-klasy pokazuje jawny komunikat poniżej `md`, zamiast
      renderować połamany layout.
- [ ] Żadna literalna gałąź `if (platform)`/`if (userAgent)` w kodzie strony —
      różnice idą przez motyw.
