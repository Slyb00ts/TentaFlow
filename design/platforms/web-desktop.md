# Platforma: web / desktop (przeglądarka)

Status: canonical. Jedyna platforma, na której TentaFlow ma dziś pełny, produkcyjny
interfejs. Token platformy: `tokens.json → platform.web_desktop` —
`{ "scale_factor": "devicePixelRatio", "density": "default", "pointer": "fine" }`.

## Co to jest

„Web desktop" to dashboard HTML/JS (`tentaflow-core/www`) otwarty w zwykłej karcie
przeglądarki — na komputerze albo w oknie aplikacji desktopowej. To **jeden i ten sam
kod** w obu przypadkach; różnicę robi wyłącznie to, czy okno przeglądarki ma paski
adresu, czy nie. Nie ma tu WebView (to mobile, patrz
[mobile.md](mobile.md)) i nie ma (jeszcze) natywnego okna TentaEngine — to również
osobna ścieżka, opisana niżej w sekcji „Co zmieni natywne okno".

## Jak jest serwowany

- Serwer: ręcznie pisany `hyper` 1.x w `tentaflow-core/src/api/dashboard/server.rs`
  (`DashboardServer::run`, `server.rs:212`; dyspozytor `handle_request`,
  `server.rs:992`) — bez crate'a routera.
- Pliki statyczne: `tentaflow-core/src/api/dashboard/static_files.rs`. Cały katalog
  `tentaflow-core/www/` jest **wkompilowany do binarki** w czasie builda przez
  `generate_wwwroot_embed` (`tentaflow-core/build.rs:956`, `include_bytes!` per plik) —
  produkcyjnie zero zależności od systemu plików. Zmienna `TENTAFLOW_WWW_DIR` włącza
  tryb dev, w którym pliki są czytane z dysku na żywo (`serve_from_disk`,
  `static_files.rs:77`).
- Fallback SPA: ścieżka bez rozpoznanego rozszerzenia trafia do `index.html`
  (`static_files.rs:145`); prawdziwe braki assetów (`.js`/`.css`) zwracają 404, nie HTML.
- **Brak bundlera.** `index.html` ma jeden punkt wejścia,
  `<script type="module" src="/js/app.js">`, i **41 osobnych**
  `<link rel="stylesheet">` (`index.html:16–56`, zweryfikowane 2026-09-14 —
  jeden tag na obszar funkcjonalny, zob. [MIGRATION.md](../MIGRATION.md) §c). Wszystko
  to natywne moduły ES ładowane wprost przez przeglądarkę; `www/package.json` służy
  tylko testom jednostkowym JS, nie buduje frontu.
- **PWA + service worker.** `www/manifest.webmanifest` deklaruje standalone-display PWA
  z własnym protokołem `web+tentaflowpair` (deep-linki parowania). `www/sw.js`
  precache'uje cały front wg wygenerowanej listy assetów, jawnie wyklucza `WS`/`WT`/
  `/api/` z cache'owania i samo-unieważnia się przy zmianie hasha builda
  (`ASSET_BUILD_HASH`, generowany w `build.rs:1049`) — użytkownik dostaje prompt
  „dostępna aktualizacja, przeładować?" zamiast cichego rozjazdu wersji front/backend.
- Transport danych: binarny WebSocket `/ws/api` (CBOR, `Envelope`/`MessageBody` z
  `tentaflow-protocol`) jest kanałem podstawowym; REST (`/api/...`) jest warstwą
  wygaszaną. Ma to znaczenie dla tej platformy tylko pośrednio — UI nie wie, którym
  kanałem dociera stan, ale on-demand redraw (patrz `foundations/motion.md`) zależy od
  push, nie pollingu.

## Wejście i interakcja

- `platform.web_desktop.pointer = "fine"` → **hover jest dozwolony i oczekiwany**:
  tooltipy na `:hover`, stany `hover` komponentów (`components/_TEMPLATE.md` tabela
  stanów), `@media (hover: none)` w CSS jest tu **nieaktywne** (te reguły istnieją dziś
  w `style.css`, `controls.css`, `meeting-live.css`, `code-studio.css` z myślą o dotyku —
  patrz [mobile.md](mobile.md)).
