# List

| | |
|---|---|
| Tier | 0 (MVP) / 1 |
| HTML | `<tf-list>` — `tentaflow-core/www/js/components/tf-list.js`, style `controls.css:2378-2456`. `<tf-key-value>` — `tf-key-value.js`, style `controls.css:3679-3705`. `<tf-tree>` — `tf-tree.js`, style `controls.css:7117-7144` (+ `controls.css:10002-10018` warianty badge). |
| Protokół addonów | `0x0212` `List` + `0x0207` `KeyValue` + `0x0213` `Tree` — `docs/ADDON_UI_COMPONENT_CATALOG_v1.md` §1504, §1687-1719 |
| Natywny (TentaEngine) | `tenta_ui_widgets::List` / `KeyValue` / `Tree` — status: planowany |
| Status dokumentu | draft |

Trzy odrębne komponenty do trzech kształtów danych: `tf-list` — płaska lista rekordów z ikoną/chipem/paskiem ważności; `tf-key-value` — siatka par klucz-wartość (np. panel szczegółów); `tf-tree` — hierarchia z rozwijaniem. Nie mylić `tf-list` z `tf-table` — lista nie ma kolumn ani sortowania; gdy potrzebne są kolumny, użyj [table.md](table.md).

## Anatomia

`tf-list`:

```text
┃ [icon] Tytuł                              [chip]   ← border-left 3px (severity: danger/warning/success/transparent)
┃        Podtytuł
──────────────────────────────────────────────────
┃ [icon] Tytuł 2                            [chip]
```

`tf-key-value`:

```text
┌──────────────┬───────────────────────────┐
│ Status       │ Active            [OK]     │  ← <table><tr><td.tf-kv-key><td.tf-kv-value chip?>
│ Version      │ 1.2.3                      │
└──────────────┴───────────────────────────┘
```

`tf-tree`:

```text
▾ Sekcja A                                    ← role="treeitem", roving tabindex
  ▸ Podsekcja A.1
    Liść A.1.a                    [M]         ← .tf-tree__badge (a/m/d/c — status pliku)
  Liść A.2
▸ Sekcja B (leniwie ładowana, lazy)
```

## Warianty

| Komponent | Warianty | Kiedy używać |
|---|---|---|
| `tf-list[compact]` | mniejszy padding (`8px 12px` zamiast `12px 14px`) | gęste panele boczne |
| `tf-list[selectable]` | klik zaznacza wiersz (tło `--tf-accent-glow`) | lista z wyborem pojedynczego elementu |
| `tf-tree[variant]` (protokół `TreeVariant`) | `default` / `compact` / `with_icons` | `compact` zmniejsza padding wiersza, `with_icons` skaluje `.tf-tree__icon` do `1em` |
| `tf-key-value` | brak wariantów — zawsze ta sama siatka dwukolumnowa | panel „szczegóły” z parami etykieta-wartość |

## Rozmiary

Żaden z trzech komponentów nie ma formalnej skali `sm/md/lg`. `tf-list` ma tylko `compact` (boolean) kontra domyślny. Wysokość wiersza wynika z paddingu + treści, nie jest wymuszana na minimum 44px — przy liście klikalnej na dotyku host powinien sam zadbać o wystarczającą wysokość wiersza (padding `12px 14px` + jedna linia tekstu 13px daje ok. 37-40px, poniżej celu 44px).

## Stany

| Stan | Komponent | Tło | Obramowanie | Uwagi |
|---|---|---|---|---|
| default | `tf-list` | — | `border-left: 3px transparent` | |
| hover | `tf-list` | `--tf-bg-card-hover` | — | + `transform: translateX(2px)`, `box-shadow` wewnętrzny akcentowy; tylko `pointer: fine` |
| selected | `tf-list[selectable]` | `--tf-accent-glow` | — | tylko gdy `selectable` ustawione |
| severity: danger/warning/success | `tf-list` | — | `border-left-color` odpowiedniego semantyka | pasek 3px, nie zmienia tła |
| hover | `tf-tree` | `--tf-bg-3` | — | |
| focus-visible | `tf-tree` | — | `controls.css:7131` | jedyny z trzech komponentów z widocznym fokusem |
| selected | `tf-tree` | zaznaczony wiersz podświetlony | `controls.css:7135` | `.tf-tree__node--selected > .tf-tree__row` |
| disabled | `tf-tree` | `opacity: 0.5` | `cursor: not-allowed` | |

