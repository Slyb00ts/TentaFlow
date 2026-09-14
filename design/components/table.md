# Table

| | |
|---|---|
| Tier | 0 (MVP) |
| HTML | `<tf-table>` + `<tf-column>` — `tentaflow-core/www/js/components/tf-table.js` (shadow DOM), style `controls.css:1022-1330` (bazowa tabela, warianty, mobile) + `controls.css:7153` (komórka pusta/loading). |
| Protokół addonów | `0x0211` `Table` — `docs/ADDON_UI_COMPONENT_CATALOG_v1.md` §1646-1685 |
| Natywny (TentaEngine) | `tenta_ui_widgets::Table` — status: planowany |
| Status dokumentu | draft |

Tabela danych z sortowaniem po kolumnie, zaznaczaniem wierszy, paginacją serwerową i responsywnym zwijaniem kolumn/kart na wąskich ekranach. To najbardziej złożony i najczęściej reimplementowany komponent systemu (patrz odstępstwa) — zanim strona doda własne style tabeli, powinna sprawdzić czy `tf-table` z odpowiednim `variant`/`density` nie wystarcza.

## Anatomia

```text
┌──────────────────────────────────────────────────────────────┐
│ NAZWA ↕          STATUS         UDZIAŁ            [⋮]          │ ← thead, sticky (position: sticky; top: 0)
├──────────────────────────────────────────────────────────────┤
│ ▸ prod-cluster-01   [● ok]      ▓▓▓▓░░░░ 62%      [⋮]          │ ← tbody tr, hover = --tf-bg-card-hover
│   staging-02        [● warn]    ▓▓░░░░░░ 28%      [⋮]          │
│ ▾ dev-03            [● err]     ▓░░░░░░░ 9%       [⋮]          │ ← wiersz rozwinięty (expandable)
│   └ panel rozwinięcia (colspan pełna szerokość)                │
└──────────────────────────────────────────────────────────────┘
              1–20 / 134  ‹ ›                                     ← pager (tylko gdy total > page-size)
```

Komórka karty na mobile (`<=720px`, wariant domyślny, nie `flush`):

```text
┌──────────────────────────────┐
│ NAZWA                        │  ← td[data-label] renderuje etykietę nad wartością
│ prod-cluster-01               │
│ STATUS                       │
│ ● ok                          │
└──────────────────────────────┘
```

## Warianty

| `variant` | Wygląd | Kiedy używać |
|---|---|---|
| *(brak — domyślny)* | tabela w karcie z ramką, `thead` szary, `tbody` przewijalne, na mobile zwija się do kart | domyślna tabela danych |
| `flush` | bez ramki wrapa (kartę rysuje host), wiersze klikalne, na mobile **nie** zwija się do kart — przewija poziomo wewnątrz karty | tabela osadzona w `tf-section-card`, gdzie karta już ma ramkę |
| `borderless` | brak linii między wierszami | gęste listy bez wizualnego podziału |
| `striped` | naprzemienne tło parzystych wierszy (`nth-child(even)`) | **obecne w CSS, ale sprzeczne z zasadą „no stripes” tego systemu** — patrz odstępstwa, nie używać w nowym kodzie |

`narrow` (atrybut boolowski, tylko z `variant="flush"`): kolumna `fill` traci minimalną szerokość, paski udziału się kurczą, a na telefonie zostaje układ procentowy zamiast poziomego scrolla — tabela mieści się w wąskiej karcie zamiast wystawać.

## Rozmiary (density)

| `density` | Padding komórki | Kiedy używać |
|---|---|---|
| `compact` | `6px 10px` | gęste widoki operacyjne, dużo wierszy na ekranie |
| *(brak — domyślny)* | `12px 14px` | tabela standardowa |
| `comfortable` | `16px 18px` | dane z długimi wartościami, mniej wierszy na ekranie |

## Stany

