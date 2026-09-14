# Odstępy i układ

Ten dokument opisuje skalę odstępów TentaFlow (baza 4px), konwencje paddingu i
gapu per klasa komponentu, tokeny powłoki aplikacji (sidebar/topbar/content),
siatkę 12-kolumnową i skalę z-index. Jedynym źródłem wartości jest
[`tokens/tokens.json`](../tokens/tokens.json) → `spacing` i `layout`.

## Skala `Spacing` (baza 4px)

Enum `Spacing` w `tentaflow-sdk-spec/src/protocol/ui/tokens.rs` (`Zero | Xxs | Xs
| Sm | Md | Lg | Xl | Xxl`) mapuje się na `spacing.*` w tokens.json:

| `Spacing` | px | Typowe użycie |
|---|---|---|
| `zero` | 0 | brak odstępu, elementy sklejone (np. segmenty `tf-segmented`) |
| `xxs` | 2 | odstęp ikona–tekst w małej odznace |
| `xs` | 4 | odstęp wewnątrz `tf-chip`/`tf-tag`, padding checkboxa |
| `sm` | 8 | gap między elementami w rzędzie akcji, padding małego inputu |
| `md` | 12 | **padding-x kontrolki** (patrz niżej), gap w liście pionowej |
| `lg` | 16 | **padding karty**, gutter siatki 12-kolumnowej |
| `xl` | 24 | **gap sekcji** wewnątrz strony |
| `xxl` | 32 | odstęp między dużymi blokami hero |

Dwie wartości layoutowe nie są częścią enumu `Spacing` (są specyficzne dla
układu strony, nie dla pojedynczej kontrolki) — `spacing.layout`:

| Token | px | Użycie |
|---|---|---|
| `layout.section` | 48 | odstęp między głównymi sekcjami strony (np. hero → lista) |
| `layout.page` | 64 | margines górny/dolny treści strony wewnątrz `<tf-screen>` |

## Konwencje paddingu/gapu per klasa komponentu

Te przypisania są normatywne dla nowego kodu — jeśli komponent nie pasuje do
żadnej klasy poniżej, wybierz najbliższy poziom gęstości (`control.density` w
tokens.json: `compact` 0.9× / `default` 1.0× / `comfortable` 1.15×), nie
wartość ad-hoc.

| Klasa komponentu | Padding / gap | Token |
|---|---|---|
| Kontrolka (input, select, button) | padding-x | `Spacing::Md` (12px) |
| Karta (`tf-section-card`, `tf-stat-card`) | padding | `Spacing::Lg` (16px) |
| Sekcja strony (odstęp między blokami) | gap | `Spacing::Xl` (24px) |
| Rząd w liście/tabeli | padding-y | `Spacing::Sm` (8px) |
| Grupa pól formularza | gap | `Spacing::Md` (12px) |
| Odznaka/chip | padding | `Spacing::Xs` (4px) |
| Strona względem topbaru/sidebaru | margines zewnętrzny | `layout.page` (64px) |

## Powłoka aplikacji (`layout.sidebar`, `layout.topbar`, `layout.content`)

| Token | Wartość | Rola |
|---|---|---|
| `sidebar.expanded` | 240px | szerokość sidebaru rozwiniętego |
| `sidebar.collapsed` | 64px | szerokość sidebaru zwiniętego (tylko ikony) |
| `sidebar.collapse_below` | `Breakpoint::Md` (1024px) | próg, poniżej którego sidebar domyślnie się zwija |
| `topbar.height` | 56px | wysokość paska górnego |
| `content.max_width` | 1440px | maksymalna szerokość treści na szerokich ekranach |
| `content.narrow` | 800px | wariant wąski treści (np. formularz jednokolumnowy, ustawienia) |

## Siatka 12-kolumnowa (`layout.grid`)

`layout.grid.columns = 12`, `layout.grid.gutter = {spacing.lg}` = **16px**.
Kolumny liczone są względem `content.max_width` (1440px) lub `content.narrow`
(800px) w zależności od wariantu strony — nie względem pełnej szerokości
viewportu, żeby treść nie rozciągała się w nieskończoność na bardzo szerokich
monitorach.

## Skala z-index (`layout.z_index`)

