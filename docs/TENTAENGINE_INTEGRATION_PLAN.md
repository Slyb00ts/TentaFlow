# Wpięcie TentaEngine (natywne UI) do TentaFlow — plan

Status: propozycja supervisora, 2026-09-14. Zakres: jak uruchomić nowy natywny
silnik UI (TentaEngine, repo `/Users/critix/repos/rust/TentaEngine`, warstwa
TentaUI opisana w `docs/UI_TOOLKIT_SPEC.md` i `ROADMAP_UI.md`) **obok** obecnego
dashboardu HTML tak, żeby stary działał bez zmian, a nowy dało się testować
ekran po ekranie za przełącznikiem, docelowo zastępując cały HTML.

Powiązane: [`../design/README.md`](../design/README.md) (system projektowy wspólny
dla obu implementacji), [`ADDON_UI_COMPONENT_CATALOG_v1.md`](ADDON_UI_COMPONENT_CATALOG_v1.md)
(deklaratywny protokół komponentów), [`SPATIAL_3D_PLAN.md`](SPATIAL_3D_PLAN.md)
(faza 3 = renderer wgpu, który tym planem dostaje gospodarza).

## 1. Stan wyjściowy (zweryfikowany w kodzie)

| Platforma | Dziś | Plik |
|---|---|---|
| przeglądarka | vanilla-JS SPA wbudowana w binarkę (`build.rs` → `wwwroot_embed.rs`), 78 modułów stron, 105 web-componentów `tf-*`, PWA + service worker | `tentaflow-core/src/api/dashboard/{server,static_files}.rs`, `www/js/{app,router}.js` |
| protokół | binarny WS `/ws/api` (CBOR, `tentaflow-protocol`), auth JWT w `Sec-WebSocket-Protocol: bearer.<jwt>`, subskrypcje push; ten sam kodek Rust skompilowany do WASM dla przeglądarki (`www/js/protocol/wasm_glue*`) | `ws_binary.rs`, `tentaflow-protocol-wasm` |
| desktop | jeden proces Rust: serwer core + tray + **okno egui** (`tentaflow-ui`, częściowa nawigacja) ; „Dashboard" w tray otwiera przeglądarkę systemową | `tentaflow-desktop/core/src/lib.rs` |
| mobile | WKWebView / `android.webkit.WebView` ładuje `https://127.0.0.1:8090`; `tentaflow-ui` usunięte z mobile jako martwy kod | `tentaflow-mobile/ios/.../ContentView.swift`, `MainActivity.kt` |
| addony | nowy protokół komponentów (u16 tag, CBOR, ~151 komponentów) renderowany przez `www/js/sdk-runtime/`; zawiera `0x0603 WGPUSurface` z sandboxem naga (bez readback) | `tentaflow-sdk-spec/src/protocol/ui/`, `www/js/sdk-runtime/` |
| 3D | three.js w `tf-robot-view`, voxel WASM (`tentaflow-voxel-wasm`); wszystkie plany 3D zakładają „wgpu skompilowane do WASM w przeglądarce" | `www/js/vendor/three*`, `www/js/voxel/` |
| ESP32-P4 | brak jakiegokolwiek klienta/protokołu w TentaFlow | — |
| TentaEngine | zero wzmianek w repo | — |

## 2. Decyzje

1. **Jeden most, nie trzy.** Powstaje crate `tentaflow-ui-native` (workspace
   TentaFlow), który zależy od `tenta-ui`/`tenta-ui-widgets` (TentaEngine, jako
   zależność ścieżkowa/git w `[workspace.dependencies]`) oraz `tentaflow-protocol`.
   Zawiera: motyw TentaFlow generowany z `design/tokens/tokens.json`, warstwę danych
   (klient `/ws/api` + subskrypcje → `Msg`), ekrany napisane raz w MVU. Kompiluje się
   do `wasm32-unknown-unknown` (faza A), natywnie na desktop (faza B), mobile i P4
   (faza C). Kod ekranów nie wie, gdzie działa.
