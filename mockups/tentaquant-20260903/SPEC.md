# TentaQuant — mockupy (2026-09-03) — kontrakt budowy

Zestaw ekranów dla aplikacji natywnej **TentaQuant** (laboratorium kwantowe, wiele instancji =
wiele laboratoriów) opisanej w `docs/TENTAQUANT_PLAN.md` (§13 lista ekranów, §3.1 model
projektu, §9.2 schemat, §18 decyzje). Kontrakt wizualny jest ten sam co
`mockups/agenci-20260822/` (breadcrumb → detail-header → tabs-bar) i
`mockups/projekty-20260723/shared/BUILD_CONTRACT.md`. TentaNas nie ma mockupów — jego realny
UI (`www/js/modules/tentanas.js`) używa tych samych prymitywów (`tf-breadcrumb`,
`.tf-detail-header`, `tf-tabs variant="underline"`, `tf-stat-card`), więc ten kontrakt jest
z nim zgodny.

Model własności projektu jest przeniesiony 1:1 z ML Studio
(`mockups/ml-studio-v1-20260614/p00-projekty.html`, `p02-udostepnianie.html`): projekt należy
do zakładającego, domyślnie jest prywatny, można go udostępnić wybranym osobom (Edytor /
Przeglądający) albo całemu laboratorium do odczytu. Członkostwo w laboratorium NIE jest
edytowane tutaj — pochodzi wyłącznie z matrycy uprawnień instancji w Addons (§10 planu).

## Pliki

```
mockups/tentaquant-20260903/
  SPEC.md                    ← ten plik
  index.html                 ← spis ekranów
  shared/styles.css          ← kopia agenci-20260822/shared/styles.css (tokeny + shell)
  shared/tentaquant.css      ← style modułu (karty, notatnik, obwód, Bloch, histogram, urządzenia, katy, koszt)
  q01-laboratoria.html       ← lista laboratoriów (instancji), do których user ma quant.read
  q02-pulpit.html            ← EKRAN REFERENCYJNY: laboratorium → zakładka Pulpit
  q03-projekty.html          ← zakładka Projekty: Moje / Udostępnione mi / Materiały laboratorium
  q04-nowy-projekt.html      ← okno „Nowy projekt” nad q03
  q05-udostepnij.html        ← okno „Udostępnij projekt” nad q03
  q06-notatnik.html          ← projekt → notatnik (komórki + panel stanu)
  q07-studio-obwodow.html    ← projekt → studio obwodów (paleta, siatka, krok po kroku)
  q08-runy.html              ← zakładka Runy: tabela + szczegół runu + porównanie symulator vs QPU
  q09-urzadzenia.html        ← zakładka Urządzenia: warstwy T0–T4, nody GPU, backendy QPU
  q10-przyklady.html         ← zakładka Przykłady: galeria + wariant CPU / GPU / QPU
  q11-nauka.html             ← zakładka Kurs (nazwa pliku zostaje): 24 katy w stałej kolejności + widok jednej katy + ranking
  q12-ustawienia.html        ← zakładka Ustawienia: Moje konto IBM · Konto laboratorium (pula s / 28 dni, zgody) · Limity osób · Kurs · Nody i izolacja · Dostęp (tylko odczyt, 6 uprawnień)
  q13-uruchom-na-qpu.html    ← okno „Uruchom na QPU” (backend IBM, konto własne / laboratorium, kosztorys w s QPU, zgoda tylko dla konta laboratorium) nad q07
  q14-nowe-laboratorium.html ← okno generycznego kreatora instancji (Addons) + kroki aplikacji z manifestu (nody GPU, deploy, test Bell) nad q01
  q15-wynik-runu.html        ← pełnoekranowy widok runu: Ewolucja (animacja bramka po bramce) · Stan (Bloch, Q-sfera, amplitudy z fazą, macierz gęstości, mapa splątania) · Histogram (wąsy, nakładki, zbieżność) · Porównanie · Dane i eksport (pakiet naukowy) — plan §13.6
  q16-wyniki-projektu.html   ← projekt → zakładka Wyniki: galeria miniatur, przypięte, porównanie zaznaczonych, stan pusty
```

## Szkielet każdego ekranu (kopiować dosłownie)

```html
<!DOCTYPE html>
<html lang="pl">
<head>
<meta charset="UTF-8">
<title>TentaFlow — TentaQuant · <Nazwa ekranu> (Qxx)</title>
<link href="https://fonts.googleapis.com/css2?family=Manrope:wght@400;500;600;700;800&family=JetBrains+Mono:wght@500;700&display=swap" rel="stylesheet">
<link rel="stylesheet" href="shared/styles.css?v=1">
<link rel="stylesheet" href="shared/tentaquant.css?v=1">
<style>
  /* tylko style specyficzne dla TEGO ekranu; wszystko powtarzalne idzie do shared/tentaquant.css */
</style>
</head>
<body>

<!-- sprite ikon (patrz niżej, identyczny w każdym pliku) -->

<section class="screen">
  <div class="screen-header">
    <span class="num">Q02</span>
    <h2>Laboratorium · Pulpit</h2>
    <div class="desc">Jedno zdanie: co pokazuje ekran i skąd wchodzi użytkownik.</div>
  </div>
  <div class="screen-frame">
    <div class="app">
      <aside class="sidebar"> … blok sidebar (niżej) … </aside>
      <main class="main">
        … breadcrumb → detail-header → tabs-bar → treść …
      </main>
    </div>
  </div>
</section>

</body>
</html>
```

