# Tabele danych

Kiedy tabela, kiedy lista, kiedy karty; jak projektować kolumny; kontrakt
`<tf-table>` (`www/js/components/tf-table.js`) — sortowanie, paginacja
serwerowa, selekcja, akcje wierszy, responsywność, dostępność. Wszystko
poniżej jest odczytane wprost z implementacji komponentu i z jego realnych
użyć (`users.js`, `project-studio.js`, `tentanas/shares.js`).

## Tabela vs lista vs karty

| Sytuacja | Wzorzec | Przykład |
|---|---|---|
| Wiele jednorodnych rekordów, ta sama struktura pól, porównywanie w kolumnach | **Tabela** (`<tf-table>` lub `table.data-table`) | `users.js` — lista userów/grup |
| Kilka bogatych obiektów z wizualizacją (gauge, diagram), gdzie każdy element to mini-panel | **Karty** (grid `.cluster-card`) | `clusters.js` — kafle klastrów z ring-gauges |
| Elementy sekwencyjne/czasowe bez potrzeby sortowania kolumnowego, każdy z własnym podglądem | **Lista** (`tf-list`, lub custom `.conv-item`) | `chat.js` — lista konwersacji w sidebarze |

Reguła praktyczna: jeśli potrzebujesz sortować/filtrować po więcej niż jednej
kolumnie danych naraz → tabela. Jeśli każdy rekord potrzebuje własnej
wizualizacji (pierścień, wykres) większej niż jedna komórka → karty.

## Projektowanie kolumn

- **Liczby wyrównane do prawej**: `<tf-column renderer="num">` lub
  `align="num"` dodaje klasę `.num` do `<th>`/`<td>` (`tf-table.js`,
  `_renderThead`/`_buildRow`).
- **Tekst wyrównany do lewej** (domyślne).
- **Jednostki w nagłówku**, nie w każdej komórce (np. „RAM (GB)”, nie „12
  GB” w każdym wierszu) — konwencja stosowana w kolumnach agregatów
  (`cluster_wizard.js` nagłówki `CPU`/`RAM`/`GPU`/`VRAM` z jednostką
  domyślną w danych, nie powtórzoną per komórka).
- **Jedna kolumna `fill`**: `<tf-column fill>` oznacza kolumnę, która
  wchłania wolną szerokość i się elipsuje (`.tf-table--variant-flush`) —
  używaj dokładnie jednej na tabelę, zwykle nazwa/tytuł rekordu.
- **`width`** przypina konkretną szerokość kolumny (dowolna jednostka CSS)
  tak, by kilka tabel typu flush dzieliło jeden szablon kolumn.
- **`nowrap`** dla kolumn, które nie mogą się zawijać (identyfikatory, daty).
- **`sticky`** przypina kolumnę przy poziomym scrollu (offset liczony z
  `STICKY_COLUMN_WIDTH = 160px`, `tf-table.js`).

## Gęstość (density)

`<tf-table density="compact|default|comfortable">` mapuje na
`control.density` z `tokens.json` (`compact: 0.9`, `default: 1.0`,
`comfortable: 1.15` — mnożnik wysokości wiersza/paddingu). Atrybut trafia do
klasy `tf-table--density-*` na realnym `<table>` w shadow DOM
(`_syncTableModifiers`).

## Sortowanie, filtrowanie, wyszukiwanie — pasek narzędzi

Toolbar nad tabelą to zwykle: `<tf-searchbox>` (debounced, `debounce="120"`)
+ grupa `<tf-chip clickable>` jako filtry-przełączniki (nie `<select>`) —
wzorzec z `users.js`:

```html
<div class="users-toolbar">
  <tf-searchbox id="users-search" debounce="120" placeholder="…"></tf-searchbox>
  <div class="tf-filter-group">
    <tf-chip clickable active data-filter="all">Wszyscy</tf-chip>
    <tf-chip clickable data-filter="active">Aktywni</tf-chip>
    <tf-chip clickable data-filter="admin">Admini</tf-chip>
  </div>
</div>
```

Filtrowanie po stronie klienta (małe zbiory, `users.js`:
`filteredUsers()` filtruje `users` w pamięci) vs serwerowe (duże zbiory,
`project-studio.js`: zmiana filtra woła `reload()` → `s.cases.page = 1` →
nowe zapytanie). Sortowanie kolumnowe: klik `<th class="sortable">` emituje
`sort` (`{key, dir}`) i **jednocześnie** sortuje `.rows` lokalnie
(`_sortedRows()`); jeśli dane są stronicowane serwerowo, host musi
zignorować lokalne sortowanie i wysłać własne zapytanie z parametrem sortu
(`tf-table` samo nie wie, że jest stronicowane po stronie serwera — to
odpowiedzialność hosta).

## Kontrakt paginacji serwerowej

