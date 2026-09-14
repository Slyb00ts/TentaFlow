# Breadcrumb + detail header

| | |
|---|---|
| Tier | 0 (MVP) |
| HTML | `<tf-breadcrumb>`/`<tf-breadcrumb-item>` — `www/js/components/tf-breadcrumb.js`, style `controls.css` linie 3260-3287; `<tf-detail-header>` — `www/js/components/tf-detail-header.js`, style `controls.css` linie 3711-3773 |
| Protokół addonów | `0x0110` `Breadcrumb` — `docs/ADDON_UI_COMPONENT_CATALOG_v1.md` §3 (`Breadcrumb.separator`, `.max_items`); brak taga dedykowanego dla `DetailHeader` |
| Natywny (TentaEngine) | `tenta_ui_widgets::Breadcrumb` / `DetailHeader` — status: planowany |
| Status dokumentu | draft |

Dwa różne komponenty nawigacyjne dokumentowane razem, bo `tf-detail-header`
często siedzi bezpośrednio pod `tf-breadcrumb` na stronach szczegółu
(addon/entity detail). Breadcrumb pokazuje ścieżkę; detail header pokazuje
tożsamość encji (ikona, tytuł, wersja, badge'e, akcje) pod nią.

## Anatomia — Breadcrumb

```text
Ustawienia › Addonsy › Eureka
└─────────┘ └───────┘ └────┘
 .tf-breadcrumb-item  ...   .tf-breadcrumb-item.current (aria-current="page")
             ↑
      .tf-breadcrumb-sep (›, aria-hidden)
```

`<tf-breadcrumb>` obserwuje mutacje swoich dzieci `<tf-breadcrumb-item>`
(`MutationObserver` na `childList`/`attributes`) i re-renderuje wewnętrzny
`<nav aria-label="Breadcrumb">` przy każdej zmianie — czyli działa jak
kontrolowany kontener, nie jak statyczny szablon czytany raz.

## Anatomia — Detail header

```text
┌────┐  Nazwa encji  v1.0.0   [status]      [Akcja podstawowa]
│icon│  Podtytuł                            [Akcja druga]
└────┘  [badge] [badge]
 60×60   .tf-detail-top-row: .tf-detail-title + .tf-chip.accent (wersja) + slot="status"
 circle   .tf-detail-subtitle
          .tf-detail-badges ← slot="badges"
```

`slot="icon"` zastępuje domyślną ikonę-sprite w kółku z `--tf-gradient-accent`;
`slot="status"` siedzi obok tytułu (stan encji, np. chip „Active”);
`slot="badges"` to osobny wiersz metadanych pod podtytułem; `slot="actions"`
trafia do `.tf-detail-actions` po prawej.

## Warianty

Breadcrumb (`BreadcrumbSeparator`, tylko w schemacie protokołu — element HTML
ma separator zaszyty na sztywno):

| Wartość | Separator | Gdzie |
|---|---|---|
| `chevron` | `›` (encja HTML `&#8250;`) | jedyny separator realnie renderowany przez `tf-breadcrumb.js` |
| `slash` / `dot` | zdefiniowane w schemacie protokołu, ale renderer nie jest udokumentowany osobno tutaj — sprawdź `layout-nav-breadcrumb-pagination.js` przed użyciem |

Detail header nie ma formalnych wariantów (`variant`) — różnicuje się przez
obecność/brak slotów (`icon`, `status`, `badges`, `actions`, `version`).

## Rozmiary

Brak `sm`/`md`/`lg` w obu komponentach. Detail header ma stały rozmiar ikony
60×60px (kółko) / SVG wewnętrzne 28×28 — poza skalą `control.icon_size`
(xs–xl = 12–32px), patrz odstępstwa.

## Stany

| Stan | Breadcrumb | Detail header |
|---|---|---|
| default | link `--tf-text-3` | tytuł `--tf-text`, podtytuł `--tf-text-3` |
| hover | `a.tf-breadcrumb-item:hover` → `--tf-accent-2` | brak reguł hover (statyczny header) |
| current / bieżąca strona | `.current`, `aria-current="page"`, kolor `--tf-text`, `font-weight: 600`, renderowany jako `<span>` nie `<a>` | nie dotyczy |
| focus-visible | dziedziczony z linku (`<a>` natywny fokus) — brak dedykowanego ringu w CSS breadcrumbu | fokus idzie na kontrolki w `slot="actions"` |
| truncation / overflow | **brak** — `.tf-breadcrumb { flex-wrap: wrap }`, item nie ma `text-overflow: ellipsis` | tytuł: `white-space: nowrap; overflow: hidden; text-overflow: ellipsis` (`.tf-detail-title`) |

## Zachowanie

- Interakcja wskaźnikiem: element breadcrumb bez `href` lub z `current`
  renderuje się jako `<span>` (nieklikalny); z `href` — jako `<a>` zwykłej
  nawigacji przeglądarki (bez `preventDefault`/SPA routing w samym
  komponencie — to zadanie hosta). Detail header nie obsługuje żadnej
  interakcji poza tym, co jest w slotowanych dzieciach.
- Klawiatura: linki breadcrumb są w naturalnej kolejności Tab (to `<a>`).
  `current` (span) nie jest fokusowalny — poprawnie, bo nie ma dokąd
  nawigować.
- Animacje: brak w obu komponentach.
- Zdarzenia/API:
  - `tf-breadcrumb-item` — atrybuty `href`, `current` (boolean).
    `attributeChangedCallback` na itemie wywołuje `_render()` na rodzicu
    przez `this.closest('tf-breadcrumb')`.
  - `tf-detail-header` — atrybuty obserwowane: `title`, `subtitle`, `icon`,
    `version`. Cztery sloty (`icon`, `status`, `badges`, `actions`)
    konsumowane raz przy `_build()` (pierwszym `connectedCallback`) — dodanie
    slotowanego dziecka **po** pierwszym podłączeniu elementu do DOM nie
    zostanie podchwycone (`_build()` nie jest wołane ponownie).
- Zaznaczanie tekstu: dozwolone w obu.

## Dostępność

Breadcrumb: `<nav aria-label="Breadcrumb">` opakowuje całość (poprawny
landmark), bieżący element ma `aria-current="page"`, separator ma
`aria-hidden="true"` (nie jest czytany). Detail header: brak `role`/landmark
dedykowanego — to zwykły `<div>`; tytuł nie ma nagłówka semantycznego
(`<span>`, nie `<h1>`/`<h2>`) mimo pełnienia roli tytułu strony/sekcji — do
rozważenia przy migracji do natywnego silnika (renderer natywny może użyć
prawdziwego poziomu nagłówka).

## Responsywność i platformy

**Brak jakiejkolwiek reguły `@media` dla `.tf-breadcrumb`/
`.tf-breadcrumb-item`/`.tf-detail-header`** w `controls.css` — `flex-wrap:
wrap` na breadcrumbie to jedyna odpowiedź na wąski ekran (items zawijają się
do nowego wiersza), nie ma kolapsu do strzałki „wstecz” ani przycinania
środkowych elementów w samym komponencie HTML. Detail header (`display: flex;
align-items: center; gap: 18px`) nie ma reguły przełączającej layout na
kolumnowy na wąskim ekranie — na telefonie ikona 60px + tytuł + akcje mogą
się ściskać bez łamania wiersza.

## Tokeny użyte

- `color.themes.dark.text.muted` (`--tf-text-3`) — breadcrumb domyślny, podtytuł
- `color.themes.dark.text.primary` (`--tf-text`) — breadcrumb current, tytuł
- `color.themes.dark.accent.secondary` (`--tf-accent-2`) — hover linku
- `color.themes.dark.gradient.accent` — tło kółka ikony detail header
- `spacing.lg` (gap `.tf-detail-header`, 18px ≈ najbliższy `lg`=16, patrz odstępstwa)
- `radius.circle` — kółko ikony 60×60

## Znane odstępstwa w kodzie (2026-09-14)

- **Brak `max_items`/kolapsu środka w elemencie HTML.** Pole protokołu
  `Breadcrumb.max_items` (domyślnie 5, „collapse middle if exceeds”) jest
  zaimplementowane **tylko** w rendererze addonów
  (`layout-nav-breadcrumb-pagination.js`, funkcja `collapseBreadcrumbItems` —
  pierwszy element + „…” + ostatnie `max_items - 2`). `<tf-breadcrumb>` użyty
  bezpośrednio w modułach hosta **nie ma tej logiki w ogóle** — długa ścieżka
  po prostu zawija się do kolejnych wierszy.
- **Brak kolapsu do strzałki „wstecz” na mobile.** Ani element HTML, ani
  renderer protokołu nie implementują tego wzorca opisanego w zadaniu —
  traktuj to jako rekomendację kierunku, nie opis obecnego zachowania.
- **Gap detail header (18px) nie jest żadnym tokenem `spacing.*`** —
  najbliższe wartości to `lg` (16) i `xl` (24); 18 jest literałem w
  `controls.css:3717`.
- **Ikona detail header (60px kółko / 28px svg) poza `control.icon_size`** —
  tokeny mają maks. `xl` = 32px.
- **Sloty czytane tylko raz.** `tf-detail-header._build()` zbiera
  `slot="icon"/"status"/"badges"/"actions"` wyłącznie przy pierwszym
  `connectedCallback`; dynamiczna podmiana zawartości slotu po fakcie
  wymaga usunięcia i ponownego dodania całego elementu.

## Przykłady

```html
<tf-breadcrumb>
  <tf-breadcrumb-item href="/settings">Ustawienia</tf-breadcrumb-item>
  <tf-breadcrumb-item href="/settings/addons">Addonsy</tf-breadcrumb-item>
  <tf-breadcrumb-item current>Eureka</tf-breadcrumb-item>
</tf-breadcrumb>

<tf-detail-header title="Eureka" subtitle="MF Public Data" icon="database" version="1.0.0">
  <span slot="badges"><tf-chip class="ok">Active</tf-chip></span>
  <span slot="actions"><tf-button variant="primary">Install</tf-button></span>
</tf-detail-header>
```

```rust
// natywnie (tenta-ui-widgets, API docelowe)
Breadcrumb::new(vec![
    BreadcrumbItem::new("Ustawienia").href("/settings"),
    BreadcrumbItem::new("Addonsy").href("/settings/addons"),
    BreadcrumbItem::new("Eureka").current(),
]).max_items(5)

DetailHeader::new("Eureka")
    .subtitle("MF Public Data")
    .icon(Icon::Database)
    .version("1.0.0")
    .action(Button::new("Install").variant(ButtonVariant::Primary))
```