Okno (modal) = ten sam ekran-tło + `<div class="window-backdrop">` jako OSTATNIE dziecko
`.app` (pozycjonowane absolutnie względem `.app`), w środku `.window` (`.window.wizard` dla
kreatora z `.stepper` ze styles.css: `.step > .n` + `.line`) z `window-head` / `window-body` /
`window-foot` i `style="animation: cardIn 0.25s ease both;"`.

### Sidebar (identyczny w każdym pliku; TentaQuant aktywny)

```html
<aside class="sidebar">
  <div class="logo"><span class="name">TentaFlow</span></div>
  <div class="nav-section">
    <div class="heading"><svg class="icon"><use href="#i-settings"/></svg>Ogólne</div>
    <div class="nav-item"><svg class="icon"><use href="#i-home"/></svg>Pulpit</div>
    <div class="nav-item"><svg class="icon"><use href="#i-services"/></svg>Serwisy</div>
    <div class="nav-item"><svg class="icon"><use href="#i-cpu"/></svg>Nody mesh</div>
  </div>
  <div class="nav-section">
    <div class="heading"><svg class="icon"><use href="#i-activity"/></svg>Aplikacje</div>
    <div class="nav-item"><svg class="icon"><use href="#i-folder"/></svg>Projekty</div>
    <div class="nav-item"><svg class="icon"><use href="#i-brain"/></svg>ML Studio</div>
    <div class="nav-item"><svg class="icon"><use href="#i-flask"/></svg>Benchmark Studio</div>
    <div class="nav-item"><svg class="icon"><use href="#i-bot"/></svg>Agenci</div>
    <div class="nav-item active"><svg class="icon"><use href="#i-atom"/></svg>TentaQuant</div>
  </div>
  <div class="nav-section">
    <div class="heading"><svg class="icon"><use href="#i-users"/></svg>Zarządzanie</div>
    <div class="nav-item"><svg class="icon"><use href="#i-users"/></svg>Użytkownicy</div>
    <div class="nav-item"><svg class="icon"><use href="#i-audit"/></svg>Dziennik audytu</div>
  </div>
  <div class="footer">
    <div class="user-chip"><div class="avatar">AK</div><div><div class="name-t">Anna · Użytkowniczka</div><div class="role">Zespół Kwanty R&D</div></div></div>
  </div>
</aside>
```

Zalogowana jest **Anna Kowalska (AK)** — użytkowniczka z typowym zestawem `quant.read` +
`quant.run` + `quant.run.gpu` + `quant.run.qpu` i własnym (aktywnym) tokenem IBM: run z jej konta
idzie bez zgody, run z konta laboratorium zawsze czeka na zgodę opiekuna. Ekran Q12 (Ustawienia) i
Q14 (kreator instancji) pokazuje widok **Piotra Jarockiego (PJ)** — opiekun (`quant.instruct`) +
`quant.admin`; wtedy user-chip w sidebarze to `PJ · Opiekun / PW · Katedra Informatyki`.

### Sprite ikon (wklejać cały, tuż po `<body>`)