Atrybuty: `page-size` (>0 włącza pager), `total` (łączna liczba rekordów),
`page` (1-based, bieżąca strona). `<tf-table>` **nigdy nie tnie** `.rows`
samo — host dostarcza wiersze WYŁĄCZNIE dla bieżącej strony
(`tf-table.js`, komentarz `_pageState`). Pager renderuje się tylko gdy
`total > pageSize`.

```js
// project-studio.js — wzorzec pełnego cyklu page-change
table.addEventListener('page-change', async (e) => {
  s.cases.page = Number(e.detail?.page ?? 1);
  await loadCasesPage();                        // host ładuje nową stronę
  table.setAttribute('page', String(s.cases.page));
  table.setAttribute('total', String(s.cases.total));
  assignRows();                                  // table.rows = nowe dane
  syncCasesFooter();
});
```

## Selekcja i pasek akcji zbiorczych

- `<tf-table selectable>` (lub `selectable="multi"`) dodaje checkbox
  „zaznacz wszystkie” w nagłówku pierwszej kolumny danych i checkbox per
  wiersz w tej samej kolumnie — **bez** dodatkowej kolumny (`_isMultiSelect`,
  `tf-table.js`).
- Eventy: `select-all` (`{selected}`) i `row-select` (`{row, index,
  selected}`).
- Pasek zbiorczy to osobny element **poza** `<tf-table>` (`.ps-bulk-bar`),
  `hidden` dopóki `selected.size === 0`, z licznikiem i przyciskami akcji:

```html
<div class="ps-bulk-bar" id="ps-cases-bulk" hidden>
  <span class="ps-bulk-count"></span>
  <tf-button variant="ghost" size="sm" icon="send" data-bulk="review">Do przeglądu</tf-button>
  <tf-button variant="ghost" size="sm" icon="check" data-bulk="approved">Zatwierdź</tf-button>
  <tf-button variant="ghost" size="sm" icon="trash" data-bulk="delete">Usuń</tf-button>
  <tf-button variant="ghost" size="sm" icon="x" data-bulk="clear">Wyczyść</tf-button>
</div>
```
Destrukcyjna akcja zbiorcza (`delete`) nadal idzie przez `TfWindow.confirm`
(patrz [`forms.md`](forms.md)).

## Akcje wiersza: menu vs ikony inline

| Liczba akcji | Wzorzec | Przykład |
|---|---|---|
| 2–3 akcje, wszystkie często używane | Ikony inline, `tf-button variant="ghost" size="sm" icon="…"` w ostatniej kolumnie | `users.js`: edytuj + usuń w `.row-actions` |
| 3+ akcji, część rzadko używana | `table.rowActions = (row) => {...}` budujący `<div class="tf-table__cell-row">` z kilkoma `tf-button` LUB kebab `<tf-menu>` | `tentanas/shares.js`: edytuj/pauza/usuń inline dla admina, jedna ikona „szczegóły” dla zwykłego użytkownika |
| Akcja tworzenia z wariantami (np. „Nowy przypadek: ręczny / z kodu”) | `<tf-menu>` z `<tf-menu-item>` przy przycisku głównym | `project-studio.js`: `#ps-cases-new-menu` |

`rowActions` to property (nie atrybut) — funkcja `(row, index) => Element`,
wywoływana przy każdym renderze wiersza; element musi być zbudowany na nowo
(albo zbindowany na nowo) bo recykling wierszy podmienia obiekt `row` pod tą
samą komórką (`_writeActionsCell`, `tf-table.js`).

## Responsywność

- **`hide-below="N"`** na `<tf-column>` — dozwolone progi:
  `{480, 640, 720, 900, 1024, 1180, 1280}` (`HIDE_BELOW_BREAKPOINTS`,
  `tf-table.js`). Wartość spoza zbioru = kolumna zawsze widoczna (fail-open,
  nie zgadywanie sąsiedniego progu).
- **`priority="low"`** ukrywa kolumnę na telefonie w wariancie flush
  (`<= 480px`, klasa `.lo`).
- **Fallback kartowy** (`<= 720px`, wariant domyślny): każda `<td>` dostaje
  `data-label` z etykiety kolumny (`_applyCardLabel`) — CSS w `controls.css`
  przestawia wiersz w kartę z podpisanymi polami. Komórka bez `label`
  (podsumowanie czytające się jak zdanie) nie dostaje podpisu.
- **`variant="flush"`**: bez ramki wrappera (kartę rysuje kontener-host),
  wiersze klikalne, **na mobile NIE zwija się do kart** — scrolluje poziomo
  wewnątrz karty. Używane gdy tabela już jest wewnątrz `.tf-section-card`.
- **`narrow`**: tabela w wąskiej karcie — kolumna `fill` traci minimalną
  szerokość, paski udziału się kurczą, szablon procentowy zostaje na
  telefonie zamiast przewijać.
- Komórki hidden przez `hide-below` **zostają w DOM** (ukrywane CSS-em) —
  zaznaczenie, ekspansja, sort i stan recyklingu wiersza przeżywają zmianę
  szerokości viewportu bez przebudowy.

## Sticky header

