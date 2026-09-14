# Anatomia strony

Jak zbudowany jest ekran TentaFlow: powłoka aplikacji, `<tf-screen>` i sześć
kanonicznych archetypów stron. Każdy wzorzec jest tu udokumentowany na
podstawie realnego modułu z `tentaflow-core/www/js/modules/`, nie wymyślony —
jeśli potrzebujesz nowego układu, znajdź go poniżej zamiast projektować od
zera (zasada 7 w [`../README.md`](../README.md)).

## Powłoka aplikacji (shell)

Powłokę montuje `www/js/app.js` (`renderApp()` → `paint()`) raz na sesję;
`<tf-screen>` żyje wewnątrz `<main class="main" id="main">`.

```text
┌───────────────────────────────────────────────────────────┐
│ .app  (grid: [sidebar] [1fr], 100dvh)                      │
│ ┌───────────┬─────────────────────────────────────────────┐│
│ │  sidebar  │  main #main                                  ││
│ │  240 px   │  (tu montuje się <tf-screen> aktywnego       ││
│ │ (zwinięty │   widoku — Router.navigate wymienia          ││
│ │  64 px)   │   #main.innerHTML)                            ││
│ │           │                                               ││
│ │ logo      │                                               ││
│ │ nav-      │                                               ││
│ │  sections │                                               ││
│ │ (scroll)  │                                               ││
│ │ ─────     │                                               ││
│ │ env badge │                                               ││
│ │ lang      │                                               ││
│ │ user-chip │                                               ││
│ │ logout    │                                               ││
│ └───────────┴─────────────────────────────────────────────┘│
└───────────────────────────────────────────────────────────┘
```