```html
<svg width="0" height="0" style="position:absolute" aria-hidden="true"><defs>
<symbol id="i-home" viewBox="0 0 24 24"><path d="m3 9 9-7 9 7v11a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"/><polyline points="9 22 9 12 15 12 15 22"/></symbol>
<symbol id="i-services" viewBox="0 0 24 24"><rect x="4" y="4" width="6" height="6" rx="1"/><rect x="14" y="4" width="6" height="6" rx="1"/><rect x="4" y="14" width="6" height="6" rx="1"/><rect x="14" y="14" width="6" height="6" rx="1"/></symbol>
<symbol id="i-settings" viewBox="0 0 24 24"><circle cx="12" cy="12" r="3"/><path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 1 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06A1.65 1.65 0 0 0 4.6 15a1.65 1.65 0 0 0-1.51-1H3a2 2 0 1 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 1 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 1 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z"/></symbol>
<symbol id="i-cpu" viewBox="0 0 24 24"><rect x="4" y="4" width="16" height="16" rx="2"/><rect x="9" y="9" width="6" height="6"/><line x1="9" y1="1" x2="9" y2="4"/><line x1="15" y1="1" x2="15" y2="4"/><line x1="9" y1="20" x2="9" y2="23"/><line x1="15" y1="20" x2="15" y2="23"/><line x1="20" y1="9" x2="23" y2="9"/><line x1="20" y1="14" x2="23" y2="14"/><line x1="1" y1="9" x2="4" y2="9"/><line x1="1" y1="14" x2="4" y2="14"/></symbol>
<symbol id="i-activity" viewBox="0 0 24 24"><polyline points="22 12 18 12 15 21 9 3 6 12 2 12"/></symbol>
<symbol id="i-users" viewBox="0 0 24 24"><path d="M16 21v-2a4 4 0 0 0-4-4H6a4 4 0 0 0-4 4v2"/><circle cx="9" cy="7" r="4"/><path d="M22 21v-2a4 4 0 0 0-3-3.87"/><path d="M16 3.13a4 4 0 0 1 0 7.75"/></symbol>
<symbol id="i-audit" viewBox="0 0 24 24"><path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z"/><polyline points="14 2 14 8 20 8"/><line x1="16" y1="13" x2="8" y2="13"/><line x1="16" y1="17" x2="8" y2="17"/></symbol>
<symbol id="i-folder" viewBox="0 0 24 24"><path d="M22 19a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h5l2 3h9a2 2 0 0 1 2 2z"/></symbol>
<symbol id="i-brain" viewBox="0 0 24 24"><path d="M12 5a3 3 0 1 0-5.5 1.5A3 3 0 0 0 5 12a3 3 0 0 0 1.5 5.5A3 3 0 0 0 12 19zm0 0a3 3 0 1 1 5.5 1.5A3 3 0 0 1 19 12a3 3 0 0 1-1.5 5.5A3 3 0 0 1 12 19z"/></symbol>
<symbol id="i-flask" viewBox="0 0 24 24"><path d="M10 2v7.5L4.5 19a2 2 0 0 0 1.8 3h11.4a2 2 0 0 0 1.8-3L14 9.5V2"/><line x1="8" y1="2" x2="16" y2="2"/><line x1="7" y1="15" x2="17" y2="15"/></symbol>
<symbol id="i-bot" viewBox="0 0 24 24"><rect x="3" y="8" width="18" height="12" rx="2"/><path d="M12 8V4M8 4h8"/><circle cx="9" cy="14" r="1"/><circle cx="15" cy="14" r="1"/></symbol>
<symbol id="i-atom" viewBox="0 0 24 24"><circle cx="12" cy="12" r="1.6"/><ellipse cx="12" cy="12" rx="10" ry="4"/><ellipse cx="12" cy="12" rx="10" ry="4" transform="rotate(60 12 12)"/><ellipse cx="12" cy="12" rx="10" ry="4" transform="rotate(120 12 12)"/></symbol>
<symbol id="i-plus" viewBox="0 0 24 24"><line x1="12" y1="5" x2="12" y2="19"/><line x1="5" y1="12" x2="19" y2="12"/></symbol>
<symbol id="i-search" viewBox="0 0 24 24"><circle cx="11" cy="11" r="8"/><line x1="21" y1="21" x2="16.65" y2="16.65"/></symbol>
<symbol id="i-edit" viewBox="0 0 24 24"><path d="M11 4H4a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h14a2 2 0 0 0 2-2v-7"/><path d="M18.5 2.5a2.12 2.12 0 0 1 3 3L12 15l-4 1 1-4z"/></symbol>
<symbol id="i-trash" viewBox="0 0 24 24"><polyline points="3 6 5 6 21 6"/><path d="M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6m3 0V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2"/></symbol>
<symbol id="i-copy" viewBox="0 0 24 24"><rect x="9" y="9" width="13" height="13" rx="2"/><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"/></symbol>
<symbol id="i-more" viewBox="0 0 24 24"><circle cx="12" cy="12" r="1"/><circle cx="19" cy="12" r="1"/><circle cx="5" cy="12" r="1"/></symbol>
<symbol id="i-x" viewBox="0 0 24 24"><line x1="18" y1="6" x2="6" y2="18"/><line x1="6" y1="6" x2="18" y2="18"/></symbol>
<symbol id="i-check" viewBox="0 0 24 24"><polyline points="20 6 9 17 4 12"/></symbol>
<symbol id="i-play" viewBox="0 0 24 24"><polygon points="5 3 19 12 5 21 5 3"/></symbol>
<symbol id="i-stop" viewBox="0 0 24 24"><rect x="6" y="6" width="12" height="12" rx="1"/></symbol>
<symbol id="i-pause" viewBox="0 0 24 24"><rect x="6" y="4" width="4" height="16"/><rect x="14" y="4" width="4" height="16"/></symbol>
<symbol id="i-clock" viewBox="0 0 24 24"><circle cx="12" cy="12" r="10"/><polyline points="12 6 12 12 16 14"/></symbol>
<symbol id="i-zap" viewBox="0 0 24 24"><polygon points="13 2 3 14 12 14 11 22 21 10 12 10 13 2"/></symbol>
<symbol id="i-sparkles" viewBox="0 0 24 24"><path d="M12 3l1.9 5.1L19 10l-5.1 1.9L12 17l-1.9-5.1L5 10l5.1-1.9L12 3z"/><path d="M5 3v4M19 17v4M3 5h4M17 19h4"/></symbol>
<symbol id="i-layers" viewBox="0 0 24 24"><polygon points="12 2 2 7 12 12 22 7 12 2"/><polyline points="2 17 12 22 22 17"/><polyline points="2 12 12 17 22 12"/></symbol>
<symbol id="i-chevron-d" viewBox="0 0 24 24"><polyline points="6 9 12 15 18 9"/></symbol>
<symbol id="i-chevron-r" viewBox="0 0 24 24"><polyline points="9 18 15 12 9 6"/></symbol>
<symbol id="i-arrow-r" viewBox="0 0 24 24"><line x1="5" y1="12" x2="19" y2="12"/><polyline points="12 5 19 12 12 19"/></symbol>
<symbol id="i-shield" viewBox="0 0 24 24"><path d="M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z"/></symbol>
<symbol id="i-save" viewBox="0 0 24 24"><path d="M19 21H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h11l5 5v11a2 2 0 0 1-2 2z"/><polyline points="17 21 17 13 7 13 7 21"/><polyline points="7 3 7 8 15 8"/></symbol>
<symbol id="i-alert" viewBox="0 0 24 24"><path d="M10.29 3.86 1.82 18a2 2 0 0 0 1.71 3h16.94a2 2 0 0 0 1.71-3L13.71 3.86a2 2 0 0 0-3.42 0z"/><line x1="12" y1="9" x2="12" y2="13"/><line x1="12" y1="17" x2="12.01" y2="17"/></symbol>
<symbol id="i-info" viewBox="0 0 24 24"><circle cx="12" cy="12" r="10"/><line x1="12" y1="16" x2="12" y2="12"/><line x1="12" y1="8" x2="12.01" y2="8"/></symbol>
<symbol id="i-help" viewBox="0 0 24 24"><circle cx="12" cy="12" r="10"/><path d="M9.09 9a3 3 0 0 1 5.83 1c0 2-3 3-3 3"/><line x1="12" y1="17" x2="12.01" y2="17"/></symbol>
<symbol id="i-globe" viewBox="0 0 24 24"><circle cx="12" cy="12" r="10"/><line x1="2" y1="12" x2="22" y2="12"/><path d="M12 2a15.3 15.3 0 0 1 4 10 15.3 15.3 0 0 1-4 10 15.3 15.3 0 0 1-4-10 15.3 15.3 0 0 1 4-10z"/></symbol>
<symbol id="i-message" viewBox="0 0 24 24"><path d="M21 15a2 2 0 0 1-2 2H7l-4 4V5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2z"/></symbol>
<symbol id="i-git" viewBox="0 0 24 24"><line x1="6" y1="3" x2="6" y2="15"/><circle cx="18" cy="6" r="3"/><circle cx="6" cy="18" r="3"/><path d="M18 9a9 9 0 0 1-9 9"/></symbol>
<symbol id="i-share" viewBox="0 0 24 24"><circle cx="18" cy="5" r="3"/><circle cx="6" cy="12" r="3"/><circle cx="18" cy="19" r="3"/><line x1="8.59" y1="13.51" x2="15.42" y2="17.49"/><line x1="15.41" y1="6.51" x2="8.59" y2="10.49"/></symbol>
<symbol id="i-crown" viewBox="0 0 24 24"><path d="M3 17l2-9 5 5 2-7 2 7 5-5 2 9z"/><line x1="3" y1="21" x2="21" y2="21"/></symbol>
<symbol id="i-lock" viewBox="0 0 24 24"><rect x="3" y="11" width="18" height="11" rx="2"/><path d="M7 11V7a5 5 0 0 1 10 0v4"/></symbol>
<symbol id="i-eye" viewBox="0 0 24 24"><path d="M1 12s4-8 11-8 11 8 11 8-4 8-11 8-11-8-11-8z"/><circle cx="12" cy="12" r="3"/></symbol>
<symbol id="i-user" viewBox="0 0 24 24"><path d="M20 21v-2a4 4 0 0 0-4-4H8a4 4 0 0 0-4 4v2"/><circle cx="12" cy="7" r="4"/></symbol>
<symbol id="i-mail" viewBox="0 0 24 24"><path d="M4 4h16c1.1 0 2 .9 2 2v12c0 1.1-.9 2-2 2H4c-1.1 0-2-.9-2-2V6c0-1.1.9-2 2-2z"/><polyline points="22,6 12,13 2,6"/></symbol>
<symbol id="i-send" viewBox="0 0 24 24"><line x1="22" y1="2" x2="11" y2="13"/><polygon points="22 2 15 22 11 13 2 9 22 2"/></symbol>
<symbol id="i-book" viewBox="0 0 24 24"><path d="M4 19.5A2.5 2.5 0 0 1 6.5 17H20"/><path d="M6.5 2H20v20H6.5A2.5 2.5 0 0 1 4 19.5v-15A2.5 2.5 0 0 1 6.5 2z"/></symbol>
<symbol id="i-award" viewBox="0 0 24 24"><circle cx="12" cy="8" r="7"/><polyline points="8.21 13.89 7 23 12 20 17 23 15.79 13.88"/></symbol>
<symbol id="i-star" viewBox="0 0 24 24"><polygon points="12 2 15.09 8.26 22 9.27 17 14.14 18.18 21.02 12 17.77 5.82 21.02 7 14.14 2 9.27 8.91 8.26 12 2"/></symbol>
<symbol id="i-grid" viewBox="0 0 24 24"><rect x="3" y="3" width="7" height="7"/><rect x="14" y="3" width="7" height="7"/><rect x="14" y="14" width="7" height="7"/><rect x="3" y="14" width="7" height="7"/></symbol>
<symbol id="i-monitor" viewBox="0 0 24 24"><rect x="2" y="3" width="20" height="14" rx="2"/><line x1="8" y1="21" x2="16" y2="21"/><line x1="12" y1="17" x2="12" y2="21"/></symbol>
<symbol id="i-code" viewBox="0 0 24 24"><polyline points="16 18 22 12 16 6"/><polyline points="8 6 2 12 8 18"/></symbol>
<symbol id="i-file" viewBox="0 0 24 24"><path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z"/><polyline points="14 2 14 8 20 8"/></symbol>
<symbol id="i-image" viewBox="0 0 24 24"><rect x="3" y="3" width="18" height="18" rx="2"/><circle cx="8.5" cy="8.5" r="1.5"/><polyline points="21 15 16 10 5 21"/></symbol>
<symbol id="i-table" viewBox="0 0 24 24"><rect x="3" y="3" width="18" height="18" rx="2"/><line x1="3" y1="9" x2="21" y2="9"/><line x1="3" y1="15" x2="21" y2="15"/><line x1="9" y1="3" x2="9" y2="21"/></symbol>
<symbol id="i-bar" viewBox="0 0 24 24"><line x1="18" y1="20" x2="18" y2="10"/><line x1="12" y1="20" x2="12" y2="4"/><line x1="6" y1="20" x2="6" y2="14"/></symbol>
<symbol id="i-gauge" viewBox="0 0 24 24"><path d="M4 18a8 8 0 0 1 16 0"/><line x1="12" y1="18" x2="17" y2="11"/></symbol>
<symbol id="i-download" viewBox="0 0 24 24"><path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4"/><polyline points="7 10 12 15 17 10"/><line x1="12" y1="15" x2="12" y2="3"/></symbol>
<symbol id="i-upload" viewBox="0 0 24 24"><path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4"/><polyline points="17 8 12 3 7 8"/><line x1="12" y1="3" x2="12" y2="15"/></symbol>
<symbol id="i-refresh" viewBox="0 0 24 24"><polyline points="23 4 23 10 17 10"/><path d="M20.49 15a9 9 0 1 1-2.12-9.36L23 10"/></symbol>
<symbol id="i-external" viewBox="0 0 24 24"><path d="M18 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h6"/><polyline points="15 3 21 3 21 9"/><line x1="10" y1="14" x2="21" y2="3"/></symbol>
<symbol id="i-sliders" viewBox="0 0 24 24"><line x1="4" y1="21" x2="4" y2="14"/><line x1="4" y1="10" x2="4" y2="3"/><line x1="12" y1="21" x2="12" y2="12"/><line x1="12" y1="8" x2="12" y2="3"/><line x1="20" y1="21" x2="20" y2="16"/><line x1="20" y1="12" x2="20" y2="3"/><line x1="1" y1="14" x2="7" y2="14"/><line x1="9" y1="8" x2="15" y2="8"/><line x1="17" y1="16" x2="23" y2="16"/></symbol>
<symbol id="i-coin" viewBox="0 0 24 24"><circle cx="12" cy="12" r="10"/><path d="M15 9.5a3 3 0 0 0-3-1.5c-2 0-3 1-3 2s1 1.5 3 2 3 1 3 2-1 2-3 2a3 3 0 0 1-3-1.5"/><line x1="12" y1="6" x2="12" y2="18"/></symbol>
<symbol id="i-gpu" viewBox="0 0 24 24"><rect x="2" y="6" width="20" height="12" rx="2"/><circle cx="9" cy="12" r="3"/><line x1="15" y1="10" x2="19" y2="10"/><line x1="15" y1="14" x2="19" y2="14"/><line x1="6" y1="18" x2="6" y2="21"/><line x1="10" y1="18" x2="10" y2="21"/></symbol>
<symbol id="i-server" viewBox="0 0 24 24"><rect x="2" y="2" width="20" height="8" rx="2"/><rect x="2" y="14" width="20" height="8" rx="2"/><line x1="6" y1="6" x2="6.01" y2="6"/><line x1="6" y1="18" x2="6.01" y2="18"/></symbol>
<symbol id="i-link" viewBox="0 0 24 24"><path d="M10 13a5 5 0 0 0 7.54.54l3-3a5 5 0 0 0-7.07-7.07l-1.72 1.71"/><path d="M14 11a5 5 0 0 0-7.54-.54l-3 3a5 5 0 0 0 7.07 7.07l1.71-1.71"/></symbol>
<symbol id="i-key" viewBox="0 0 24 24"><path d="M21 2l-2 2m-7.61 7.61a5.5 5.5 0 1 1-7.778 7.778 5.5 5.5 0 0 1 7.777-7.777zm0 0L15.5 7.5m0 0l3 3L22 7l-3-3m-3.5 3.5L19 4"/></symbol>
<symbol id="i-archive" viewBox="0 0 24 24"><polyline points="21 8 21 21 3 21 3 8"/><rect x="1" y="3" width="22" height="5"/><line x1="10" y1="12" x2="14" y2="12"/></symbol>
<symbol id="i-filter" viewBox="0 0 24 24"><polygon points="22 3 2 3 10 12.46 10 19 14 21 14 12.46 22 3"/></symbol>
<symbol id="i-bulb" viewBox="0 0 24 24"><path d="M9 18h6M10 22h4M12 2a7 7 0 0 0-4 12.7V17h8v-2.3A7 7 0 0 0 12 2z"/></symbol>
<symbol id="i-flag" viewBox="0 0 24 24"><path d="M4 15s1-1 4-1 5 2 8 2 4-1 4-1V3s-1 1-4 1-5-2-8-2-4 1-4 1z"/><line x1="4" y1="22" x2="4" y2="15"/></symbol>
</defs></svg>
```