`<tf-table>` nie ma dziś wbudowanego sticky `<thead>` — `sticky` na
`<tf-column>` przypina **kolumnę** (poziomy scroll), nie wiersz nagłówka
(pionowy scroll). Jeśli strona potrzebuje sticky header przy długiej
tabeli, rozwiąż to w CSS strony (`position: sticky` na `thead` przez
`::part`/zewnętrzny wrapper) — nie jest to dziś ustandaryzowane w komponencie.

## Puste / ładowanie / błąd wierszy

`<tf-table>` samo nie renderuje stanu pustego — host odpowiada za to
**przed** ustawieniem `.rows` (patrz [`states.md`](states.md)):

```js
if (list.length === 0) {
  host.innerHTML = users.length === 0
    ? `<div class="users-empty">${T('users.no_users')}</div>`   // pusto naprawdę
    : `<div class="users-empty">${T('users.no_match')}</div>`;  // pusto po filtrze
  return;
}
```
Rozróżnienie „brak danych w ogóle” vs „brak wyników po filtrze” jest
świadome — inny tekst, czasem inna akcja ([`states.md`](states.md)).
Wiersz błędu ładowania idzie tym samym kanałem co reszta strony (alert +
retry), nie jako specjalny wiersz `<tr>` wewnątrz tabeli w audytowanych
modułach.

## Wirtualizacja (wskazówka dla renderera natywnego)

HTML `<tf-table>` nie wirtualizuje wierszy — recykluje istniejące `<tr>` przy
zmianie `.rows`/sortu (`_renderTbody`: update w miejscu, dodaj brakujące, usuń
nadmiarowe), ale renderuje **wszystkie** wiersze bieżącej strony na raz.
Prawdziwa wirtualizacja (tylko widoczne wiersze layoutowane) istnieje gdzie
indziej w repo — `chat.js` używa `createVirtualList` (`js/lib/virtual-list.js`)
dla setek wiadomości. **Dla natywnego renderera**: layoutuj tylko wiersze w
widocznym oknie (+ overscan), tak jak `virtual-list.js` robi to dla czatu —
`tf-table` samo jest wzorcem tylko dla kontraktu danych/zdarzeń (kolumny,
sort, paginacja), nie dla strategii layoutu przy dużych zbiorach.

## Dostępność

| Wymaganie | Stan w kodzie |
|---|---|
| `<thead>`/`<tbody>` semantyczne | ✅ zawsze (`tf-table.js`, `_build`) |
| Sortowalny nagłówek jako `<button>` z własnym fokusem klawiatury | ❌ **nie zaimplementowane** — `<th class="sortable">` reaguje na `click` na całej tabeli (delegacja), nie jest fokusowalnym elementem interaktywnym z osobna; brak `tabindex`/`role="button"` na `<th>`. Do poprawy przy następnej iteracji komponentu. |
| `aria-sort` na aktywnie posortowanej kolumnie | ❌ **nie zaimplementowane** — stan sortu komunikowany wyłącznie wizualnie przez klasy `.sorted-asc`/`.sorted-desc` (`_updateSortIndicators`), czytnik ekranu nie dowiaduje się, która kolumna i w którym kierunku. |
| Etykieta kolumny akcji dla czytnika ekranu | ✅ `actions-label` (widoczny tekst) albo `aria-label="Akcje"` (`_renderThead`) |
| Etykieta „zaznacz wszystkie” / „zaznacz wiersz” | ✅ `aria-label` na `tf-checkbox` (`Zaznacz wszystkie`/`Zaznacz wiersz`) |
| Przycisk rozwijania wiersza | ✅ `aria-expanded` + `aria-label` (Rozwiń/Zwiń) |

Jeśli dodajesz nową tabelę z krytyczną zależnością od sortowania
klawiaturą/czytnikiem ekranu, traktuj powyższe dwa braki jako znany dług
komponentu, nie jako wzorzec do naśladowania — zgłoś/popraw w
`tf-table.js`, zamiast obchodzić je per-strona.

## Checklist

- [ ] Wybrałem tabelę/kartę/listę wg tabeli decyzyjnej wyżej, nie odruchowo.
- [ ] Liczby wyrównane do prawej, jednostka w nagłówku kolumny.
- [ ] Dokładnie jedna kolumna `fill`.
- [ ] `hide-below` tylko z dozwolonego zbioru progów.
- [ ] Paginacja: host aktualizuje `page`/`total`/`.rows` po `page-change`,
      tabela nigdy sama nie tnie `.rows`.
- [ ] Akcje zbiorcze w osobnym pasku, chowanym gdy `selected.size === 0`.
- [ ] Akcje wiersza: ≤3 inline, więcej → `rowActions` z `tf-menu`.
- [ ] Stan pusty rozróżnia „brak danych” od „brak wyników po filtrze”.
- [ ] Duże zbiory (setki+ wierszy jednocześnie widocznych) → rozważ
      wirtualizację po wzorcu `virtual-list.js`, nie poleganie na
      recyklingu `tf-table`.
