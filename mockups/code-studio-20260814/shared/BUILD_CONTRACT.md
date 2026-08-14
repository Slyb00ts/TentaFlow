# KONTRAKT BUDOWY MOCKUPÓW — Code Studio

Obowiązuje `mockups/projekty-20260723/shared/BUILD_CONTRACT.md` (head, sidebar, sprite ikon, okna
jako overlaye, kompletność CRUD, odstępy, język polski). Poniżej **tylko różnice** tego zestawu.
Wzorzec jakości i źródło prawdy dla układu: `k01-konsola.html`.

## Różnice względem kontraktu bazowego

1. **Head**: dodatkowo `<meta name="viewport" content="width=device-width, initial-scale=1,
   viewport-fit=cover">` i wersjonowane arkusze (`shared/styles.css?v=N`,
   `shared/code-studio.css?v=N`). Wersję podbijamy przy każdej zmianie CSS — bez tego przeglądarka
   podaje stary arkusz i maskuje poprawkę.
2. **Sidebar**: aktywny jest `#i-terminal` „Code Studio"; pozycja stoi przed „Projekty".
3. **Jeden plik = oba układy.** Ekran ma być responsywny naprawdę: poniżej 900 px przechodzi w układ
   telefonu. Osobne ramki telefonu (`m01`) są wyłącznie poglądowe.
4. **Pełna wysokość okna.** `html, body { height:100%; overflow:hidden }`, łańcuch
   `.screen → .screen-frame → .app → .main → .cs-shell` z `flex:1; min-height:0`. Przewijają się
   wyłącznie: strumień rozmowy, ciało doku i treść w scenie. Poniżej 900 px `.screen-header` znika.
5. **JS jest dozwolony i wymagany** do przełączania widoków (inline, na końcu `<body>`, bez
   zależności). Stan trzymają trzy atrybuty na `.cs-shell`:
   - `data-stage` — co jest w scenie (`konsola|plik|zmiany|terminal|subagent|commit`),
   - `data-dock` — która kategoria w doku (`agenci|pliki|zmiany|git|terminal`),
   - `data-view` — co widać na telefonie (te same nazwy co dolny pasek).
   `data-stage` **musi** mieć wartość startową w HTML, inaczej żaden panel sceny nie pasuje.

## Pułapki, które już nas kosztowały

- **`flex-shrink` w kolumnie przewijanej.** `.cs-stream` to kolumna flex o ograniczonej wysokości,
  więc jej dzieci trzeba pinować `flex-shrink: 0`. Bez tego przeglądarka ściska wiadomości i karty
  w pionie: teksty się urywają, a bloki dostają własne paski przewijania.
- **Dwa zestawy reguł widoczności.** Przy zmianie układu usuwaj stare reguły `@media`, nie dokładaj
  obok. Współistniejące „ukryj scenę" i „ukryj dok" dały widoki, w których nie było widać nic.
- **Kontrolka po stronie, z której rzecz wychodzi.** Arkusz workspace'ów zjeżdża z góry, bo przycisk
  jest u góry; szuflada drzewa wychodzi z prawej, więc przycisk stoi po prawej.
- **Stan aktywny nie może być ukryty.** Pytanie agenta jest zawsze widoczne wraz z opcjami; nie ma
  wariantu „rozwiń, żeby odpowiedzieć".

## Słownik statusów (jeden dla całego modułu)

| Klasa | Znaczenie | Kolor |
|---|---|---|
| `.cs-dot.run` | pracuje | indygo, wolny puls |
| `.cs-dot.ask` | czeka na Ciebie | bursztyn, szybszy puls + skok skali |
| `.cs-dot.ok` | zakończone | zieleń, bez animacji |
| `.cs-dot.idle` | w kolejce / bezczynny | szary, bez animacji |
| `.cs-dot.err` | błąd | czerwień, puls |

## Animacje

Zestaw pochodzi z `connection-overlay.css` i `update-overlay.css`, żeby moduł nie brzmiał obco:
sprężyna `cubic-bezier(.34,1.56,.64,1)` na wejściu kart i arkuszy, puls kropki z rozchodzącym się
cieniem, obrót ikony przy pracującym narzędziu. Własne dla konsoli: przesuwający się połysk na
wierszu narzędzia w trakcie, sprężysty „pop" znacznika po zakończeniu, płynący pasek na lewej
krawędzi karty pracującego subagenta, migający kursor przy strumieniowanym tekście.