- **Klawiatura jako pierwszy obywatel.** Fokus widoczny (`control.focus_ring`: 2 px,
  offset 2 px, kolor `border.focus`), pełna nawigacja Tab/Shift+Tab, `tf-command-palette`
  (⌘K/Ctrl+K) jako skrót do nawigacji bez myszy, listy/drzewa z roving tabindex (114
  użyć `tabindex` w kodzie JS wg audytu inwentaryzacyjnego). To jedyna platforma, na
  której skróty klawiszowe są podstawowym, nie awaryjnym, sposobem pracy — admini
  TentaFlow spędzają tu godziny dziennie.
- Cele dotykowe (`control.touch_target_min = 44`) nie są tu twardym wymogiem — myszka
  operuje precyzyjnie — ale komponenty i tak ich nie zmniejszają poniżej `control.height.sm`
  (28 px), żeby jeden styl działał też w oknie touchscreenowego laptopa.

## Layout i breakpointy

- Gęstość: `default` (mnożnik `control.density.default = 1.0`) — bez dodatkowego
  paddingu density `comfortable`, jak na telefonie/panelu.
- Sidebar rozwinięty (`layout.sidebar.expanded = 240px`) domyślnie od `md` (1024px)
  w górę; poniżej `layout.sidebar.collapse_below` sidebar się zwija do 64px
  (`layout.sidebar.collapsed`). Strony administracyjne mogą wymagać `≥ md`, ale muszą
  degradować się czytelnie (README zasada 6), nie łamać layoutu.
- Breakpointy docelowe z tokena: `xs 640 / sm 768 / md 1024 / lg 1280 / xl 1536 / xxl 1920`
  (`layout.breakpoints_px`). **Stan rzeczywisty jest inny** — grep `@media` po
  `css/*.css` (2026-09-14) zwrócił **195 wystąpień `@media`** w **35 z 40 plików**
  i m.in. progi 640, 720, 767, 768, 900, 960, 980, 1000, 1020, 1023, 1024, 1100, 1180,
  1280 px używane w praktyce, przeważnie jako `max-width` (mobile-last), nie
  `min-width` (mobile-first) jak zakłada zasada 6 z `README.md`. To jest dług
  opisany w [MIGRATION.md](../MIGRATION.md) — nowe strony mają używać wyłącznie
  progów z `layout.breakpoints_px`, jako `min-width`.
- Treść: `layout.content.max_width = 1440px`, wariant wąski `layout.content.narrow = 800px`
  dla widoków tekstowych (np. `notes.js`).

## Resize okna i zmiana DPR

- `scale_factor` tej platformy to `devicePixelRatio` przeglądarki — **nie** jest stały,
  zmienia się w locie (przeciągnięcie okna między monitorem 1x i 2x, zoom
  przeglądarki `Ctrl+/-`). Zmiana DPR musi wywołać ponowny layout, nie tylko
  redraw — dokładnie tak samo jak w przyszłym backendzie WebGPU opisanym w raporcie
  hostingowym §10.
- Resize okna dotyczy w praktyce viewportu karty przeglądarki (dziś nie ma tu żadnego
  natywnego okna z paskiem tytułowym — patrz niżej). Reakcja to zwykłe media queries
  CSS; nie ma osobnego kanału „window resized" poza tym, co przeglądarka i tak
  wystawia przez CSS.
- Gdy TentaEngine zacznie renderować UI na `<canvas>` (kierunek opisany w
  `docs/SPATIAL_3D_PLAN.md` i `docs/UNIFIED_SLAM_ARCHITECTURE.md` — wgpu skompilowany
  do WASM w przeglądarce), resize canvasu **nie** wywołuje zwykłego `window.resize`;
  wymaga `ResizeObserver` na elemencie canvas plus nasłuchu na zmianę
  `devicePixelRatio` osobno (raport hostingowy §10) — to konkretny wymóg dla
  implementacji tej ścieżki, nie coś co dziś istnieje.

## Aplikacja desktopowa dziś: przeglądarka + tray, nie WebView

`tentaflow-desktop/{core,linux,macos,windows}` to **jedno binarium** (`core`) + cienkie
wrappery per-OS. `tentaflow_desktop_core::run()` (`lib.rs`):

