# Platforma: ESP32-P4 (panel dotykowy, bare-metal)

Status: **cel, nie stan**. TentaFlow nie ma dziś żadnego klienta na tej platformie —
repozytorium-wide grep (case-insensitive) po `esp32-p4`, `esp32p4`, `esp32`, `tab5`
po każdym pliku `.rs`/`.md`/`.toml` (poza `target/`/`vendor/`) zwraca **zero
trafień**. `tentaflow-hardware` istnieje, ale obsługuje wyłącznie robota Unitree Go2
przez WebRTC — nic z tego nie dotyczy wyświetlacza. Ten dokument opisuje, jak ma
wyglądać UI TentaFlow, gdy natywny silnik TentaEngine (renderer CPU/`no_std`) zacznie
je faktycznie rysować na jednym z trzech paneli zdefiniowanych już w
`tokens.json → platform`.

## Trzy panele

| Klucz tokena | Rozdzielczość | Format | `scale_factor` | Gęstość |
|---|---|---|---|---|
| `platform.esp32p4_tab5` | 720×1280 | RGB565 | 1.25 | `comfortable` |
| `platform.esp32p4_jc8012` | 800×1280 | RGB565 | 1.25 | `comfortable` |
| `platform.esp32p4_jc4880` | 480×800 | RGB565 | 1.0 | `default` |

Wszystkie trzy: `pointer: "coarse"` (pojemnościowy dotyk, brak myszy/klawiatury w
domyślnej konfiguracji), `shadows: "flat"`. `esp32p4_tab5` dodatkowo deklaruje
`fonts: "Latin+Polish subset, 4 sizes × 2 weights"` — ten sam wymóg dotyczy w praktyce
wszystkich trzech, tylko Tab5 jest tam, gdzie go zapisano jako pierwszy.

`scale_factor` jest tu **stałą czasu kompilacji**, nie odczytem systemowym jak na
web/mobile — panel ma znany fizyczny rozmiar i DPI w momencie budowania firmware'u
dla konkretnej płytki, więc nie ma powodu do dynamicznego wykrywania.

## Twarde ograniczenia rysowania

Renderer CPU na tej platformie **nie ma GPU** — dwurdzeniowy RISC-V do 400 MHz, bez
rozszerzenia wektorowego RVV, z jednym sprzętowym blokiem 2D (PPA: blit/blend/scale/
rotate niezależny od CPU). Stąd wynikają ograniczenia projektowe, nie stylistyczne
preferencje:

- **Brak cieni i blura.** `elevation.*` na tej platformie spłaszcza się do 1 px
  obramowania `border.default` (dokładnie to mówi komentarz w `tokens.json →
  elevation._comment`: „On the CPU/ESP32-P4 renderer shadows are flattened to a 1px
  border of border.default — no live blur"). Miękki cień do rekonstrukcji per klatka
  to koszt ALU, na który nie ma budżetu; jeśli kiedyś potrzebny efekt głębi, jedyna
  dopuszczalna droga to prerenderowany bitmap 9-slice, nie blur na żywo.
  Uwaga: „on-demand redraw" (klatka rysowana tylko gdy coś faktycznie się zmienia) to
  wymóg budżetu klatki opisany niżej — nie jest sam w sobie ograniczeniem estetyki.
  **Jedyny wyjątek od „brak blura" to materiał szkła** w trybie `lite`
  ([foundations/materials.md](../foundations/materials.md) §5): rozmycie liczone na
  ×4 zmniejszonej migawce tła (sumy prefiksowe), soczewkowanie z LUT, wynik cache'owany
  do zmiany tła — z twardym limitem pola `platform.esp32p4_*.glass_max_area_px`
  (wstępnie Tab5/JC8012 180 000 px ≈ pasek 720×96 + kilka przycisków; JC4880
  60 000 px, bez dyspersji — do korekty po pomiarze na płytce). Powyżej limitu szkło
  degraduje się do `Flat` (półprzezroczysty prostokąt + obwódka). Szkło nad
  przewijaną listą zostaje pełne (decyzja właściciela), ale taki scroll to klatka
  ~30 fps, nie 60. Pełnoekranowe szkło jest tu dopuszczalne wyłącznie statycznie.
  Odbłysk reaguje na przechylenie z IMU tylko na Tab5 (BMI270); pozostałe płytki
  mają stałe światło.
- **Brak stanów hover.** `pointer: "coarse"` bez wyjątków — nie ma najeżdżania nad
  panelem dotykowym. Wszystko, co desktop pokazuje na `:hover`, tu musi mieć
  odpowiednik dotykowy (tap, long-press) albo być zawsze widoczne.
- **Tekst rysowany na siatce pełnych pikseli.** Bez subpikselowego antyaliasingu (RGB565
  nie opłaca ALU potrzebnego do subpikselowej korekcji) — zaokrąglać *advance* każdego
  glifu do całych pikseli urządzenia, inaczej między sąsiednimi glifami pojawiają się
  1-pikselowe szwy.
- **Brak generalnego shapingu tekstu.** Dla łaciny + polskich znaków diakrytycznych
  (`ąćęłńóśźż`/wielkie odpowiedniki — to prekomponowane kodpunkty NFC, nie sekwencje
  łączące) shaping sprowadza się do tabeli advance'ów + statycznego kerningu,
  generowanej offline (build-time) z fontu, nie liczonej na urządzeniu. Bez hintingu,
  bez BiDi (łacina jest zawsze LTR).

## Typografia: 4 rozmiary × 2 wagi, reszta się zwija

Zestaw znaków ograniczony do ASCII drukowalnego + polskich diakrytyków + typowej
interpunkcji/waluty (~130 glifów na rozmiar/wagę). To wymusza **redukcję 15-stylowej
skali `typography.scale`** (`tokens.json`) do czterech logicznych rozmiarów × 2 wag
(regular/bold — z rodziny `weights: [400, 500, 600, 700, 800]` w `typography.family.
sans` realistycznie da się utrzymać w atlasie tylko dwie wagi, nie pięć):

| Logiczny rozmiar (px) | `TextStyle` który tu ląduje | Style, które się zwijają na ten sam bitmap |
|---|---|---|
| 20 | `h1` | `title` (24px) i `display` (32px) **nie mieszczą się** w 4-rozmiarowym zestawie — jeśli potrzebny nagłówek większy niż 20px, to osobny, piąty rozmiar poza budżetem podstawowym, do rozważenia tylko dla Tab5/JC8012 (większe panele) |
| 16 | `h2` | — |
| 14 | `h3`, `body_lg` | oba mają różne wagi/line-height na webie (600 vs 400) — na P4 to dwa renderowania tego samego rozmiaru bitmapowego z inną wagą, nie dwa różne rozmiary |
| 13 | `h4`, `body`, `body_strong`, `quote` | `body_strong` = waga bold tego samego rozmiaru; `quote` (kursywa) **nie ma odpowiednika** — kursywa wymaga osobnego fontu/oblique-transform, poza zakresem „4×2"; traktować jak `body` bez pochylenia na tej platformie |

Style **poniżej** 13px z tokena (`caption` 11, `caption_strong` 11, `overline` 10) oraz
`code`/`mono` (12, rodzina `mono`) **nie wchodzą** do domyślnego zestawu P4 — panel
dotykowy oglądany z odległości ręki nie potrzebuje 10-11px tekstu, a druga rodzina
fontów (JetBrains Mono) to drugi komplet atlasów, którego budżet pamięci (patrz niżej)
nie zakłada. Jeśli ekran terminala/kodu kiedyś trafi na P4 — nie powinien, patrz sekcja
architektur stron niżej — to osobna, świadoma decyzja o rozszerzeniu budżetu.

Budżet atlasu glifów dla 4 rozmiarów × 2 wagi, łacina+polski: **≈0.5 MB** spakowane
(liczone jako ~130 glifów/rozmiar/wagę, bitmapa coverage-alpha 1 bajt/px, shelf-packing
z ~20-30% marginesu). Poza zestawem (CJK, emoji, cokolwiek nietypowego) → glif
zastępczy `.notdef` (tofu box), nie próba doładowania fontu na urządzeniu.

## Budżet pamięci

| Element | Rozmiar | Uwaga |
|---|---|---|
| Framebuffer (pojedynczy) | 720×1280×2B = **1,843,200 B ≈ 1,84 MB** | RGB565, Tab5 |
| Framebuffer (podwójny bufor) | **≈ 3,69 MB** | standard dla płynnych częściowych odświeżeń |
| Atlas glifów (4×2, łacina+polski) | **≈ 0,5 MB** | rośnie, jeśli zestaw znaków/rozmiarów się rozszerzy |
| Atlas ikon/bitmap aplikacji | 0,5–2 MB | budżet, nie twardy limit — 32 MB PSRAM ma dużo zapasu |
| Drzewo widgetów retained (~1000 węzłów) | ~50–96 KB | ~48-96 B/węzeł: indeksy rodzic/dziecko + uchwyt stylu + `Rect`/`Size` + flagi dirty |
| Draw-list per klatka (transient) | 10–128 KB | budowany na nowo przy każdej „brudnej" klatce, nie trzymany między klatkami |
| **Razem, realistyczny worst case** | **~4,5–7 MB** | dobrze poniżej 32 MB PSRAM — **pamięć nie jest tu wąskim gardłem** |

Wąskim gardłem jest **czas CPU na piksel**, nie pamięć — patrz budżet klatki niżej.

## Maksymalnie ~1000 węzłów na ekran

Drzewo retained (parent/first-child/next-sibling indeksowane, bez `Box<dyn Widget>` i
bez haszmapy w stylu `egui::Memory`) skaluje się liniowo z liczbą węzłów, ale każdy
węzeł to też potencjalny cel layoutu i dirty-trackingu. ~1000 węzłów to praktyczny
sufit dla pojedynczego ekranu — dashboard z kilkunastoma kartami statystyk, tabela z
rozsądną paginacją (nie renderuj 500 wierszy naraz — paginacja/wirtualizacja jest tu
obowiązkowa, nie opcjonalna optymalizacja) mieszczą się z zapasem; edytor kodu czy
graf przepływu z setkami swobodnie rozmieszczonych węzłów — nie.

## Budżet klatki: 30 fps dla przejść, redraw na żądanie poza tym

Matematyka z raportu referencyjnego (§9 tamtego dokumentu): jeden pełnoekranowy,
nieprzezroczysty blend na 720×1280 (Tab5) kosztuje rzędu **4,6–9,2 ms** przy 400 MHz
skalarnie — czyli jedna trzecia do ponad połowy budżetu **30 fps** (33,3 ms), zanim
policzy się layout, tekst czy transfer do panelu DSI. Wnioski wprost stąd:

- **60 fps pełnoekranowego odświeżania każdej klatki nie jest tu osiągalne** dla
  ekranów o realistycznej złożoności.
- **30 fps to bezpieczny cel wyłącznie dla przejść** (np. slajd między ekranami), i
  najlepiej wspomaganych sprzętowo przez PPA (`ppa_do_scale_rotate_mirror`/
  `ppa_do_blend`), żeby zwolnić rdzeń skalarny z pchania pikseli.
- **Dirty-rectangle redraw jest obowiązkowy od pierwszego milestone'u**, nie
  optymalizacją „na później": odświeżenie migającego karetu 40×20px kosztuje
  mikrosekundy; odświeżenie całego ekranu — milisekundy. Bez tego nawet migający
  kursor tekstowy zjada nieproporcjonalnie dużo budżetu.
- **Redraw na żądanie (on-demand), nie ciągła pętla vsync.** Klatka jest rysowana,
  gdy: (a) przyszło wejście dotykowe, (b) jakiś driver animacji (sprężyna/tween)
  jeszcze nie zbiegł do celu, (c) aplikacja jawnie zażądała odświeżenia (np. nowe dane
  z WS). W spoczynku panel nie rysuje nic — to jednocześnie jedyny sposób zmieścić się
  w budżecie *i* nie podgrzewać/rozładowywać urządzenia bez powodu.
- Sprężyny (`motion.spring.*` z tokena) muszą mieć epsilon zbieżności (np.
  |wartość − cel| < 0,5px), inaczej nigdy formalnie się nie kończą i on-demand redraw
  przestaje działać.

## Nawigacja: tylko dotyk

Bez myszy/klawiatury w domyślnej konfiguracji → **dolny pasek nawigacji** (3-5 dużych
ikon, cele ≥ `control.touch_target_min = 48px` fizycznie — na panelu dotykowym warto
celować wyżej niż webowe minimum 44px, bo nie ma kursora korygującego niedokładność
palca) albo **duży sidebar ikon** na panelach szerszych (JC8012 800px). Bez hover-menu,
bez tooltipów jako jedynego źródła etykiety — ikony w pasku nawigacji zawsze mają
widoczną etykietę tekstową albo są na tyle rozpoznawalne, że nie potrzebują opisu.
Gesty ograniczone do zestawu, który sprzęt faktycznie obsługuje: tap, long-press, drag,
scroll — **bez pinch/multi-touch** (typowe kontrolery pojemnościowe na tej klasie
płytek to single-touch).

## Jakie strony mają tu sens

**Tak — dobrze pasują do panelu dotykowego, budżetu węzłów i braku klawiatury:**
- Dashboard/statystyki: karty stanu, gauge, sparkline (patrz `dashboard.js` jako wzorzec
  po stronie HTML — ten sam kształt informacyjny, inny renderer).
- Listy stanu: klastry, węzły mesh, urządzenia — lista + drill-down do szczegółu,
  bez edycji inline złożonych struktur.
- Proste formularze: kilka pól, wybór z listy, przełączniki — nie formularze
  wielosekcyjne z walidacją krzyżową między dziesiątkami pól.
- Czat: `tf-chat-bubble`/`tf-chat-composer` mają naturalny odpowiednik dotykowy;
  klawiatura ekranowa (jeśli panel ją w ogóle ma) ogranicza się do prostego committed
  text — bez IME, bo zestaw znaków to łacina+polski (patrz sekcja typografii).
- Podgląd kamery/wideo: `tf-live-camera-tile`/`tf-video-stream` jako pełnoekranowy
  widok z minimalnym chrome wokół — dokładnie profil obciążenia, który panel udźwignie
  (jeden duży blit, nie setki małych widgetów).

**Nie — złożoność interakcji albo gęstość węzłów przekracza budżet:**
- Edytor kodu (`tf-code-editor`, `tf-terminal`) — wymaga klawiatury fizycznej, drugiej
  rodziny fontów (mono) poza budżetem atlasu, i tekstu w rozmiarach poniżej progu 13px
  tej platformy.
- Edytor grafu przepływu (`flows-builder.js`, `tf-relation-graph`) — setki swobodnie
  pozycjonowanych węzłów + gesty pinch/zoom precyzyjne, żadne z tych dwóch nie mieści
  się w limicie ~1000 węzłów ani w zestawie gestów single-touch.
- Cokolwiek wymagające jednoczesnego trzymania wielu paneli/kolumn (project-studio,
  ml-studio) — brak miejsca na ekranie 480–800px szerokości i brak modelu interakcji
  dla tak gęstego layoutu bez myszy.

## Offline i local-first

Panel nie ma dziś (i architektonicznie nie powinien zakładać) stałego dostępu do
internetu zewnętrznego — łączy się z lokalnym `tentaflow-core` w tej samej sieci LAN,
tym samym binarnym protokołem `/ws/api`, który już dziś obsługuje resume tokenów HMAC
(`ws_binary.rs`) na wypadek przerwy w łączności. UI musi zakładać stan „brak
połączenia" jako normalny, nie wyjątkowy (patrz `patterns/states.md` — stan offline
jest jednym z czterech wymaganych stanów każdej operacji sieciowej, README zasada 8),
i pokazywać ostatni znany stan z lokalnego bufora zamiast pustego ekranu przy zaniku
Wi-Fi. Brak też założenia o service workerze/PWA (to mechanizm przeglądarkowy, nie
`no_std`) — trwałość między restartami to zadanie firmware'u, nie warstwy UI.