2. **Ten sam protokół.** Natywny klient mówi `/ws/api` (CBOR, JWT w subprotokole)
   — żadnego drugiego kanału. W WASM token przekazuje shell JS przy montowaniu.
3. **Komponenty addonów też natywnie.** Drugi konsument `tentaflow-ui-native`:
   renderer drzewa `Component` (u16 tag) z `tentaflow-sdk-spec` na widgety
   `tenta-ui-widgets` — tak, by UI addonów wyglądało identycznie w HTML i natywnie.
   Sandbox `WGPUSurface` pozostaje dla addonów; TentaEngine jako gospodarz **nie**
   przechodzi przez ten sandbox (jest first-party).
4. **Stary system nietknięty.** Żadna zmiana w `tf-*`, `controls.css` ani routerze
   poza addytywną rejestracją ekranu podglądu za flagą.
5. **Migracja ekran po ekranie.** Każdy moduł strony ma stan: `html` → `html+native
   (preview)` → `native` (HTML usunięty). Rejestr w `design/components/README.md`
   (kontrolki) i w tabeli §6 tego dokumentu (strony).

## 3. Faza A — `/native` w przeglądarce i WebView (po TentaEngine U3 + U5)

Najmniejsze ryzyko, wszystkie platformy naraz, zgodne z dotychczasowym kierunkiem
„wgpu w WASM".

- `build.rs` w `tentaflow-core`: nowy krok obok `build_browser_wasm_bindings`
  kompilujący `tentaflow-ui-native` (`--target wasm32-unknown-unknown`,
  `wasm-bindgen --target web`) do `www/js/native/tentaflow_ui_native{.js,_bg.wasm}`;
  trafia do `wwwroot_embed.rs` automatycznie. Profil `release-wasm`.
- `www/js/modules/native-preview.js`: ekran zarejestrowany w routerze
  (`Router.register('native', …)`) **tylko gdy** `config.dashboard.native_preview =
  true` (nowe pole konfiguracji, domyślnie `false`); montuje `<canvas>` w treści
  `<tf-screen>`, inicjuje WASM, przekazuje JWT, język i18n, `devicePixelRatio`,
  motyw (dark/light) i identyfikator ekranu (`?screen=clusters`).
- Wejście: `tenta-host-web` obsługuje pointer/klawiaturę/IME na canvasie (U5-003);
  scroll strony wyłączony wewnątrz canvasu.
- Fallback: brak WebGPU → `tenta-host-web` w trybie CPU (`putImageData`), ten sam
  kod.
- Pozycja w sidebarze: grupa „Laboratorium" → „Natywny podgląd (beta)" z listą
  ekranów dostępnych natywnie.
- Bramka: 5 ekranów referencyjnych (klastry, ustawienia, czat, dashboard,
  użytkownicy) działa w Chrome/Safari/Firefox oraz w WebView iOS/Android na tych
  samych danych co HTML; porównanie wizualne z HTML w `mockups/native-parity/`
  (zrzuty obu obok siebie); test e2e Playwright otwierający `/native?screen=…`.

## 4. Faza B — okno natywne na desktopie (po TentaEngine U6)

- `tentaflow-desktop/core`: flaga `--ui native|egui|none` (domyślnie `egui` do czasu
  parytetu); `native` uruchamia `tenta-host-desktop` z `tentaflow-ui-native` na
  głównym wątku zamiast `run_gui`; tray bez zmian („Dashboard" nadal otwiera
  przeglądarkę).
- Dane: ten sam klient `/ws/api` łączący się do serwera w tym samym procesie
  (loopback) — jedna ścieżka kodu z fazą A; opcjonalnie później kanał in-process.
- Bramka: pełna nawigacja Tier 0 ekranów, IME, schowek, skalowanie DPI, 60 fps.
- Po parytecie: `tentaflow-ui` (egui) do usunięcia (czysto, bez wrappera).

## 5. Faza C — mobile natywnie i ESP32-P4 (po TentaEngine U8 / U4)

