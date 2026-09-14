# TentaFlow Design System

Jedno miejsce, w którym opisujemy **jak wygląda i jak się zachowuje** UI TentaFlow —
niezależnie od tego, czy renderuje je dziś HTML/CSS (`tentaflow-core/www`), protokół
komponentów addonów (`tentaflow-sdk-spec::protocol::ui`), czy jutro natywny silnik
TentaEngine (wgpu / CPU / ESP32-P4). Kod jest po angielsku, dokumentacja po polsku.

Stan na 2026-09-14: to jest wersja **2.0** systemu. Wersja 1.0 (`DESIGN.md` z 2026-04-17)
opisywała słownik tokenów, którego nigdy nie było w kodzie — szczegóły rozjazdu i plan
prostowania są w [MIGRATION.md](MIGRATION.md).

## Hierarchia źródeł prawdy

1. **[`tokens/tokens.json`](tokens/tokens.json)** — każda wartość koloru, rozmiaru,
   promienia, cienia, czasu animacji, breakpointu. Nazwy semantyczne pokrywają się
   1:1 z enumami w `tentaflow-sdk-spec/src/protocol/ui/tokens.rs` (`Tone`, `Spacing`,
   `TextStyle`, `RadiusToken`, `ShadowToken`, `Breakpoint`, `IconSize`).
2. **Ten katalog** — reguły, anatomia komponentów, wzorce stron. Dokument opisuje
   *który token* i *dlaczego*; nigdy nie powtarza wartości bez odwołania do tokena.
3. **Implementacje** — `www/css/controls.css` + `www/js/components/tf-*.js` (HTML),
   `tentaflow-sdk-spec` (protokół addonów), `tentaflow-ui-native` (natywny, planowany).
4. **Mockupy** w [`../mockups/`](../mockups/) — referencja wizualna *co ma wyglądać jak*,
   nie źródło wartości. Najnowszy kierunek: `tentaquant-20260903`, `agenci-20260822`,
   `analityka-20260819`, `zdarzenia-20260819`.

Zmiana tokena: najpierw `tokens.json`, potem implementacje, potem dokument. Docelowo
generator (`scripts/gen-design-tokens.py`, zadanie w MIGRATION.md) produkuje z JSON
`www/css/tokens.css` oraz `tentaflow-ui-native/src/theme_generated.rs`; do tego czasu
lustrzane zmiany robimy ręcznie i wpisujemy do PR checklisty.

## Mapa katalogu

```text
design/
├── README.md                  ← ten plik: zasady, governance, jak dodać stronę
├── MIGRATION.md               ← rozjazd v1 → v2, długi CSS, bramki lint
├── tokens/tokens.json         ← kanoniczne tokeny (maszynowo czytelne)
├── foundations/               ← fundamenty
│   ├── colors.md              kolory, tone, tryby dark/light, dataviz
│   ├── typography.md          Manrope + JetBrains Mono, skala TextStyle, metryki
│   ├── spacing-layout.md      siatka 4 px, Spacing, shell (sidebar/topbar), grid
│   ├── radius-elevation.md    promienie, cienie, obramowania
│   ├── materials.md           matowe powierzchnie i „płynne szkło" (Liquid Glass): warianty, budżety, gdzie wolno
│   ├── motion.md              czasy, easingi, sprężyny, reduced-motion
│   ├── iconography.md         sprite 130 ikon, stroke 1.75, rozmiary, dodawanie ikon
│   ├── responsive.md          breakpointy, strategie per ekran, touch vs pointer
│   ├── accessibility.md       fokus, kontrast, klawiatura, ARIA, cele dotykowe
│   └── dataviz.md             palety wykresów, osie, legendy, sparkline/gauge
├── components/                ← katalog kontrolek (jeden plik = jedna kontrolka)
│   ├── README.md              indeks, tiery, status HTML / protokół / natywny
│   ├── _TEMPLATE.md           szablon specyfikacji
│   └── *.md                   button.md, input.md, table.md, …
├── patterns/                  ← wzorce złożone
│   ├── page-anatomy.md        szkielet strony: shell, nagłówek, treść, akcje
│   ├── new-page-checklist.md  krok po kroku: jak dodać nową stronę
│   ├── forms.md               układ formularzy, walidacja, zapis
│   ├── data-tables.md         tabele danych: sortowanie, paginacja, gęstość, mobile
│   ├── states.md              empty / loading / error / offline
│   └── navigation.md          sidebar, breadcrumb, tabs, command palette, routing
└── platforms/                 ← ograniczenia per platforma
    ├── web-desktop.md
    ├── mobile.md              iOS/Android WebView dziś, natywny surface jutro
    └── esp32p4.md             panel 720×1280 RGB565, dotyk, budżet klatki
```

## Zasady (twarde)

1. **Zero literalnych wartości w nowym kodzie.** Kolor, rozmiar czcionki, odstęp,
   promień, cień, czas — zawsze z tokena. W CSS `var(--…)`, w Rust `theme.…`, w protokole
   enum. Wyjątek: palety domenowe (bramki kwantowe, grupy GPU) zdefiniowane jako osobny
   token w sekcji `dataviz`/`voice`, nie jako hex w pliku strony.
