# Materiały: powierzchnie i „płynne szkło"

Ten dokument opisuje **materiały powierzchni** — z czego „zrobione" są panele i
kontrolki. Do dziś TentaFlow miał jeden materiał: matowa powierzchnia (`bg.card`,
`bg.elevated`) z cieniem. Wersja 2.0 dodaje **szkło** w stylu Apple Liquid Glass
(WWDC 2025, iOS 26 / macOS Tahoe): półprzezroczysty materiał, który załamuje treść
pod sobą, rozmywa ją, ma odbłyski reagujące na ruch i morfuje między kształtami.
Wartości: `tokens.json → material.glass`. Implementacja natywna: TentaEngine
`docs/UI_TOOLKIT_SPEC.md` §5.4. W HTML szkło ma tylko przybliżenie (§6).

## 1. Słownik

| Materiał | Token | Co widzi użytkownik | Gdzie |
|---|---|---|---|
| **Matte** (domyślny) | `color.themes.*.bg.card` + `elevation.*` | nieprzezroczysta powierzchnia, cień wg elevation | karty, tabele, formularze, treść |
| **Glass / Regular** | `material.glass.regular` | rozmyte tło (24 px), soczewkowanie na 14 px brzegu, tint 25 %, obwódka Fresnela, odbłysk | pływające paski (tab/tool/nav), przyciski nad mediami, popovery |
| **Glass / Clear** | `material.glass.clear` | ledwo rozmyte tło (8 px), tint 8 %, cieńszy brzeg | kontrolki nad zdjęciem/wideo/3D, gdzie treść pod spodem ma być widoczna |
| **Glass / Tinted(tone)** | `material.glass.tinted` + `Tone` | szkło zabarwione barwą tonu (45 %) | akcentowane akcje (primary), stany (success/critical) na szkle |
| **Glass / Flat** | `material.glass.flat` | półprzezroczysty prostokąt + obwódka 1 px + gradient odbłysku, **bez** próbkowania tła | fallback: ograniczona przezroczystość, przekroczony budżet, brak backendu |

Materiał to właściwość kontenera (`GlassPanel`, `FloatingBar`, `Button.material`),
nie osobny komponent — ten sam przycisk może być matowy w formularzu i szklany nad
mapą.

## 2. Anatomia szkła

```text
        ┌── odbłysk (specular, od LightDir) ──┐
   ╭────┴────────────────────────────────────┴────╮  ← obwódka Fresnela (rim), 1–2 px
   │ ░░ brzeg: soczewkowanie 14 px (edge_width) ░░ │     treść pod spodem lekko
   │ ░  ┌───────────────────────────────────┐   ░ │     „wciągnięta" do środka,
   │ ░  │   tafla: rozmyte tło + tint       │   ░ │     na krawędzi rozszczepienie
   │ ░  │   (blur_px, tint_strength)        │   ░ │     R/G/B (dispersion_px)
   │ ░  └───────────────────────────────────┘   ░ │
   ╰──────────────────────────────────────────────╯
        └──── miękki cień (shadow) pod taflą ────┘
```

Parametry (`material.glass.regular`): `blur_px 24`, `ior 1.15`, `thickness_px 6`,
`edge_width_px 14`, `tint_strength 0.25`, `dispersion_px 1.0`, `fresnel_power 3`,
`rim_strength 0.35`, `specular_strength 0.45`, `shininess 48`, cień `elevation.medium`.

## 3. Zachowanie dynamiczne

- **Ruch elementu** (scroll, drag, animacja): tło pod szkłem się zmienia → szkło
  przelicza się w każdej klatce ruchu; po zatrzymaniu wynik jest cache'owany (0 kosztu).
- **Ruch urządzenia**: `LightDir` z IMU (telefon, tablet, Tab5 — BMI270) z filtrem
  low-pass 6 Hz i wzmocnieniem 0.8; na desktopie z pozycji wskaźnika (0.3); na
  urządzeniach bez czujnika — stałe światło z góry-lewa (`rest_azimuth 300°`,
  `rest_elevation 55°`). Zmiana światła aktualizuje **tylko** obwódkę i odbłysk,
  nie soczewkowanie — to trzyma koszt niski.
- **Adaptacja do treści**: średnia luminancja tła > `0.45` → wariant „ciemne szkło"
  (`tint_dark`), inaczej „jasne" (`tint_light`); zapobiega nieczytelnym etykietom
  na jasnym zdjęciu.
- **Morfing**: dwa kształty łączą się `smin` z promieniem 12 px sterowanym sprężyną
  `motion.spring.gentle` — pasek zakładek zwija się do kółka przy scrollu, dwa
  przyciski „zlewają się" w jeden segmentowany, popover wyrasta z przycisku.
- **Reduced transparency / reduced motion**: `Flat` z tintem 0.9; morfing → skok.

## 4. Gdzie wolno, gdzie nie

