# <Nazwa komponentu>

| | |
|---|---|
| Tier | 0 (MVP) / 1 / 2 |
| HTML | `<tf-nazwa>` — `www/js/components/tf-nazwa.js`, style `controls.css` linie … |
| Protokół addonów | `0x0…` `NazwaKomponentu` — `docs/ADDON_UI_COMPONENT_CATALOG_v1.md` §… |
| Natywny (TentaEngine) | `tenta_ui_widgets::Nazwa` — status: planowany / w toku / gotowy |
| Status dokumentu | draft / reviewed |

Jedno zdanie: do czego służy i kiedy go **nie** używać (co użyć zamiast).

## Anatomia

Lista części z nazwami (np. *container*, *leading icon*, *label*, *trailing icon*,
*helper text*). Dla każdej: który token typografii/koloru/odstępu.

```text
┌─────────────────────────────┐
│ [icon] Label          [chev]│  ← ASCII szkic z wymiarami w tokenach
└─────────────────────────────┘
   height: control.height.md (36)   padding-x: spacing.md (12)
```

## Warianty

Tabela: wariant → wygląd → kiedy używać. Nazwy wariantów = enumy sdk-spec, jeśli istnieją
(`ButtonVariant`, `BadgeVariant`, `Tone`…).

## Rozmiary

`sm` / `md` / `lg` → wysokość (`control.height`), typografia (`TextStyle`), odstępy,
rozmiar ikony (`IconSize`). Minimalny cel dotykowy 44 px — jak go osiąga wariant `sm`.

## Stany

| Stan | Tło | Tekst/ikona | Obramowanie | Uwagi |
|---|---|---|---|---|
| default | | | | |
| hover | | | | tylko `pointer: fine` |
| active / pressed | | | | |
| focus-visible | | | ring `control.focus_ring` | |
| disabled | | | | `aria-disabled`, brak hover |
| loading | | | | spinner, `aria-busy` |
| error / invalid | | | | `aria-invalid` + komunikat |
| selected / checked | | | | |

## Zachowanie

- Interakcja wskaźnikiem (mysz, dotyk, pióro): co robi tap, long-press, drag.
- Klawiatura: klawisze i ich efekt (Enter/Space/Esc/strzałki/Tab).
- Animacje: co się animuje, którym tokenem `motion` (i sprężyną w natywnym).
- Zdarzenia/API: atrybuty i eventy `tf-*`, handlery w protokole, `Msg` w natywnym.
- Zaznaczanie tekstu: dozwolone / zablokowane.

## Dostępność

Rola ARIA, wymagane atrybuty, etykieta (`aria-label` dla icon-only), live region,
kontrast każdego stanu (podać wynik AA), zachowanie przy `prefers-reduced-motion`.

## Responsywność i platformy

Co zmienia się poniżej `sm`/`md`; gęstość (`compact/default/comfortable`); różnice na
ESP32-P4 (brak cieni, brak hover, whole-pixel), na telefonie (cele dotykowe).

## Tokeny użyte

Pełna lista odwołań do `tokens.json` (ścieżki), żeby generator/lint mógł je zweryfikować.

## Znane odstępstwa w kodzie (2026-09-14)

Co dziś w `controls.css`/`tf-*.js` robi inaczej niż ten spec i jak to prostujemy
(link do pozycji w `design/MIGRATION.md`).

## Przykłady

```html
<tf-nazwa variant="primary" size="md">Etykieta</tf-nazwa>
```

```rust
// natywnie (tenta-ui-widgets, API docelowe)
Nazwa::new("Etykieta").variant(Variant::Primary).on_press(Msg::Save)
```