| Stan | Tło | Tekst/ikona | Obramowanie | Uwagi |
|---|---|---|---|---|
| default (wiersz) | — | `--tf-text` | `border-top: 1px --tf-border` między wierszami | |
| hover (wiersz) | `--tf-bg-card-hover` | bez zmiany | bez zmiany | tylko `pointer: fine`; na `flush` cały wiersz jest `cursor: pointer` |
| selected (wiersz) | `--tf-accent-glow` | `--tf-text` (wymuszony pełny kontrast) | bez zmiany | zaznaczenie przez `tf-checkbox` w pierwszej kolumnie danych |
| nagłówek sortowalny, hover | `--tf-bg-3` | `--tf-text` | — | strzałka `↕` (nieaktywna, 35% opacity) → `↑`/`↓` (100%, `--tf-accent-2`) po sortowaniu |
| expand toggle | — | `▸` (zwinięty) / `▾` (rozwinięty) | — | `aria-expanded` na przycisku |
| empty (brak wierszy) | — | `.tf-table__empty-state`, wyśrodkowany | — | tabela sama **nie** renderuje treści pustego stanu — patrz Zachowanie |
| loading (komórka) | — | `.tf-empty-cell--loading` | — | styl istnieje w CSS (`controls.css:7153`) dla pojedynczej komórki, nie dla całej tabeli |

Tabela **nie ma pasków zebry w domyślnym wariancie** — `striped` to opt-in odstępstwo od reguły projektowej (patrz odstępstwa).

## Zachowanie

- **Sortowanie**: klik nagłówka z `sortable` przełącza `asc → desc → asc…` (nie ma trzeciego stanu „brak sortowania” po pierwszym kliknięciu), emituje zdarzenie `sort` (`detail: {key, dir}`) i sortuje `.rows` lokalnie po stronie klienta (`localeCompare` dla stringów, odejmowanie dla liczb). Przy sortowaniu serwerowym host ignoruje lokalny wynik i podmienia `.rows` po otrzymaniu odpowiedzi.
- **Zaznaczanie**: `selectable` (bez wartości lub `"multi"`) pokazuje checkbox „zaznacz wszystkie” w nagłówku pierwszej kolumny danych i checkbox per wiersz w odpowiadającej komórce (bez dodatkowej kolumny). Klik checkboxa emituje `row-select` (`detail: {row, index, selected}`), checkbox nagłówka emituje `select-all` (`detail: {selected}`) — **tabela sama nie zaznacza wszystkich wierszy**, tylko informuje hosta, który musi ustawić `_selected` na każdym obiekcie wiersza.
- **Rozwijanie wierszy**: właściwość `.expandable = true` + `.expandRenderer = (row, idx) => Node`. Klik przycisku toggle emituje `row-expand` (`detail: {row, index, expanded}`) i wstawia wiersz z `colspan` pełnej szerokości. Stan rozwinięcia jest kluczowany przez `.rowKey` (stabilne pole identyfikatora) gdy ustawione — inaczej po indeksie widocznym, co gubi się przy zmianie sortowania/strony.
- **Paginacja serwerowa**: atrybuty `page-size` / `total` / `page` (1-based). Pager renderuje się automatycznie tylko gdy `total > page-size`. Klik prev/next emituje `page-change` (`detail: {page, pageSize}`) — **tabela nigdy sama nie tnie `.rows`**; host musi załadować nową stronę i podmienić zarówno `.rows`, jak i atrybut `page`.
- **Responsywność kolumn**: `<tf-column hide-below="N">` ukrywa kolumnę CSS-em poniżej podanej szerokości viewportu. Dozwolone wartości: `480 640 720 900 1024 1180 1280` — dowolna inna liczba jest ignorowana (kolumna zostaje zawsze widoczna), bo reguła musi istnieć jako gotowa klasa w `controls.css` (media query nie może odczytać custom property). Komórki **zostają w DOM** nawet ukryte — zaznaczenie/ekspansja/sortowanie przeżywają zmianę szerokości okna.
- **Kolumna `fill`**: dokładnie jedna kolumna może nosić `fill` — przejmuje wolną szerokość i się elipsuje (`text-overflow: ellipsis`) w wariancie `flush`.
- **Kolumna `priority="low"`**: ukrywana na telefonie (`<=480px`) w wariancie `flush`, niezależnie od `hide-below`.
- **Kolumny sticky**: `.stickyColumns = N` (pierwsze N kolumn) lub `<tf-column sticky>` per kolumna — offset `left` liczony na sztywnej stałej `160px` na kolumnę (`STICKY_COLUMN_WIDTH`), nie na realnie zmierzonej szerokości.
- **Renderery komórek** (`<tf-column renderer="…">`): `text` (domyślny), `num` (monospace, wyrównanie do prawej), `chip` (`{status, label, dot?}` → `.tf-chip`), `html` (surowe HTML — zaufanie do źródła danych wymagane), `img` (miniatura `<img loading="lazy">`, pusty URL → em dash).
- **Akcje wiersza**: `.rowActions = (row, idx) => Element` renderuje dowolny element (np. menu kebab) w dodatkowej kolumnie końcowej; klik w tę kolumnę nie wyzwala `row-click`.
- Zdarzenia/API: `row-click`, `row-dblclick`, `row-select`, `select-all`, `row-expand`, `sort`, `page-change` — wszystkie bąbelkujące.