| Token | Wartość | Warstwa |
|---|---|---|
| `base` | 0 | treść strony |
| `sticky` | 10 | nagłówki tabel przyklejone przy scrollu, sticky topbar |
| `dropdown` | 100 | `tf-menu`, `tf-combobox` lista |
| `overlay` | 1000 | scrim pod modalem/drawerem |
| `modal` | 1100 | `tf-modal`, `tf-window` |
| `toast` | 1200 | `tf-toast` |
| `tooltip` | 1300 | `tf-tooltip` — zawsze nad wszystkim, łącznie z modalem |

Kolejność jest celowa: tooltip musi być czytelny nawet nad otwartym modalem
(np. tooltip na przycisku wewnątrz dialogu), dlatego ma najwyższą wartość.

Zastrzeżenie zweryfikowane w kodzie: dzisiejszy `style.css` używa `z-index` jako
liczb ad-hoc (np. `50`, `90`, `100`, `200`, `10000`), niepowiązanych z tą skalą.
Tabela powyżej to skala **docelowa** dla nowego kodu i dla natywnego renderera —
nie opis obecnego stanu CSS. Nie dopisuj nowej strony z własną arbitralną
wartością z-index; użyj skali z tokens.json, nawet jeśli otaczający, starszy CSS
jeszcze jej nie stosuje.

## Breakpointy (`layout.breakpoints_px`)

Enum `Breakpoint` (`Xs | Sm | Md | Lg | Xl | Xxl`), mobile-first (`min-width`):

| `Breakpoint` | px |
|---|---|
| `xs` | 640 |
| `sm` | 768 |
| `md` | 1024 |
| `lg` | 1280 |
| `xl` | 1536 |
| `xxl` | 1920 |

Pełne zasady degradacji per ekran są w [responsive.md](responsive.md); ten
dokument podaje tylko wartości progów.

## Kompozycja bez wartości arbitralnych — przykłady

Ważne zastrzeżenie zweryfikowane w kodzie: `tokens.json → spacing` dziś **nie ma**
pola `css` (w przeciwieństwie do `color`), a grep po `--spacing-` w
`tentaflow-core/www/css/*.css` nie daje trafień — w CSS nie istnieje jeszcze
zestaw zmiennych `--space-*`. Dopóki generator (`scripts/gen-design-tokens.py`,
`design/MIGRATION.md`) nie wyprodukuje `www/css/tokens.css`, kompozycja "bez
wartości arbitralnych" oznacza: użyj liczby z tabeli `Spacing` powyżej i opisz ją
komentarzem wskazującym token, zamiast wymyślać własną wartość.

Zamiast:

```css
.my-panel { padding: 18px; margin-bottom: 40px; gap: 10px; }
```

Skomponuj z wartości ze skali, z komentarzem wskazującym token (do czasu, aż
istnieją realne zmienne CSS):

```css
.my-panel {
  padding: 16px;      /* Spacing::Lg — to jest karta */
  margin-bottom: 24px; /* Spacing::Xl — odstęp sekcji */
  gap: 12px;           /* Spacing::Md — grupa pól */
}
```

W kodzie Rust (natywny renderer, protokół addonów) nie ma tego problemu — tam
zawsze pisz `Spacing::Lg`, nie `16`, bo enum jest już tam pierwszoklasowym typem.

Jeśli żadna wartość ze skali nie pasuje — to sygnał, że układ jest źle
zdekomponowany (za dużo w jednym bloku), nie powód do dopisania nowego
literalnego px. Zgłoś brakujący przypadek zamiast obchodzić skalę.

## Checklist dla autora strony

- [ ] Każdy padding/margin/gap to wartość z `Spacing` (2/4/8/12/16/24/32px) albo
      `layout.section`/`layout.page` (48/64px) — zero arbitralnych px.
- [ ] Padding kontrolki to `md`, padding karty to `lg`, gap sekcji to `xl`.
- [ ] Układ strony mieści się w `content.max_width` (1440px) albo świadomie
      używa wariantu `content.narrow` (800px).
- [ ] Warstwy z-index używają tokenów `layout.z_index`, nie literalnych liczb.
- [ ] Siatka to 12 kolumn z gutterem `lg` (16px), nie własna liczba kolumn.
- [ ] Breakpointy strony to podzbiór `layout.breakpoints_px`, nie nowe wartości.