### Nawigacja wewnątrz laboratorium (Q02–Q12)

```html
<div class="breadcrumb">
  <span class="crumb" onclick="location.href='q01-laboratoria.html'">TentaQuant</span>
  <span class="sep"><svg viewBox="0 0 24 24" style="width:12px;height:12px;stroke:currentColor;fill:none;stroke-width:2;"><polyline points="9 18 15 12 9 6"/></svg></span>
  <span class="crumb current">Kwanty R&D</span>
</div>

<div class="detail-header">
  <div class="big-ico quant"><svg viewBox="0 0 24 24"><circle cx="12" cy="12" r="1.6"/><ellipse cx="12" cy="12" rx="10" ry="4"/><ellipse cx="12" cy="12" rx="10" ry="4" transform="rotate(60 12 12)"/><ellipse cx="12" cy="12" rx="10" ry="4" transform="rotate(120 12 12)"/></svg></div>
  <div class="d-meta">
    <div class="d-name">Kwanty R&D
      <span class="chip success"><span class="dot"></span>Usługa działa</span>
      <span class="chip qpu">2 backendy QPU</span>
    </div>
    <div class="d-id">tentaquant · instancja lab-kwanty-rd · utworzono 2026-09-01 · 42 osoby z dostępem</div>
    <div class="d-badges">
      <span class="tier t0">T0 · przeglądarka</span>
      <span class="tier t1">T1 · Core</span>
      <span class="tier t2">T2 · Python</span>
      <span class="tier t3">T3 · GPU</span>
      <span class="tier t4">T4 · QPU</span>
      <span class="chip">quantum-python 1.0 · spark-01</span>
      <span class="your-role"><svg class="icon" style="width:12px;height:12px"><use href="#i-user"/></svg>Twoja rola: użytkownik (read · run · run.gpu · run.qpu)</span>
    </div>
  </div>
  <div class="d-actions">
    <button class="btn btn-primary"><!-- otwiera: Q04 --><svg class="icon"><use href="#i-plus"/></svg>Nowy projekt</button>
    <button class="btn btn-secondary"><!-- otwiera: Q10 --><svg class="icon"><use href="#i-grid"/></svg>Przykłady</button>
  </div>
</div>

<div class="tabs-bar">
  <button class="tab"><!-- otwiera: Q02 --><svg class="icon"><use href="#i-home"/></svg>Pulpit</button>
  <button class="tab"><!-- otwiera: Q03 --><svg class="icon"><use href="#i-folder"/></svg>Projekty <span class="count">7</span></button>
  <button class="tab"><!-- otwiera: Q08 --><svg class="icon"><use href="#i-clock"/></svg>Runy <span class="count">128</span></button>
  <button class="tab"><!-- otwiera: Q09 --><svg class="icon"><use href="#i-server"/></svg>Urządzenia <span class="count">6</span></button>
  <button class="tab"><!-- otwiera: Q10 --><svg class="icon"><use href="#i-grid"/></svg>Przykłady <span class="count">24</span></button>
  <button class="tab"><!-- otwiera: Q11 --><svg class="icon"><use href="#i-book"/></svg>Kurs</button>
  <button class="tab"><!-- otwiera: Q12 --><svg class="icon"><use href="#i-settings"/></svg>Ustawienia</button>
</div>
```