## Dostępność

- Tabela renderuje semantyczny `<table>`/`<thead>`/`<tbody>` (nie `role="grid"`) — nawigacja czytnikiem ekranu po tabeli działa natywnie po strukturze HTML.
- Nagłówki sortowalne **nie mają `aria-sort`** ani `role="button"`/`tabindex` — to zwykłe `<th class="sortable">` reagujące wyłącznie na `click` myszy. Sortowanie nie jest osiągalne z klawiatury poza natywnym Tab, który i tak nie zatrzymuje się na `<th>` bez `tabindex`.
- **Tabela nie ma żadnej obsługi klawiatury poza natywną tabulacją** przez interaktywne elementy w komórkach (checkboxy, przyciski akcji, toggle rozwinięcia). Nie ma strzałek do nawigacji po komórkach/wierszach, nie ma `Enter` do otwarcia wiersza z klawiatury bez przejścia przez wszystkie checkboxy najpierw.
- Przyciski paginacji mają `aria-label` („Poprzednia/Następna strona” — teksty w polskim, niezależnie od `I18n` reszty aplikacji, patrz odstępstwa) i `disabled` na granicach zakresu.
- Kolumna zaznaczania ma `aria-label="Zaznacz wiersz"`/`"Zaznacz wszystkie"` na checkboxach.
- `prefers-reduced-motion`: jedyna animacja to `transition: background 0.12s` na hover wiersza — pokryta globalną regułą wildcard.

## Responsywność i platformy

- `<=720px` (wariant domyślny): `thead` znika (`display: none`), każdy wiersz staje się blokiem kart, każda komórka pokazuje etykietę z `data-label` nad wartością.
- `<=720px` (wariant `flush`): tabela **zostaje tabelą** i przewija się poziomo wewnątrz karty (scrollbar cienki, 4px) zamiast zwijać się do kart — świadomie inne zachowanie dla osadzonych tabel.
- Kolumny `hide-below` reagują na 7 dyskretnych progów (patrz Zachowanie) — to bogatszy, ale i bardziej rozdrobniony zestaw niż breakpointy `layout.breakpoints_px` z `tokens.json` (640/768/1024/1280/1536/1920); tylko 640/1024/1280 pokrywają się dokładnie.
- Na ESP32-P4 (`pointer: coarse`, brak hover) stan hover wiersza nie ma odpowiednika — jedynym sygnałem interakcji zostaje `selected`/checkbox; brak klawiatury sprzętowej czyni brak nawigacji klawiszowej komórek nieistotnym na tej platformie, ale istotnym na desktopie.

## Tokeny użyte

- `color.themes.dark.bg.elevated` (`--tf-bg-2`) — tło `thead`.
- `color.themes.dark.bg.card_hover` (`--tf-bg-card-hover`) — hover wiersza.
- `color.themes.dark.accent.soft` (`--tf-accent-glow`) — wiersz zaznaczony.
- `color.themes.dark.border.default` — separator wierszy/kolumn.
- `radius.md` (`--tf-radius`) — promień `.tf-table-wrap`.
- `spacing.md`/`spacing.lg` — zbliżone do paddingu domyślnej gęstości (12/14, nie dokładne wartości ze skali).
- `layout.breakpoints_px` — częściowo pokrywa się z `hide-below` (patrz odstępstwa).
- `typography.scale.overline` (10px, uppercase, tracking) — nagłówek `thead th` (rozmiar zgodny, ale bez odwołania do tokena w CSS).

