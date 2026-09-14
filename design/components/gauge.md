# Gauge

| | |
|---|---|
| Tier | 0 (MVP) |
| HTML | `<tf-gauge>` (SVG-arc, zgodny z protokołem) — `tentaflow-core/www/js/components/tf-gauge.js`, style `controls.css:7582-7610`. **Równoległa, niezwiązana implementacja** ad-hoc: `.gauge` / `.gauge-ring` (CSS `conic-gradient`, klasy `hot`/`warm`/`dim`) — `tentaflow-core/www/css/style.css:918-974`, budowana ręcznie w `js/modules/clusters.js:316-329` (funkcja `renderRing`). Zobacz „Znane odstępstwa” — to dwa różne komponenty o tej samej nazwie funkcjonalnej. |
| Protokół addonów | `0x021C` `Gauge` — `docs/ADDON_UI_COMPONENT_CATALOG_v1.md` §1840-1854 |
| Natywny (TentaEngine) | `tenta_ui_widgets::Gauge` — status: planowany |
| Status dokumentu | draft |

Okrągły/łukowy wskaźnik wartości w zakresie `min..max`, z opcjonalnymi progami zmieniającymi kolor łuku (`thresholds`). Tylko do odczytu — nie jest kontrolką wejściową (do tego służy `tf-slider`). Ten dokument opisuje **`tf-gauge`** (implementację zgodną z protokołem addonów) jako kanoniczną; osobno dokumentuje odstępstwo, jakim jest równoległy wzorzec `.gauge-ring` używany dziś na dashboardzie i liście klastrów.

## Anatomia

`tf-gauge` (SVG, łuk kreślony `<path>`, nie CSS `conic-gradient`):

```text
      ╭───────╮
    ╱           ╲
   │      72     │   ← .tf-gauge__value-text (18px, --tf-text, tabular-nums)
   │     RAM      │   ← .tf-gauge__label (11px, --tf-text-3), opcjonalny
    ╲           ╱
      ╰───────╯
  track: --tf-border, pełny łuk (zależnie od variant)
  value-arc: kolor = tone najwyższego przekroczonego progu
  threshold ticks: krótkie kreski na obwodzie w miejscu każdego progu
```

Domyślny rozmiar to `160×160px` (atrybut `size`, w pikselach) — **nie 64×64**; `size` jest w pełni swobodny, patrz odstępstwa.

Wzorzec ad-hoc `.gauge-ring` (używany dziś w dashboardzie/klastrach, poza `tf-gauge`):

```text
┌─────────────┐
│   ╭───╮     │  ← .gauge-ring: 76px zewn. / 64px wewn. dysk, conic-gradient(tone pct%, bg-2 0)
│   │242│GB   │  ← .gauge-val: wartość + jednostka
│   ╰───╯     │
│    RAM      │  ← .gauge-label (uppercase, 10px)
│  / 512 GB   │  ← .gauge-sub (11px)
└─────────────┘
```

## Warianty

| `variant` (`tf-gauge`) | Rozpiętość łuku | Kiedy używać |
|---|---|---|
| `circular` (domyślny) | 360° (`2π`), start u góry (`-90°`) | pojedyncza metryka bez kontekstu kierunkowego |
| `arc` | 270° (`1.5π`), start w prawym-dolnym rogu | gdy potrzeba więcej miejsca na etykietę pod łukiem |
| `semi` | 180° (`π`), start po lewej | panel z rzędem kilku półokręgów obok siebie |

Tony łuku wartości (`--tf-gauge__value-arc--tone-*`): `neutral` / `primary` / `info` / `success` / `warning` / `critical` / `muted` — pełny zestaw `Tone`, wybierany automatycznie jako najwyższy próg, którego wartość ≤ bieżąca wartość gauge'a (nie ma wbudowanej reguły „>85% = critical” — to caller ustawia `thresholds`, patrz Zachowanie).

## Rozmiary

`tf-gauge` nie ma skali `sm/md/lg` — rozmiar to atrybut `size` w pikselach (domyślnie 160), `stroke-width` skaluje się proporcjonalnie (`size * 0.08`). Wzorzec ad-hoc `.gauge-ring` ma jeden stały rozmiar: `width: min(76px, 100%)` z wewnętrznym dyskiem 64px (`::before { inset: 6px }` na 76px ringu).

## Stany