2. **Jedna skala typograficzna.** 15 stylów `TextStyle` (tokens.json → `typography.scale`).
   Rozmiar body to 13 px; minimalny rozmiar w UI to 10 px (`overline`). Połówki pikseli
   są zabronione.
3. **Ikony tylko SVG stroke**, viewBox 24, `currentColor`. Zero emoji, zero icon
   fontów. Dziś istnieją **dwa** niezależne sprite'y (`index.html` `i-*`, 130 symboli,
   stroke 1.75 — chrome hosta; `www/img/icons.svg` `icon-*`, 143 symbole, stroke 1.5 —
   protokół addonów); docelowo jeden zestaw o stroke 1.75, plan w
   [foundations/iconography.md](foundations/iconography.md) i [MIGRATION.md](MIGRATION.md).
4. **Dark-first, light jako pełnoprawny motyw** — każdy nowy kolor musi mieć wartość w
   obu motywach w `tokens.json` (light ma status *draft* do czasu przeglądu wizualnego).
5. **Dostępność nie jest opcjonalna**: widoczny fokus (2 px accent, offset 2 px), kontrast
   AA dla tekstu, cele dotykowe ≥ 44 px, obsługa klawiatury i `prefers-reduced-motion`
   w każdym komponencie z animacją.
6. **Responsywność mobile-first** po breakpointach z tokena (640/768/1024/1280/1536/1920).
   Strony admin mogą wymagać ≥ 1024 px, ale muszą degradować się czytelnie, nie łamać.
7. **Komponent przed markupem.** Jeśli potrzebujesz kontrolki, użyj `tf-*`; jeśli jej nie
   ma — najpierw spec w `components/`, potem implementacja, potem użycie. Style per strona
   (`www/css/<strona>.css`) mogą tylko układać komponenty, nie redefiniować ich wyglądu.
8. **Każda lista ma stan pusty**, każda operacja sieciowa stan ładowania i błędu
   ([patterns/states.md](patterns/states.md)).
9. **Jeden kod na wszystkie platformy.** Strona nie wie, czy jest w przeglądarce, WebView,
   natywnym oknie czy na panelu ESP32-P4; różnice (gęstość, `scale_factor`, brak cieni)
   załatwia motyw platformy (`tokens.json → platform`).

## Jak dodać nową stronę

Pełna lista kroków: [patterns/new-page-checklist.md](patterns/new-page-checklist.md).
W skrócie:

1. Zacznij od **mockupu** w `mockups/<nazwa>-<YYYYMMDD>/` zbudowanego z tokenów
   (skopiuj `mockups/component-catalog` jako start).
2. Zidentyfikuj **wzorzec strony** (lista+szczegół, formularz, dashboard, czat, edytor,
   widok 3D) w [patterns/page-anatomy.md](patterns/page-anatomy.md) — nie wymyślaj układu.
3. Sprawdź, czy wszystkie kontrolki istnieją w [components/README.md](components/README.md);
   brakujące najpierw dostają spec.
4. Zaimplementuj w `www/js/modules/<nazwa>.js` na `<tf-screen>` z komponentami `tf-*`;
   CSS strony tylko układ. Klucze i18n we wszystkich 5 językach.
5. Przejdź listę: stan pusty/ładowanie/błąd, klawiatura, fokus, kontrast, breakpointy
   640/768/1024, `prefers-reduced-motion`, zero hex/px poza tokenami (grep gate).
6. Jeśli strona ma być dostępna natywnie — dodaj odpowiednik w `tentaflow-ui-native`
   ([../docs/TENTAENGINE_INTEGRATION_PLAN.md](../docs/TENTAENGINE_INTEGRATION_PLAN.md)).

## Jak dodać nowy komponent

1. Skopiuj [components/_TEMPLATE.md](components/_TEMPLATE.md) → `components/<nazwa>.md`,
   wypełnij anatomię, warianty, stany, tokeny, zachowanie, a11y, mapowanie
   (HTML `tf-*`, tag protokołu `0x….`, natywny `tenta_ui_widgets::…`).
2. Dodaj wiersz do indeksu w `components/README.md` z tierem i statusem per implementacja.
3. Implementacja HTML: `www/js/components/tf-<nazwa>.js` + style w `controls.css`
   (docelowo `css/components/tf-<nazwa>.css`), demo w `www/component-demo.html`, test
   `tf-<nazwa>.test.js`.
4. Jeśli komponent ma być dostępny addonom — tag w `tentaflow-sdk-spec` i wpis w
   `docs/ADDON_UI_COMPONENT_CATALOG_v1.md`.

## Governance

- Właścicielem systemu jest zespół UI; PR zmieniający `tokens.json` wymaga przeglądu
  jednej osoby spoza autora i aktualizacji obu implementacji (CSS + natywnej, gdy istnieje).
- Bramki CI (docelowe, [MIGRATION.md](MIGRATION.md)): brak hex poza `tokens.css`, brak
  emoji w UI, brak `outline: none` bez zamiennika, `lang` w root, `prefers-reduced-motion`
  w każdym pliku z `@keyframes`.
- Historia wersji: **2.0** (2026-09-14) — nowy katalog, tokeny z żywego CSS, słownik
  zgodny z sdk-spec, przygotowanie pod natywny silnik. **1.0** (2026-04-17) — pierwszy
  `DESIGN.md`, zarchiwizowany jako opis intencji.
