# Tabs

| | |
|---|---|
| Tier | 0 (MVP) |
| HTML | `<tf-tabs>` + `<tf-tab>` — `tentaflow-core/www/js/components/tf-tabs.js`, style `controls.css:666-948` (bazowy `.tf-tab`, warianty `soft`/`underline`) + `controls.css:10108-10225` (wariant `bar`, layout `stacked`, `safe-area`). |
| Protokół addonów | `0x010B` `Tabs` — `docs/ADDON_UI_COMPONENT_CATALOG_v1.md` §1289-1302 |
| Natywny (TentaEngine) | `tenta_ui_widgets::Tabs` — status: planowany |
| Status dokumentu | draft |

Pasek przełączników pokazujący dokładnie jeden panel treści na raz. Nie używaj do nawigacji między niepowiązanymi stronami (od tego jest sidebar/breadcrumb, patrz [patterns/navigation.md](../patterns/navigation.md)) ani jako zamiennika segmentowanej kontrolki filtrów krótkotrwałych (`tf-segmented`, `controls.css:689-731`) — zakładki reprezentują trwałe sekcje tej samej treści.

## Anatomia

```text
┌──────────────────────────────────────────────────────┐
│ ░ Ogólne   Sieć   Bezpieczeństwo ●3   Zaawansowane ▸  │  ← .tf-tabs-viewport (scroll poziomy)
│ ▔▔▔▔▔▔▔                                                │  ← .tf-tab-indicator / .tf-tab-underline-bar / .tf-tab-bar-line (FLIP)
└──────────────────────────────────────────────────────┘
  ░ = .tf-tabs-fade-left/-right (gradient przy przewijalnej krawędzi)
  ▸ = .tf-tabs-chev-left/-right (strzałki scroll, widoczne tylko gdy jest co przewijać)
```

Pojedyncza zakładka (`<tf-tab>` → wewnętrzny `<button class="tf-tab" role="tab">`):

```text
[● | A | icon]  Etykieta          [•dirty]  [licznik]  [×]
  leading slot     tf-tab-text                            tf-tab-close (sibling, nie child)
  (dot > marker > icon, dokładnie jeden)
```

## Warianty

Rzeczywiste warianty HTML (atrybut `variant` na `<tf-tabs>`) różnią się nazwami od enuma protokołu — patrz „Znane odstępstwa”:

| Wariant HTML | Wygląd | Kiedy używać |
|---|---|---|
| `solid` (domyślny) | pigułkowe tło aktywnej zakładki, FLIP-owy prostokątny wskaźnik | domyślny pasek zakładek w panelach |
| `soft` | karta z lekko podniesionym tłem (`--tf-bg-card`), aktywna zakładka dostaje poświatę accent | ustawienia, panele z gęstą treścią |
| `underline` | płaska linia 2px pod aktywną zakładką | zakładki wewnątrz nagłówka sekcji, mniej wizualnego ciężaru |
| `bar` | pełnowysokościowe komórki, ruchoma linia gradientowa na krawędzi (`indicator="top"\|"bottom"`) | dolna nawigacja telefonu, pasek scen, dock |

## Rozmiary

Zakładki nie mają formalnej skali `sm/md/lg` — jeden rozmiar tekstu (12px, `font-weight: 700`, `letter-spacing: 0.04em`), padding `7px 14px`. Atrybut `layout="stacked"` przenosi ikonę nad etykietę (dla wariantu `bar`, np. dolna nawigacja mobilna) zamiast zmieniać rozmiar. Atrybut `safe-area` podnosi minimalną wysokość do 46px i dodaje inset na home-indicator iOS — to jedyny sposób osiągnięcia celu dotykowego 44px w tym komponencie.

## Stany

