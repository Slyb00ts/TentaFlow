# Icon

| | |
|---|---|
| Tier | 0 (MVP) |
| HTML | `.icon` (element utility, nie custom element) — bazowa reguła w `tentaflow-core/www/css/style.css:210-221`, zdublowana w `controls.css:2835-2844` (potrzebna wewnątrz shadow DOM — zobacz „Zachowanie”). Sprite źródłowy: `<svg data-role="sprite">` w `index.html`, 130 symboli `<symbol id="i-*">`. |
| Protokół addonów | `IconRef` (inline struct, nie ma własnego tagu) — `docs/ADDON_UI_COMPONENT_CATALOG_v1.md` §217-219. `IconButton` `0x0402` — §2569-2584. |
| Natywny (TentaEngine) | `tenta_ui_widgets::Icon` / `tenta_ui_widgets::IconButton` — status: planowany |
| Status dokumentu | draft |

Ikona to `<svg class="icon"><use href="#i-nazwa"/></svg>` odwołujący się do sprite'a — nigdy plik SVG osobno, nigdy font ikon, nigdy emoji. Zobacz [foundations/iconography.md](../foundations/iconography.md) po pełną listę 130 symboli i zasady dodawania nowych. Ten dokument opisuje tylko wymiary/stroke i wzorzec „icon button”.

## Anatomia

```text
<svg class="icon [icon-lg|icon-xl]">
  <use href="#i-<nazwa>"/>
</svg>
```

```text
┌────┐
│ i  │   viewBox 24×24, stroke: currentColor, stroke-width: 1.75,
└────┘   stroke-linecap/linejoin: round, fill: none
  16×16 (.icon, domyślny) · 20×20 (.icon-lg) · 24×24 (.icon-xl)
```

Icon button (przycisk tylko-ikona, wzorzec `.btn-icon` / protokół `IconButton`):

```text
┌──────────┐
│    [i]   │  ← padding 8px wokół ikony 16-20px; box min 44×44 wymuszony
└──────────┘    tylko na mobile (@media max-width:767px), patrz „Rozmiary”
```

## Warianty

| Klasa/rozmiar | Wygląd | Kiedy używać |
|---|---|---|
| `.icon` (bazowy) | 16×16 | domyślny rozmiar w przyciskach, polach, wierszach tabeli |
| `.icon-lg` | 20×20 | nagłówki sekcji, karty statystyk |
| `.icon-xl` | 24×24 | puste stany, duże akcje, nagłówki modali |
| `IconRef { kind: "named" }` | odwołanie po nazwie z `IconName` + `IconSize` + `Tone` | addon renderujący ikonę z tokenami zamiast surowego SVG |
| `IconRef { kind: "asset" }` | zewnętrzny obraz (`signed_url_ref`) zamiast symbolu ze sprite'a | ikony dostawców/integracji spoza zestawu 130 symboli |

## Rozmiary

| Nazwa klasy (żywy kod) | px | Token `IconSize` (protokół, `tokens.rs:180-189`) | Uwaga |
|---|---|---|---|
| *(brak — token `xs`)* | — | `xs` → 12px | **nie istnieje w CSS**, patrz odstępstwa |
| `.icon` (bazowy, bez sufiksu) | 16px | `sm` → 16px | zgodne |
| `.icon-lg` | 20px | `md` → 20px | nazwa klasy sugeruje „duży”, token mówi „średni” — patrz odstępstwa |
| `.icon-xl` | 24px | `lg` → 24px | nazwa klasy sugeruje „extra-large”, token mówi „duży” — patrz odstępstwa |
| *(brak — token `xl`)* | — | `xl` → 32px | **nie istnieje w CSS**, patrz odstępstwa |

Cel dotykowy 44px dla ikony samodzielnej (icon button): sama ikona zostaje 16-20px, ale otaczający `.btn-icon` dostaje `min-width/min-height: 44px` — **tylko poniżej 767px** (`style.css:197-208`). Na desktopie `.btn-icon` ma jedynie `padding: 8px` (`style.css:517-518`), co przy ikonie 14px daje ok. 30×30px — poniżej 44px celu dotykowego, akceptowalne wyłącznie dla `pointer: fine` (mysz), zgodnie z regułą „minimalny cel dotykowy 44px” stosowaną tam, gdzie realnie występuje dotyk.

## Stany

| Stan | Kolor obrysu | Uwagi |
|---|---|---|
| default | `currentColor` (dziedziczy z rodzica) | ikona nie ma własnego koloru — zawsze koloruje ją kontekst (tekst przycisku, tone chipa…) |
| hover (wewnątrz `.btn-icon`) | bez zmiany koloru ikony; zmienia się tło/obramowanie przycisku | patrz [button.md] (do napisania) |
| disabled | `currentColor` dziedziczone z przygaszonego tekstu rodzica | ikona sama nie ma stanu disabled |
| decorative (`aria-hidden="true"`) | — | domyślne dla ikon towarzyszących tekstowi (patrz Dostępność) |

Ikona nie ma własnych stanów `focus`/`active`/`loading` — te należą do przycisku/kontrolki, która ją hostuje.

## Zachowanie

