# Ruch

Ten dokument opisuje czasy trwania animacji, krzywe easingu, parametry sprężyn
dla natywnego renderera, zasadę doboru sprężyna-vs-tween, co wolno animować na
ścieżce webowej, choreografię wejścia/wyjścia dla modala/drawera/toastu/
tooltipa oraz twardą regułę `prefers-reduced-motion`. Jedynym źródłem wartości
jest [`tokens/tokens.json`](../tokens/tokens.json) → `motion`.

## Czasy trwania (`motion.duration_ms`)

| Token | ms | Użycie |
|---|---|---|
| `instant` | 0 | zmiana bez animacji (np. `prefers-reduced-motion`, patrz niżej) |
| `fast` | 120 | hover, focus ring, mikro-feedback |
| `normal` | 200 | domyślna zmiana koloru/transform (przycisk, karta) |
| `slow` | 300 | wejście/wyjście komponentu (toast, dropdown) |
| `deliberate` | 450 | sekwencje wieloetapowe, duże przejścia layoutu (otwarcie drawera na całą wysokość) |

## Easing (`motion.easing`)

| Token | Krzywa | Rola | Zmienna CSS dziś |
|---|---|---|---|
| `standard` | `cubic-bezier(0.32, 0.72, 0, 1)` | domyślna dla zmian koloru/transform | `--tf-spring-smooth` (zweryfikowane w `controls.css:65`) |
| `emphasized` | `cubic-bezier(0.16, 1, 0.3, 1)` | wejścia (modal, drawer, toast) | brak dedykowanej zmiennej dziś |
| `exit` | `cubic-bezier(0.7, 0, 0.84, 0)` | wyjścia | brak dedykowanej zmiennej dziś |
| `overshoot` | `cubic-bezier(0.34, 1.56, 0.64, 1)` | tylko playful potwierdzenia (np. zaznaczenie checkboxa) | `--tf-spring-snappy` (zweryfikowane w `controls.css:64`) |

`--tf-spring-smooth`/`--tf-spring-snappy` są dziś realnie używane w
`controls.css` (np. `transition: all 0.2s var(--tf-spring-smooth);` w regułach
hover przycisku, `transition: all 0.25s var(--tf-spring-snappy);` w regułach
potwierdzenia). `emphasized`/`exit` istnieją w tokens.json jako wartości
docelowe, ale nie mają dziś odpowiednika w CSS — do czasu wprowadzenia trafiają
tam jako literalny `cubic-bezier(…)` inline.

## Sprężyny dla natywnego renderera (`motion.spring`)

Natywny renderer (planowany `tentaflow-ui-native`, patrz [colors.md](colors.md))
nie animuje przez `duration + easing` — animacje niejawne (implicit) opisuje
sprężyna tłumiona, `mass = 1`:

| Token | `stiffness` | `damping` | Charakter |
|---|---|---|---|
| `snappy` | 400 | 30 | szybka, wyraźny przeskok — odpowiednik `overshoot`/`fast` |
| `gentle` | 170 | 26 | umiarkowana, bez przeskoku — odpowiednik `standard`/`normal` |
| `slow` | 90 | 20 | powolna, ciężka — odpowiednik `deliberate` |

### Zasada: sprężyna dla przerywalnych zmian stanu, tween dla sekwencji autorskich

- **Sprężyna niejawna** — gdy stan komponentu może się zmienić w dowolnym
  momencie animacji i nowa animacja musi płynnie przejąć aktualną prędkość
  (np. użytkownik szybko klika toggle kilka razy pod rząd, hover wchodzi i
  wychodzi w trakcie przejścia). Sprężyna zachowuje ciągłość prędkości przy
  przerwaniu — tween wymusza skok.
- **Tween (czas trwania + easing)** — gdy sekwencja ma z góry ustaloną
  choreografię wieloetapową, którą autor projektuje krok po kroku (np. wejście
  modala: scrim fade-in → panel translate+scale → focus na pierwszym polu, z
  konkretnymi opóźnieniami między krokami). Tu przewidywalny czas trwania
  całej sekwencji jest ważniejszy niż reakcja na przerwanie.

## Co wolno animować (ścieżka webowa)

Animuj wyłącznie **kolor, `opacity`, `transform`** (translate/scale/rotate).
**Nigdy** `width`/`height`/inne właściwości wyzwalające layout na ścieżce
webowej — animacja layoutu wymusza reflow co klatkę (measure → layout → paint
w pętli), co jest dokładnie tym, czego unika kompozytor przeglądarki przy
`transform`/`opacity` (te dwie właściwości mogą być animowane wyłącznie na
warstwie kompozytora, bez re-layoutu). Jeśli komponent musi wizualnie
"rozszerzyć się", animuj `transform: scale()` z odpowiednim `transform-origin`,
a docelowy rozmiar ustaw natychmiast pod spodem — nie animuj `width` wprost.

## Choreografia wejścia/wyjścia

| Komponent | Wejście | Wyjście |
|---|---|---|
| **Modal** | scrim `opacity 0→1` (`normal`, `standard`), panel `transform: scale(0.96)→scale(1)` + `opacity 0→1` (`slow`, `emphasized`) | odwrotnie, `easing: exit`, `fast`–`normal` |
| **Drawer** | `transform: translateX/Y(100%)→0` (`slow`, `emphasized`), scrim równolegle `opacity 0→1` | `transform` z powrotem do `100%` (`easing: exit`, `normal`) |
| **Toast** | `transform: translateY(8px)→0` + `opacity 0→1` (`normal`, `standard`) | `opacity 1→0` (`fast`, `exit`), bez transformu — zniknięcie ma być ciche, nie odjeżdżać |
| **Tooltip** | `opacity 0→1`, bez transformu, bardzo krótko (`fast`, `standard`) | `opacity 1→0` natychmiast po utracie hover/focus (`fast`) |