Na danym ekranie odpowiedni `.tab` ma klasę `active`. Zakładka **Ustawienia** widoczna tylko
dla `quant.admin` (Anna jej NIE widzi — na Q02–Q11 zakładkę Ustawienia pominąć; na Q12 sidebar
i user-chip są opiekuna/admina).

Ekrany PROJEKTU (Q06, Q07) mają breadcrumb `TentaQuant › Laboratorium PW … › Grover 4-kubitowy`
i **własny, mniejszy** nagłówek projektu: `.detail-header` z `big-ico quant` (ikona `i-folder`),
nazwą projektu + chip własności (`chip accent` „Twój projekt · prywatny” / `chip info`
„Udostępniony: 2 osoby”), `d-id` = `grover-4q · utworzono 2026-09-02 · zapis 14:02`, akcje:
„Udostępnij” (→ Q05), „Uruchom” (primary), „⋯”. Pod nim `.tabs-bar` projektu:
**Notatnik · Studio obwodów · Pliki · Runy projektu <count>** (aktywna wg ekranu). Bez zakładek
laboratorium — poziom laboratorium jest w breadcrumb.

## Dane przykładowe (używać spójnie)

- Laboratorium: **Kwanty R&D**, `lab-kwanty-rd`, opiekun + admin
  Piotr Jarocki (PJ), 42 osoby z `quant.read` (40 z `quant.run.qpu`), `quantum-python 1.0`
  zdeployowane na `spark-01`. Laboratorium NIE ma właściciela — kafelek pokazuje nazwę, liczbę osób
  z dostępem (z matrycy), moją rolę i ostatnią aktywność. Inne laboratoria na Q01: **Euvic R&D ·
  Optymalizacja QAOA** (`lab-euvic-qaoa`, 6 osób, Anna jest obserwatorem = read), **Sandbox
  lokalny** (`lab-local`, „tylko Ty”, zainstalowane lokalnie, bez konta IBM). Kafelek „+ Nowe laboratorium” prowadzi do Q14 (kreator instancji w
  Addons — dla użytkownika bez uprawnienia do instalowania kafelek jest wyszarzony z podpowiedzią).
