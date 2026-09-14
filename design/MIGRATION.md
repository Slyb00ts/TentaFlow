# Migracja: v1 → v2 i prostowanie CSS

Ten dokument wyjaśnia, dlaczego wersja 2.0 tego systemu tokenów (`tokens/tokens.json`,
2026-09-14) różni się od wersji 1.0 (`DESIGN.md` w korzeniu repo, 2026-04-17), co
dokładnie się rozjechało w żywym CSS, i jaki jest plan doprowadzenia kodu do zgodności
z tokenem zamiast odwrotnie. Wszystkie liczby w sekcji (c) i (f) zostały zmierzone
grepem po `tentaflow-core/www/css/*.css` (40 plików + `css/components/tf-audio-capture.css`)
2026-09-14; metoda pomiaru jest podana przy każdej liczbie, żeby dało się ją odtworzyć.

## a) Nazwy tokenów v1 → realna zmienna CSS → ścieżka w `tokens.json`

`DESIGN.md` deklarował siebie jako „extracted z `tentaflow-core/www/css/variables.css`
+ `components.css`" (`DESIGN.md:5`) — **żaden z tych dwóch plików nigdy nie istniał**
w repozytorium. Token wyekstrahowano z pliku, który nie był źródłem; nazwy poniżej są
więc fikcją nazewniczą, mimo że część *wartości* przypadkiem jest bliska rzeczywistości.

| Nazwa w `DESIGN.md` (v1) | Realna zmienna dziś | Wartość v1 → dziś | `tokens.json` |
|---|---|---|---|
| `--color-bg-primary` | `--bg` (`style.css:5`) | `#0f1117` → `#050818` | `color.themes.dark.bg.base` |
| `--color-bg-secondary` | `--bg-2` (`style.css:6`) | `#1a1d27` → `#0a0d24` | `color.themes.dark.bg.raised` |
| `--color-bg-tertiary` | `--bg-3` (`style.css:7`) | `#232733` → `#111535` | `color.themes.dark.bg.elevated` |
| `--color-text-primary` | `--text` (`style.css:17`) | `#e4e6ed` → `#e8ebf5` | `color.themes.dark.text.primary` |
| `--color-text-secondary` | `--text-2` (`style.css:18`) | `#8b8fa3` → `#a0a8c8` | `color.themes.dark.text.secondary` |
| `--color-accent` | `--accent-1` (`style.css:22`) | `#6366f1` → `#6366f1` (zgodne) | `color.themes.dark.accent.primary` |
| `--color-info` | `--info` (`style.css:30`) | `#3b82f6` → `#60a5fa` | `color.themes.dark.semantic.info.value` |
| `--font-family` | brak jednej zmiennej — literał w `style.css:68` | `'Manrope', -apple-system, ...` → zgodne w treści, niezdefiniowane jako zmienna | `typography.family.sans` |
| `--font-size-md` | brak — literał `14px`/`13px` rozsiany po plikach | `0.875rem` (14px) → **żadna zmienna `--font-size-*` nie istnieje w kodzie** (0 trafień grepa `--spacing-`/`--font-size-` poza `DESIGN.md`) | `typography.scale.body` (13px) |
| `--spacing-md` | brak — `16px`/`12px` jako literał | `16px` → **brak jakiejkolwiek `--spacing-*` w CSS** | `spacing.md` (12px) |
| `--radius-md` | `--radius` (`style.css:49`, `controls.css:60`) | `8px` → `10px` | `radius.md` |
| `--radius-sm` | `--radius-sm` (`style.css:48`) | `6px` → `6px` (zgodne) | `radius.sm` |
| `--shadow-md` | `--shadow` (`style.css:55`) | `0 4px 12px rgba(0,0,0,0.4)` → `0 8px 24px rgba(0,0,0,0.5)` | `elevation.medium` |
| `--transition-fast` | brak zmiennej — literały `0.12s`/`0.15s` | `150ms` → zbliżone w praktyce (najczęstsze literały to 0.12–0.2s), ale bez wspólnej zmiennej | `motion.duration_ms.fast` (120) |

Wiersze bez odpowiednika w prawej kolumnie (spacing, font-size) znaczą dosłownie to,
na co wyglądają: `DESIGN.md` opisywał skalę, która nigdy nie miała reprezentacji w
kodzie. `tokens.json` v2 jest zbudowany z **żywych** `:root` w `style.css` i
`controls.css`, nie z `DESIGN.md` — tam gdzie się różniły, patrz (b).

