# Promienie, cienie i obramowania

Ten dokument opisuje skalę zaokrągleń (`RadiusToken`), skalę cieni
(`ShadowToken`) i tokeny obramowań (`BorderToken`) TentaFlow, wraz z regułą
degradacji cieni na renderze ESP32-P4/CPU, gdzie żywy blur nie istnieje.
Jedynym źródłem wartości jest [`tokens/tokens.json`](../tokens/tokens.json) →
`radius`, `elevation`, `border`.

## `RadiusToken` (`radius`)

Enum `RadiusToken` w `tentaflow-sdk-spec/src/protocol/ui/tokens.rs` (`None | Xs
| Sm | Md | Lg | Xl | Pill | Circle`):

| `RadiusToken` | Wartość | Użycie | CSS dziś |
|---|---|---|---|
| `none` | 0 | krawędzie ostre, wyjątkowo (np. wnętrze tabeli) | — |
| `xs` | 4px | checkbox, tagi, chipy kodu | — |
| `sm` | 6px | odznaki, małe inputy | `--radius-sm` |
| `md` | 10px | przyciski, inputy, małe karty | `--radius` |
| `lg` | 14px | karty, modale | `--radius-lg` |
| `xl` | 20px | bloki hero | `--radius-xl` |
| `pill` | 9999px | pigułki statusu, kontrolki segmentowe | — |
| `circle` | 50% | **tylko** awatary i przyciski ikonowe | — |

Zmapowane zmienne CSS (`--radius-sm`/`--radius`/`--radius-lg`/`--radius-xl`)
zweryfikowane w `tentaflow-core/www/css/style.css` `:root` — reszta
(`xs`/`none`/`pill`/`circle`) nie ma dziś dedykowanej zmiennej, wartość jest
zapisywana wprost tam, gdzie potrzebna.

## Reguła: promień podąża za gęstością

Im gęstszy komponent (mniejszy, bliżej krawędzi ekranu, więcej instancji na
raz — np. chip w liście filtrów vs. karta na pulpicie), tym mniejszy promień z
tabeli. Nie odwracaj tej relacji: duża karta z `radius.xs` wygląda technicznie,
mały chip z `radius.xl` wygląda jak błąd. Skala `xs → xl` w tabeli jest
uporządkowana rosnąco właśnie wg rozmiaru/gęstości komponentu, nie losowo.

## Reguła: `circle` tylko dla awatarów i przycisków ikonowych

`radius.usage.circle = "avatars, icon buttons only"` — okrąg (50%) jest
zarezerwowany dla treści, która jest naturalnie punktowa (twarz w awatarze,
pojedyncza ikona wyśrodkowana w przycisku). Karta, panel czy pigułka statusu z
wieloma znakami tekstu nigdy nie dostaje `circle` — do zaokrąglonych krawędzi
przy długiej treści służy `pill`.

## `ShadowToken` (`elevation`)

Enum `ShadowToken` (`None | Subtle | Medium | Elevated | Floating |
AccentGlow`) — tokens.json dodaje też dwa warianty glow poza enumem
(`glow_success`, `glow_critical`), używane punktowo przez komponenty stanu:

| `ShadowToken` | Wartość | Użycie |
|---|---|---|
| `none` | `none` | element płaski, bez separacji od tła |
| `subtle` | `0 2px 6px rgba(0,0,0,0.40)` | karty w spoczynku |
| `medium` | `0 8px 24px rgba(0,0,0,0.50)` | hover, dropdowny, popovery |
| `elevated` | `0 16px 48px rgba(0,0,0,0.70)` | modale, drawery |
| `floating` | `0 24px 64px rgba(0,0,0,0.70), 0 8px 24px rgba(99,102,241,0.15)` | command palette, pływające okna |
| `accent_glow` | `0 0 14px rgba(99,102,241,0.55), 0 0 28px rgba(99,102,241,0.35), 0 0 52px rgba(99,102,241,0.18), 0 0 84px rgba(99,102,241,0.08)` | maskotka hero, aktywny orb głosowy |
| `glow_success` (poza enumem) | `0 0 12px rgba(34,197,94,0.35), 0 0 28px rgba(34,197,94,0.18)` | potwierdzenie sukcesu (punktowe użycie) |
| `glow_critical` (poza enumem) | `0 0 14px rgba(239,68,68,0.55), 0 0 28px rgba(239,68,68,0.35), 0 0 52px rgba(239,68,68,0.18)` | alarm krytyczny (punktowe użycie) |

Odpowiadające zmienne w `style.css` `:root`: `--shadow-sm`, `--shadow`,
`--shadow-lg`, `--glow-indigo` — nazewnictwo CSS nie pokrywa się 1:1 z nazwami
tokenów (`subtle`/`medium`/`elevated` vs `sm`/(brak)/`lg`); to jest ten sam
rozjazd nazw co w kolorach, opisany w `design/MIGRATION.md`.