- Użytkownicy: Anna Kowalska (AK, użytkowniczka, zalogowana), Marek Nowak (MN), Piotr Jarocki (PJ,
  opiekun + admin), Kasia Wiśniewska (KW), Tomasz Zieliński (TZ), Ola Mazur (OM — BEZ `quant.read`
  → przy udostępnianiu ostrzeżenie „nie ma dostępu do laboratorium, udostępnienie uśpione”).
- Projekty Anny: **Grover 4-kubitowy** (`grover-4q`, prywatny, notatnik + obwód, 14 runów),
  **QFT vs FFT** (`qft-vs-fft`, udostępniony MN jako Przeglądający, 31 runów), **VQE H₂**
  (`vqe-h2`, udostępniony KW jako Edytor + całe laboratorium do odczytu, 62 runy, ostatni run
  na `ibm_torino`). Udostępnione Annie: **Teleportacja stanu** (PJ, Przeglądający, bez terminu),
  **Benchmark GPU 28q** (MN, Edytor). Materiały laboratorium (visibility=lab, właściciel PJ):
  **Materiał 4 · Algorytm Deutscha-Jozsy**, **Warsztat 5 · Szum i mitigacja**.
- Nody GPU: `spark-01` (NVIDIA GB10, 128 GB unified, CUDA, T3 do 33 kubitów), `ws-amd`
  (Radeon RX 7900 XTX 24 GB, Vulkan/wgpu, do 30 kubitów), `mac-m3` (Apple M3 Max 64 GB, Metal,
  do 31 kubitów), `lab-cpu-02` (AMD EPYC 32 rdzenie, tylko T2, do 27 kubitów, offline).