## b) Rozwiązane konflikty

Dwa różne pliku `:root` (`style.css` — tokeny poziomu shellu aplikacji, bez prefiksu;
`controls.css` — tokeny poziomu komponentów, prefiks `--tf-*`) definiowały **osobno tę
samą koncepcję**, często z inną wartością. Poniżej realne pary porównane 2026-09-14:

| Token | `style.css` (`:root`, linia) | `controls.css` (`:root`, linia) | Decyzja w `tokens.json` |
|---|---|---|---|
| tekst podstawowy | `--text: #e8ebf5` (17) | `--tf-text: #f5f6ff` (18) | `#e8ebf5` — wygrywa `style.css` |
| tekst drugorzędny | `--text-2: #a0a8c8` (18) | `--tf-text-2: #c1c5e0` (19) | `#a0a8c8` — wygrywa `style.css` |
| tło podniesione (sidebar/topbar) | `--bg-2: #0a0d24` (6) | `--tf-bg-2: #0a0e22` (11) | `#0a0d24` — wygrywa `style.css` |
| tło wyniesione (panele) | `--bg-3: #111535` (7) | `--tf-bg-3: #131736` (12) | `#111535` — wygrywa `style.css` |
| promień domyślny | `--radius: 10px` (49) | `--tf-radius: 10px` (60) | zgodne między sobą; **oba** rozjeżdżają się z `DESIGN.md`'s `--radius-md: 8px`, który nigdy nie trafił do kodu — `10px` wygrywa jako wartość faktycznie renderowana |

**Reguła rozstrzygnięcia** (zapisana też w `tokens.json → meta.notes`): gdy `style.css`
i `controls.css` się różniły, wygrywa wartość z `style.css` — bo to plik ładowany jako
pierwszy i determinujący wygląd shellu aplikacji (sidebar/topbar/tło), a komponenty
`tf-*` renderują się *wewnątrz* tego shellu, więc ich token powinien się dostroić do
niego, nie odwrotnie. Rozbieżne `--tf-*` w `controls.css` są długiem — patrz plan (d).

**Monospace**: dwa równoległe stosy współistnieją w kodzie — `'JetBrains Mono'`
(**180 wystąpień w 18 plikach**, w tym `style.css` 56×, `controls.css` 19×) i
`'SF Mono'` (**52 wystąpienia w 6 plikach**: `style.css` 39×, `agents.css` 5×,
`addons.css` 3×, `controls.css` 2×, `flows-builder.css` 1×, `access-keys.css` 2×).
Decyzja w `tokens.json → typography.family.mono`: **`JetBrains Mono` jest kanoniczny,
`SF Mono` jest wygaszany** (`tokens.json:129`, pole `"decision"`) — wybrany, bo ma
więcej wystąpień, jest już `--tf-font-mono`/`--tf-mono` w `controls.css`, i jest
samo-hostowalny (`SF Mono` to font systemowy Apple, niedostępny na Linux/Windows/
Android bez fallbacku na coś losowego).

## c) Dług — zmierzone liczby (2026-09-14, grep po `tentaflow-core/www/css/`)

