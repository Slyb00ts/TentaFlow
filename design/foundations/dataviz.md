# Wizualizacja danych

TentaFlow rysuje wszystkie wykresy jako własne, inline SVG (bez zewnętrznej
biblioteki: D3, Chart.js itp.) w `www/js/components/tf-*-chart.js`, plus
`tf-sparkline`/`tf-gauge` na `<canvas>`/SVG. Ten dokument opisuje palety,
anatomię osi/siatki, kontrakt danych każdego komponentu wykresu (zweryfikowany
w kodzie, 2026-09-14) i wymagania wydajności/dostępności. Kolor serii/nie ma
prawa być hexem wpisanym w stronę — zawsze paleta z `tokens.json` →
`color.dataviz` (wyjątek zasady 1 w [`../README.md`](../README.md) — palety
domenowe są osobną warstwą tokenów, nie literalnymi wartościami w kodzie
strony).

## Palety — `color.dataviz`

| Paleta | Wartości | Kiedy używać |
|---|---|---|
| `categorical` | `#22c55e, #60a5fa, #f59e0b, #a78bfa, #06b6d4, #f472b6` | Serie o stałej tożsamości (nazwane kategorie: GPU, agent, region). Kolejność jest **stała** — seria musi zachować ten sam kolor między stronami/przeładowaniami, nie jest przydzielana losowo. Źródło: `--gpu-group-0..5` w `style.css`. |
| `sequential_accent` | `#0b0e22 → #1e1b4b → #3730a3 → #4f46e5 → #6366f1 → #818cf8 → #a5b4fc → #c7d2fe` (8 stopni, ciemny→jasny) | Jedna zmienna o rosnącej wielkości (gęstość, obciążenie, natężenie) — heatmapa, macierz gęstości w trybie `heat`. |
| `diverging` | `#ef4444 (ujemne) → #f59e0b → #6a7196 (zero) → #60a5fa → #22c55e (dodatnie)` | Wartość ma znaczący środek/zero — odchylenie od baseline, część rzeczywista/urojona macierzy gęstości (`tf-density-plot`, `part="im"`), porównania (delta). |
| `heat` | `#0b0e22 → #312e81 → #6366f1 → #a78bfa → #f472b6 → #fde68a` (6 stopni) | Heatmapy o dużym rozstępie wartości, gdzie `sequential_accent` (jednolicie fioletowy) nie daje dość kontrastu na górnym końcu skali — `tf-heatmap` domyślnie. |