| Stan | Tło | Tekst/ikona | Obramowanie/wskaźnik | Uwagi |
|---|---|---|---|---|
| default | przezroczyste | `--tf-text-3` | brak | |
| hover | `--tf-bg-3` (soft) / bez zmiany tła (inne warianty) | `--tf-text` | — | tylko `pointer: fine` |
| active (aktywna zakładka) | zależne od wariantu (patrz Warianty) | `--tf-accent-2` / `white` | wskaźnik FLIP animuje się na pozycję | `aria-selected="true"` |
| focus-visible | bez zmiany tła | bez zmiany | `controls.css:10524` | outline standardowy |
| disabled | bez zmiany tła | przygaszony (`disabled` na `<button>`) | — | natywne wyłączenie `<button disabled>` |
| dirty (niezapisana treść) | bez zmiany | kropka `.tf-tab-dirty` między etykietą a licznikiem | — | np. edytor plików z niezapisanymi zmianami |
| nudge | bez zmiany (nieaktywna) | `color: --tf-warning` na hover | — | „to czeka na ciebie” — bursztynowy akcent |
| pinned | `--tf-bg-2` (tylko wariant `bar`) | — | przypięta do lewej krawędzi, reszta się przewija | |

## Zachowanie

- Interakcja wskaźnikiem: klik zakładki emituje `change` na `<tf-tabs>` (`detail.value`); klik `×` (gdy `closable`) emituje `tab-close` na `<tf-tab>` (bąbelkujące, `cancelable`) i **nie** wyzwala jednocześnie zmiany aktywnej zakładki (`stopPropagation` w handlerze).
- Klawiatura: strzałki (prawo/dół = następna, lewo/góra = poprzednia, `Home`/`End` = pierwsza/ostatnia) **tylko przesuwają fokus** między przyciskami zakładek — aktywacja zostaje na natywnym `Enter`/`Space` przycisku (`manual activation`, nie `automatic activation` z APG). Zakładki nie mają roving tabindex — każda pozostaje w naturalnej kolejności Tab, strzałki nie zmieniają `tabindex`.
- Overflow: kółko myszy z przewagą pionową (`deltaY`) jest tłumaczone na przewijanie poziome (`_onWheel`), dotyk przewija natywnie (`touch-action: pan-x`), strzałki chevron przewijają o 60% szerokości widocznego paska.
- Aktywna zakładka po zmianie `value` automatycznie przewija się do widoku z marginesem 24px (`_scrollActiveIntoView`).
- Wskaźnik FLIP: mierzony przez `getBoundingClientRect`, offset/szerokość liczone względem `scrollLeft` scrollera; animacja wejścia (`is-entering`) odtwarza się tylko przy realnej zmianie aktywnej zakładki, nie przy każdym przeliczeniu (scroll/resize).
- Zdarzenia/API: `<tf-tabs variant| value| layout| indicator>`, zdarzenie `change` (`detail.value`). `<tf-tab label| icon| count| count-tone| disabled| dirty| dot| tone| marker| sub| mono| closable| pinned| nudge| panel>`, zdarzenie `tab-close`. Atrybut `panel` na `<tf-tab>` ustawia `aria-controls` na przycisku — pokazywanie/ukrywanie panelu treści jest **poza komponentem**: `tf-tabs` nie zarządza panelami samo (patrz „lazy panels” w odstępstwach).

## Dostępność

- `<tf-tabs>` renderuje `role="tablist"` na scrollerze, każda zakładka to `role="tab"` (natywny `<button>`) z `aria-selected` synchronizowanym przy każdej zmianie `value`.
- `aria-controls` wskazuje panel treści **tylko gdy** host poda atrybut `panel` na `<tf-tab>` — bez niego relacja tab↔panel nie jest deklarowana dla technologii wspomagających.
- Aktywacja manualna (Enter/Space, nie auto-focus-select) jest zgodna z jednym z dwóch dozwolonych wzorców APG dla `tablist` — poprawna, ale odmienna od wzorca „automatic activation” który mogliby oczekiwać użytkownicy strzałek.
- `×` zamknięcia to osobny `<button>` (sibling, nie dziecko przycisku zakładki) — unika nieprawidłowego zagnieżdżenia `button > button`, które byłoby niedostępne dla czytników ekranu.
- `prefers-reduced-motion`: animacja wskaźnika (`is-entering`, FLIP transform) korzysta z `transition`, pokrytej globalną regułą wildcard `controls.css:9586` — brak własnej, dedykowanej klauzuli w sekcji `tf-tabs`.

## Responsywność i platformy

