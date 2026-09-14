# Avatar

| | |
|---|---|
| Tier | 0 (MVP) |
| HTML | `<tf-avatar>` — `tentaflow-core/www/js/components/tf-avatar.js`, style `controls.css:3208-3254`. Druga, niezwiązana implementacja renderowana przez host addonów żyje w `controls.css:6973-7047` (`.tf-avatar-block` / `.tf-avatar__source` / `.tf-avatar-group`) i nie ma własnego pliku JS — buduje ją `addon-ui-host` na podstawie CBOR. Trzecia, ad-hoc odmiana istnieje w markupie stron czatu (`.avatar.user` / `.avatar.assistant`, `style.css:3065-3080`) i w `.user-chip .avatar` (`style.css:457-463`) — patrz „Znane odstępstwa”. |
| Protokół addonów | `0x020D` `Avatar` + `0x020E` `AvatarGroup` — `docs/ADDON_UI_COMPONENT_CATALOG_v1.md` §1594-1617 |
| Natywny (TentaEngine) | `tenta_ui_widgets::Avatar` — status: planowany |
| Status dokumentu | draft |

Okrągły (lub zaokrąglony/kwadratowy w wariancie protokołu) znacznik tożsamości: zdjęcie, inicjały na tonowanym tle albo ikona, z opcjonalną kropką statusu. Nie używaj go jako zwykłej ikonki dekoracyjnej (od tego jest `.icon`, patrz [icon.md](icon.md)) ani jako klikalnego przycisku — awatar sam w sobie nie jest interaktywny; jeśli ma otwierać menu, owiń go w `tf-button` lub `<button>`.

## Anatomia

`tf-avatar` (HTML, 3 rozmiary, prostokątny wachlarz tonów):

```text
┌────────┐
│  img   │   ← src ustawiony: <img class="tf-avatar-img"> wypełnia okrąg (object-fit: cover)
└────────┘
   lub
┌────────┐
│  "PJ"  │   ← brak src: inicjały (uppercase), tło = tone.bg, tekst = tone.fg
└────────┘
  width/height: sm 28 / md 36 / lg 48 (px, poza siatką 4px — patrz odstępstwa)
```

Wariant protokołu (`Avatar` 0x020D) dodaje kropkę statusu i kształt:

```text
┌────────┐●   ← status dot: 30% szerokości/wysokości, dolny-prawy róg,
│  img   │     obwódka 2px koloru tła inputu (odcina kropkę od zdjęcia)
└────────┘
  shape: circle (promień 50%) | rounded (0.5em) | square (0)
  size: xs 1.25em · sm 1.75em · md 2.5em · lg 3em · xl 4em (jednostki em — skalują się z font-size kontekstu, nie z px jak tf-avatar.js)
```

`AvatarGroup` (0x020E): do 8 awatarów nachodzących na siebie (`margin-left` ujemny), plus licznik „+N” w stylu `.tf-avatar-group__more`.

## Warianty

| Wariant | Wygląd | Kiedy używać |
|---|---|---|
| `src` (zdjęcie) | `<img>` wypełniający okrąg | użytkownik/kontakt z prawdziwym zdjęciem profilowym |
| `initials` | 1-3 znaki, wielkie litery, na tonowanym tle | brak zdjęcia — domyślny fallback |
| `icon` (tylko protokół) | ikona z `IconRef` zamiast inicjałów | system/bot/placeholder bez tożsamości osobowej |
| `AvatarGroup` | stos nachodzących awatarów + „+N” | lista uczestników spotkania, przypisani agenci |

Tony `tf-avatar.js` (atrybut `tone`): `accent` (domyślny), `success`, `danger` — tylko te trzy, nie pełny `Tone`. Wariant protokołu przyjmuje pełny `Tone` (`neutral/primary/success/warning/critical/info/muted`) dla tła inicjałów/ikony — patrz „Znane odstępstwa”.

## Rozmiary

| Rozmiar | `tf-avatar` (px) | Wariant protokołu (em, kontekstowo) | Cel dotykowy |
|---|---|---|---|
| `sm` / `xs`-`sm` | 28×28, `font-size: 10px` | xs 1.25em / sm 1.75em | dekoracyjny, nie interaktywny — nie musi spełniać 44px |
| `md` | 36×36, `font-size: 12px` | 2.5em | domyślny w listach, kartach, nagłówku czatu |
| `lg` / `xl` | 48×48, `font-size: 16px` | 3em / xl 4em | nagłówek profilu, pusty stan pełnoekranowy |