## `BorderToken` (`border`)

| Token | Szerokość | Kolor | Użycie |
|---|---|---|---|
| `hairline` | 1px | `border.default` (`#1f2548` dark) | domyślna krawędź karty/inputu w spoczynku |
| `thin` | 1px | `border.hover` (`#2f3668` dark) | krawędź przy hover, bez pogrubienia |
| `strong` | 2px | `border.hover` (`#2f3668` dark) | krawędź wyróżniona (np. focus-within kontenera) |
| `accent` | 1px | `tone.fg` (kolor zależny od aktualnego `Tone`) | krawędź komponentu niosącego status (np. karta z `Tone::Critical`) |

`accent` jest jedynym tokenem obramowania, którego kolor nie jest stały — bierze
`fg` z aktualnie rozwiązanego `Tone` (patrz [colors.md](colors.md) → tabela
`Tone`), więc karta z tonem `critical` dostaje czerwoną krawędź, a z `success`
zieloną, bez osobnego tokena na każdą kombinację.

## Fallback cieni na ESP32-P4/CPU: 1px border zamiast blura

`elevation._comment` w tokens.json wprost: *"On the CPU/ESP32-P4 renderer
shadows are flattened to a 1px border of `border.default` — no live blur."*
Potwierdzone też w `platform.esp32p4_tab5.shadows`, `platform.esp32p4_jc8012.shadows`
i `platform.esp32p4_jc4880.shadows`, wszystkie ustawione na `"flat"`.

**Dlaczego:** blur wielowarstwowego `box-shadow` (jak `floating` powyżej, z
czterema nakładającymi się warstwami rozmycia) wymaga renderowania offscreen z
konwolucją Gaussa na każdą klatkę — kosztowne na GPU desktopowym, nieopłacalne
na mikrokontrolerze bez dedykowanego GPU i z budżetem klatki liczonym w
pojedynczych milisekundach (patrz `platforms/esp32p4.md`). Zamiast tego
renderer CPU rysuje **jedną linię 1px** w kolorze `border.default` wokół
elementu podniesionego — to zachowuje czytelność separacji warstw (widać, gdzie
kończy się karta, a zaczyna tło) bez kosztu blura. Konsekwencja dla autora
strony: element, który na webie polega wyłącznie na cieniu do oddzielenia się
od tła (bez własnego obramowania), na ESP32-P4 może wyglądać płasko — jeśli
separacja wizualna jest krytyczna dla czytelności (np. karta nad listą kart),
dodaj też `border.hairline`, nie tylko `ShadowToken`.

## Jak dodać nowy token promienia/cienia

1. Sprawdź, czy potrzebna wartość naprawdę nie mieści się w istniejącej skali —
   `RadiusToken` ma 8 kroków, `ShadowToken` 6 (+2 warianty glow poza enumem);
   nowy krok pośredni (np. "między `md` a `lg`") to zwykle sygnał, że
   komponent powinien użyć istniejącego tokena, nie dowód na lukę w skali.
2. Jeśli luka jest realna (np. nowy typ powiadomienia potrzebuje własnej
   poświaty, jak `glow_success`/`glow_critical`) — dodaj wpis do `radius`/
   `elevation`/`border` w `tokens/tokens.json`, z jawną `role`/`usage`, tak jak
   istniejące wpisy.
3. Zdefiniuj zachowanie na renderze CPU/ESP32-P4 od razu — jeśli to cień,
   podaj też odpowiednik płaski (`border.hairline` w kolorze `border.default`),
   nie zakładaj, że blur "jakoś" się spłaszczy.
4. PR zmieniający `tokens.json` wymaga przeglądu jednej osoby spoza autora
   (Governance w [`../README.md`](../README.md)).

## Checklist dla autora strony

- [ ] Każdy `border-radius` to wartość z `RadiusToken`, nie literalny px.
- [ ] Promień rośnie z rozmiarem/gęstością komponentu (mały element → mały
      promień).
- [ ] `circle` użyty tylko dla awatara albo przycisku ikonowego.
- [ ] Każdy `box-shadow` to wartość z `ShadowToken` (albo `glow_success`/
      `glow_critical` dla punktowych stanów), nie własna kombinacja blura.
- [ ] Element polegający na cieniu do separacji od tła ma też `border.hairline`,
      żeby zachować czytelność po spłaszczeniu na ESP32-P4.
- [ ] Obramowanie niosące status używa `border.accent` (`tone.fg`), nie
      hardkodowanego koloru semantycznego.