- QPU: tylko IBM. **ibm_torino** (Heron r1, 133 kubity, kolejka ~42 min) i **ibm_kingston** (Heron r2,
  156 kubitów, kolejka ~17 min). Konta: **Moje konto IBM** = osobisty klucz API + CRN każdej osoby z
  `quant.run.qpu` (plan Open, Anna: pozostało 7:48 z 10:00 / 28 dni, bez zgody, bez puli); **Konto
  laboratorium** = jeden token instancji, pula **3 600 s / 28 dni** (zużyto 2 360 s, zostało 1 240 s),
  domyślny limit 300 s na osobę (opiekun nadpisuje per osoba, np. KW 600 s), każdy run po zgodzie
  opiekuna. Jednostka rozliczenia: sekundy QPU.
- Runy: statusy `ok / fail / run / queue / wait (czeka na zgodę) / cancel`; identyfikatory
  `run-2f9a1c`, czasy `2026-09-03 14:02`, czasy wykonania `18 ms (T1)`, `1,4 s (T3 spark-01)`,
  `42 min kolejki + 3,1 s (ibm_torino)`, `17 min + 2,8 s (ibm_kingston, konto laboratorium)`; wyniki jako histogram counts (1024 shotów).
- Przykłady (24): m.in. Bell, GHZ, Teleportacja, Deutsch-Jozsa, Bernstein-Vazirani, Grover,
  QFT, Phase estimation, VQE H₂, QAOA MaxCut, Shor 15, Kod powtórzeniowy, Losowe obwody 28q
  (benchmark), Monte Carlo klasyczne vs kwantowe. Każdy ma warianty CPU / GPU / QPU (chipy
  `.variants`) i poziom `.lvl` 1–5.
- Katy (Kurs): 24 katy w STAŁEJ kolejności od najłatwiejszej do najtrudniejszej, grupy „Podstawy:
  kubit i bramki” (01–06), „Splątanie” (07–12), „Algorytmy” (13–20), „Szum i mitigacja” (21–24);
  postęp Anny 10/24 (860 pkt), właśnie zaliczona kata 10 „Bramka CNOT i stan Bella” (TVD 0,012),
  następna 11 „Cztery stany Bella”. Kata 20 = T3 (wymaga `quant.run.gpu`); katy 22–24 ocenia
  symulator z modelem szumu z kalibracji IBM (T1), prawdziwy QPU to opcjonalny przycisk „Sprawdź
  naprawdę” z własnego konta IBM.

## Reguły treści

1. Każdy ekran ma `.screen-header` z numerem `Qxx`, tytułem i jednym zdaniem opisu.
2. Każda lista ma akcję dodawania i akcje na wierszu (edytuj/usuń lub `⋯` menu `.tf-menu`);
   każdy przycisk prowadzący do innego ekranu ma komentarz `<!-- otwiera: Qxx -->`.
3. Wszystko po polsku, realistyczne dane z listy powyżej, żadnych lorem ipsum.
4. Animacje: treść `main` wchodzi `fadeUp` (automatycznie), okna `cardIn`, słupki histogramu
   `growBar` (automatycznie przez `.hist-bar`), komórka w trakcie wykonania `.cell.running`,
   status runu `.run-status.run` mruga. Nic nie miga w nieskończoność poza wskaźnikami „w toku”.
5. Spacing: `.section-card` 18–20 px padding i 16 px margines dolny, siatki gap 12–14 px.
6. Kolory warstw są stałe: T0 `--browser`, T1 `--core`, T2 `--py`, T3 `--gpu`, T4 `--qpu`
   (różowy). Wszystko związane z prawdziwym QPU (chipy, koszt, zgoda, słupki) używa `--qpu`.
7. Kod w mockupie to statyczny `<div class="code">` z klasami `k/s/c/n/f/d`; Python + Qiskit,
   nasz backend wygląda tak: `from tentaquant import TentaQuantBackend` →
   `backend = TentaQuantBackend(device="auto")` (albo `"gpu"`, `"cpu"`), `backend.run(qc, shots=1024)`.
8. „Uruchom na…” to jeden `.select` z opcjami: `Przeglądarka (T0, ≤ 20 kubitów)`,
   `Core · spark-01 (T1)`, `Python · quantum-python (T2)`, `GPU · spark-01 GB10 (T3)`,
   `GPU · ws-amd RX 7900 XTX (T3, Vulkan)`, `GPU · mac-m3 (T3, Metal)`, `QPU · ibm_torino`,
   `QPU · ibm_kingston`. Domyślnie `auto` z prostą regułą (≤ 20 q T0, ≤ 28 q T1, powyżej T3 przy
   `quant.run.gpu` i nodzie GPU online, Python z jądrem → T2, QPU nigdy) — UI pokazuje wynik, np.
   `auto → T1 · spark-01`; wybór QPU zawsze otwiera Q13.
