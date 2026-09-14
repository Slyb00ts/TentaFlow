# Kolory

Ten dokument opisuje paletę TentaFlow — tła, tekst, obramowania, akcenty i kolory
semantyczne — oraz sposób, w jaki mapują się one na dzisiejsze zmienne CSS
(`www/css/style.css` `:root`) i na planowany motyw natywny. Jedynym źródłem wartości
jest [`tokens/tokens.json`](../tokens/tokens.json) → `color`; ten plik nigdy nie
podaje koloru, który nie ma tam odpowiednika. Motyw `dark` ma status `canonical`
(jest tym, co realnie renderuje aplikacja); motyw `light` ma status **draft** —
wartości istnieją w tokenach, ale nie przeszły jeszcze przeglądu wizualnego i nie są
włączone (`[data-theme="light"]` nie jest nigdzie zaimplementowane w CSS).

## Tła (`color.themes.<mode>.bg`)

| Token | dark | light (draft) | Rola | CSS dziś |
|---|---|---|---|---|
| `bg.base` | `#050818` | `#f7f8fc` | tło aplikacji | `--bg` |
| `bg.raised` | `#0a0d24` | `#ffffff` | sidebar, topbar | `--bg-2` |
| `bg.elevated` | `#111535` | `#eef0f7` | panele podniesione, przyciski drugorzędne | `--bg-3` |
| `bg.card` | `#141836` | `#ffffff` | powierzchnie kart | `--bg-card` |
| `bg.card_hover` | `#1a1f45` | `#f0f2fa` | stan hover interaktywnej karty | `--bg-card-hover` |
| `bg.input` | `#0b0e22` | `#ffffff` | pola formularzy, bloki kodu | `--bg-input` |
| `bg.overlay` | `rgba(0,0,0,0.80)` | `rgba(10,13,36,0.55)` | scrim modala (+ 8px blur na wariancie web) | brak dedykowanej zmiennej |

## Tekst (`color.themes.<mode>.text`)

| Token | dark | light (draft) | Rola | CSS dziś | Kontrast na `bg.base` |
|---|---|---|---|---|---|
| `text.primary` | `#e8ebf5` | `#141836` | tekst body | `--text` | 15.4:1 |
| `text.secondary` | `#a0a8c8` | `#4a5178` | etykiety, metadane | `--text-2` | 8.2:1 |
| `text.muted` | `#6a7196` | `#8087a8` | podpowiedzi, disabled — **wyłącznie dekoracyjnie, nigdy jako tekst body** | `--text-3` | 3.9:1 |
| `text.inverse` | `#050818` | `#ffffff` | tekst na wypełnionych akcentem powierzchniach | brak dedykowanej zmiennej | — |
| `text.on_accent` | `#ffffff` | `#ffffff` | tekst na przyciskach primary | brak dedykowanej zmiennej | 4.7:1 (na `accent.primary`) |

`text.muted` (3.9:1) nie spełnia WCAG AA dla tekstu body (wymóg 4.5:1) — to jest
zamierzone: token jest zarezerwowany dla treści dekoracyjnych/pomocniczych, nigdy
dla głównej treści strony. Zasada 3 poniżej egzekwuje to wprost.

## Obramowania (`color.themes.<mode>.border`)

| Token | dark | light (draft) | Rola | CSS dziś |
|---|---|---|---|---|
| `border.default` | `#1f2548` | `#dfe3f0` | domyślna krawędź karty/inputu | `--border` |
| `border.hover` | `#2f3668` | `#c4cade` | krawędź przy hover/focus-within | `--border-hover` |
| `border.focus` | `#6366f1` | `#4f52d9` | kolor pierścienia fokusu = `accent.primary` | brak dedykowanej zmiennej (używa `--accent-1`) |

## Akcenty (`color.themes.<mode>.accent`)

| Token | dark | light (draft) | Rola | CSS dziś |
|---|---|---|---|---|
| `accent.primary` | `#6366f1` | `#4f52d9` | indygo — akcje główne, linki, fokus | `--accent-1` |
| `accent.primary_hover` | `#818cf8` | `#6366f1` | hover na `accent.primary` | `--tf-accent-2` (`controls.css`) |
| `accent.secondary` | `#a78bfa` | `#8b6cf0` | lilak — akcent brandowy do hero/gradientów, **nigdy do chrome UI** | `--accent-2` |
| `accent.glow` | `rgba(99,102,241,0.18)` | `rgba(79,82,217,0.15)` | poświata wokół elementów akcentowanych | `--accent-glow` |
| `accent.soft` | `rgba(99,102,241,0.15)` | `rgba(79,82,217,0.12)` | zaznaczone wiersze, miękkie odznaki | brak dedykowanej zmiennej |