| Stan | Wygląd | Uwagi |
|---|---|---|
| wartość obecna (`value` ustawione, skończone) | łuk wypełniony proporcjonalnie do `(value-min)/(max-min)`, kolor = ton najwyższego przekroczonego progu | domyślny stan |
| pusty (`value` nieustawione) | łuk zerowy, tekst „—”, ton `muted`, `aria-label="— (min-max)"` | brak danych, nie błąd |
| nieprawidłowy (`value` ustawione, ale `NaN`/`Infinity`) | łuk zerowy, tekst „—”, ton `critical`, `aria-invalid="true"` | odróżnione od stanu pustego — patrz Dostępność |
| próg przekroczony | ton łuku zmienia się skokowo na kolor progu, kreska progu na obwodzie pozostaje widoczna niezależnie od tonu łuku | |
| `.gauge-ring.warm` (ad-hoc) | `conic-gradient(--warning …)` | pct > 60% w `clusters.js` |
| `.gauge-ring.hot` (ad-hoc) | `conic-gradient(--danger …)` | pct > 85% — kolor **critical/danger**, nie „warning” (patrz odstępstwa) |
| `.gauge-ring.dim` (ad-hoc) | tło płaskie `--bg-2`, brak łuku | brak danych (`pct == null`) |

## Zachowanie

- `tf-gauge` jest czysto prezentacyjny — brak interakcji wskaźnikiem/klawiaturą, brak zdarzeń.
- Progi (`thresholds`, właściwość JS — tablica `{value, tone, label?}`) są sortowane logicznie przez porównanie `clamped >= th.value` w kolejności podania; **caller odpowiada za podanie ich w rosnącym porządku** — komponent nie sortuje ani nie waliduje kolejności.
- Wartość jest przycinana do `[min, max]` (`Math.max(min, Math.min(max, num))`) przed wyliczeniem kąta — wartość poza zakresem nie przepełnia łuku, po prostu zatrzymuje się na 0%/100%.
- `display-value` (atrybut) nadpisuje tekst liczbowy w środku (np. do pokazania jednostki/formatu) bez wpływu na wyliczenie kąta, które zawsze bazuje na `value`.
- Animacja: `.tf-gauge__value-arc { transition: d 0.2s ease }` — zmiana wartości animuje przejście kształtu łuku (SVG `path d`), pokryta globalną regułą `prefers-reduced-motion` w `controls.css:9586`.
- `.gauge-ring` (ad-hoc): `transition: background 0.3s ease` na zmianę `--pct` (custom property czytana przez `conic-gradient(calc(var(--pct) * 1%))`) — **bez pokrycia `prefers-reduced-motion`**, bo `style.css` nie ma globalnej reguły wildcard (patrz odstępstwa).

## Dostępność

- `tf-gauge` renderuje `<svg role="img">` z `aria-label` zawierającym wartość i zakres (np. `"72 (0-100)"`) — czytnik ekranu dostaje kontekst liczbowy bez konieczności parsowania SVG.
- Stan nieprawidłowy dodaje `aria-invalid="true"` na `<svg>` — odróżnia „błąd danych” od „brak danych” (który zostaje bez `aria-invalid`), co pozwala technologii wspomagającej zasygnalizować różne poziomy istotności.
- Kreski progów mają `<title>` + `aria-label` gdy próg niesie `label` — opisowe etykiety progów są dostępne, nie tylko wizualne.
- `.gauge-ring` (ad-hoc) nie ma żadnego odpowiednika `aria-label`/`role="img"` — wartość liczbowa jest w zwykłym tekście (`.gauge-val`), więc czytnik ekranu odczyta ją poprawnie, ale bez kontekstu zakresu `min-max` ani jawnej roli.
- `prefers-reduced-motion`: `tf-gauge` pokryty globalnie; `.gauge-ring` **nie jest** — patrz odstępstwa.

## Responsywność i platformy

- `tf-gauge` skaluje się liniowo z atrybutem `size` — host decyduje o rozmiarze per breakpoint, komponent nie ma wbudowanej responsywności.
- `.gauge-ring` ma responsywność wbudowaną w CSS: `width: min(76px, 100%)` kurczy pierścień w wąskiej komórce siatki zamiast go przycinać — przydatne w gridzie 4 kolumn (`clusters.js`), ale to zachowanie nie istnieje w `tf-gauge`.
- Na ESP32-P4 (`shadows: flat`) żaden z dwóch wariantów nie używa cienia, więc nie wymaga specjalnej obsługi platformy — SVG-owy `tf-gauge` renderuje się identycznie; `conic-gradient` może wymagać zamiennika w renderze CPU/natywnym, jeśli silnik nie wspiera gradientów stożkowych (do zweryfikowania przy implementacji natywnej).