`tf-key-value` nie ma stanów interaktywnych — to statyczna prezentacja danych.

## Zachowanie

- **`tf-list`**: klik wiersza emituje `item-click` (`detail: {item, index}`). Gdy `selectable`, ten sam klik dodatkowo przełącza klasę `.selected` na klikniętym wierszu (pojedynczy wybór, poprzednie zaznaczenie znika). **Brak jakiejkolwiek obsługi klawiatury** — wiersze nie są fokusowalne (brak `tabindex`), nie ma `role="listbox"`/`"option"`, nie ma roving tabindex ani wielokrotnego wyboru mimo że dane wejściowe (`item.severity`, `item.chip`) sugerują bogatszy model.
- **`tf-key-value`**: właściwość `.entries = [{key, value, chip?, chipTone?, keyNode?, valueNode?}]`. `keyNode`/`valueNode` pozwalają hostowi wstawić własne, reaktywne węzły DOM zamiast statycznego tekstu — jedyny z trzech komponentów z tym mechanizmem. Brak zdarzeń — czysto prezentacyjny.
- **`tf-tree`**: kontrolowany przez właściwości `.expandedIds` (Set) i `.selectedId`; intencja użytkownika wyrażana przez zdarzenia `expand`/`collapse`/`select` (nie-bąbelkujące, `detail: {id}`) — komponent **nie zmienia własnego stanu** przy interakcji, host musi odpowiedzieć na zdarzenie i nadać nowe `.expandedIds`/`.selectedId`. `expand` niesie dodatkowo `lazy: true/false`, pozwalając hostowi doładować dzieci węzła na żądanie.
- Klawiatura `tf-tree` (prawdziwy roving tabindex — dokładnie jeden `.tf-tree__row` ma `tabindex="0"`, reszta `-1"`): `ArrowDown`/`ArrowUp` — przesuwają fokus na kolejny/poprzedni widoczny węzeł (płaska lista widocznych węzłów, licząc zagnieżdżenie); `ArrowRight` — rozwija węzeł zwinięty albo (gdy już rozwinięty) wchodzi do pierwszego dziecka; `ArrowLeft` — zwija węzeł rozwinięty albo (gdy zwinięty/liść) przechodzi do rodzica; `Enter`/`Space` — emituje `select`; `Home`/`End` — pierwszy/ostatni widoczny węzeł.
- Zaznaczanie tekstu: `tf-list-item` ma `user-select: none` (`controls.css:2397`) — nie da się zaznaczyć tekstu wiersza listy. `tf-key-value`/`tf-tree` nie blokują zaznaczania.

## Dostępność

- **`tf-tree`** jest jedynym z trzech z pełną semantyką ARIA: `role="tree"` na kontenerze, `role="group"` na zagnieżdżonych listach dzieci, `role="treeitem"` + `aria-expanded` na węzłach z dziećmi. Roving tabindex jest zaimplementowany poprawnie wg wzorca APG treeview.
- **`tf-list` nie ma żadnej roli ARIA** (`role="list"`/`"listbox"` brak) i żadnego atrybutu `aria-selected` na wierszu zaznaczonym — dla czytnika ekranu to zwykłe `<div>`, nawigacja i odczyt zaznaczenia nie działają inaczej niż przez treść tekstową.
- **`tf-key-value` renderuje prawdziwy `<table>`**, więc struktura klucz/wartość jest odczytywalna natywnie mimo braku jawnych ról ARIA.
- Ikony w `tf-list-item` mają `aria-hidden="true"` (dekoracyjne, `tf-list.js:84`).
- `prefers-reduced-motion`: `transform: translateX(2px)` na hover `tf-list-item` pokryte globalną regułą wildcard, brak dedykowanej klauzuli.

## Responsywność i platformy