- Interakcja wskaźnikiem: sama ikona nie reaguje — reaguje kontener (`tf-button`, `.btn-icon`, `tf-tab` itd.).
- Klawiatura: brak — ikona nigdy nie jest bezpośrednio fokusowalna.
- Animacje: brak wbudowanych; jeśli ikona się obraca/pulsuje (np. spinner), animacja żyje na kontenerze, nie na `.icon`.
- Duplikacja reguły `.icon` między `style.css` i `controls.css` jest zamierzona, nie przypadkowa: komponenty renderowane w shadow DOM (np. `tf-table`) nie widzą `style.css` (dołączonego tylko do light DOM), więc `adoptControlsInto()` wstrzykuje `controls.css` do ich shadow roota — stąd `.icon` musi tam istnieć osobno (`controls.css:2827-2844`, komentarz w kodzie wyjaśnia że bez tego ikona rozjeżdżała się do domyślnych wymiarów SVG 300×150, rozdymając wiersze tabeli do ~180px).
- Sprite jest w light DOM `index.html`, więc `<use href="#i-...">` z wnętrza shadow roota nie trafia w cel w Chrome/Safari — komponenty shadow-DOM klonują sprite do siebie przez `injectSpriteIntoShadow()` (`shared-styles.js`).

## Dostępność

- Ikona towarzysząca widocznemu tekstowi (np. w przycisku z etykietą) jest **dekoracyjna**: `aria-hidden="true"`, treść czyta czytnik ekranu z tekstu obok.
- Icon-only button (bez widocznej etykiety) **musi** mieć `aria-label` na przycisku-rodzicu — protokołowy `IconButton` wymusza to polem `aria_label: tstr` jako *required* (`docs/ADDON_UI_COMPONENT_CATALOG_v1.md:2579`). W HTML odpowiedzialność spoczywa na wywołującym — `.icon`/`.btn-icon` same tego nie egzekwują.
- Kontrast: ikona dziedziczy `currentColor`, więc kontrast liczy się jak dla tekstu w tym samym miejscu (AA wobec tła).
- `prefers-reduced-motion`: dotyczy wyłącznie animowanych wariantów (spinner, pulsujący badge) — statyczna ikona nie wymaga klauzuli.

## Responsywność i platformy

- Rozmiary ikon nie skalują się automatycznie z breakpointem — zmiana rozmiaru to zawsze świadoma zmiana klasy (`.icon` → `.icon-lg`), nie media query.
- Cel dotykowy 44px dla `.btn-icon` aktywuje się dopiero `@media (max-width: 767px)` — na tablecie/desktopie z `pointer: coarse` (bez media query touch) nie ma gwarancji 44px; do rozważenia przy przenoszeniu reguły na `(pointer: coarse)` zamiast szerokości viewportu.
- ESP32-P4 (`platform.esp32p4_tab5`, `pointer: coarse`, `density: comfortable`): brak media query `max-width` na panelu 720×1280 nie aktywuje automatycznie reguły 44px — natywny theme musi wymusić touch target niezależnie od CSS-owej ścieżki web.
- `stroke-width` token (`control.icon_stroke`) różnicuje 1.75 (domyślny) / 2.0 (mały) / 1.5 (duży) — żywy CSS ma jedną stałą wartość 1.75 dla wszystkich rozmiarów (`style.css:214`, `controls.css:2839`), nie różnicuje po rozmiarze.

## Tokeny użyte

- `control.icon_size.sm/md/lg` (16/20/24px) — realnie pokryte przez `.icon`/`.icon-lg`/`.icon-xl`.
- `control.icon_size.xs/xl` (12/32px) — zdefiniowane w tokenach, **bez pokrycia w CSS** (patrz odstępstwa).
- `control.icon_stroke.default` (1.75) — jedyna wartość faktycznie używana.
- `control.touch_target_min` (44) — reguła `.btn-icon` na mobile.
- `radius.circle` — kształt icon-buttona okrągłego wariantu (gdy używany jako avatar-like trigger).

## Znane odstępstwa w kodzie (2026-09-14)

1. **Nazwy klas CSS nie zgadzają się z nazwami tokenów `IconSize`.** Token `md` = 20px, ale klasa nosi nazwę `.icon-lg`; token `lg` = 24px, klasa `.icon-xl`. Kod ma tylko 3 rozmiary (16/20/24), token ma 5 (12/16/20/24/32) — `xs` (12px) i `xl` (32px) nie istnieją w CSS w ogóle. Generator tokenów (`design/README.md` §"Docelowo generator") powinien albo dodać brakujące klasy, albo przenumerować istniejące na zgodne z tokenem.
2. **`stroke-width` jest stały (1.75) niezależnie od rozmiaru**, mimo że `control.icon_stroke` w `tokens.json` definiuje warianty `small: 2.0` / `large: 1.5` dla wizualnego wyrównania grubości kreski przy różnych rozmiarach.
3. **Icon-only 44px nie jest wymuszane w kodzie** — to konwencja opisana w komentarzu `style.css:197`, egzekwowana wyłącznie przez media query szerokości, nie przez politykę komponentu (np. minimalny `min-height` niezależny od breakpointu, gdy `pointer: coarse`).

## Przykłady

```html
<svg class="icon" aria-hidden="true"><use href="#i-check"/></svg>

<!-- icon button z wymaganym aria-label -->
<button class="btn-icon" aria-label="Zamknij">
  <svg class="icon" aria-hidden="true"><use href="#i-x"/></svg>
</button>
```

```rust
// natywnie (tenta-ui-widgets, API docelowe)
Icon::named("check").size(IconSize::Sm).tone(Tone::Success);

IconButton::new(Icon::named("x"))
    .aria_label("Zamknij")
    .on_press(Msg::Close);
```