## Znane odstępstwa w kodzie (2026-09-14)

1. **Brak jakiejkolwiek nawigacji klawiaturowej po tabeli.** Nie ma `keydown` handlera w `tf-table.js` w ogóle. Nagłówki sortowalne są zwykłymi `<th>` bez `tabindex`/`role="button"`/`aria-sort` — sortowanie jest osiągalne wyłącznie myszą/dotykiem. To rozmija się z regułą #5 „obsługa klawiatury… w każdym komponencie” z `design/README.md`.
2. **Wariant `striped` istnieje w CSS i jest w pełni funkcjonalny** (`controls.css:1180`, `tf-table--variant-striped tbody tr:nth-child(even)`), mimo że projekt jawnie deklaruje zasadę „no stripes” dla tabel w tym systemie. Jest to martwy kod czekający na usunięcie albo świadomy wyjątek nieudokumentowany nigdzie indziej — do rozstrzygnięcia w `design/MIGRATION.md`.
3. **Kolumny liczbowe (`td.num`) używają `'SF Mono', Menlo, monospace`** (`controls.css:1109`), nie `JetBrains Mono` — ten sam retired stack, który `tokens.json → typography.family.mono.decision` już jawnie uznaje za wycofany na rzecz JetBrains Mono.
4. **Sticky columns liczą offset na sztywnej stałej 160px/kolumnę** (`tf-table.js:59`, `STICKY_COLUMN_WIDTH`), nie na realnie zmierzonej szerokości — kolumna szersza niż 160px nakłada się wizualnie na kolejną przypiętą kolumnę.
5. **Tabela nie ma wbudowanego stanu pustego/ładowania na poziomie całej tabeli** — istnieje tylko styl dla pojedynczej komórki (`.tf-empty-cell--loading`) i klasa `.tf-table__empty-state` bez logiki renderującej ją automatycznie z `.rows = []`; host musi sam podmienić zawartość, co w praktyce prowadzi do niespójnych pustych stanów między stronami (zgodnie z ogólną obserwacją audytu, że tabele są reimplementowane w 25 z ~40 plików CSS).
6. **Protokołowe pole `virtualize` (0x0211, pole 15) nie ma odpowiednika w `tf-table.js`** — HTML-owa implementacja renderuje/recykluje wszystkie `<tr>` w DOM (z recyklingiem po indeksie, nie prawdziwą wirtualizacją okna przewijania); duże zbiory danych polegają wyłącznie na paginacji serwerowej, nie na wirtualnym scrollu.

## Przykłady

```html
<tf-table sortable selectable="multi" page-size="20" total="134" page="1" actions-label="Akcje">
  <tf-column key="name" label="Nazwa" sortable fill></tf-column>
  <tf-column key="status" label="Status" renderer="chip" align="num" hide-below="640"></tf-column>
  <tf-column key="usage" label="Udział" renderer="num"></tf-column>
</tf-table>
<script>
  const t = document.querySelector('tf-table');
  t.rows = [{ name: 'prod-cluster-01', status: { status: 'ok', label: 'OK' }, usage: '62%' }];
  t.addEventListener('sort', (e) => loadPage({ sort: e.detail }));
  t.addEventListener('page-change', (e) => loadPage({ page: e.detail.page }));
</script>
```

```rust
// natywnie (tenta-ui-widgets, API docelowe)
Table::new(vec![
    Column::new("name", "Nazwa").sortable().fill(),
    Column::new("status", "Status").render(ColumnRender::Chip).hide_below(Breakpoint::Sm),
    Column::new("usage", "Udział").render(ColumnRender::Number),
])
.rows(rows)
.selectable(TableSelectMode::Multi)
.pagination(TablePagination { page_size: 20, current_page: page })
.sticky_header(true)
.on_sort(Msg::Sort)
.on_page_change(Msg::PageChange)
.on_selection_change(Msg::SelectionChange);
```