- Żaden z trzech komponentów nie ma wbudowanych breakpointów — `tf-list`/`tf-tree` przewijają się pionowo w kontenerze o ograniczonej wysokości (`max-height: 100%` na `tf-list`), układ zależy od rodzica.
- Na dotyku (`pointer: coarse`) `tf-list-item` bez wymuszonego minimum 44px może być trudna do trafienia — do rozważenia przy density `comfortable` na telefonie/ESP32-P4.
- Na ESP32-P4 (`shadows: flat`) `box-shadow` wewnętrzny na hover `tf-list-item` nie ma znaczenia (brak wskaźnika); stan `selected`/`severity` (oparte na kolorze/obramowaniu, nie cieniu) pozostają czytelne.

## Tokeny użyte

- `color.themes.dark.bg.card_hover` (`--tf-bg-card-hover`) — hover `tf-list-item`.
- `color.themes.dark.accent.soft` (`--tf-accent-glow`) — selected `tf-list-item`.
- `color.themes.dark.semantic.critical/warning/success.value` — paski `severity` w `tf-list`, tło `.tf-tree__badge--*`.
- `color.themes.dark.text.secondary` (`--tf-text-3`) — `.tf-tree__caret`/`.tf-tree__icon` nieaktywne.
- `motion.easing.standard` (`--tf-spring-smooth`) — przejście `transform` hover listy.
- `spacing.md`/`spacing.lg` — zbliżone do paddingu wierszy (12/14), nie dokładne wartości.

## Znane odstępstwa w kodzie (2026-09-14)

1. **`tf-list` nie ma żadnej obsługi klawiatury ani roli ARIA**, mimo że protokołowy `List` (0x0212) i ogólna zasada dostępności systemu zakładają pełną nawigowalność. Porównaj z `tf-tree`, który w tym samym pliku CSS/JS-owym ekosystemie implementuje wzorcowy roving tabindex — brak spójności między komponentami z tej samej rodziny „dane w liście”.
2. **`tf-list` obsługuje wyłącznie pojedynczy wybór**, mimo że zadanie/dokumentacja zakłada tryb multi-select analogiczny do tabeli (`tf-table[selectable="multi"]`). Nie ma odpowiednika `page-size`/checkboxów/`select-all` znanych z tabeli.
3. **`tf-key-value` nie waliduje `chipTone`** wobec żadnego zbioru dozwolonych wartości (w przeciwieństwie do np. `tf-avatar`/`tf-choice-card`, które filtrują przez `Set` dozwolonych tonów) — dowolny string trafia bezpośrednio do `class="tf-chip ${entry.chipTone}"`, co przy literówce cicho nie wyrenderuje żadnego stylu tonu zamiast spaść do bezpiecznego domyślnego.
4. **Trzy komponenty, zero wspólnego bazowego stylu zaznaczenia** — `tf-list` używa `background: --tf-accent-glow`, `tf-tree` osobnej reguły w linii 7135 (inny selektor, inna specyficzność), `tf-table` jeszcze innej (`tr.selected`). Wizualnie zbieżne, ale utrzymywane jako trzy oddzielne reguły CSS.

## Przykłady

```html
<tf-list selectable>
</tf-list>
<script>
  document.querySelector('tf-list').items = [
    { id: '1', title: 'prod-cluster-01', sub: 'Online', severity: 'success', icon: 'server', chip: 'OK', chipTone: 'ok' },
  ];
</script>

<tf-key-value></tf-key-value>
<script>
  document.querySelector('tf-key-value').entries = [
    { key: 'Status', value: 'Active', chip: 'OK', chipTone: 'ok' },
    { key: 'Wersja', value: '1.2.3' },
  ];
</script>
```

```rust
// natywnie (tenta-ui-widgets, API docelowe)
List::new(items)
    .item_template(|item| ListRow::new(&item.title).sub(&item.sub).severity(item.severity))
    .on_item_click(Msg::OpenItem);

Tree::new(nodes)
    .expanded_ids(expanded)
    .selected_id(selected)
    .lazy_load(true)
    .on_expand(Msg::ExpandNode)
    .on_select(Msg::SelectNode);
```