| Metryka | Wzorzec grepa | Wynik |
|---|---|---|
| Literały hex `#rgb`–`#rrggbbaa` | `#[0-9a-fA-F]{3,8}\b` | **1081 wystąpień w 30 z 41 plików** (wcześniejszy audyt inwentaryzacyjny podawał 1192/169 unikalnych — różnica wynika z detali regexu/zakresu plików między przebiegami; rząd wielkości jest ten sam, oba przebiegi zgadzają się, że to największa liczba literałów w audycie) |
| `border-radius: <n>px` | `border-radius:\s*[0-9.]+px` | **450 wystąpień w 36 plikach.** Rozkład najczęstszych wartości: `4px` ×90, `999px` (pill) ×67, `10px` ×58, `8px` ×55, `6px` ×43, `2px`+`3px` razem ×59, `12px` ×24 — wobec 4-stopniowej skali `radius` w tokenie (`none/xs/sm/md/lg/xl/pill/circle`, faktycznie 8 wartości) |
| `font-size: <n>px` (literał) | `font-size:\s*[0-9.]+px` | **1966 wystąpień w 40 plikach**, z czego **485 to wartości z ułamkiem** (`font-size:\s*[0-9]+\.[0-9]+px`, np. `11.5px`/`12.5px`/`10.5px`) — połówki pikseli, których `typography.rules.half_pixel_sizes` w tokenie wprost zabrania |
| `@media (...)` | `@media` | **195 wystąpień w 35 z 41 plików**, przeważnie `max-width` (mobile-last) zamiast `min-width` zgodnego z `layout.breakpoints_px` (README zasada 6: mobile-first po 640/768/1024/1280/1536/1920) |
| Pliki z `prefers-reduced-motion` | `prefers-reduced-motion` | **7 plików**: `style.css`, `controls.css`, `tentanas.css`, `tentabus.css`, `project-studio.css`, `face.css`, `tf-agent-activity.css` — wobec 35 plików z `@keyframes` (patrz niżej), czyli **28 plików animuje bez respektowania `prefers-reduced-motion`**, mimo że README zasada 5 stawia to jako twardy wymóg każdego komponentu z animacją |
| `@keyframes` | `@keyframes` | **195 wystąpień w 35 plikach** (wcześniejszy audyt podawał 124 — ponownie, różnica regexu/zliczania nazw vs bloków; własny przebieg jest liczbą deklaracji `@keyframes`, nie unikalnych nazw animacji) |
| `data-theme` | `data-theme` | **0 wystąpień** — motyw jasny (`color.themes.light`, status `draft` w tokenie) nie ma żadnej implementacji przełącznika w CSS, mimo że `tokens.json` już ma pełny zestaw wartości dla `light` |
| Reguły `table`/`.tf-table` | `\btable\b\|\.tf-table\b\|<table` | **25 z 41 plików** — większość stron ma własne, częściowo zduplikowane style tabel zamiast polegać wyłącznie na `tf-table` |
| Reguły `.btn`/`tf-btn` | `\.btn\b\|tf-btn\b` | **7 plików** (`style.css`, `code-studio.css`, `controls.css`, `chat-audio.css`, `ml-studio.css`, `services-edit.css`, `compat.css`) — nadpisania przycisku poza `controls.css` |
| Łączny rozmiar CSS | liczba linii `^` we wszystkich plikach | **41 806 linii w 41 plikach** (największe: `controls.css` 11 496, `style.css` 6135, `project-studio.css` 3648, `ml-studio.css` 3036, `code-studio.css` 2324) |

**Ogólny kształt problemu**: system tokenów istnieje jako intencja (`tokens.json`,
realny komponent library 105 plików `tf-*.js`), ale 41 tys. linii CSS per-strona
regularnie go omija literałami hex, promieniami i rozmiarami czcionek zamiast
odwołań do zmiennej — klasyczny wzorzec „biblioteka + ucieczka przez CSS strony",
opisany też w `patterns/new-page-checklist.md` jako coś, czego nowa strona ma unikać.

## d) Plan prostowania — fazy

**P0 — wygeneruj `www/css/tokens.css`, podepnij aliasy, nic nie łam.**
Skrypt `scripts/gen-design-tokens.py` (dziś nie istnieje — do napisania) czyta
`tokens/tokens.json` i emituje `tentaflow-core/www/css/tokens.css` z pełnym zestawem
zmiennych `--bg`, `--text`, `--radius-*`, `--tf-*` (oba namespace'y na raz, jako
aliasy do tych samych wartości źródłowych) plus nowe, których dziś brakuje
(`--spacing-*`, `--font-size-*` per `typography.scale`). Podłączony jako pierwszy
`<link>` w `index.html`, przed `style.css`/`controls.css`, żeby istniejące reguły dalej
działały bez zmiany ani jednej linii strony. To jedyna faza, która dotyka pliku
produkcyjnego przed jakimkolwiek lintem.

**P1 — bramki CI jako warningi, potem errory dla nowych plików.**
Reguły grepowe z `README.md → Governance` (brak hexa poza `tokens.css`, brak
`outline: none` bez zamiennika, `prefers-reduced-motion` przy każdym `@keyframes`)
najpierw jako `warn` w CI dla całego drzewa (widoczność długu bez blokowania PR-ów),
potem jako `error`, ale **tylko dla plików utworzonych po dacie włączenia bramki** —
istniejący dług nie blokuje niezwiązanych zmian, tylko nie może rosnąć.