- Pasek zakładek jest zawsze przewijalny poziomo (`overflow-x: auto`, scrollbar ukryty) — nie ma osobnego układu „mobile” poza `layout="stacked"` + `safe-area` dla wariantu `bar` jako dolnej nawigacji.
- `safe-area` dodaje `env(safe-area-inset-bottom)` (przez CSS, `controls.css:10215-10225`) dla iOS home-indicator.
- Fade + chevron przy krawędziach pojawiają się/znikają dynamicznie (`ResizeObserver` + `scroll`), więc pasek na wąskim ekranie sam sygnalizuje że jest więcej zakładek — nie ma stałego progu breakpointu.
- Na ESP32-P4 (`pointer: coarse`, brak `hover: fine`) fade/chevron nadal działają (bazują na realnym overflow, nie na hover), ale hover-only style (`sortable:hover`, itp. w innych komponentach) nie mają odpowiednika — tutaj nie dotyczy, bo `tf-tab` nie ma stanu hover krytycznego dla funkcji.

## Tokeny użyte

- `color.themes.dark.text.secondary` (`--tf-text-3`), `color.themes.dark.accent.secondary` (`--tf-accent-2`) — tekst nieaktywny/aktywny.
- `color.themes.dark.accent.soft` (`--tf-accent-glow`) — tło aktywnej zakładki w wariancie `soft`.
- `color.themes.dark.bg.card`, `color.themes.dark.bg.elevated` — tła wariantu `soft`.
- `radius.sm` (`--tf-radius-sm`) — promień pojedynczej zakładki.
- `motion.easing.standard` (`--tf-spring-smooth`) — przejścia hover/aktywacji.
- `motion.spring.snappy` (`--tf-spring-snappy`) — animacja wejścia wskaźnika w wariancie `bar`.
- `control.touch_target_min` (44) — częściowo pokryty przez `safe-area` (46px), nie domyślnie.

## Znane odstępstwa w kodzie (2026-09-14)

1. **Nazewnictwo wariantów nie pasuje do protokołu.** `TabsVariant` w `docs/ADDON_UI_COMPONENT_CATALOG_v1.md:1295` definiuje `"default" | "pills" | "underlined" | "boxed"`, a żywy HTML implementuje `solid | soft | underline | bar` (`tf-tabs.js:6`). **Nie ma wariantu „pill” w kodzie HTML** — najbliższy wizualnie jest `soft` (zaokrąglone tło aktywnej zakładki na `--tf-radius-sm`, nie w pełni pigułkowe `999px`). Kto implementuje natywny toolkit wg nazw z protokołu, musi zmapować `pills→soft`, `underlined→underline`, `boxed→solid`/`bar` ręcznie — nie ma czystego 1:1.
2. **`tf-tabs` nie zarządza panelami.** Mimo że protokołowy `Tabs` (0x010B) ma pole `content_slot` sugerujące, że renderowany jest tylko panel aktywnej zakładki (funkcjonalny odpowiednik lazy-load), HTML-owy `tf-tabs` **nie pokazuje/ukrywa żadnej treści** — atrybut `panel` tylko ustawia `aria-controls`. Pokazywanie właściwego panelu (i ewentualne leniwe ładowanie) jest w całości obowiązkiem hosta strony; nie ma z tego żadnej gwarancji w komponencie.
3. **Brak roving tabindex.** Zakładki pozostają w naturalnej kolejności Tab (każda ma domyślny `tabindex`), co jest odmienne od wzorca list/drzewa w tym systemie (`tf-tree` ma prawdziwy roving tabindex, patrz [list.md](list.md)) — niespójność międzykomponentowa w obsłudze klawiatury.
4. **Inset wskaźnika underline jest zahardkodowany (10px)**, a dla `bar[layout=stacked]` to `24%` szerokości zakładki (`tf-tabs.js:438-444`) — żadna z tych wartości nie pochodzi z `spacing`.

## Przykłady

```html
<tf-tabs variant="underline" value="general">
  <tf-tab id="general" label="Ogólne"></tf-tab>
  <tf-tab id="network" label="Sieć"></tf-tab>
  <tf-tab id="security" label="Bezpieczeństwo" count="3" count-tone="warn"></tf-tab>
</tf-tabs>
```

```rust
// natywnie (tenta-ui-widgets, API docelowe)
Tabs::new(vec![
    TabItem::new("general", "Ogólne"),
    TabItem::new("network", "Sieć"),
    TabItem::new("security", "Bezpieczeństwo").badge("3", Tone::Warning),
])
.variant(TabsVariant::Underlined)
.active("general")
.on_select(Msg::SelectTab);
```