| Element | Token (`design/tokens/tokens.json`) | Wartość | Uwaga |
|---|---|---|---|
| Sidebar rozwinięty | `layout.sidebar.expanded` | 240 px | **Drift**: żywe `css/style.css` (`.app { grid-template-columns: 260px 1fr }`) nadal renderuje 260 px. `tokens.json` jest kanoniczny (`README.md` „Hierarchia źródeł prawdy") — nowy/przepisywany kod powłoki ma celować w 240, nie kopiować 260. |
| Sidebar zwinięty | `layout.sidebar.collapsed` | 64 px | Próg zwijania: `layout.sidebar.collapse_below = "md"` (< 1024 px). W obecnym HTML sidebar poniżej `sm` (767 px) nie zwija się do 64 px ikon — staje się off-canvas drawerem (`.sidebar { position: fixed; transform: translateX(-100%) }`, szerokość 280 px, otwierany `#mobile-menu-btn` / swipe z krawędzi). Tryb ikon-only z tokena jest celem dla natywnego renderera, nie dzisiejszym zachowaniem HTML. |
| Topbar | `layout.topbar.height` | 56 px | **Na desktopie aplikacja HTML nie ma topbara** (komentarz D1 w `app.js`: odznaka środowiska żyje w stopce sidebara, „NIE w topbarze — aplikacja go nie ma na desktopie"). Token opisuje powierzchnię, którą ma dostać natywny/mobilny odpowiednik. Na mobile (≤ 767 px) rolę topbara pełni `.mobile-header`, wysoki **52 px** (nie 56 — kolejny drobny drift do wyrównania przy migracji). |
| Treść, szerokość maks. | `layout.content.max_width` | 1440 px | Górny limit szerokości treści strony (dashboard, listy). |
| Treść, wariant wąski | `layout.content.narrow` | 800 px | Formularze i czat centrują się do tej szerokości (patrz `chat.js`: `padding-inline: max(24px, calc((100% - 800px)/2))`). |
| Odstęp sekcji | `spacing.layout.section` | 48 px | Odstęp pionowy między głównymi sekcjami strony (hero → stat-grid → sekcje listy w `dashboard.js`). |
| Odstęp strony | `spacing.layout.page` | 64 px | Górny/dolny margines całej strony w układach pełnoekranowych (aspiracyjny — HTML dziś używa głównie `spacing.xl`/`spacing.xxl`, 24/32 px, w praktyce). |

## `<tf-screen>` — regiony

`www/js/components/tf-screen.js` to bezatrybutowa powłoka ekranu. Przy
pierwszym `connectedCallback()` rozdziela dzieci po atrybucie `slot=` do
wewnętrznych kontenerów; **wszystko bez `slot=` trafia do body**. Puste
kontenery (`crumbs`, `header`, `tabs`) dostają `hidden` i nie zajmują miejsca.

```text
<tf-screen>
  ├─ .tf-screen-head              (sticky)
  │   ├─ .tf-screen-head__crumbs  ← slot="breadcrumb"
  │   ├─ .tf-screen-head__header  ← slot="header"  (.tf-page-header | .tf-detail-header)
  │   └─ .tf-screen-head__tabs    ← slot="tabs"    (<tf-tabs>)
  └─ .tf-screen-body              (scrollowalny)    ← default slot (reszta treści)
```

Reguły treści nagłówka (`.tf-detail-header` — patrz `users.js`):

| Slot | Zawiera | Przykład |
|---|---|---|
| `breadcrumb` | trail `.crumb` (`current` na ostatnim), separator `›` między poziomami | `Użytkownicy › Adam Kowalski` |
| `header` | ikona/awatar (`.big-ico`), `h1`-poziom tytuł (`.d-name`), podtytuł (`.d-sub`), opcjonalne odznaki (`.d-badges`, chipy statusu), akcje (`.d-actions`, `tf-button`) | patrz §Anatomia nagłówka niżej |
| `tabs` | `<tf-tabs variant="underline">` z licznikami (`count="…"`) | zakładki Profil / Grupy / Uprawnienia |

**Nagłówek strony (`h1`)** żyje w `slot="header"`, nigdy w topbarze — na
desktopie topbara nie ma (patrz wyżej). Tytuł, breadcrumb i akcje strony
mieszkają razem w tym samym pasku sticky nad treścią; jedyny element poza
`<tf-screen>`, który dziś pełni rolę „topbara”, to sidebar (nawigacja, user
chip, przełącznik języka) i — tylko na telefonie — `.mobile-header`.

Starsze moduły (`clusters.js`, `dashboard.js`, `settings.js`) **nie używają
`<tf-screen>`** — renderują `.page-header` (h1 + `.sub` + `.actions`)
bezpośrednio jako pierwszy element treści. Oba wzorce współistnieją;
`users.js` dokumentuje w komentarzu nagłówka, że `<tf-screen>` + sloty to
kierunek docelowy („wzorzec wg mockupu addons-permissions”). **Nowe strony
mają używać `<tf-screen>`.**

## Sześć archetypów strony

### (a) Lista + drill-down — `clusters.js` / `cluster-detail.js`

```text
┌ .page-header ───────────────────────────────────────────┐
│ h1 „Klastry"           .sub „3 klastry · 2 healthy”  [+ Nowy]│
├───────────────────────────────────────────────────────────┤
│ .clusters-grid (kafle)                                      │
│ ┌ .cluster-card ─────┐ ┌ .cluster-card ─────┐               │
│ │ ico  nazwa  status │ │ ...                 │               │
│ │ [4× gauge ring]     │ │                     │               │
│ │ node-chipy          │ │                     │               │
│ │ link-chipy          │ │                     │               │
│ │ footer meta         │ │                     │               │
│ └─────────────────────┘ └─────────────────────┘             │
└───────────────────────────────────────────────────────────┘
      │ klik kafla → ClusterDetailScreen.show(id) (poza Routerem)
      ▼
┌ .cluster-detail-topbar ──────────────────────────────────┐
│ ← Wstecz   nazwa + status chip        [Edytuj][Test][Wdróż]│
├──────────┬───────────────┬───────────────────────────────┤
│ nodes    │ diagram SVG   │ summary card                  │
│ column   │ topologii     │ (CPU/RAM/VRAM sumy, strategia)│
├──────────┴───────────────┴───────────────────────────────┤
│ macierz połączeń (tabela N×N)                             │
│ sekcja RDMA / deploy / routing / shared models             │
└───────────────────────────────────────────────────────────┘
```
Lista = kafle z auto-refresh 5 s (`createRefresher`). Klik nie zmienia hasha
przez `Router.navigate` — `ClusterDetailScreen.show(id)` renderuje wprost do
`#main` i sam wiąże przycisk powrotu. Edycja/tworzenie otwiera **wizard**
(`ClusterWizard.open`) w `<tf-window modal draggable>`, nie osobną stronę.

### (b) Ustawienia / formularz — `settings.js`

```text
┌ .page-header:  h1 „Ustawienia” ──────────────────────────┐
├ <tf-tabs variant="underline">: Ogólne│SSO│OAuth│TLS│Mesh… ┤
├ #settings-tab-body ───────────────────────────────────────┤
│ .card                                                      │
│  .card-header  h3 + akcja odśwież                          │
│  .card-body    table.data-table LUB form-row + tf-input…   │
│                 przycisk Zapisz na dole karty               │
└──────────────────────────────────────────────────────────┘
```
Każda zakładka = własny `render*Tab()` + `bind*Tab()`, podmieniane w
`#settings-tab-body` przy `change` na `<tf-tabs>`. Karty grupują pola
tematycznie (§ patrz [`forms.md`](forms.md)).

### (c) Dashboard / statystyki — `dashboard.js`

```text
┌ .page-header:  h1 + [Odśwież][+ Dodaj node] ──────────────┐
├ .hero (canvas particles + orby + maskotka) ────────────────┤
├ .stat-grid (auto-fit, minmax 180px)  3× stat-card ─────────┤
├ .mesh-section-title „Metryki” ──────────────────────────────┤
├ .stat-grid  4× stat-card (tps/active/errors/services) ─────┤
├ .stat-grid  3× stat-card (cpu/ram/total) ───────────────────┤
├ 2-kolumnowy grid: „Ostatnie zdarzenia” │ „Aktywne przepływy”│
└──────────────────────────────────────────────────────────┘
```
Odświeżanie: `createRefresher` co 5 s (widoczna karta) / 20 s (ukryta), plus
subskrypcja `AuditEvent` z serwera do live-feedu zdarzeń
(`client.addUnsolicitedListener`).

### (d) Chat / stream — `chat.js`

```text
┌───────────┬──────────────────────────────────────────────┐
│ konwersacje│ model picker + tytuł + akcje                 │
│ 296 px    ├──────────────────────────────────────────────┤
│ (grupy:   │        [wycentrowana kolumna max 800px]       │
│ Dziś/     │        wirtualizowane dymki (VirtualList)      │
│ Wczoraj/  │        streaming: przyrostowa wysokość ogona   │
│ Wcześniej)│──────────────────────────────────────────────┤
│           │        composer (pill, wysyłka Enter)          │
└───────────┴──────────────────────────────────────────────┘
```
Centrowanie treści: `padding-inline: max(24px, calc((100% - 800px)/2))` —
dokładnie `layout.content.narrow`. Historia trzymana lokalnie
(`localStorage`), tryb audio przełącza state machine listen/think/speak
(`color.themes.dark.voice.*` w tokens.json).

### (e) Edytor / workbench — `code-studio.js`, `flows-builder.js`

```text
┌ workspace bar ───────────────────────────────────────────┐
├ dock (drzewo plików / paleta) │ stage (edytor/canvas/terminal)│
│  panel boczny                 │  panel główny + panel config │
└────────────────────────────────────────────────────────────┘
```
`code-studio.js` renderuje wyłącznie *powłokę* sesji (4 atrybuty stanu na
`.cs-shell`, pasek workspace, phone sheet) — panele dok/stage są w osobnym
`code-studio-panes.js`. `flows-builder.js` dzieli się identycznie: paleta
węzłów + canvas + panel konfiguracji węzła, w podmodułach
`flows-builder/{canvas,config,palette,variables}.js`. Wzorzec: trzy strefy
(nawigacja lewa wąska / obszar roboczy centralny / inspektor prawy wąski),
każda w osobnym pliku modułu.

### (f) Widok przestrzenny / 3D — `robots.js`, `tentaquant/`

```text
LISTA:  1 karta na robota — status, KPI, quick actions, E-stop zawsze aktywny
DETAIL: <tf-tabs>: Przegląd │ Kamera │ LiDAR 3D │ Sterowanie │ Informacje │ Log
  ┌────────────────────────────────────────────────────────┐
  │ <tf-robot-view>  (model 3D + telemetria)                │
  │ <tf-video-stream>  (kamera na żywo, StreamHub push)      │
  │ panel sterowania generowany z advertised actions_meta    │
  └────────────────────────────────────────────────────────┘
```
3D/strumienie idą przez `StreamHub` (`streamId = "lidar:<robotId>"`), renderer
WebGPU/wgpu odbiera surowe klatki. Akcje wysokiego ryzyka (flipy, akrobacje)
wymagają `TfWindow.confirm` niezależnie od tego, co deklaruje serwer — klient
nie ufa metadanym ryzyka bezkrytycznie. `tentaquant/` powiela wzorzec dla
wizualizacji SVG 3D (Bloch sphere, Q-sphere, quantum circuit).

## Co idzie do nagłówka strony, co do topbara

| Element | Miejsce | Dlaczego |
|---|---|---|
| Tytuł `h1`, breadcrumb, akcje główne (Nowy/Edytuj/Usuń) | `slot="header"` w `<tf-screen>` (lub `.page-header` w starszych modułach) | To są dane STRONY — poruszają się z Routerem, znikają przy zmianie widoku. |
| Zakładki wewnątrz strony | `slot="tabs"` (`<tf-tabs>`) | Podnawigacja jednej strony, nie globalna nawigacja. |
| Nazwa aplikacji, nawigacja między sekcjami, user chip, przełącznik języka, odznaka środowiska (PROD/TEST/DEV) | Sidebar (stopka `.footer`) | Globalne dla całej sesji, nie należą do żadnej pojedynczej strony. Na desktopie nie ma osobnego topbara — patrz tabela wyżej. |
| Hamburger + logo + odznaka środowiska (mobile) | `.mobile-header` (≤ 767 px) | Zastępuje sidebar, gdy ten chowa się do drawera. |

## Odstępy między regionami

- Head (`.tf-screen-head`) do body: brak dodatkowego marginesu — head jest
  `sticky`, body zaczyna się zaraz pod nim (patrz `tf-screen.js`).
- Wewnątrz treści: karty/sekcje rozdzielone `spacing.xl` (24 px) do
  `spacing.layout.section` (48 px) zależnie od wagi wizualnej — hero → stat-grid
  w `dashboard.js` używa 24 px (`margin-top: 24px`), sekcje najwyższego
  poziomu (macierz połączeń, RDMA, deploy) w `cluster-detail.js` idą jedna po
  drugiej bez jawnego tokena marginesu — kontrolowane przez CSS strony.
- Karty formularzy: pola w rzędzie rozdzielone `spacing.lg` (16 px), rzędy
  pionowo `spacing.lg`–`spacing.xl`.

## Checklist

- [ ] Sprawdziłem, czy nowa strona pasuje do jednego z sześciu archetypów
      powyżej — nie projektuję siódmego bez realnej potrzeby.
- [ ] Nowa strona używa `<tf-screen>` (nie gołego `.page-header`).
- [ ] `h1` żyje w `slot="header"`, nie w topbarze (topbara nie ma na desktopie).
- [ ] Breadcrumb w `slot="breadcrumb"` tylko na widokach drill-down/detail.
- [ ] Zakładki strony w `slot="tabs"` przez `<tf-tabs>`, nie custom markup.
- [ ] Szerokość treści: `layout.content.max_width` (listy/dashboardy) albo
      `layout.content.narrow` (formularze/czat) — nie literalny px.
- [ ] Akcje globalne dla sesji (język, użytkownik, środowisko) zostają w
      sidebarze/`.mobile-header`, nie migrują do nagłówka strony.