- Mobile: drugi kontroler widoku za flagą build (`MTKView`/`SurfaceView` przez
  `tenta-host-ios/-android`), FFI z `tentaflow-mobile/core` tylko do przekazania
  uchwytu powierzchni i cyklu życia; WebView pozostaje domyślne do parytetu.
- ESP32-P4 (Tab5 i in.) jako **klient TentaFlow**: firmware z `tentaflow-ui-native`
  (`no_std`, `alloc`) + minimalny klient protokołu (CBOR przez esp-hosted Wi-Fi w
  `tenta-drivers`); ekrany: dashboard/stat, statusy, czat, kamera. Wymaga w TentaFlow
  osobnego planu parowania urządzenia (poza tym dokumentem).

## 6. Rejestr stron

| Strona (moduł) | Archetyp | Stan | Plan |
|---|---|---|---|
| `clusters` / `cluster-detail` | lista + szczegół | html | A |
| `settings` | formularz | html | A |
| `chat` | czat/stream | html | A |
| `dashboard` | dashboard/stat | html | A |
| `users` | tabela | html | A |
| `robots` (3D, kamera) | spatial | html (three.js) | A+ (po U8; zastępuje three.js) |
| `tentaquant/*` | notebook + 3D | html | B |
| `code-studio`, `flows-builder` | edytor | html | C (Tier 2) |
| pozostałe 70 | wg archetypu | html | po parytecie Tier 1 |

## 7. Co trzeba zmienić w TentaFlow (lista plików)

| Plik | Zmiana | Faza |
|---|---|---|
| `Cargo.toml` (root) | nowy member `tentaflow-ui-native`; zależności `tenta-ui`, `tenta-ui-widgets`, `tenta-host-web`, `tenta-host-desktop` (path/git, wersja przypięta) | A |
| `tentaflow-ui-native/` | nowy crate: `theme_generated.rs` (z `design/tokens/tokens.json`), `data/` (klient WS → `Msg`), `screens/` (MVU), `catalog/` (Component → widgety), `wasm.rs`, `native.rs` | A |
| `scripts/gen-design-tokens.py` | generator `www/css/tokens.css` + `theme_generated.rs` z JSON (deterministyczny, zapis tylko przy zmianie) | A |
| `tentaflow-core/build.rs` | krok WASM dla `tentaflow-ui-native` | A |
| `tentaflow-core/www/js/modules/native-preview.js`, `app.js` (rejestracja za flagą), `i18n/*.json` (klucze) | ekran podglądu | A |
| `tentaflow-core/src/config.rs` (lub odpowiednik) | `dashboard.native_preview: bool` | A |
| `tests/e2e/native-preview.spec.*` | test otwarcia i renderu | A |
| `tentaflow-desktop/core/src/lib.rs` | `--ui` | B |
| `tentaflow-mobile/{ios,android}` + `core/ffi_surface.rs` | natywna powierzchnia za flagą | C |
| `docs/ADDON_UI_COMPONENT_CATALOG_v1.md` | adnotacja: renderer natywny jako drugi host | A |

## 8. Ryzyka i mitigacje

- **Protokół addonów w przebudowie** („zero backward compatibility"): renderer
  natywny katalogu buduje się dopiero po ustabilizowaniu `tentaflow-sdk-spec`
  (`emit_manifest` stabilny), nie równolegle.
- **WebGPU w WebView** (Firefox Linux, starsze WebKit): fallback CPU jest tym samym
  backendem co P4 — nie jest kodem jednorazowym.
- **Dwa motywy przez chwilę** (CSS vs natywny): oba generowane z jednego JSON;
  test porównujący nazwy enumów `tenta_ui` ↔ `tentaflow-sdk-spec` ↔ klucze JSON.
- **Rozjazd wizualny HTML vs natywny**: zrzuty par w `mockups/native-parity/`
  przy każdym ekranie; różnice zapisane w `design/components/<x>.md` → „Znane
  odstępstwa" i prostowane po stronie HTML wg `design/MIGRATION.md`.
- **Wielkość WASM**: profil `release-wasm`, `wasm-opt`, atlasy fontów ładowane
  leniwie; budżet startowy < 2 MB gz (pomiar w bramce A).