## Tokeny użyte

- `color.themes.dark.border.default` (`--tf-border`) — `.tf-gauge__track`.
- `color.themes.dark.text.primary`/`secondary` — tekst wartości/etykiety.
- `color.themes.dark.semantic.success/warning/critical/info.value` — tony łuku i kresek progu.
- `color.themes.dark.accent.primary` — ton `primary` (domyślny przy braku przekroczonego progu).
- `typography.scale.h3`-zbliżony (18px) — `.tf-gauge__value-text` (bez dokładnego dopasowania do kroku skali).
- `typography.scale.caption` (11px) — `.tf-gauge__label` (zgodne rozmiarem).
- `motion.duration_ms.normal` (200) — `transition: d 0.2s`.

## Znane odstępstwa w kodzie (2026-09-14)

1. **Dwie niezależne implementacje „gauge’a” bez wspólnego kodu.** `tf-gauge` (SVG, zgodny z protokołem `Gauge` 0x021C co do pól `value`/`min`/`max`/`thresholds`/`variant`/`label`/`size_px`) współistnieje z ręcznie budowanym `.gauge-ring` (`clusters.js:316-329`, CSS `style.css:918-974`) używanym na dashboardzie i liście klastrów. Różne API (atrybuty vs. `--pct` custom property), różne rozmiary domyślne (160px vs 76px), różny model tonów (dowolny próg vs. stałe `hot`/`warm`/`dim` progi 85%/60%).
2. **Domyślny rozmiar `tf-gauge` to 160×160px, nie 64×64.** 64px odpowiada wewnętrznemu dyskowi `.gauge-ring` (ring zewnętrzny 76px) w drugiej implementacji — kto szuka „gauge 64×64” w kodzie, znajdzie go w `style.css`, nie w `tf-gauge.js`.
3. **„Hot” powyżej 85% to `--danger` (critical), nie `--warning`.** `renderRing()` (`clusters.js:318`) mapuje `pct > 85 → class 'hot' → conic-gradient(var(--danger))`; `warning`/`--warning` jest zarezerwowane dla progu pośredniego `60 < pct <= 85` (klasa `warm`). Jeśli projekt zakłada „>85% = warning”, to nie zgadza się z żywym kodem, który traktuje >85% jako stan krytyczny (czerwony), a `warning` jako stan pośredni.
4. **`.gauge-ring` nie jest objęty żadną regułą `prefers-reduced-motion`.** `style.css` ma tylko dwie klauzule reduced-motion, obie zawężone do selektorów hero/mascot (`style.css:616-619`, `:4684`) — nie ma globalnej reguły wildcard jak w `controls.css:9586`. Przejście `background 0.3s ease` na `.gauge-ring` (`style.css:943`) ignoruje więc preferencję systemową użytkownika.
5. **`tf-gauge` nie ma pola „sub”** (drugiej linii tekstu pod wartością, np. „/ 512 GB”) — tylko `label`. `.gauge-ring` ma zarówno `.gauge-label` jak i `.gauge-sub` jako osobne linie. Ujednolicenie w stronę `tf-gauge` wymagałoby dodania drugiego opcjonalnego pola tekstowego do komponentu i do protokołu.

## Przykłady

```html
<tf-gauge value="72" min="0" max="100" label="RAM" size="120" variant="circular"></tf-gauge>
<script>
  document.querySelector('tf-gauge').thresholds = [
    { value: 0, tone: 'success', label: null },
    { value: 60, tone: 'warning', label: 'Podwyższone' },
    { value: 85, tone: 'critical', label: 'Krytyczne' },
  ];
</script>
```

```rust
// natywnie (tenta-ui-widgets, API docelowe)
Gauge::new(72.0, 0.0..=100.0)
    .label("RAM")
    .variant(GaugeVariant::Circular)
    .size_px(120)
    .thresholds(vec![
        GaugeThreshold::new(0.0, Tone::Success),
        GaugeThreshold::new(60.0, Tone::Warning).label("Podwyższone"),
        GaugeThreshold::new(85.0, Tone::Critical).label("Krytyczne"),
    ]);
```