1. startuje pełny `tentaflow-core` w tym samym procesie (`runtime::start_services`),
2. tworzy ikonę w tray (`tray::create_tray`, `tray.rs`) z pozycją „Dashboard",
3. **chyba że podano `--headless`**, otwiera natywne okno `eframe`/`egui`
   (`tentaflow-ui`, częściowa reimplementacja nawigacji dashboardu — `Screen` enum,
   nie wszystkie ekrany są zaimplementowane).

Kliknięcie „Dashboard" w tray **nie** otwiera okna WebView — woła
`open::that("http://127.0.0.1:<port>")` (`lib.rs:196-201`), czyli otwiera dokładnie ten
sam HTML dashboard w domyślnej przeglądarce systemowej. Tak więc na desktopie
współistnieją dziś dwa UI w jednym procesie: częściowe natywne okno `egui` (otwarte od
razu) i pełny dashboard HTML (otwierany na żądanie w przeglądarce) — obydwa mówią do
tego samego serwera w tym samym procesie.

## Co zmieni planowane natywne okno (`tentaflow-desktop --engine`)

Zgodnie z opcją A raportu integracyjnego (`04-tentaflow-hosting-integration.md` §5):
planowana flaga `--engine` (obok istniejących `--headless`/`--no-tray`) ma uruchamiać
natywną powierzchnię `wgpu` TentaEngine w tej samej pętli okna głównego wątku
**zamiast** `run_gui` (obecny `eframe`/`egui`), przy nietkniętej ścieżce tray →
przeglądarka jako fallback/legacy UI.

**Co się zmienia:**
- Okno przestaje być kartą przeglądarki — dostaje prawdziwą ramkę systemową, menu,
  kontrolę nad własną pętlą zdarzeń (`winit`).
- `scale_factor` przestaje pochodzić z `devicePixelRatio` DOM-u, a zaczyna z
  odpowiednika `winit`-owego (`PhysicalSize`/`LogicalSize`) — koncepcyjnie to samo
  pojęcie, inny mechanizm odczytu.
- Cienie/blur, którymi CSS dysponuje swobodnie, muszą przejść przez pipeline
  `elevation` tokena (`tokens.json → elevation`) renderowany w shaderze SDF (patrz
  raport toolkitowy §4.1) zamiast `box-shadow`.
- `egui`/`tentaflow-ui` i TentaEngine nie mogą działać jednocześnie na wątku głównym —
  wybór jednego wyklucza drugi per uruchomienie.

**Co zostaje bez zmian:**
- `platform.web_desktop` jako token gęstości/wskaźnika — natywne okno używa tych
  samych wartości `density: "default"`, `pointer: "fine"`, bo to wciąż mysz + klawiatura
  na dużym ekranie, nie panel dotykowy.
- Próg `md` jako granica „layout szeroki" i logika zwijania sidebaru.
- Ścieżka tray → przeglądarka jako pełnoprawny, nieusuwany fallback (README zasada 9:
  „jeden kod na wszystkie platformy" — strona nie wie, w jakim oknie się znalazła).
- Transport danych: zalecenie z raportu hostingowego (opcja E) to picie tego samego
  binarnego `/ws/api` (albo wprost przez `tentaflow-protocol` jako crate Rust, bez
  WASM) zamiast budowania drugiego kanału.

## Czego tu nie ma (i nie powinno być)

- Braku hover — to ograniczenie panelu ESP32-P4 i częściowo telefonu, nie tej
  platformy (patrz [esp32p4.md](esp32p4.md), [mobile.md](mobile.md)).
- Stałego `scale_factor` — to specyfika ESP32-P4, gdzie panel ma znany fizyczny
  rozmiar w czasie kompilacji; tu DPR jest zawsze dynamiczny.
- IME jako coś specyficznego dla tej platformy — kontrakt hosta na kompozycję IME
  (raport toolkitowy §5) dotyczy web/desktop i mobile jednakowo, bo obie ścieżki mogą
  mieć klawiatury CJK; różnica jest tylko w tym, kto pozycjonuje pole kompozycji
  (DOM `<input>` tu, `UITextInput`/`InputConnection` na mobile).
