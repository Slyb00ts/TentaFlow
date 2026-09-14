# Typografia

Ten dokument opisuje skalę typograficzną TentaFlow — rodziny fontów, 15 stylów
`TextStyle`, reguły rozmiaru i metryki tekstu potrzebne natywnemu rendererowi do
piksel-dokładnego zaznaczania i karetki. Jedynym źródłem wartości jest
[`tokens/tokens.json`](../tokens/tokens.json) → `typography`.

## Rodziny fontów (`typography.family`)

| Token | Font | Wagi | Fallback | Status |
|---|---|---|---|---|
| `family.sans` | Manrope | 400, 500, 600, 700, 800 | `-apple-system, BlinkMacSystemFont, Segoe UI, system-ui, sans-serif` | canonical, ładowany dziś z Google Fonts |
| `family.mono` | JetBrains Mono | 400, 600 | `ui-monospace, SF Mono, Menlo, Consolas, monospace` | canonical — równoległy stos `'SF Mono', monospace` w `style.css` jest **wycofany** |

**Decyzja: JetBrains Mono.** W żywym CSS współistnieją dziś dwa stosy
monospace — `'JetBrains Mono', ui-monospace, monospace` w większości miejsc i
`'SF Mono', monospace` w kilku deklaracjach starszych (`style.css`, terminal/kod).
Zgodnie z `typography.family.mono.decision` w tokens.json, JetBrains Mono jest
kanoniczny; wystąpienia `'SF Mono'` to dług migracyjny do usunięcia (patrz
`design/MIGRATION.md`), nie alternatywa do wyboru w nowym kodzie.

## Skala `TextStyle` (`typography.scale`)

Klucze skali pokrywają się 1:1 z enumem `TextStyle` w
`tentaflow-sdk-spec/src/protocol/ui/tokens.rs`. Rozmiar i `line_height` w
logicznych px, `weight` to waga Manrope (albo mono dla `code`/`mono`), `tracking`
w em. Natywny renderer zaokrągla `size * scale_factor` do całych pikseli
urządzenia — nigdy nie renderuje na pół piksela.

| `TextStyle` | Rozmiar | Line-height | Waga | Tracking | Transform | Użycie |
|---|---|---|---|---|---|---|
| `display` | 32 | 38 | 800 | −0.02em | — | hero, liczby na pulpicie |
| `title` | 24 | 30 | 700 | −0.01em | — | tytuł strony (`<tf-screen>` header) |
| `h1` | 20 | 26 | 700 | −0.01em | — | nagłówek sekcji nadrzędnej |
| `h2` | 16 | 22 | 600 | 0 | — | nagłówek karty/panelu (`tf-section-card` header) |
| `h3` | 14 | 20 | 600 | 0 | — | podnagłówek wewnątrz karty |
| `h4` | 13 | 18 | 600 | 0 | — | etykieta grupy pól, nagłówek tabeli |
| `body_lg` | 14 | 21 | 400 | 0 | — | treść w dialogach/potwierdzeniach |
| `body` | 13 | 20 | 400 | 0 | — | domyślny tekst — **rozmiar bazowy UI** |
| `body_strong` | 13 | 20 | 600 | 0 | — | wyróżniona wartość w wierszu (np. label: wartość) |
| `caption` | 11 | 16 | 400 | 0 | — | metadane, znacznik czasu, pomocniczy opis pola |
| `caption_strong` | 11 | 16 | 600 | 0 | — | wyróżniony caption (np. liczba w badge) |
| `overline` | 10 | 14 | 700 | 0.08em | uppercase | etykieta kategorii nad tytułem — **najmniejszy dozwolony rozmiar w UI** |
| `code` | 12 | 19 | 400 | — | — | mono, blok kodu (`family: mono`) |
| `mono` | 12 | 18 | 400 | — | — | mono, wartość inline (ID, hash) (`family: mono`) |
| `quote` | 13 | 20 | 400 | — | — | cytat/dymek (`font-style: italic`) |

