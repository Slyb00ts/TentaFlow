# Nawigacja

Sidebar, breadcrumb, zakładki wewnątrz strony, command palette, routing i
zachowanie na mobile. Źródła: `www/js/app.js` (drzewo nawigacji, powłoka),
`www/js/router.js` (SPA router), `www/js/components/tf-command-palette.js`.

## Drzewo sidebara — `ADMIN_NAV` / `USER_NAV`

Zdefiniowane jako statyczne tablice w `app.js`, budowane raz przy
`renderApp()` i renderowane w `paint()`:

```js
const ADMIN_NAV = [
  { headingKey: 'nav.section_core', icon: 'core', items: [
    { id: 'mesh', labelKey: 'nav.mesh', icon: 'network' },
    { id: 'clusters', labelKey: 'nav.clusters', icon: 'cluster' },
    { id: 'prompts', labelKey: 'nav.prompts', icon: 'prompt' },
  ]},
  // … section_workflows, section_ai_agents, section_management
];
```

- **Zależność od roli**: admin dostaje `[APPS_NAV, ...ADMIN_NAV,
  ...USER_NAV.slice(1)]`; zwykły user dostaje
  `[APPS_NAV, ...userVisibleAdminSections(), ...USER_NAV.slice(1)]`, gdzie
  `userVisibleAdminSections()` filtruje sekcje admina do pozycji jawnie
  oznaczonych `userVisible: true` (dziś tylko `events`, bo backend sam zwęża
  wynik do `scoped_to_self` gdy caller nie ma `events.read_all`). Pozycje z
  `requiresPowerUser` odpadają dla zwykłego `user` (nie dla `power_user` ani
  `admin` — `isPowerUser = isAdmin || role === 'power_user'`).
- **Kolejność jest stała**: sekcja Apps zawsze pierwsza (wspólna dla każdej
  roli), sekcje admina środkiem, sekcja konta (`USER_NAV.slice(1)`) zawsze
  ostatnia.
- **Grupy** = `.nav-section` z nagłówkiem (`.heading`, ikona + tłumaczony
  tekst) i listą `.nav-item`.
- **Odznaki licznikowe**: `<span class="nav-count" data-count-for="clusters"
  hidden>` — puste/`hidden` dopóki `refreshNavCounts()` (co 30 s + event
  `tf:nav-counts-stale` dla natychmiastowego odświeżenia) nie wypełni ich
  liczbą > 0; wartość `0` chowa odznakę zamiast pokazywać „0”.