Scrim i panel/drawer animują **równolegle**, nie sekwencyjnie — scrim nie
czeka, aż panel skończy się poruszać; to trzyma odczuwalny czas otwarcia
bliżej pojedynczego `slow` (300ms), nie sumy dwóch animacji.

## `prefers-reduced-motion` — reguła twarda

`motion.reduced_motion`: *"all durations → 0ms, springs → snap to target;
mandatory on every platform"*. To nie jest opcjonalne ulepszenie dostępności —
każdy plik z `@keyframes` albo `transition` **musi** mieć guard. Minimalny
wzorzec CSS:

```css
@media (prefers-reduced-motion: reduce) {
  *, *::before, *::after {
    animation-duration: 0.01ms !important;
    animation-iteration-count: 1 !important;
    transition-duration: 0.01ms !important;
    scroll-behavior: auto !important;
  }
}
```

(`0.01ms` zamiast `0`, bo niektóre przeglądarki traktują `animation-duration:
0s` jako "brak animacji zdefiniowanej", a nie "animacja trwająca zero
sekund", i pomijają event `animationend`, na który część komponentów czeka do
posprzątania stanu.)

Na natywnym rendererze odpowiednik to sprężyna, która nie interpoluje w ogóle —
skacze bezpośrednio do wartości docelowej w jednej klatce.

## Dlaczego redraw na żądanie, nie stała pętla klatek

Natywny renderer (CPU/ESP32-P4) rysuje klatkę **tylko wtedy, gdy coś się
zmieniło** (input, timer animacji, dane z sieci) — nie w stałej pętli 60fps
niezależnie od aktywności. Dwa powody, oba z `platform.esp32p4_tab5`/
`esp32p4_jc8012`/`esp32p4_jc4880` w tokens.json (panel 720×1280 lub 800×1280
RGB565, gęstość `comfortable`, wskaźnik `coarse`): urządzenie jest zasilane
bateryjnie/przez ograniczony zasilacz USB, więc każda zbędna klatka to zużyta
energia bez efektu widocznego dla użytkownika; a mikrokontroler ma budżet
klatki liczony w pojedynczych milisekundach na całą kompozycję sceny (patrz
`platforms/esp32p4.md`) — renderowanie klatek, w których nic się nie
zmieniło, zjada ten budżet kosztem realnych interakcji. Konsekwencja dla
autora animacji: każda animacja musi jawnie sygnalizować "jestem w trakcie",
żeby renderer wiedział, kiedy przestać żądać kolejnych klatek — animacja,
która nigdy nie osiąga stanu spoczynku (np. źle strojona sprężyna z bardzo
niskim `damping`), utrzymuje urządzenie w ciągłym redraw bez końca.

## Trzy najczęstsze antywzorce ze stanu dzisiejszego (audyt CSS)

1. **Surowe literały czasu zamiast tokenów.** `controls.css` miesza token i
   literał w jednej deklaracji, np. `transition: color 0.2s ease, transform
   0.2s var(--tf-spring-smooth);` (linia 276) — `color` dostaje surowe `0.2s
   ease`, `transform` dostaje `var(--tf-spring-smooth)`, mimo że oba powinny
   spójnie sięgać po `motion.duration_ms.normal` + `motion.easing.standard`.
2. **`@keyframes` bez guardu na `prefers-reduced-motion`.** W CSS istnieje 124
   odrębnych bloków `@keyframes` w całym `www/css/`, a regułę
   `prefers-reduced-motion` implementuje dziś tylko 7 z 40 plików CSS
   (`style.css`, `controls.css`, `tentanas.css`, `tentabus.css`,
   `project-studio.css`, `face.css`, `tf-agent-activity.css`) — większość
   plików per-strona definiuje własne `@keyframes` bez żadnego guardu, mimo
   że reguła jest deklarowana jako globalna i twarda.
3. **Ta sama para właściwości animowana różnymi krzywymi w różnych miejscach.**
   Poza `--tf-spring-smooth`/`--tf-spring-snappy` w `controls.css`, wiele
   deklaracji `transition`/`animation` w plikach per-strona (np.
   `flows-builder.css`, `profiling.css`) definiuje własne, niepowtarzalne
   `cubic-bezier(…)` inline zamiast sięgać po jedną z czterech krzywych
   `motion.easing` — efekt to niespójne "tempo" ruchu między stronami, mimo
   wspólnego systemu tokenów.

## Checklist dla autora strony

- [ ] Każdy `transition`/`animation-duration` to wartość z `motion.duration_ms`,
      nie literalne `0.15s`/`0.2s`.
- [ ] Każda krzywa easingu to jedna z czterech z `motion.easing`
      (`--tf-spring-smooth`/`--tf-spring-snappy` albo odpowiednik
      `emphasized`/`exit`), nie własny `cubic-bezier(…)`.
- [ ] Animowane są tylko kolor, `opacity`, `transform` — nigdy `width`/
      `height`/inne właściwości layoutowe.
- [ ] Każdy plik z `@keyframes` ma guard `@media (prefers-reduced-motion:
      reduce)`.
- [ ] Wejście/wyjście modala/drawera/toastu/tooltipa podąża za choreografią z
      tabeli wyżej (scrim i panel równolegle, nie sekwencyjnie).
- [ ] Animacja ma jawny stan spoczynku (nie żąda klatek w nieskończoność).