Mapowanie na HTML/`tf-*` nie jest 1:1 zakodowane w CSS dziś (per-page CSS ma
własne literalne `font-size`, patrz sekcja „Stan dzisiejszy" niżej) — powyższa
kolumna „Użycie" to intencja docelowa dla nowych stron, nie opis obecnego stanu.
Elementy, które powinny sięgać po dany styl: `title`/`h1` → `<tf-detail-header>`,
`h2` → nagłówek `<tf-section-card>`, `body`/`body_strong` → tekst wewnątrz
`<tf-table>`/`<tf-list>`/formularzy, `caption` → `<tf-key-value>` etykiety,
`overline` → etykiety kategorii nad `title`, `code`/`mono` → `<tf-code-editor>`,
`<tf-terminal>`, wartości ID.

## Reguły (twarde)

1. **Body = 13px.** `typography.rules.min_body_size_px = 13` — żaden tekst
   traktowany jako treść główna nie schodzi poniżej.
2. **Minimum absolutne w UI = 10px** (`typography.rules.min_any_size_px`) —
   wyłącznie `overline`; nic mniejszego nie istnieje w skali.
3. **Połówki pikseli zabronione** (`typography.rules.half_pixel_sizes`) —
   `11.5px`/`12.5px`/`10.5px`/`13.5px`/`14.5px`/`9.5px`/`8.5px`, obecne dziś w
   `css/*.css` (efekt mnożników typu `* 0.9`), są długiem migracyjnym, nie
   wzorcem. Każdy nowy rozmiar to wartość z tabeli `TextStyle` powyżej, bez
   wyjątków.
4. **Zaokrąglanie całopikselowe na natywnym CPU rendererze.** Renderer CPU
   (ESP32-P4) liczy `round(size_px * scale_factor)` przed rasteryzacją glifu —
   nigdy nie interpoluje subpikselowo; to samo dotyczy pozycji baseline.

## Metryki tekstu dla natywnego renderera

Piksel-dokładne zaznaczanie i karetka na natywnym rendererze (CPU/wgpu) wymagają
metryk fontu wykraczających poza `size`/`line_height` z tabeli `TextStyle`:

- **ascent / descent** — wysokość glifu powyżej/poniżej baseline; suma
  `ascent + descent` (+ ewentualny `line gap`) daje wysokość linii używaną do
  pozycjonowania karetki w pionie, niezależnie od zadeklarowanego `line_height`
  w tokenie (który jest wartością projektową do CSS, nie surową metryką fontu).
- **line gap** — dodatkowy odstęp między liniami raportowany przez sam font;
  renderer dodaje go do `ascent + descent`, nie zastępuje `line_height`.
- **advance (szerokość przesunięcia)** — ile pikseli przesuwa się kursor po
  narysowaniu glifu; podstawa trafień myszą/dotykiem na pozycję znaku
  (hit-testing) i pozycjonowania karetki w poziomie.
- **kerning** — korekta odstępu między konkretną parą glifów; musi być
  uwzględniona przy sumowaniu `advance` w pętli, inaczej pozycja karetki
  rozjeżdża się z tym, co widać po prawej stronie zaznaczenia wieloznakowego.

Autor strony nie oblicza tych wartości ręcznie — są własnością implementacji
renderera tekstu (fontdb/rasterizer), nie tokenów. Ten fragment tłumaczy *po co*
metryki istnieją, żeby autor rozumiał, dlaczego `line_height` z tokena i
rzeczywista wysokość linii na ekranie ESP32-P4 mogą się nieznacznie różnić.

## Polityka zaznaczania tekstu

`typography.rules.text_selection`: zaznaczanie jest dozwolone **wszędzie z
wyjątkiem** przycisków, ikon i uchwytów przeciągania (drag handles) — tam
`user-select: none` jest zamierzone (klik nie może przypadkowo zaznaczyć etykiety
przycisku zamiast go aktywować).

## i18n

Aplikacja obsługuje 5 języków: `pl`, `en`, `fr`, `es`, `de`
(`tentaflow-core/www/i18n/{pl,en,fr,es,de}.json`), wszystkie LTR — **RTL nie
jest wspierane i nie jest planowane** (żadnego `dir="rtl"` w kodzie); to jest
świadome ograniczenie zakresu, nie luka do wypełnienia. Manrope pokrywa polskie
znaki diakrytyczne w formie prekomponowanej (ą, ć, ę, ł, ń, ó, ś, ź, ż jako
pojedyncze punkty kodowe Unicode, nie kombinacje bazowy-znak + akcent) — autor
strony nie musi normalizować tekstu przed renderem.

## Self-hosting fontów — dziś i plan

Dziś Manrope ładuje się przez Google Fonts (`tentaflow-core/www/index.html:15`):

```html
<link href="https://fonts.googleapis.com/css2?family=Manrope:wght@400;500;600;700;800&display=swap" rel="stylesheet">
```

Zero `@font-face` w repozytorium — brak self-hostowanego fallbacku, brak plików
fontów w `www/`. To jest ryzyko dla trzech scenariuszy, w których TentaFlow ma
działać:

- **Offline / PWA** — instalacja bez połączenia z internetem traci font (spada
  na fallback systemowy), łamiąc spójność wizualną poza kontrolą aplikacji.
- **ESP32-P4** — panel nie ma przeglądarki ani dostępu do Google Fonts; natywny
  renderer musi mieć font zapisany lokalnie w firmware.
- **Prywatność/sieć** — każde żądanie do `fonts.googleapis.com` to zależność
  sieciowa i wyciek adresu IP do Google przy każdym starcie aplikacji.

Plan (zgodny z `platform.esp32p4_tab5.fonts` w tokens.json: „Latin+Polish
subset, 4 sizes × 2 weights"): self-hostować podzbiór Manrope ograniczony do
alfabetu łacińskiego + polskich diakrytyków, resztę platform (web, mobile)
serwować pełniejszym subsetem z własnej domeny zamiast Google Fonts, a na
ESP32-P4 wbudować w firmware tylko 4 rozmiary × 2 wagi (nie całą skalę
`TextStyle` × wszystkie wagi — panel dotykowy 720×1280 nie potrzebuje 15
stylów w 5 wagach jednocześnie zapisanych we flashu).

## Checklist dla autora strony

- [ ] Każdy tekst używa jednego z 15 stylów `TextStyle`, nie literalnego
      `font-size`.
- [ ] Żaden rozmiar nie jest połówką piksela.
- [ ] Treść główna (body) nie schodzi poniżej 13px; nic w UI nie schodzi
      poniżej 10px.
- [ ] Mono używa JetBrains Mono, nie SF Mono.
- [ ] Tekst nie jest niezaznaczalny, chyba że to przycisk/ikona/uchwyt
      przeciągania.
- [ ] Nowe klucze i18n dodane we wszystkich 5 językach (`pl/en/fr/es/de`).