- **Aplikacje addonów** dopisywane asynchronicznie na koniec sekcji Apps
  (`injectAddonAppsIntoSidebar()`, po `appsListRequest`), posortowane
  `sortOrder` ASC → `title` ASC. Element addonu ma `data-view` inny niż
  standardowe pozycje: `<target>` dla apek natywnych (z `data-instance`, bo
  jedna paczka może mieć wiele zainstalowanych instancji dzielących `id`
  route'a), `addon-app:<addonId>` dla WASM.

## Tryb ikon-only (collapsed)

`layout.sidebar.collapse_below = "md"` (< 1024 px, `tokens.json`) deklaruje
próg zwijania sidebara do samych ikon (64 px). **W żywym HTML ten tryb nie
istnieje** — poniżej `sm` (767 px) sidebar zamienia się w cały czas w
off-canvas drawer (patrz niżej), nie w wąski pasek ikon. Tryb ikon-only 64 px
jest dziś wyłącznie deklaracją tokena, celem dla przyszłej
implementacji/natywnego renderera — nie kopiuj tego jako opisu obecnego
zachowania w innej dokumentacji.

## Mobile: hamburger + drawer

```text
┌ .mobile-header (≤767px, sticky top, 52px) ─────────────────┐
│ [☰]   TentaFlow logo          [env badge]                   │
└──────────────────────────────────────────────────────────┘
```
- `#mobile-menu-btn` (44×44 px — `control.touch_target_min`) toggle'uje
  `body.drawer-open`; `.sidebar` dostaje `transform: translateX(-100%)` gdy
  zamknięty, `translateX(0)` gdy otwarty (280 px, `max-width: 85vw`, cień
  `8px 0 32px`).
- `.sidebar-backdrop` (półprzezroczysty scrim) zamyka drawer po kliknięciu.
- **Gesty**: swipe od lewej krawędzi (`touchstart.clientX < 20`) o > 60 px
  otwiera drawer; swipe w lewo o < -60 px na otwartym sidebarze zamyka go
  (`setupDrawer()`, `app.js`).
- **Klik pozycji nawigacji na mobile zamyka drawer, ale dopiero po
  potwierdzeniu przez Router**: `const moved = await Router.navigate(view);
  if (moved) closeDrawer();` — odmowa nawigacji (`canUnmount() === false`)
  zostawia drawer otwarty na starej pozycji, nie zamyka go „na wyrost”.

## Topbar

**Na desktopie topbara nie ma** (patrz [`page-anatomy.md`](page-anatomy.md)
§Shell) — komentarz D1 w `app.js` jest wprost: odznaka środowiska
(PROD/TEST/DEV) żyje w stopce sidebara (`#env-sidebar-badge`, obok
`.user-chip`), **nie** w topbarze, „aplikacja go nie ma na desktopie”. Na
mobile rolę topbara przejmuje `.mobile-header` z odpowiednikiem odznaki
(`#env-mobile-badge`). `layout.topbar.height = 56` w `tokens.json` opisuje
powierzchnię dla natywnego/mobilnego odpowiednika, nie dzisiejszy desktop.

## Breadcrumb

Nie jest komponentem `tf-*` — to zwykły `<div class="tf-breadcrumb">` w
`slot="breadcrumb"` `<tf-screen>`, budowany ręcznie per moduł:

```html
<div slot="breadcrumb" class="tf-breadcrumb">
  <span class="crumb" data-action="back">Użytkownicy</span>
  <span class="sep">›</span>
  <span class="crumb current">Adam Kowalski</span>
</div>
```

Reguły z `users.js`:
- Widok listy (najwyższy poziom sekcji) ma breadcrumb **jednoelementowy**:
  `<span class="crumb current">Tytuł sekcji</span>` — sam siebie, bez
  strzałki wstecz.
- Widok drill-down (detail) dostaje **dwa** elementy: poprzedni poziom jako
  klikalny `.crumb[data-action="back"]`, bieżący jako `.crumb.current`
  (nieklikalny).
- Nawigacja wstecz z breadcrumb idzie przez lokalny handler modułu
  (`backToList()`), nie przez `Router.navigate` — drill-down w `users.js`
  jest stanem wewnątrz jednego zarejestrowanego ekranu (`view` = `'list' |
  'user-detail' | 'group-detail'`), nie osobną trasą.
- `cluster-detail.js` używa innego wzorca: `.cluster-detail-topbar` z
  przyciskiem `← Wstecz` (`tf-button variant="ghost"`) zamiast breadcrumbu —
  dopuszczalne dla ekranów montowanych przez `screen.show()` poza Routerem.

## Zakładki wewnątrz strony

`<tf-tabs variant="underline" value="…">` w `slot="tabs"`, z opcjonalnym
`count` per `<tf-tab>`:

```html
<tf-tabs slot="tabs" variant="underline" value="profile" id="ud-tabs">
  <tf-tab id="profile" icon="user">Profil</tf-tab>
  <tf-tab id="groups" icon="users" count="3">Członkostwa</tf-tab>
  <tf-tab id="perms" icon="shield">Uprawnienia</tf-tab>
</tf-tabs>
```
Event `change` (`e.detail.value`) przełącza zawartość panelu poniżej —
**bez** zmiany hasha routera (zakładka to stan wewnątrz jednej strony, nie
osobna trasa; `settings.js`, `users.js` obie tak robią). Wyjątek:
`tentaquant.js` woła `this.setLocation()` przy zmianie zakładki projektu —
moduły z głębszym stanem mogą świadomie odzwierciedlać zakładkę w URL, to
decyzja per moduł, nie zachowanie `<tf-tabs>` samego w sobie.

## Command palette

`<tf-command-palette>` (`www/js/components/tf-command-palette.js`) istnieje,
jest rejestrowany globalnie w `components/index.js` i nasłuchuje
`Cmd/Ctrl+K` na `document` **od chwili gdy jego instancja trafi do DOM**
(`connectedCallback`). **W żywej aplikacji żaden element
`<tf-command-palette>` nie jest dziś montowany** — nie ma go w
`index.html`, żaden moduł nie tworzy go dynamicznie ani nie ustawia
`.items`. Skrót `Cmd/Ctrl+K` w obecnym stanie repo **nic nie robi** w
uruchomionej aplikacji; komponent jest gotowym, przetestowanym blokiem
czekającym na spięcie z drzewem nawigacji (`ADMIN_NAV`/`USER_NAV` +
ewentualnie akcje stron), nie ukrytą, działającą funkcją. Jeśli podłączasz
palette: `TfCommandPalette.open()`/`.close()` to statyczne helpery
(singleton przez `document.querySelector('tf-command-palette')`), `.items`
przyjmuje `[{id, group, title, shortcut}]`.

## Deep linki i routing

`router.js` pisze trasę do `window.location.hash` przez `replaceState` (nie
`pushState`) po każdej udanej nawigacji: `#/<id>?<query>` — jeden wpis
historii na nawigację, nie jeden na repaint. `Router.init(defaultId)`:
hash w pasku adresu wygrywa nad domyślnym ekranem (`Router.fromHash()`),
więc wklejony link otwiera dokładnie ten widok. `hashchange` (wsteczne/do
przodu przeglądarki, ręczna edycja URL) porównuje **i** `id`, **i**
parametry (`paramsKey`) — dwie instancje jednej natywnej aplikacji dzielą
`id` i różnią się tylko `?instance=`, więc porównanie samego `id` pokazałoby
instancję A z adresem mówiącym B.

**Odmówiona nawigacja i URL**: gdy `canUnmount()` zwróci `false` w wyniku
`hashchange`, router przywraca poprzedni URL (`window.history.replaceState
(null, '', ev.oldURL)`) — adres nie może obiecywać widoku, na którym
nikogo nie ma.

## Zachowanie wstecz na mobile

Brak specjalnej obsługi przycisku „wstecz” systemu poza standardowym
`hashchange` — działa tak samo jak na desktopie (jeden wpis historii per
nawigacja). Jedyne mobile-specyficzne zachowanie nawigacyjne to zamykanie
drawera po udanej nawigacji (patrz §Mobile wyżej); gest swipe-wstecz
systemu iOS/Android nie jest przechwytywany osobno — po prostu cofa hash
jak zwykły `history.back()`.

## Zarządzanie fokusem przy zmianie trasy

**Nie znaleziono w `router.js` przenoszenia fokusu na `h1`/nagłówek strony
po nawigacji** — `Router.navigate()` podmienia `#main.innerHTML` i nie
wywołuje `.focus()` na żadnym elemencie nowego widoku. To jest luka wobec
zasady 5 w [`../README.md`](../README.md) („dostępność nie jest opcjonalna”)
i typowej dobrej praktyki SPA: użytkownik czytnika ekranu po nawigacji
zostaje z fokusem tam, gdzie był (zwykle na klikniętym elemencie sidebara),
nie na nowej treści. **Rekomendacja, nie potwierdzone zachowanie**: nowy
kod powłoki powinien po `Router.navigate()` ustawić `tabindex="-1"` i
`.focus()` na elemencie `h1`/`slot="header"` nowego ekranu — do
zaimplementowania, nie do skopiowania z istniejącego miejsca.

## „Wymaga szerszego ekranu” — komunikat viewport

**Nie znaleziono w repo** dedykowanego komponentu/komunikatu blokującego
stronę poniżej pewnej szerokości (żaden i18n klucz, żaden moduł JS/CSS
implementujący taki gate). Zasada 6 w [`../README.md`](../README.md)
(„strony admin mogą wymagać ≥ 1024 px, ale muszą degradować się czytelnie,
nie łamać”) jest dziś realizowana przez zwykłe reguły responsywne
(`hide-below`, `@media`), nie przez blokadę pełnoekranową z komunikatem.
Jeśli strona faktycznie nie da się użyć poniżej `md` (1024 px) — np. edytor
grafu węzłów — udokumentuj to per moduł (analogicznie do `code-studio.js`:
`NARROW_QUERY`/`CARD_QUERY` degradują tabelę, nie blokują całego ekranu) i
rozważ dodanie takiego komunikatu jako nowy, świadomy wzorzec, nie
zakładaj że gdzieś już istnieje.

## Checklist

- [ ] Pozycja w sidebarze: sekcja + `labelKey` i18n + ikona z whitelisty
      sprite'u, licznik tylko gdy > 0.
- [ ] Widoczność pozycji zależna od roli przez `requiresPowerUser`/
      `userVisible`, nie przez ukrywanie w CSS.
- [ ] Breadcrumb: jeden element na liście, dwa (wstecz + current) na
      drill-down; nawigacja wstecz lokalna dla stanu-w-ekranie, `Router
      .navigate` dla osobnej trasy.
- [ ] Zakładki strony przez `<tf-tabs>`, zmiana nie rusza hasha (chyba że
      moduł świadomie odzwierciedla zakładkę w URL).
- [ ] Deep link (`#/<id>?<params>`) działa po wklejeniu i po odświeżeniu.
- [ ] Odmowa nawigacji (`canUnmount() === false`) zostawia URL i sidebar
      spójne ze starym widokiem.
- [ ] Jeśli dodajesz command palette do nowej powłoki: pamiętaj że
      `<tf-command-palette>` nie jest dziś nigdzie zamontowany — musisz go
      dodać do `index.html`/`app.js` i wypełnić `.items`, nie zakładać że
      już działa.
- [ ] Fokus po nawigacji: rozważ jawne przeniesienie na `h1` nowego ekranu
      (dziś nie jest to zrobione centralnie w routerze).