Sam awatar nigdy nie jest celem dotykowym — jeśli klikalny (np. otwiera menu profilu), owijający `<button>` musi mieć min. `control.touch_target_min` (44px), niezależnie od wizualnego rozmiaru awatara w środku.

## Stany

| Stan | Tło | Tekst/ikona | Obramowanie | Uwagi |
|---|---|---|---|---|
| default (inicjały) | `tone.bg` (np. `color.themes.dark.accent.soft`) | `tone.fg` | 1px, 30% alpha koloru tonu | `controls.css:3233-3247` |
| default (zdjęcie) | — | — | brak | `object-fit: cover`, okrąg przycina kwadratowe źródło |
| hover | bez zmian | bez zmian | bez zmian | `transform: scale(1.08)`, tylko `pointer: fine` (`controls.css:3223-3227`) |
| status: online/offline/busy/away | kropka `tone.fg` odpowiedniego semantyka | — | 2px `bg.input` | tylko wariant protokołu |
| błędne/brakujące `src` | fallback na inicjały nie jest automatyczny | `<img alt>` = inicjały | — | `tf-avatar.js` nie nasłuchuje `onerror` — złamany URL zostawia pusty okrąg z samym `alt`, patrz odstępstwa |

Awatar nie ma stanów `disabled`, `loading` ani `selected` — to nie kontrolka formularza.

## Zachowanie

- Interakcja wskaźnikiem: `tf-avatar` reaguje jedynie hover-scale (dekoracja); żadna interakcja klawiaturowa nie jest wbudowana, bo element nie jest fokusowalny (`tabindex` nie jest ustawiany).
- Animacje: `transform 0.15s var(--tf-spring-smooth)` na hover, `box-shadow 0.2s ease` — bez `@keyframes`, więc nie wymaga własnej klauzuli `prefers-reduced-motion` (skalowanie 1.08 jest subtelne, ale technicznie nieobjęte redukcją ruchu poza globalną regułą wildcard w `controls.css:9586`).
- Zdarzenia/API: `tf-avatar` nie emituje żadnych zdarzeń własnych. Atrybuty `initials`/`size`/`tone`/`src` są `observedAttributes`, re-renderują się synchronicznie.
- `AvatarGroup` przycina listę do `max_visible` i sam liczy nadwyżkę na „+N” — logika po stronie hosta addonów, nie w HTML-owym `tf-avatar`.
- Zaznaczanie tekstu: inicjały mają `user-select: none` (`controls.css:3221`) — nie da się zaznaczyć tekstu awatara.

## Dostępność