## Semantyka (`color.themes.<mode>.semantic`)

| Token | dark | light (draft) | Rola | CSS dziś |
|---|---|---|---|---|
| `semantic.success` | `#22c55e` (soft `rgba(34,197,94,0.15)`) | `#15803d` | online, przyznano, zapisano | `--success` (zweryfikowane w `style.css`) |
| `semantic.warning` | `#f59e0b` (soft `rgba(245,158,11,0.15)`) | `#b45309` | oczekujące, zdegradowane, beta | `--warning` (zweryfikowane w `style.css`) |
| `semantic.critical` | `#ef4444` (soft `rgba(239,68,68,0.15)`, hover `#dc2626`) | `#b91c1c` (hover `#991b1b`) | błędy, akcje destrukcyjne | `--danger` |
| `semantic.info` | `#60a5fa` (soft `rgba(96,165,250,0.15)`) | `#1d4ed8` | informacyjne, remote/tailscale | `--info` |

## Rozwiązywanie `Tone` (`color.themes.dark.tone`)

Enum `Tone` z `tentaflow-sdk-spec/src/protocol/ui/tokens.rs` (`Neutral | Primary |
Success | Warning | Critical | Info | Muted`) rozwiązuje się na parę `fg` (tekst/
ikona/obrys) + `bg` (miękkie wypełnienie). Tylko motyw `dark` ma zdefiniowaną
tabelę rozwiązywania w tokens.json — `light` dziedziczy tę samą strukturę z
analogicznych wpisów `text`/`semantic`/`accent`, ale nie została jeszcze osobno
wypisana jako draft.

| Tone | fg | bg |
|---|---|---|
| `neutral` | `color.themes.dark.text.secondary` (`#a0a8c8`) | `color.themes.dark.bg.elevated` (`#111535`) |
| `primary` | `color.themes.dark.accent.primary` (`#6366f1`) | `color.themes.dark.accent.soft` (`rgba(99,102,241,0.15)`) |
| `success` | `color.themes.dark.semantic.success.value` (`#22c55e`) | `…success.soft` (`rgba(34,197,94,0.15)`) |
| `warning` | `…warning.value` (`#f59e0b`) | `…warning.soft` (`rgba(245,158,11,0.15)`) |
| `critical` | `…critical.value` (`#ef4444`) | `…critical.soft` (`rgba(239,68,68,0.15)`) |
| `info` | `…info.value` (`#60a5fa`) | `…info.soft` (`rgba(96,165,250,0.15)`) |
| `muted` | `color.themes.dark.text.muted` (`#6a7196`) | `transparent` |

Nie wybieraj koloru "z oka" dla odznaki/statusu — wybierz `Tone`, resztę daje
tabela powyżej.

## Reguły (twarde)

1. **Zero hex poza tokenami.** Każdy kolor w stronie/komponencie pochodzi z tokena
   (`var(--…)` w CSS, `theme.…` w Rust, enum `Tone`/`ColorToken` w protokole
   addonów). Wyjątek: palety domenowe (`voice`, `dataviz`) — patrz niżej.
2. **Lilak (`accent.secondary`, `#a78bfa`) tylko do brandu.** Hero, gradienty,
   logo — nigdy jako kolor chrome UI (przyciski, linki, stan aktywny nawigacji
   używają `accent.primary`, indygo).
3. **`text.muted` nigdy jako tekst body.** Kontrast 3.9:1 nie spełnia AA dla
   treści głównej — tylko podpowiedzi, znaczniki disabled, metadane drugorzędne.
4. **Kontrast raportowany, nie zgadywany.** Wartości `contrast_on_bg_base` /
   `contrast_on_accent` w tokens.json są jedynym źródłem liczb kontrastu; jeśli
   dodajesz nowy kolor tekstu, dolicz i wpisz własny współczynnik, nie szacuj.

## Gradienty (`color.themes.dark.gradient`)

| Token | Wartość | CSS |
|---|---|---|
| `gradient.accent` | `linear-gradient(135deg, #6366f1 0%, #a78bfa 100%)` | `--gradient-accent` |
| `gradient.logo` | `linear-gradient(90deg, #ffffff 0%, #60a5fa 50%, #a78bfa 100%)` | `--gradient-logo` |
| `gradient.soft` | `linear-gradient(135deg, rgba(99,102,241,0.15) 0%, rgba(167,139,250,0.12) 100%)` | `--gradient-soft` |

Gradienty istnieją tylko dla dark — light (draft) nie ma jeszcze zdefiniowanych
odpowiedników w tokens.json.

## Paleta domenowa: stan głosu (`color.themes.dark.voice`)