| Wolno (`allowed_on`) | Nie wolno (`forbidden_on`) |
|---|---|
| pływające paski: zakładki, narzędzia, nawigacja dolna | tabele i siatki danych |
| przyciski i chipy nad zdjęciem, wideo, mapą, widokiem 3D/voxel | formularze, pola tekstowe |
| karty nad mediami (podpis zdjęcia, sterowanie kamerą robota) | kontenery tekstu ciągłego (czat, notatki, dokumentacja) |
| nakładki: toast, popover, command palette, sheet | wszystko, co użytkownik czyta „na raz" na gęstym tle |

Zasada: **szkło jest ozdobą i wskazówką „to pływa nad treścią", nie nośnikiem
informacji**. Etykieta na szkle ma spełniać kontrast AA na najgorszym możliwym tle —
jeśli nie da się tego zagwarantować (zdjęcia użytkownika), użyj `Tinted` lub `Flat`.
Maksymalnie **3 poziomy** szkła jedno na drugim (`max_levels`); w praktyce dwa.

## 5. Platformy i budżet

| Platforma | Tryb (`platform.*.glass`) | Źródło światła | Uwagi |
|---|---|---|---|
| web/desktop GPU | `full` | wskaźnik | pełnoekranowe panele OK (60 fps) |
| web bez WebGPU | `lite` (CPU) | wskaźnik | jak P4, większy budżet |
| telefon / tablet | `full` | IMU | jak iOS |
| Tab5 (720×1280) | `lite`, `glass_max_area_px 180 000` (wstępnie) | IMU BMI270 | ≈ pasek 720×96 + kilka przycisków na klatkę; panel pełnoekranowy tylko statyczny |
| JC8012 | `lite`, 180 000 (wstępnie) | stałe | brak IMU |
| JC4880 (480×800) | `lite` bez dyspersji, 60 000 (wstępnie) | stałe | jeden pasek lub 3 przyciski |

Liczby dla P4 są **szacunkami** do czasu pomiaru na płytce (TentaEngine U0-005/
U4-006); szacunek kosztu paska 720×96 to ~10–13 ms na klatkę przeliczenia.
**Szkło nad przewijaną treścią pozostaje pełne podczas scrolla** (decyzja
właściciela 14.09.2026) — na P4 oznacza to, że scroll pod szklanym paskiem jest
klatką ~30 fps, a nie 60; projektując ekran P4 z paskiem szkła nad listą przyjmij
tę płynność. Po przekroczeniu budżetu pola w klatce silnik degraduje **najniżej
położone** szkło do `Flat` (nie odmawia renderu). Autor strony nie liczy budżetu —
projektuje wg tabeli „gdzie wolno" i ufa degradacji; ale nie planuje ekranów, które
zależą od szkła, żeby być zrozumiałe.

## 6. HTML dziś (przybliżenie)

W dashboardzie HTML szkło można przybliżyć tylko `backdrop-filter: blur()` +
półprzezroczyste tło + `inset box-shadow` jako obwódka — bez soczewkowania,
dyspersji i światła od ruchu. Klasa pomocnicza (do dodania w P0 `tokens.css`):

```css
.tf-material-glass {
  background: color-mix(in srgb, var(--glass-tint-dark) 25%, transparent);
  backdrop-filter: blur(24px) saturate(1.2);
  -webkit-backdrop-filter: blur(24px) saturate(1.2);
  box-shadow: inset 0 0 0 1px rgba(255,255,255,0.12), var(--shadow);
  border-radius: var(--radius-lg);
}
@media (prefers-reduced-transparency: reduce) {
  .tf-material-glass { backdrop-filter: none; background: var(--glass-flat-fill); }
}
```

Pełny efekt (refrakcja, odbłysk od IMU, morfing) jest celem natywnego silnika
i jednym z powodów jego budowy — HTML nie jest w stanie go dać wydajnie.

## 7. Checklist dla autora strony

- [ ] Szkło tylko na elementach z tabeli „wolno"; nic, co trzeba przeczytać, nie leży
      *na* gęstym, zmiennym tle bez `Tinted`/`Flat`.
- [ ] Etykiety na szkle: `text.primary` lub `tone.fg`, kontrast sprawdzony na jasnym
      i ciemnym tle.
- [ ] Nie więcej niż 2 poziomy szkła; brak szkła w tabelach/formularzach/tekście.
- [ ] Zachowanie przy `reduce_transparency` i `reduced_motion` przemyślane (co się
      dzieje, gdy szkło staje się `Flat`, a morfing skokiem).
- [ ] Na ekranach dla P4: suma pola szkła w jednej klatce ≤ budżet płytki
      (`platform.esp32p4_*.glass_max_area_px`) albo świadoma degradacja.
- [ ] Mockup pokazuje szkło na **najgorszym** tle (jasne zdjęcie, gęsta lista), nie
      tylko na gradientowym hero.
