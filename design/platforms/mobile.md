# Platforma: mobile (iOS/Android)

Status: canonical dla dzisiejszego WebView; sekcja o natywnej powierzchni jest
planem, nie stanem. Tokeny platformy: `tokens.json → platform.mobile_phone` —
`{ "scale_factor": "os", "density": "comfortable", "pointer": "coarse",
"min_touch_target": 44 }` oraz `platform.tablet` — `{ "scale_factor": "os",
"density": "default", "pointer": "coarse" }` (bez `min_touch_target` osobno —
dziedziczy globalne `control.touch_target_min = 44`).

## Dziś: WebView ładujący ten sam dashboard co desktop

Nie ma osobnego UI mobilnego — jest ten sam HTML/JS dashboard co
[web-desktop.md](web-desktop.md), ładowany przez natywny WebView wskazujący na
lokalny serwer HTTPS wbudowany w aplikację.

- **iOS**: `tentaflow-mobile/ios/TentaFlowAI/ContentView.swift`. Komentarz nagłówkowy
  pliku (linia 3): *„Glowny widok iOS — WKWebView ladujacy dashboard z lokalnego HTTPS
  serwera. Rust core (serwer + mesh + inference) dziala w tle."* `TentaFlowWebView`
  odpytuje `https://127.0.0.1:8090` aż serwer odpowie (`pollServer`, linia 172), potem
  woła `webView.load(request)` (linia 201). Obsługuje zaufanie do self-signed
  certyfikatu na localhost, ponowne użycie process-poolu WKWebView, automatyczne
  przyznanie uprawnień kamery/mikrofonu dla `getUserMedia` oraz odzyskiwanie po
  zabiciu procesu WebContent w tle.
- **Android**: `tentaflow-mobile/android/app/src/main/java/ai/tentaflow/mobile/MainActivity.kt`.
  Nagłówek (linia 3): *„Glowna aktywnosc Android — docelowo laduje dashboard w
  WebView (...) UI mobilne jest renderowane przez dashboard w WebView."* Stała
  `DASHBOARD_URL = "https://127.0.0.1:8090/"` (linia 313), ładowana przez
  `webView.loadUrl(DASHBOARD_URL)` (linia 262).
- Rdzeń Rust (`tentaflow-mobile/core`, crate `tentaflow_mobile`) jest statycznie/
  dynamicznie zlinkowany w proces aplikacji (iOS: `staticlib` wprost w projekcie
  Xcode; Android: JNI przez crate `jni`, opakowane w `NativeLib.kt`). Jego FFI to
  **wyłącznie** discovery/sensor/lifecycle (`ffi_discovery.rs`, `ffi_sensors.rs`,
  `platform.rs`, `lifecycle.rs`) — **nie ma dziś żadnego FFI do UI**.
- `tentaflow-ui` (egui/wgpu z web-desktop.md) jest na mobile **jawnie usunięty**:
  `tentaflow-mobile/core/Cargo.toml:18-21` — *„tentaflow-ui (egui/wgpu) usuniete z
  mobile — UI idzie przez webview (SwiftUI), egui app (run_gui) byl martwym kodem
  (nigdy nie wolany)."* To ważne dla planowania natywnej powierzchni niżej: TentaFlow
  już raz świadomie zrezygnował z natywnego renderowania na mobile dla rozmiaru
  binarki.

## Bezpieczne obszary i klawiatura ekranowa

- `visualViewport` i `env(safe-area-inset-*)` **są już używane** w części plików CSS
  (`style.css`, `controls.css`, `code-studio.css`, `meeting-live.css` — zweryfikowane
  grepem 2026-09-14) — to nie jest nowy wymóg, tylko wzorzec do rozszerzenia na każdy
  ekran, który dokuje coś do dołu (composer czatu, pasek akcji formularza).
- Klawiatura systemowa na iOS/Android zmienia rozmiar visual viewportu, nie
  `window.innerHeight` — elementy dokowane do dołu (composer, sticky CTA) muszą
  reagować na zdarzenie `visualViewport.resize`/`scroll`, inaczej zostają schowane pod
  klawiaturą. To ten sam pitfall, co dla przyszłej ścieżki WebGPU na Web (raport
  hostingowy §10) — jeden kod do napisania, dwa miejsca, gdzie się liczy.
- Bezpieczne obszary (notch, pasek gestów) idą przez `env(safe-area-inset-top/right/
  bottom/left)` na kontenerze `tf-screen`; sidebar/topbar nie mogą zakładać, że `0`
  ekranu to bezpieczny `0`.

## Wskaźnik: coarse, bez hover