9. Prawa: przy QPU zawsze widać konto — własny token IBM (bez zgody) albo konto laboratorium
   (chip `warning` „zgoda opiekuna”); Anna nie widzi zakładki Ustawienia (brak `quant.admin`).
   Udostępnianie projektu NIE nadaje dostępu do laboratorium (callout w Q05); Przeglądający
   uruchamia komórki tylko w przeglądarce (T0) bez zapisu wyników.

## Decyzje 2026-09-03 (z właścicielem)

1. Nie produkt uczelniany: „użytkownik” zamiast studenta, „opiekun” (`quant.instruct`) zamiast
   prowadzącego, zero terminów/zadań/ocen/semestrów. Zakładka „Nauka” → **Kurs** (plik zostaje
   `q11-nauka.html`): 24 katy w stałej kolejności od najłatwiejszej do najtrudniejszej dla firmy,
   uczelni i hobbysty; ranking z wyborem okresu (30 / 90 dni / cały czas), opiekun może go wyłączyć
   w Ustawieniach → Kurs. Katy 22–24 oceniane na symulatorze z szumem z kalibracji IBM (T1/T2),
   prawdziwy QPU = opcjonalny „Sprawdź naprawdę” z własnego konta; kata 20 zostaje T3.
2. Tylko IBM (ibm_torino 133 q, ibm_kingston 156 q); IonQ i IQM usunięte. Jednostka: sekundy QPU.
3. Konta QPU: domyślnie **osobisty token** (klucz API + CRN w Ustawienia → Moje konto IBM, widoczny
   tylko dla właściciela, run bez zgody i bez puli). Opcjonalnie jedno **konto laboratorium**
   (`quant.admin`): pula sekund na 28 dni + domyślny limit na osobę, opiekun nadpisuje limit w
   „Limity osób” (lista = odczyt matrycy); run z konta laboratorium ZAWSZE po zgodzie opiekuna.
   Q13 ma krok „Konto”; Q02 ma kartę „Do zatwierdzenia” tylko dla `quant.instruct`.
4. Katalog uprawnień = dokładnie sześć: `quant.read`, `quant.run`, `quant.run.gpu`,
   `quant.run.qpu`, `quant.instruct`, `quant.admin` (użytkownik = read+run+run.gpu+run.qpu,
   opiekun = + instruct, obserwator = read). `quant.providers.manage` i `quant.qpu.submit` nie istnieją.
5. Laboratorium nie ma właściciela (projekty mają). Kafelek: nazwa, osoby z dostępem, moja rola,
   ostatnia aktywność; jednoosobowe = „tylko Ty”.
6. Natywny sandbox: nod bez środowiska kontenerowego = python-bundle bez izolacji od hosta; bez
   potwierdzenia tylko gdy `quant.run` ma jedna osoba, inaczej checkbox „Rozumiem, uruchamiam kod
   wielu osób bez izolacji” (Q14 i Q12 → Nody i izolacja).
7. Q14 = generyczny kreator instancji Addons (nazwa, uprawnienia) + „kroki aplikacji” z manifestu
   pakietu (nody GPU, deploy usługi, test Bell).
8. Przeglądający projektu liczy komórki tylko w przeglądarce (T0) bez zapisu; Edytor uruchamia wszystko.
9. T3 = GPU noda, własny silnik `tentaflow-quantum` (CUDA / Vulkan / Metal), bez osobnego obrazu.
10. Symulacja z szumem istnieje na T1 (własne kanały Krausa, model z kalibracji IBM) i T2 (Aer).
11. `device="auto"`: ≤ 20 q T0, ≤ 28 q T1, powyżej T3 (z `quant.run.gpu` i nodem GPU online), Python
    z jądrem → T2, nigdy QPU; UI pokazuje wynik („auto → T1 · node-a”).
12. Zakładki projektu: piąta „Wyniki” (→ Q16) po „Runy projektu”; w Runach i pod wynikiem komórki
    przycisk „Otwórz wynik” / „Pełny widok wyniku” (→ Q15).
13. Spójność liczb: Grover 4q głębokość 38 → 118 po transpilacji (84 CZ; Heron natywnie CZ), Bell
    w kacie TVD 0,012, 20 kubitów = 8 MiB; Qiskit 2.x w komórkach; TREX tylko dla Estimatora
    (Sampler dostaje mitigację odczytu).

## Mapowanie na plan

| Ekran | Plan |
|-------|------|
| Q01 | §13.1 ekran 1 (lista instancji), §2.2 (kafelek z `instance`) |
| Q02 | §13.1 ekran 2 (pulpit) |
| Q03–Q05 | §13.1 ekran 3, §3.1 (projekt), §9.2 `projects` + `project_shares`, §18 decyzja 15 |
| Q06 | §13.1 ekran 4 (notatnik), §6 (tiery), §7 (`tentaquant` w Pythonie) |
| Q07 | §13.1 ekran 5 (studio obwodów), `tf-quantum-circuit` |
| Q08 | §13.1 ekran 6 (runy, porównanie symulator vs QPU) |
| Q09 | §13.1 ekran 7 (urządzenia), §6.3 backendy GPU, §8 broker QPU |
| Q10 | §13.1 ekran 8 (przykłady CPU/GPU/QPU), §12 |
| Q11 | §13.1 ekran 9 (kurs), §12 |
| Q12 | §13.1 ekran 10 (ustawienia: konta IBM, limity, kurs, nody), §10 (matryca w Addons — tu tylko odczyt) |
| Q13 | §8.4 (kosztorys w s QPU, konto własne / laboratorium, zgoda) |
| Q14 | §2.5 (generyczny kreator instancji + kroki aplikacji z manifestu, auto-deploy `quantum-python`) |