- `tf-avatar` nie ustawia żadnej roli ARIA — jest traktowany jak obraz dekoracyjny/informacyjny. Gdy `src` jest ustawiony, `alt` = inicjały (czyli fallback tekstowy działa dla czytników ekranu nawet przy złamanym obrazie).
- Gdy awatar reprezentuje osobę bez opisu w otoczeniu (np. sam w komórce tabeli), host powinien dodać `aria-label` na elemencie nadrzędnym — `tf-avatar` samo tego nie robi.
- Kontrast inicjał/tło: `accent` (`#a78bfa` na `rgba(99,102,241,0.18)`) i `success`/`danger` (kolor semantyczny na 15% alpha tego samego koloru) nie mają udokumentowanego wyniku AA w `tokens.json` — do zweryfikowania wizualnie, bo tło jest półprzezroczyste na zmiennym kontekście karty.
- `prefers-reduced-motion`: hover-scale nie ma dedykowanej klauzuli w `tf-avatar`; pokrywa ją globalna reguła wildcard w `controls.css:9586`, o ile strona ładuje `controls.css` (nie dotyczy stron korzystających wyłącznie z `style.css`, patrz odstępstwa modala/gauge'a dla analogicznego problemu).

## Responsywność i platformy

- `tf-avatar` ma stałe rozmiary px (28/36/48) niezależne od breakpointu — nie skaluje się z density platformy.
- Wariant protokołu skaluje się przez `em`, więc dziedziczy `font-size` kontekstu (np. gęstszy layout na ESP32-P4 może pomniejszyć awatar bez osobnej reguły).
- Na ESP32-P4 (`platform.esp32p4_tab5`, `shadows: flat`) hover-scale nie ma znaczenia (brak wskaźnika) — awatar renderuje się statycznie w rozmiarze bazowym.
- `AvatarGroup` nie ma zdefiniowanego zachowania overflow na wąskich ekranach poza stałym `max_visible` — host musi świadomie zmniejszyć `max_visible` na telefonie.

## Tokeny użyte

- `color.themes.dark.accent.soft`, `color.themes.dark.accent.secondary` — tło/tekst tonu `accent`.
- `color.themes.dark.semantic.success.value` / `.soft`, `color.themes.dark.semantic.critical.value` / `.soft` — tony `success`/`danger`.
- `color.themes.dark.bg.elevated`, `color.themes.dark.text.secondary` — tło/tekst wariantu `initials` bez ustawionego tonu (protokół `neutral`).
- `radius.circle` — kształt `circle` (domyślny w obu implementacjach).
- `radius.md` (odpowiednik `0.5em` w kontekście) — kształt `rounded` (tylko wariant protokołu).
- `typography.scale.caption_strong` / `.h4` — zbliżony rozmiar czcionki inicjałów w `md`/`lg` (12px/16px nie mapują się 1:1 na żaden krok skali — patrz odstępstwa).
- `motion.easing.standard` (`--tf-spring-smooth`) — przejście hover.

## Znane odstępstwa w kodzie (2026-09-14)

1. **Trzy niezależne implementacje „avatara” bez wspólnego źródła prawdy.** `tf-avatar.js` (sm/md/lg w px, tony `accent/success/danger`), blok protokołu `.tf-avatar-block`/`.tf-avatar__source` (xs-xl w em, `Tone` pełny, kształty, status dot, deterministyczna paleta gradientów `--auto-0..5`) i ad-hoc `.avatar.user`/`.avatar.assistant` w czacie (`style.css:3065-3080`) oraz `.user-chip .avatar` (`style.css:457-463`) nie dzielą ani nazw klas, ani skali rozmiarów, ani wektora tonów. Migracja do jednego komponentu jest opisana jako otwarty temat w `design/README.md` (rozjazd v1→v2).
2. **Rozmiary `tf-avatar` (28/36/48px) łamią siatkę 4px w nieoczywisty sposób** — same są wielokrotnością 4, ale nie odpowiadają żadnemu krokowi `control.height` (28/36/44) ani `spacing`; to przypadkowa zgodność, nie odwołanie do tokena.
3. **Awatar „maskotki AI” w czacie to statyczna ikona w kółku, nie animowany `tf-face`.** `.avatar.assistant` (`style.css:3075-3080`) to `<div>` z wklejonym SVG sprite'em `model` (22×22, w `tf-chat-panel` 14×14 — `controls.css:4318`), całkowicie odrębny od `tf-face.js` (duży, osobny komponent 3D blendshape używany w hero/voice-mode). Kto szuka „maskotki w czacie” w kodzie, znajdzie tę statyczną ikonę, nie `tf-face`.
4. **Hardcoded hex w tle awatara użytkownika w czacie:** `.msg-row .avatar.user { background: linear-gradient(135deg, #06b6d4, #3b82f6); }` (`style.css:3072`) — nie ma odpowiednika w `color.gradient.*` z `tokens.json`; najbliższy byłby nowy token gradientu (np. `color.gradient.chat_user`), którego dziś nie ma.
5. **`tf-avatar.js` nie ma fallbacku `onerror` dla złamanego `src`** (`tf-avatar.js:49-53`) — pusty/uszkodzony URL renderuje pusty okrąg z samym `alt`, zamiast automatycznie przełączyć się na inicjały, mimo że oba dane (`initials` i `src`) są dostępne w atrybutach jednocześnie.

## Przykłady

```html
<!-- tf-avatar: inicjały, ton success, rozmiar md -->
<tf-avatar initials="PJ" tone="success" size="md"></tf-avatar>

<!-- tf-avatar: zdjęcie, rozmiar lg -->
<tf-avatar src="/avatars/piotr.jpg" initials="PJ" size="lg"></tf-avatar>
```

```rust
// natywnie (tenta-ui-widgets, API docelowe)
Avatar::initials("PJ")
    .tone(Tone::Success)
    .size(AvatarSize::Md)
    .status(Some(AvatarStatus::Online));

AvatarGroup::new(vec![avatar_a, avatar_b, avatar_c])
    .max_visible(3)
    .overlap(AvatarOverlap::Default);
```