Maszyna stanów czatu głosowego (`chat-audio.js`) ma własną, osobną paletę — to
jest przykład dozwolonego wyjątku od reguły 1 (paleta domenowa, zadeklarowana
jako token, nie jako hex w pliku strony):

| Stan | Kolor | Poświata | Gradient |
|---|---|---|---|
| `listen` | `#22c55e` | `rgba(34,197,94,0.45)` | `linear-gradient(135deg, #22c55e 0%, #06b6d4 100%)` |
| `think` | `#f59e0b` | `rgba(245,158,11,0.40)` | — |
| `speak` | `#a78bfa` | `rgba(167,139,250,0.55)` | `linear-gradient(135deg, #a78bfa 0%, #6366f1 100%)` |

Analogicznie traktuj palety wykresów (`color.dataviz`, patrz
[dataviz.md](dataviz.md)) — kategoryczna/sekwencyjna/rozbieżna/heat to osobna
warstwa tokenów, nie hex wpisany bezpośrednio w komponent wykresu.

## Jak dodać nowy kolor

1. Zdecyduj, czy to jest kolor **rdzenia** (bg/text/border/accent/semantic —
   trafia do `color.themes.dark.*` i `color.themes.light.*`) czy **domenowy**
   (specyficzny dla jednego widoku — trafia do osobnej sekcji jak `voice` albo
   `dataviz`, nie do rdzenia).
2. Dodaj wpis w **obu** motywach w `tokens/tokens.json` — `dark` z realną wartością
   z żywego CSS (albo nową, przemyślaną), `light` z wartością o parytecie
   kontrastu, oznaczoną jako `draft`, dopóki nie przejdzie przeglądu wizualnego.
3. Zmapuj na zmienną CSS (`css` pole w tokenie) i dodaj `--nazwa` w `style.css`
   `:root` (lub odpowiedniku w `controls.css`, jeśli token jest specyficzny dla
   kontrolki) — patrz zasada rozjazdu dwóch słowników w
   [`../README.md`](../README.md).
4. Jeśli kolor niesie znaczenie (status, ton) — dodaj go do tabeli `tone`, nie
   zostawiaj komponentu z hardkodowanym `Tone`→kolor.
5. PR zmieniający `tokens.json` wymaga przeglądu jednej osoby spoza autora
   (patrz Governance w [`../README.md`](../README.md)).

## Jak natywny motyw konsumuje te same tokeny

`tokens.json` (`meta.notes`) deklaruje `tentaflow-ui-native/src/theme_generated.rs`
jako planowany artefakt generowany z tego pliku — **ten crate jeszcze nie istnieje
w repozytorium** (`design/README.md` wymienia `tentaflow-ui-native` jako
"natywny, planowany"). Docelowo generator (`scripts/gen-design-tokens.py`,
zadanie z `MIGRATION.md`) ma produkować strukturę motywu 1:1 z `color.themes.*`,
tak by natywny render (wgpu / CPU / ESP32-P4) czytał te same wartości co CSS.

Ważne zastrzeżenie zweryfikowane w kodzie: w repo istnieje już osobny crate
`tentaflow-ui` (`tentaflow-ui/src/theme/mod.rs`) — natywne narzędzie na bazie
`egui`, z własną, ręcznie zdefiniowaną strukturą `Theme { palette: Palette, … }`
(`Palette::dark()`/`Palette::light()`). Jego kolory **nie pochodzą z
`tokens.json`** i się z nim rozjeżdżają — np. `Palette::dark().bg_primary` to
`rgb(17,17,27)`, podczas gdy `color.themes.dark.bg.base` to `#050818`. To jest
osobne, wcześniejsze narzędzie, nie planowany `tentaflow-ui-native`, i nie jest
dziś wymienione w mapie katalogu `design/README.md`. Traktuj to jako znany
rozjazd do naprawienia (przepięcie `tentaflow-ui` na `tokens.json`), a nie jako
wzorzec do naśladowania w nowym kodzie natywnym.

## Checklist dla autora strony

- [ ] Każdy kolor w markupie/CSS strony to `var(--…)` albo `Tone`/`ColorToken` —
      zero literalnego hex (grep gate z `README.md`).
- [ ] Lilak (`accent.secondary`) użyty tylko w kontekście brandowym (hero,
      gradient), nie jako kolor przycisku/linku.
- [ ] `text.muted` nie jest użyty jako kolor głównej treści.
- [ ] Jeśli dodajesz nowy status/badge — wybrałeś `Tone`, nie kolor bezpośrednio.
- [ ] Jeśli dodajesz nowy kolor rdzenia — ma wpis w obu motywach w `tokens.json`.