`tf-heatmap` (`colorScale` property) przełącza między `sequential`/inną nazwaną
skalą, ale sam komponent implementuje dziś **własne, zaszyte 5 kubełków**
(`_levelFor`: `>0.75/>0.5/>0.3/>0.15/else`, dopasowane do konkretnego mockupu
„m01 activity heatmap") zamiast interpolować ciągłą paletę z tokena — traktuj
to jako znany dług przy przepisywaniu komponentu na `color.dataviz.heat`.

## Anatomia wykresu

| Element | Token | Zweryfikowana wartość w CSS |
|---|---|---|
| Linia osi / ticki | `border.default` | `.tf-chart__axis-line, .tf-chart__axis-tick { stroke: var(--tf-border) }` — zgodne z `color.dataviz.grid_line` (`#1f2548`) |
| Siatka (gridlines) | `border.default`, przerywana | `.tf-chart__gridline { stroke: var(--tf-border); stroke-dasharray: 3 4 }` |
| Etykiety osi | **rozjazd z tokenem** — patrz niżej | `.tf-chart__axis-label { fill: var(--tf-text-3); font-size: 10px }` |
| Crosshair | `border.hover` | `.tf-chart__crosshair { stroke: var(--tf-border-hover) }` |
| Linia serii domyślna | `accent.primary` | `.tf-chart__series-line { stroke: var(--tf-accent-1) }`, tony nadpisują przez `--tone-*` |

**Rozjazd zweryfikowany 2026-09-14:** `tokens.json` → `color.dataviz.axis_text`
deklaruje `#a0a8c8`, czyli wartość `text.secondary`. Żywy CSS renderuje etykiety
osi w `--tf-text-3` (`#6a7196`, `text.muted`). To jest dokładnie ten sam typ
rozjazdu `controls.css` vs. `style.css`/tokeny, który `tokens.json` już
nazywa w swoich `meta.notes` — kanoniczna wartość docelowa to `text.secondary`
(czytelniejsza, `8.2:1` zamiast `3.9:1` kontrastu), CSS wymaga poprawki.

**Zero chart junk:** brak 3D, brak cieni na słupkach/liniach (elewacja jest
płaska w dataviz, spójnie z `platform.*.shadows: "flat"` na ESP32-P4), brak
gradientowych wypełnień poza jawnym `fill-opacity` przy `stacking`. Legenda i
tooltip są częścią kontraktu (`TfCartesianChart`), nie ad-hoc per strona.

## Komponenty wykresów

| Komponent | Tag protokołu | Kluczowe atrybuty/property (z JS) |
|---|---|---|
| **Liniowy** | `tf-line-chart` (`LineChart 0x0216`) | `series: [{id, name, tone, style: solid\|dashed\|dotted, showInLegend, points: [{x,y}]}]`, `xAxis`/`yAxis` (`scale: linear\|log\|time\|category`), `legend`, `tooltip`, `crosshair`, `narrow: {breakpoint: 560, maxPoints: 10}`, `zoom`, `brush`, `locale`. Jedyny z rodziny `TfCartesianChart`, który wspiera aktualizację przyrostową (`updateSeries`) bez pełnego re-renderu, gdy kształt serii się nie zmienia. |
| **Słupkowy** | `tf-bar-chart` (`BarChart 0x0217`, tryb `single` = `StackedBar 0x021A`) | `orientation: vertical\|horizontal`, `stacking: none\|stacked\|percent`, `maxBarWidth` (domyślnie 34px). Tryb `single`: `segments: [{id,label,value,tone}]`, `total`, `showPercentages`. |
| **Warstwowy** | `tf-area-chart` (`AreaChart 0x0218`) | Kontrakt jak `tf-line-chart` + `stacking: none\|stacked\|percent`, `opacity` (0..1, wypełnienie poligonu). |
| **Kołowy/donut** | `tf-pie-chart` (`PieChart 0x0219`) | `slices: [{id,label,value,tone}]`, `variant: pie\|donut`, `maxSegments` (nadmiar agregowany jako „Other"), `showLabels`/`showLegend`. Wycinki < 3% wartości nie dostają etykiety na wykresie (nieczytelna). |
| **Sparkline** | `tf-sparkline` (Specialized::Sparkline) | `points: number[]`, `variant: line\|bar\|area`, `smooth`, `lineWidth` (domyślnie 1.5), `color` (rola tonu: `primary/success/warning/danger/info/accent`), `height` (domyślnie **32px**, nie 20 — zweryfikowane w `tf-sparkline.js:14`), szerokość **min. 60px** (`Math.max(60, clientWidth)`). Canvas, nie SVG — skalowany do `devicePixelRatio`, żeby nie rozmazywał się na ekranach retina/telefonach. |
| **Gauge** | `tf-gauge` (radial gauge) | Atrybuty `value`, `min`, `max`, `variant: circular\|arc\|semi`, `size`, `display-value`; property `thresholds: [{value, tone, label}]`. Brak `value` → stan pusty (myślnik, `muted`); `value` nieskończone/NaN → stan błędu (`critical`, `aria-invalid`). |
| **Heatmapa** | `tf-heatmap` (Specialized::Heatmap, WeeklyScheduleGrid) | `values` (macierz), `rowLabels`/`colLabels`, `rows`/`cols`, `colorScale`, `showLegend`, `onCellClick` → event `cell-click`. 5 kubełków koloru zaszytych w komponencie (patrz wyżej). |
| **Macierz gęstości** | `tf-density-plot` | `matrix: {dim, rho, labels?}`, atrybuty `part: re\|im`, `mode: heat\|city`, `size`. Skala **diverging** — ujemne wpisy części urojonej rosną w dół, nie jako wartość bezwzględna (są realną cechą stanu kwantowego, nie szumem do ukrycia). |
| **Histogram strzałów** | `tf-shot-histogram` | `series: [{id,label,tone,counts,probabilities,shots}]`, atrybuty `max-bars` (domyślnie 16), `log`, `whiskers="off"`, `height`. Wąsy 95% Wilsona tylko na seriach z realnym `shots` (rozkład idealny bez próbkowania nie dostaje wąsów — błąd na dokładnej liczbie byłby kłamstwem o jej pochodzeniu). Metryki (TVD, wierność Hellingera) liczone na **pełnym** rozkładzie, nie na przyciętym oknie `max-bars`. |
| **Strumieniowy** | `tf-stream-chart` | Kontrakt jak `tf-line-chart`, plus `window` (sekundy widoczne, domyślnie **300**), metoda `push(x, values)` zamiast reassignu `series` — polilinie są reprojektowane w miejscu, warstwa serii przesuwa się transformem CSS o jedną próbkę zamiast pełnego re-renderu. Etykiety osi X względne (`-4m`/`-30s`/`0`). |
| **Sygnał/postęp** | — (patrz Gauge) | brak osobnego komponentu poza `tf-gauge`. |

## Zasada gauge'a

`tf-gauge` sam nie narzuca progów — `thresholds` to otwarta tablica
`{value, tone}` ustawiana przez wywołującego. **Konwencja projektowa** (nie
domyślne zachowanie zaszyte w komponencie, zweryfikowane: brak w kodzie
sztywnego `85`): ustawiaj `accent`/`primary` jako ton bazowy i dodawaj próg
`warning` od **85%** zakresu (`{value: max * 0.85, tone: 'warning'}`),
ewentualnie `critical` bliżej 95-100%. Ton wykresu = ton **ostatniego progu
osiągniętego** (`clamped >= th.value`), więc progi podawaj rosnąco.

## Formatowanie liczb i dat

Przez i18n (`www/js/i18n.js`) i `www/js/utils.js` (`Intl.NumberFormat`/
`Intl.DateTimeFormat` z lokalą bieżącego języka), nigdy ręczne `toFixed`/
konkatenacje:

| Helper (`utils.js`) | Przykład | Użycie |
|---|---|---|
| `fmtCompact(n, lang)` | `12,3 tys.` | ticki osi przy dużych wartościach (`≥ 1e4`) — używane przez `formatTick()` w `tf-line-chart.js` |
| `fmtExact(n, lang)` | `12 345` | domyślna wartość w tooltipie (`_formatTooltipValue`) |
| `fmtCurrency(n, currency, lang)` | `1 044,18 zł` | kwoty |
| `fmtPct(n, digits, lang)` | `0,08%` | udziały |
| `fmtMs`/`fmtDuration` | `4,1 h` / `40 s` | czasy trwania, profiler |

Osie czasowe (`xAxis.scale === 'time'`) formatują tick przez
`Intl.DateTimeFormat(locale, {month:'short', day:'numeric'})`
(`formatTick()` w `tf-line-chart.js:128-134`) — data w formacie natywnym dla
języka użytkownika, nie stały format ISO.

## Dostępność wykresów

Każdy `<svg>` wykresu ma `role="img"` + `aria-label` (`_ariaLabel()` per typ:
„Line chart", „Pie chart"/„Donut chart"). To jest **pojedynczy string**, nie
pełny opis danych — dziś **nie ma** zaimplementowanego fallbacku w postaci
tabeli danych ani rozszerzonego podsumowania tekstowego dla żadnego z
komponentów wykresu (zweryfikowane grepem: brak `<table>`/`role="table"` w
`tf-line-chart.js` i pokrewnych). Przy budowie strony z ważnym wykresem
(nie ozdobnym) traktuj to jako lukę do wypełnienia samodzielnie:

- Nadaj `aria-label` z konkretną treścią przez atrybut/property hosta, nie
  poleganie na domyślnym „Line chart".
- Dla danych, które użytkownik musi odczytać (nie tylko dostrzec trend),
  udostępnij równoległą tabelę (`tf-table` obok wykresu, wizualnie ukrytą lub
  w zakładce) — wykres nie zastępuje tabeli dla czytnika ekranu.
- Tooltipy wykresów wyłączają się automatycznie pod `(hover: none)`
  (`_hoverEnabled()` sprawdza `mediaMatches('(hover: none)')`) — na dotyku
  interakcja hover i tak nie działa; nie polegaj na tooltipie jako jedynym
  nośniku wartości.

## Wydajność wykresów real-time

- **Ogranicz liczbę punktów.** `tf-stream-chart` trzyma tylko punkty w oknie
  `window` (domyślnie 300 s) + jedną próbkę zapasu; starsze są odrzucane przy
  każdym `push()`. Nie karm wykresu nieograniczonym buforem.
- **Rysuj na żądanie, nie w pętli.** Re-render triggeruje się przez zmianę
  property (`series` setter) albo `ResizeObserver` (`_renderPlot(force=false)`
  pomija przebudowę, gdy zmierzony box się nie zmienił) — brak własnej pętli
  `requestAnimationFrame` poza pojedynczą animacją wejścia/przewinięcia.
- **Aktualizacja w miejscu, nie re-mount.** `tf-line-chart`/`tf-stream-chart`
  patchują atrybuty istniejących `<polyline>`/`<circle>` zamiast
  odtwarzać SVG od zera, gdy kształt serii (liczba/id/tone/styl) się nie
  zmienił — zachowuje stan hover i unika kosztu `_render()`.
- **Rzadka animacja, nie za każdą próbkę.** Przewinięcie okna strumieniowego
  jedną transformacją CSS (`translateX`, 260ms), nie interpolacją każdego
  punktu co klatkę; wyłączone pod `prefers-reduced-motion`.

## Jak natywny renderer narysuje wykresy (planowane)

W repozytorium TentaFlow nie ma dziś natywnej implementacji wykresów. Silnik
docelowy, TentaEngine, planuje (`docs/UI_TOOLKIT_SPEC.md` w repo TentaEngine,
crate'y `tenta-draw`/`tenta-ui` — na razie specyfikacja, nie kod) warstwę
rysowania 2D niezależną od HTML/SVG; wykresy mapowałyby się na te same
prymitywy co reszta UI natywnego:

- Linia/obszar serii → **polyline**/**wypełniony poligon** w draw-list IR
  (ta sama prymitywa co obramowania kart i ikony wektorowe planowane po
  atlasach pokrycia, patrz `foundations/iconography.md`).
- Gauge/arc → **łuk** (`stroke` po okręgu z `start`/`sweep`), identyczna
  matematyka co dziś w `tf-gauge.js` (`describeArc`), tylko emitowana do IR
  zamiast do `<path d="…">`.
- Kolor serii/tonu → te same tokeny `color.dataviz.*`/`tone.*.fg`, czytane
  przez motyw platformy, nie zaszyte per wykres.
- Siatka/osie → cienkie linie `border.default`, tekst osi przez ten sam
  renderer glifów co reszta UI (atlas pokrycia per `TextStyle`).

## Checklist dla autora strony

- [ ] Kolor serii z `color.dataviz.categorical` (kolejność stała), nie hex
      wpisany w CSS/JS strony.
- [ ] Wykres z istotnymi danymi ma tabelę-fallback (widoczną lub ukrytą) —
      `role="img"` + jeden `aria-label` to nie jest pełna alternatywa
      tekstowa.
- [ ] Liczby/daty formatowane przez `utils.js`/`Intl`, nie ręcznie.
- [ ] Wykres real-time ma jawne okno/limit punktów (`window`, `max-bars`),
      nie rośnie bez ograniczenia.
- [ ] Gauge ma próg `warning` (konwencja: od 85% zakresu), nie tylko jeden
      kolor niezależnie od wartości.
- [ ] Sparkline używa realnego domyślnego rozmiaru komponentu (min. 60px
      szerokości, 32px wysokości) — nie zakładaj 60×20 bez ustawienia
      `height` jawnie.
- [ ] Legenda/tooltip wyłączają się poprawnie pod `(hover: none)` — brak
      funkcjonalności dostępnej wyłącznie przez hover.