- `pointer: "coarse"` → **żadnych stanów `:hover`** jako jedynego nośnika informacji
  (nic nie jest widoczne „tylko po najechaniu", bo nic tu nie najeżdża). Reguły
  `@media (hover: none)`/`(pointer: coarse)` istniejące dziś w `controls.css` (linie
  1705, 1970, 2215, 2615) i `style.css` (linia 186) są dokładnie tym mechanizmem —
  do skopiowania, nie wynajdywania od nowa, przy każdym nowym komponencie z hoverem.
- **Cele dotykowe minimum 44 px** (`control.touch_target_min`, `platform.mobile_phone.
  min_touch_target`) — to twardy próg z README (zasada 5), nie sugestia. Warianty
  komponentów `sm` (`control.height.sm = 28px`) nie mogą być jedynym wariantem
  dostępnym na tej platformie.
- **Long-press zamiast hover-tooltip.** Tam gdzie desktop pokazuje `tf-tooltip` na
  `:hover`, mobile pokazuje go po przytrzymaniu (gesture recognizer długiego
  naciśnięcia, nie osobny komponent) — patrz `components/tooltip.md` (jeśli istnieje;
  jeśli nie, dodać wariant `trigger="long-press"` w specyfikacji komponentu przy
  najbliższej rewizji).
- Toasty (`tf-toast`) i inne powiadomienia efemeryczne są **dokowane do dołu ekranu**,
  nie w rogu jak na desktopie — dolny obszar jest w zasięgu kciuka i nie koliduje z
  paskiem statusu/notchem u góry.

## Sidebar jako overlay, modale jako sheet

- Sidebar nawigacyjny, na desktopie stały pasek `layout.sidebar.expanded` (240px),
  na telefonie jest **overlayem** wysuwanym znad treści (pełna wysokość, scrim
  `color.themes.dark.bg.overlay`), nie kolumną współdzielącą miejsce z contentem —
  na szerokości telefonu nie ma miejsca na dwie kolumny.
- Modale wyśrodkowane (`tf-modal`, `elevation.elevated`) na telefonie zamieniają się
  w **sheet** wysuwany od dołu (pełna szerokość, górne rogi zaokrąglone
  `radius.lg`), z uchwytem do przeciągnięcia w dół jako gest zamknięcia — to wzorzec
  natywny obu platform (UIKit sheet / Material bottom sheet), którego dashboard HTML
  musi imitować przez CSS/JS, nie dostaje go za darmo z WebView.
- `tf-window` (dialog/window chrome z fokus-trapem, patrz `_TEMPLATE.md` wzorzec) na
  telefonie zawsze pełnoekranowy — nie ma tu miejsca na swobodnie pozycjonowane okno.

## Gęstość: comfortable na telefonie, default na tablecie

- `platform.mobile_phone.density = "comfortable"` → mnożnik `control.density.
  comfortable = 1.15`: więcej paddingu, większe odstępy między wierszami tabel/list,
  żeby cele dotykowe miały fizyczny oddech mimo małego ekranu.
- `platform.tablet.density = "default"` (1.0) — tablet ma dość miejsca, żeby nie
  potrzebować dodatkowego paddingu; różni się od telefonu głównie layoutem (patrz
  niżej), nie gęstością kontrolek.
- **Tablet od `sm` (768px) w górę pokazuje split view**: lista + szczegół obok siebie
  (dwie kolumny), zamiast nawigacji „lista → ekran szczegółu → wstecz" telefonu.
  Próg `sm` jest tu celowo niższy niż desktopowy próg `md` dla sidebaru — split view
  na tablecie ma mniej wymagań co do szerokości niż pełny shell z sidebarem.

## IME i kompozycja tekstu

WebView **nie ma** własnego pola tekstowego dla treści rysowanej w DOM-ie — to akurat
nie problem tutaj, bo pola formularzy TentaFlow to zwykłe elementy DOM (`<input>`,
`<textarea>`), więc IME (klawiatury chińska/japońska/koreańska z kompozycją) działa
przez natywny mechanizm WebView bez dodatkowego kodu. To założenie **przestaje być
prawdziwe**, gdy pole tekstowe jest rysowane na `<canvas>` (ścieżka TentaEngine
WASM/WebGPU) — wtedy obowiązuje kontrakt hosta z raportu toolkitowego (§5): host
pozycjonuje prawdziwy `<input>`/`contenteditable` nad logicznym karetem, przekazuje
zdarzenia `compositionstart/update/end` do widgetu, kompozycja (preedit) renderuje się
z podkreśleniem wg konwencji platformy i nie jest traktowana jako finalny tekst do
committed. Ten kontrakt musi być gotowy od pierwszego pola tekstowego rysowanego przez
TentaEngine — dopisanie go później jest bolesne (raport toolkitowy, §5).

## Co zmieni przyszła natywna powierzchnia

Raport integracyjny (`04-tentaflow-hosting-integration.md`, opcja D) opisuje zamianę
WebView na natywny widok TentaEngine: iOS `MTKView` zamiast `WKWebView` w
`ContentView.swift`, Android `SurfaceView` zamiast `android.webkit.WebView` w
`MainActivity.kt`, wybierane flagą builda lub przełącznikiem w aplikacji, z WebView
jako domyślnym fallbackiem.

**Koszt tej opcji jest wyższy niż na desktopie**: `tentaflow-mobile/core` nie ma dziś
żadnego FFI do UI (tylko discovery/sensors/lifecycle) — trzeba by dodać kanał do
przekazania natywnego uchwytu powierzchni i pompowania klatek/wejścia przez FFI, a
istniejący kod iOS ma nietrywialną logikę odzyskiwania po zabiciu procesu w tle
(`ContentView.swift:120-291`), którą natywna powierzchnia musiałaby odtworzyć.
Mobile jest też jedyną platformą, która już raz **usunęła** natywne renderowanie
(`tentaflow-ui`) jako martwy ciężar — ponowne wprowadzenie go wymaga realnego
uzasadnienia (rozmiar binarki, interakcja z MLX na iOS), nie tylko spójności z
desktopem. Traktować jako plan zależny od wyników opcji A/B na desktopie/web (raport
hostingowy, „Recommendation shape"), nie jako coś do robienia równolegle już teraz.

**Co zostaje bez zmian niezależnie od wyniku**: `platform.mobile_phone`/`platform.
tablet` jako źródło gęstości i progu dotykowego, sidebar-jako-overlay i modal-jako-sheet
jako wzorce interakcji (to wynika z rozmiaru ekranu, nie z technologii renderowania),
oraz kontrakt IME z sekcji wyżej — dokładnie ta sama specyfikacja, którą i tak trzeba
zaimplementować dla ścieżki `<canvas>` na web-desktop.