**P2 — normalizacja plik po pliku, w kolejności.** `controls.css` jako pierwszy (to
plik źródłowy tokenów komponentów — jego literały najbardziej „zarażają" resztę przez
kopiowanie wzorców), potem 5 reprezentatywnych stron wskazanych w audycie inwentaryzacyjnym
(`clusters.js`/`cluster-detail.js`, `settings.js`, `chat.js`+`chat-audio.css`,
`dashboard.js`, `robots.js`), potem reszta uszeregowana malejąco wg liczby literałów hex
— patrz tabela śledzenia (f).

**P3 — usuń aliasy.** Gdy wszystkie pliki z tabeli (f) są na zero literałów i bramka
CI jest `error` bez wyjątków, usuń stare nazwy (`--bg-2`, `--tf-bg-2`, ...) z
`tokens.css`, zostaw tylko jedną, kanoniczną nazwę na koncept. To jedyna faza, która
jest technicznie breaking — wymaga, żeby żaden plik nie odwoływał się już do starej
nazwy (weryfikowalne grepem przed usunięciem).

## e) Dwa systemy komponentów — który jest źródłem prawdy

Istnieją dziś równolegle: biblioteka `tf-*` (105 plików `www/js/components/tf-*.js`,
`custom element` per komponent, stylowane przez `controls.css`) i protokół addonów
`docs/ADDON_UI_COMPONENT_CATALOG_v1.md` (~151 komponentów, u16 tag, CBOR, konsumowany
przez `www/js/sdk-runtime/`), który explicite zapowiada usunięcie starych komponentów
`tf-*` w ramach własnej migracji („Zero backward compatibility... Stare komponenty są
usuwane").

**Decyzja**: `design/components/*.md` (ten katalog) jest **jedyną specyfikacją
behawioralną** dla obu implementacji naraz — anatomia, warianty, stany, zachowanie i
dostępność opisane raz, per komponent. Tabela nagłówkowa specyfikacji
(`_TEMPLATE.md`) ma osobny wiersz na każdą implementację: `HTML` (`tf-nazwa`),
`Protokół addonów` (tag `0x...` z katalogu), `Natywny` (`tenta_ui_widgets::Nazwa`,
planowany). Tagi katalogu addonów są więc **referencjonowane z** każdej specyfikacji
komponentu, nie odwrotnie — `design/components/` nie duplikuje 3580 linii katalogu,
tylko wskazuje na numer sekcji, gdy schemat pól jest tam już wyczerpująco opisany.
Gdy `tf-*` faktycznie zostanie wycofany (data nieznana — katalog addonów jest
„mid-rewrite" per raport hostingowy §3.2), wiersz `HTML` w każdej specyfikacji zmienia
status na „wycofany", specyfikacja **zostaje** — bo opisuje zachowanie, nie
konkretny plik JS.

## f) Tabela śledzenia — 12 plików z największą liczbą literałów hex

Zmierzone 2026-09-14, `grep -c '#[0-9a-fA-F]{3,8}\b'` per plik. Kolumna „data celu" i
„właściciel" nie są ustalone — brak w repozytorium przypisania właścicieli per plik
CSS; do uzupełnienia przy planowaniu sprintu P2, nie wymyślone tutaj.

| Plik | Literałów hex dziś | Data celu | Właściciel |
|---|---|---|---|
| `style.css` | 224 | do ustalenia | do ustalenia |
| `controls.css` | 211 | do ustalenia | do ustalenia |
| `flows-builder.css` | 205 | do ustalenia | do ustalenia |
| `profiling.css` | 136 | do ustalenia | do ustalenia |
| `profile-report.css` | 46 | do ustalenia | do ustalenia |
| `ml-studio.css` | 34 | do ustalenia | do ustalenia |
| `code-studio.css` | 27 | do ustalenia | do ustalenia |
| `profile-flamegraph.css` | 27 | do ustalenia | do ustalenia |
| `profile-compare.css` | 21 | do ustalenia | do ustalenia |
| `profile-permissions.css` | 21 | do ustalenia | do ustalenia |
| `roles_catalog.css` | 17 | do ustalenia | do ustalenia |
| `addon-app.css` | 14 | do ustalenia | do ustalenia |

`style.css` i `controls.css` są jednocześnie źródłem prawdy dla `tokens.json` (ich
zmienne w `:root` **nie liczą się** do powyższego — to literały *poza* blokiem
`:root`, w regułach stron/komponentów niżej w tych samych plikach) i zarazem
największymi ofiarami literałów — sygnał, że nawet pliki-źródła tokenów same siebie
nie przestrzegają w pełni.

## g) Rozjazdy wykryte przy pisaniu specyfikacji (2026-09-14)

Zebrane z sekcji „Znane odstępstwa" w `foundations/`, `components/` i `patterns/`;
każda pozycja ma właściciela decyzji „token wygrywa" chyba że zaznaczono inaczej.

| Obszar | W kodzie dziś | Token / spec | Działanie |
|---|---|---|---|
| szerokość sidebara | `.app { grid-template-columns: 260px 1fr }` (`style.css`) | `layout.sidebar.expanded = 240` | P2: przejść na `var(--sidebar-width)`; 240 zgodne z mockupem `screen-shell-20260423` |
| topbar | desktop nie ma topbara; nagłówek mobile 52 px | `layout.topbar.height = 56` | spec `patterns/page-anatomy.md` opisuje nagłówek strony, nie globalny topbar; token zostaje dla natywnego shellu |
| sprite ikon | dwa niezależne: `index.html` `i-*` (130, stroke 1.75) i `img/icons.svg` `icon-*` (143, stroke 1.5); duplikat `i-key` w `index.html` | jeden zestaw, stroke 1.75, `IconName` z sdk-spec | P2: wygenerować oba z jednego źródła (`design/icons/*.svg` → sprite), usunąć duplikat |
| rozmiary ikon | trzy systemy: `.icon/.icon-lg/.icon-xl` (px), `.tf-icon--size-*` (em), `IconSize` | `control.icon_size` 12/16/20/24/32 | P2: `.tf-icon--size-*` na px z tokena |
| etykiety osi wykresów | `.tf-chart__axis-label { fill: var(--tf-text-3) }` | `color.dataviz.axis_text = text.secondary` | P2 w `controls.css` |
| sparkline | 60 px min-width × 32 px (`tf-sparkline.js`) | spec: 60×32 (poprawiono z 60×20 z v1) | brak — token zaktualizowany do kodu |
| warianty przycisku | `primary/secondary/ghost/outline/danger/danger-solid/danger-outline/success` | `ButtonVariant` primary/secondary/tertiary/ghost/destructive/link | P2: mapowanie aliasów w `tf-button.js`, potem usunięcie starych nazw |
| rozmiary kontrolek | jeden rozmiar (button 38 px, input 40 px, checkbox 18 px) | `control.height` 28/36/44 | P2: atrybut `size` w Tier 0 |
| tooltip | `:hover` natychmiast, brak long-press na dotyku | opóźnienie + long-press | P2 (a11y) |
| radio-group | brak strzałek (segmented je ma) | wzorzec WAI-ARIA radiogroup | P2 (a11y) |
| toast | `TfToast.show()` bez `role`/`aria-live`, bottom-right, bez pauzy hover i undo | `role=status`, stos top-right (desktop) / bottom (mobile), undo 10 s | P2 |
| modal | tylko Esc, brak pułapki fokusu | pułapka fokusu | P2 (a11y) |
| tabela | brak `aria-sort`, nagłówek nie fokusowalny | wzorzec sortable header | P2 (a11y) |
| chat-composer | Enter ignoruje `isComposing` (psuje IME) | commit kompozycji przed wysłaniem | P1 — błąd, nie dług |
| command palette | zarejestrowany, nigdy nie zamontowany | Tier 1 | decyzja produktowa |
| modal | szerokość 520 px, scrim `rgba(5,8,24,0.6)`, `z-index` 9990/9991 na sztywno, `aria-modal` bez pułapki | 480 px, `bg.overlay`, `layout.z_index.modal`, pułapka fokusu | P2 |
| gauge | dwie implementacje: `tf-gauge.js` (SVG, 160 px) i ad-hoc `.gauge-ring` w `clusters.js`/`style.css` (conic, 64–76 px, `hot` → `--danger`) | jeden `tf-gauge`, `hot` > 85 % → `semantic.warning` | P2: usunąć `.gauge-ring` po migracji `clusters.js` |
| list | `tf-list` bez roli ARIA i klawiatury (tf-tree ma poprawny treeview) | wzorzec listbox z roving tabindex | P2 (a11y) |
| avatar | trzy systemy (`tf-avatar` px, protokół em, ad-hoc `.user-chip` z hex gradientem `style.css:3072`) | jeden `tf-avatar` z `IconSize`-owymi rozmiarami | P2 |
| motyw jasny | 0 wystąpień `data-theme` | `color.themes.light` (draft) | po P0 (aliasowanie umożliwia przełącznik) |
