# Checklist: nowa strona

Krok po kroku, od mockupu do wdrożenia. Skrócona wersja żyje w
[`../README.md`](../README.md#jak-dodać-nową-stronę); to jest pełna,
kopiowalna lista. Każdy krok odsyła do realnego pliku, nie do teorii.

## 1. Mockup

- [ ] Skopiuj `mockups/component-catalog/` (lub najnowszy folder z kierunkiem
      wizualnym, np. `mockups/tentaquant-20260903/`) jako punkt startowy —
      **nie** projektuj od pustej kartki.
- [ ] Zbuduj mockup z realnych tokenów (`design/tokens/tokens.json`), nie z
      dowolnych hexów — mockup, który nie da się później zmapować na tokeny,
      trzeba będzie przerabiać dwa razy.
- [ ] Nazwij folder `mockups/<nazwa>-<YYYYMMDD>/`.

## 2. Archetyp

- [ ] Zidentyfikuj, który z sześciu wzorców z
      [`page-anatomy.md`](page-anatomy.md) pasuje: lista+detail, formularz,
      dashboard, czat, edytor, widok 3D. Jeśli żaden nie pasuje w 100% —
      wybierz najbliższy i udokumentuj różnicę w komentarzu nagłówka modułu
      (wzorem `users.js`: „Uzywa `<tf-screen>` jako shell. […]”).
- [ ] Sprawdź, czy strona potrzebuje topbara — **nie potrzebuje**, na
      desktopie topbara nie ma (patrz `page-anatomy.md` §Shell). Nagłówek
      strony (`h1`, breadcrumb, akcje) idzie w `slot="header"`
      `<tf-screen>`.

## 3. Audyt komponentów

- [ ] Przejrzyj [`../components/README.md`](../components/README.md) — czy
      wszystkie potrzebne `tf-*` już istnieją?
- [ ] Brakujący komponent: najpierw spec w `components/<nazwa>.md` (kopia
      `components/_TEMPLATE.md`), potem implementacja
      `www/js/components/tf-<nazwa>.js`, dopiero potem użycie na stronie.
- [ ] Style strony (`www/css/<strona>.css`) mogą tylko **układać** komponenty
      (grid, flex, odstępy) — nie redefiniować ich wyglądu (border, kolor,
      cień żyją w `controls.css`/tokenach).

## 4. Klucze i18n w 5 językach

- [ ] Dodaj klucze do `www/i18n/{pl,en,de,es,fr}.json` — **wszystkie pięć
      naraz**, nie tylko `pl.json` z planem dokończenia później.
- [ ] Wartości w `en.json` nie mogą być kopią klucza (`nav.foo` = `nav.foo`
      to martwy placeholder).
- [ ] Placeholdery interpolacji (`{name}`, `{n}`) muszą być identyczne we
      wszystkich pięciu plikach — patrz test „interpolation placeholders
      match the Polish source” w
      `js/modules/tentanas/i18n-parity.test.js`.
- [ ] Diakrytyki w `de`/`es`/`fr`/`pl` nie mogą być „odchudzone" do ASCII
      (`sie`→`się`, `fuer`→`für`, `accion`→`acción`) — patrz test „no locale
      carries a value with its diacritics stripped” w tym samym pliku.
- [ ] Jeśli strona jest duża/ma własny namespace i18n (jak `tentanas.*`,
      `tentaquant.*`) — skopiuj wzorzec testu parzystości z
      `js/modules/tentanas/i18n-parity.test.js` do
      `js/modules/<nazwa>/i18n-parity.test.js`, dostosuj `NAMESPACE` i
      `SINGLE_KEYS`.

## 5. Szkielet modułu

- [ ] Plik `www/js/modules/<nazwa>.js`, eksport domyślny obiekt-ekran.
- [ ] Zarejestruj w `Router.register('<id>', <Nazwa>Screen)` w `app.js`
      (blok wywołań `Router.register(...)` przy końcu `renderApp()`).
- [ ] Zaimplementuj **jeden** z dwóch trybów Routera
      (`www/js/router.js`):

  ```js
  // Tryb 1 — screen.show(params): pełna kontrola nad #main, potrzebna
  // gdy ekran ma drill-down bez zmiany route (jak cluster-detail).
  const Screen = {
    async show(params) { /* renderuje wprost do #main */ },
    cleanup() { /* zatrzymuje interwały, subskrypcje SSE */ },
  };

  // Tryb 2 — render()/mount(): standardowy ekran sidebar (większość stron).
  const Screen = {
    title: 'Nazwa',
    render() { return `<tf-screen>...</tf-screen>`; },
    async mount(params) { /* bind eventów, load danych, start refreshera */ },
    async canUnmount() { return true; }, // patrz krok 8
    unmount() { /* stop refresher, unsubscribe, wyczyść stan modułu */ },
  };
  ```
- [ ] `unmount`/`cleanup` **musi** zatrzymać każdy `setInterval`,
      `createRefresher`, subskrypcję `ApiBinary.subscribe`/SSE i listener na
      `document`/`window` założony w `mount`/`show` — inaczej po nawigacji
      dalej działa w tle (patrz `clusters.js`: `stopRefresh()` wołane i w
      `unmount()`, i wewnątrz samego `refresher.run` gdy `.clusters-shell`
      zniknął z DOM).

## 6. CSS tylko dla układu

- [ ] `www/css/<nazwa>.css` — wyłącznie `display`, `grid-template-columns`,
      `gap`, `padding`/`margin` z tokenów spacing, media queries.
- [ ] Zero surowego hexa (`#6366f1` zamiast `var(--accent-1)`), zero
      połówek pikseli (`11.5px`), zero literalnych rozmiarów fontu spoza
      skali `typography.scale` w `tokens.json`.
- [ ] Dodaj `<link rel="stylesheet" href="/css/<nazwa>.css">` do
      `www/index.html` (wzorem pozostałych ~35 wpisów w `<head>`).

## 7. Stany (empty / loading / error / offline)

Patrz [`states.md`](states.md) po szczegóły. Skrót:

- [ ] Pusta lista → `<tf-empty-state icon="…" title="…" message="…">` z CTA,
      nie goły napis.
- [ ] Ładowanie o znanym layoucie → `<tf-skeleton>`; nieznanym → `<tf-spinner>`.
- [ ] Błąd sieci → komunikat inline + przycisk retry, **nigdy** pusty ekran
      (`router.js` już ma fallback dla `render()`/`show()` który rzucił —
      nie polegaj wyłącznie na nim, obsłuż błąd we własnym `try/catch`).
- [ ] Utrata połączenia z demonem → nic dodatkowego do zrobienia na poziomie
      strony, `connection-overlay.js` (`ConnectionOverlay.init()` w `app.js`)
      obsługuje to globalnie. Jeśli strona ma WŁASNE połączenie poza
      platformowym (jak Code Studio z węzłem-właścicielem), użyj
      `createConnectionOverlay()` z `js/modules/connection-overlay.js` —
      sprawdź `isPlatformDown()` najpierw, overlay platformy ma pierwszeństwo.

## 8. Klawiatura i fokus

- [ ] Każdy interaktywny element osiągalny Tabem, widoczny fokus (ring 2 px,
      offset 2 px — `control.focus_ring` w tokens.json), nie usuwaj
      `outline` bez zamiennika.
- [ ] Formularz z niezapisanymi zmianami blokuje nawigację przez
      `canUnmount()` zwracające `false`/pytające usera — patrz
      `tentaquant.js`: `canUnmount() { return this.confirmLeaveProjectView(); }`.
- [ ] `Enter` zatwierdza formularz jednopolowy/krótki (patrz
      [`forms.md`](forms.md) §Submit klawiaturą); `Esc` zamyka modal/wizard
      (`tf-window` już to robi — `document.addEventListener('keydown', ...)`
      w `tf-window.js`).

## 9. Breakpointy 640 / 768 / 1024

- [ ] Testuj przy `layout.breakpoints_px` z `tokens.json`: `xs=640`,
      `sm=768`, `md=1024` (plus `lg=1280`, `xl=1536`, `xxl=1920`).
- [ ] Tabele: użyj `<tf-column hide-below="…">` z dozwolonych wartości
      `{480,640,720,900,1024,1180,1280}` (`tf-table.js`,
      `HIDE_BELOW_BREAKPOINTS`) zamiast własnych `@media`.
- [ ] Poniżej `sm` (767 px) sidebar staje się drawerem — strona nie może
      zakładać stałej szerokości sidebara w swoich obliczeniach layoutu.

## 10. `prefers-reduced-motion`

- [ ] Każdy plik CSS strony z własnym `@keyframes` **musi** mieć blok
      `@media (prefers-reduced-motion: reduce) { … }` obok — dziś ma go
      tylko 7 z 40 plików CSS w repo, nie powielaj tego długu.
- [ ] Czasy animacji z `motion.duration_ms`/`motion.easing`, nie literalne
      `0.15s`/`ease`.

## 11. Bramki lintu

- [ ] Brak hexa poza `tokens.css`/`tokens.json`.
- [ ] Brak emoji w UI (dopuszczalne tylko w treści i18n, jeśli produkt tego
      wymaga — nie w markupie/ikonach).
- [ ] Brak `outline: none` bez zamiennika.
- [ ] Brak połówek pikseli w rozmiarach fontu (`11.5px` itp.).
- [ ] `lang` ustawiony na `<html>` (globalne, nie per-strona — już zrobione
      przez `document.documentElement.lang` w `i18n.js`, nie dotykaj).

## 12. Testy

- [ ] `www/js/modules/<nazwa>.test.js` obok modułu (`node:test` + `assert`,
      wzorem plików `*.test.js` w repo — ~18 z 105 komponentów i część
      modułów mają już taki test).
- [ ] Jeśli strona ma własny namespace i18n: test parzystości kluczy (krok 4)
      **plus** test „every literal key the block modules ask for exists”
      (skanuje `T('…')` w kodzie i sprawdza że klucz istnieje w bundlu) i
      test „no locale key … is dead” (odwrotny kierunek) — oba wzorce są w
      `js/modules/tentanas/i18n-parity.test.js`; analogiczny plik istnieje
      dla `tentaquant` (`js/modules/tentaquant/views-quantum.test.js` i
      sąsiednie).

## 13. Odpowiednik natywny

- [ ] Jeśli strona ma być dostępna w natywnym silniku TentaEngine — dodaj
      wpis w [`../../docs/TENTAENGINE_INTEGRATION_PLAN.md`](../../docs/TENTAENGINE_INTEGRATION_PLAN.md)
      (moduł, archetyp z `page-anatomy.md`, komponenty `tf-*` użyte, czy
      wymaga GPU/3D).
- [ ] Jeśli strona NIE ma być dostępna natywnie (np. moduł czysto
      administracyjny web-only) — zapisz to jawnie w tym samym dokumencie,
      nie zostawiaj domysłu.

## Checklist (skrót do wklejenia w PR)

```markdown
- [ ] Mockup w mockups/<nazwa>-<data>/, zbudowany z tokenów
- [ ] Archetyp z page-anatomy.md zidentyfikowany i zacytowany
- [ ] Komponenty z components/README.md, brakujące dostały spec
- [ ] i18n: 5 języków, klucze zgodne (parity test jeśli namespace własny)
- [ ] Router.register + show/unmount albo render/mount/canUnmount
- [ ] unmount/cleanup zatrzymuje WSZYSTKIE interwały/subskrypcje
- [ ] CSS strony = tylko układ, zero hex/half-px poza tokenami
- [ ] empty / loading / error / offline pokryte
- [ ] Klawiatura: fokus widoczny, Enter submit, Esc zamyka modal
- [ ] Breakpointy 640/768/1024 sprawdzone ręcznie
- [ ] prefers-reduced-motion w każdym pliku z @keyframes
- [ ] Testy: moduł + i18n-parity (jeśli dotyczy)
- [ ] Wpis w TENTAENGINE_INTEGRATION_PLAN.md (dostępna natywnie / świadomie nie)
```
