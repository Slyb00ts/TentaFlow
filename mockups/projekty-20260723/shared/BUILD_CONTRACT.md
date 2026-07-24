# KONTRAKT BUDOWY MOCKUPÓW — Projekty (Project Studio)

Każdy ekran to samodzielny plik HTML w `mockups/projekty-20260723/`. Wszystkie muszą wyglądać jak JEDEN spójny produkt. Ten plik jest wiążący.

## Reguły twarde
1. **Head** każdego pliku:
```html
<!DOCTYPE html>
<html lang="pl">
<head>
<meta charset="UTF-8">
<title>TentaFlow — Projekty · <NAZWA EKRANU> (<ID>)</title>
<link href="https://fonts.googleapis.com/css2?family=Manrope:wght@400;500;600;700;800&family=JetBrains+Mono:wght@500;700&display=swap" rel="stylesheet">
<link rel="stylesheet" href="shared/styles.css">
<link rel="stylesheet" href="shared/projekty.css">
<style> /* tylko ekranowo-specyficzne drobiazgi, minimum */ </style>
</head>
```
2. **Struktura** identyczna dla ekranów pełnostronicowych: `<section class="screen">` → `.screen-header` (`.num` = ID, `<h2>` nazwa, `.desc` = 1 zdanie po co ten ekran) → `.screen-frame` → `.app` (grid) → `aside.sidebar` + `main.main`.
3. **Sidebar** — WKLEJ DOKŁADNIE blok z sekcji „SIDEBAR" niżej (ta sama nawigacja wszędzie; aktywny element = „Projekty"). Wewnątrz modułu nawigacja idzie przez `.breadcrumb` + `.tabs-bar`, NIE przez sidebar.
4. **Ikony** — wklej blok `<svg width="0" height="0">…</svg>` z sekcji „IKONY" na początku `<body>`. Używaj `<svg class="icon"><use href="#i-…"/></svg>`. Nie wymyślaj innych id.
5. **Okna/modale** = PRAWDZIWE OKNA: `.window-backdrop` (przyciemnione tło) nad widocznym, lekko przygaszonym ekranem bazowym, w środku `.window` (użyj `.window.wizard` dla kreatorów). Okno NIGDY nie jest fragmentem strony — zawsze overlay z `animation: cardIn`. Kreatory mają `.stepper` u góry ciała okna. Dodaj `<style>.screen-frame .app{filter:blur(1px) saturate(.85);opacity:.6}</style>` NIE — zamiast tego przyciemnij przez sam backdrop; ekran bazowy zostaw ostry, backdrop robi robotę.
6. **Kompletność (OBOWIĄZKOWA)**: każda lista/tabela ma widoczne akcje Dodaj (nagłówek) oraz per-wiersz Edytuj i Usuń (menu `⋯` lub przyciski). Każdy ekran, który może być pusty, pokaż w wariancie z danymi (stan pusty jako osobny ekran tylko gdy wskazano). Każdy przycisk „Nowy/Dodaj/Generuj/Uruchom" musi mieć wskazane, jakie okno otwiera (komentarz HTML `<!-- otwiera: <ID> -->`).
7. **Odstępy**: sekcje `.section-card` (padding 18–20px), odstęp między blokami `margin-bottom:16px`, grid gap 12–14px, przyciski w rzędach gap 8px. NIE zostawiaj przyklejonych do krawędzi elementów, NIE rób podwójnych obramowań (kontener + element). Tabela w `.section-card` — bez dodatkowego paddingu wokół (tabela ma własny border).
8. **Animacje**: używaj istniejących (`fadeUp` treści, `cardIn` okien, `spin`, `blink`). Elementy live (przebieg/generowanie) — pulsujące kropki/spinner.
9. **Język**: cały UI po polsku. Dane przykładowe realistyczne (projekt „Portal Klienta B2B", „System Rezerwacji", pliki, testerzy: Anna Kowalska, Marek Nowak; agenci: „Generator scenariuszy", „Krytyk wymagań").
10. **Role**: pokazuj kontekstowo (np. chip „Twoja rola: Manager"); akcje niedostępne dla roli = przygaszone z tooltipem.
11. Zero zewnętrznych zależności poza Google Fonts + shared CSS. JS tylko jeśli konieczny do pokazania interakcji (przełączniki stanu), inline, minimalny.
12. **NIE** rób horizontalnego scrolla body; szerokie tabele/kod w kontenerze `overflow-x:auto`.

## Ton wizualny
Ciemny motyw (tokeny z styles.css), akcent indygo→fiolet, dane czytelne, sporo „powietrza". Ma być PROSTE dla nietechnicznego usera i prowadzić za rękę (podpowiedzi `.hint`, opisy pod polami, puste stany z jasną akcją). Dla ekranów kreatorów: krótkie zdania, jeden cel na krok.

## SIDEBAR (wklej dokładnie)
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
    <div class="nav-item active"><svg class="icon"><use href="#i-folder"/></svg>Projekty</div>
    <div class="nav-item"><svg class="icon"><use href="#i-brain"/></svg>ML Studio</div>
    <div class="nav-item"><svg class="icon"><use href="#i-flask"/></svg>Benchmark Studio</div>
    <div class="nav-item"><svg class="icon"><use href="#i-bot"/></svg>Agenci</div>
  </div>
  <div class="nav-section">
    <div class="heading"><svg class="icon"><use href="#i-users"/></svg>Zarządzanie</div>
    <div class="nav-item"><svg class="icon"><use href="#i-users"/></svg>Użytkownicy</div>
    <div class="nav-item"><svg class="icon"><use href="#i-audit"/></svg>Dziennik audytu</div>
  </div>
  <div class="footer">
    <div class="user-chip"><div class="avatar">AK</div><div><div class="name-t">Anna · Manager</div><div class="role">Euvic · QA</div></div></div>
  </div>
</aside>
```
Dla ekranu „Agenci" aktywny jest `#i-bot` „Agenci” zamiast „Projekty”.

## IKONY (wklej dokładnie na początku <body>)
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
<symbol id="i-plus" viewBox="0 0 24 24"><line x1="12" y1="5" x2="12" y2="19"/><line x1="5" y1="12" x2="19" y2="12"/></symbol>
<symbol id="i-list" viewBox="0 0 24 24"><line x1="8" y1="6" x2="21" y2="6"/><line x1="8" y1="12" x2="21" y2="12"/><line x1="8" y1="18" x2="21" y2="18"/><line x1="3" y1="6" x2="3.01" y2="6"/><line x1="3" y1="12" x2="3.01" y2="12"/><line x1="3" y1="18" x2="3.01" y2="18"/></symbol>
<symbol id="i-grid" viewBox="0 0 24 24"><rect x="3" y="3" width="7" height="7"/><rect x="14" y="3" width="7" height="7"/><rect x="3" y="14" width="7" height="7"/><rect x="14" y="14" width="7" height="7"/></symbol>
<symbol id="i-search" viewBox="0 0 24 24"><circle cx="11" cy="11" r="8"/><line x1="21" y1="21" x2="16.65" y2="16.65"/></symbol>
<symbol id="i-filter" viewBox="0 0 24 24"><polygon points="22 3 2 3 10 12.46 10 19 14 21 14 12.46 22 3"/></symbol>
<symbol id="i-edit" viewBox="0 0 24 24"><path d="M11 4H4a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h14a2 2 0 0 0 2-2v-7"/><path d="M18.5 2.5a2.12 2.12 0 0 1 3 3L12 15l-4 1 1-4z"/></symbol>
<symbol id="i-trash" viewBox="0 0 24 24"><polyline points="3 6 5 6 21 6"/><path d="M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6m3 0V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2"/></symbol>
<symbol id="i-copy" viewBox="0 0 24 24"><rect x="9" y="9" width="13" height="13" rx="2"/><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"/></symbol>
<symbol id="i-more" viewBox="0 0 24 24"><circle cx="12" cy="12" r="1"/><circle cx="19" cy="12" r="1"/><circle cx="5" cy="12" r="1"/></symbol>
<symbol id="i-x" viewBox="0 0 24 24"><line x1="18" y1="6" x2="6" y2="18"/><line x1="6" y1="6" x2="18" y2="18"/></symbol>
<symbol id="i-check" viewBox="0 0 24 24"><polyline points="20 6 9 17 4 12"/></symbol>
<symbol id="i-check-circle" viewBox="0 0 24 24"><path d="M22 11.08V12a10 10 0 1 1-5.93-9.14"/><polyline points="22 4 12 14.01 9 11.01"/></symbol>
<symbol id="i-x-circle" viewBox="0 0 24 24"><circle cx="12" cy="12" r="10"/><line x1="15" y1="9" x2="9" y2="15"/><line x1="9" y1="9" x2="15" y2="15"/></symbol>
<symbol id="i-alert" viewBox="0 0 24 24"><path d="M10.29 3.86 1.82 18a2 2 0 0 0 1.71 3h16.94a2 2 0 0 0 1.71-3L13.71 3.86a2 2 0 0 0-3.42 0z"/><line x1="12" y1="9" x2="12" y2="13"/><line x1="12" y1="17" x2="12.01" y2="17"/></symbol>
<symbol id="i-bug" viewBox="0 0 24 24"><rect x="8" y="6" width="8" height="14" rx="4"/><path d="M19 7l-3 2M5 7l3 2M12 6V3M4 13H2M22 13h-2M5 19l3-1M19 19l-3-1"/></symbol>
<symbol id="i-book" viewBox="0 0 24 24"><path d="M4 19.5A2.5 2.5 0 0 1 6.5 17H20"/><path d="M6.5 2H20v20H6.5A2.5 2.5 0 0 1 4 19.5v-15A2.5 2.5 0 0 1 6.5 2z"/></symbol>
<symbol id="i-doc" viewBox="0 0 24 24"><path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z"/><polyline points="14 2 14 8 20 8"/></symbol>
<symbol id="i-file" viewBox="0 0 24 24"><path d="M13 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V9z"/><polyline points="13 2 13 9 20 9"/></symbol>
<symbol id="i-code" viewBox="0 0 24 24"><polyline points="16 18 22 12 16 6"/><polyline points="8 6 2 12 8 18"/></symbol>
<symbol id="i-terminal" viewBox="0 0 24 24"><polyline points="4 17 10 11 4 5"/><line x1="12" y1="19" x2="20" y2="19"/></symbol>
<symbol id="i-git" viewBox="0 0 24 24"><line x1="6" y1="3" x2="6" y2="15"/><circle cx="18" cy="6" r="3"/><circle cx="6" cy="18" r="3"/><path d="M18 9a9 9 0 0 1-9 9"/></symbol>
<symbol id="i-link" viewBox="0 0 24 24"><path d="M10 13a5 5 0 0 0 7.54.54l3-3a5 5 0 0 0-7.07-7.07l-1.72 1.71"/><path d="M14 11a5 5 0 0 0-7.54-.54l-3 3a5 5 0 0 0 7.07 7.07l1.71-1.71"/></symbol>
<symbol id="i-external" viewBox="0 0 24 24"><path d="M18 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h6"/><polyline points="15 3 21 3 21 9"/><line x1="10" y1="14" x2="21" y2="3"/></symbol>
<symbol id="i-globe" viewBox="0 0 24 24"><circle cx="12" cy="12" r="10"/><line x1="2" y1="12" x2="22" y2="12"/><path d="M12 2a15.3 15.3 0 0 1 4 10 15.3 15.3 0 0 1-4 10 15.3 15.3 0 0 1-4-10 15.3 15.3 0 0 1 4-10z"/></symbol>
<symbol id="i-play" viewBox="0 0 24 24"><polygon points="5 3 19 12 5 21 5 3"/></symbol>
<symbol id="i-stop" viewBox="0 0 24 24"><rect x="5" y="5" width="14" height="14" rx="2"/></symbol>
<symbol id="i-refresh" viewBox="0 0 24 24"><polyline points="23 4 23 10 17 10"/><path d="M20.49 15a9 9 0 1 1-2.12-9.36L23 10"/></symbol>
<symbol id="i-download" viewBox="0 0 24 24"><path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4"/><polyline points="7 10 12 15 17 10"/><line x1="12" y1="15" x2="12" y2="3"/></symbol>
<symbol id="i-upload" viewBox="0 0 24 24"><path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4"/><polyline points="17 8 12 3 7 8"/><line x1="12" y1="3" x2="12" y2="15"/></symbol>
<symbol id="i-clock" viewBox="0 0 24 24"><circle cx="12" cy="12" r="10"/><polyline points="12 6 12 12 16 14"/></symbol>
<symbol id="i-calendar" viewBox="0 0 24 24"><rect x="3" y="4" width="18" height="18" rx="2"/><line x1="16" y1="2" x2="16" y2="6"/><line x1="8" y1="2" x2="8" y2="6"/><line x1="3" y1="10" x2="21" y2="10"/></symbol>
<symbol id="i-bell" viewBox="0 0 24 24"><path d="M18 8a6 6 0 0 0-12 0c0 7-3 9-3 9h18s-3-2-3-9"/><path d="M13.73 21a2 2 0 0 1-3.46 0"/></symbol>
<symbol id="i-tag" viewBox="0 0 24 24"><path d="M20.59 13.41 13.42 20.6a2 2 0 0 1-2.83 0L2 12V2h10l8.59 8.59a2 2 0 0 1 0 2.82z"/><line x1="7" y1="7" x2="7.01" y2="7"/></symbol>
<symbol id="i-shield" viewBox="0 0 24 24"><path d="M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z"/></symbol>
<symbol id="i-zap" viewBox="0 0 24 24"><polygon points="13 2 3 14 12 14 11 22 21 10 12 10 13 2"/></symbol>
<symbol id="i-gauge" viewBox="0 0 24 24"><path d="M12 14 8 10"/><path d="M3.34 19a10 10 0 1 1 17.32 0"/></symbol>
<symbol id="i-trend" viewBox="0 0 24 24"><polyline points="23 6 13.5 15.5 8.5 10.5 1 18"/><polyline points="17 6 23 6 23 12"/></symbol>
<symbol id="i-chart" viewBox="0 0 24 24"><line x1="12" y1="20" x2="12" y2="10"/><line x1="18" y1="20" x2="18" y2="4"/><line x1="6" y1="20" x2="6" y2="16"/></symbol>
<symbol id="i-sparkles" viewBox="0 0 24 24"><path d="M12 3l1.9 5.1L19 10l-5.1 1.9L12 17l-1.9-5.1L5 10l5.1-1.9L12 3z"/><path d="M5 3v4M19 17v4M3 5h4M17 19h4"/></symbol>
<symbol id="i-wand" viewBox="0 0 24 24"><path d="M15 4V2M15 16v-2M8 9h2M20 9h2M17.8 11.8 19 13M15 9h0M17.8 6.2 19 5M3 21l9-9M12.2 6.2 11 5"/></symbol>
<symbol id="i-user-plus" viewBox="0 0 24 24"><path d="M16 21v-2a4 4 0 0 0-4-4H6a4 4 0 0 0-4 4v2"/><circle cx="9" cy="7" r="4"/><line x1="19" y1="8" x2="19" y2="14"/><line x1="22" y1="11" x2="16" y2="11"/></symbol>
<symbol id="i-chevron-r" viewBox="0 0 24 24"><polyline points="9 18 15 12 9 6"/></symbol>
<symbol id="i-chevron-d" viewBox="0 0 24 24"><polyline points="6 9 12 15 18 9"/></symbol>
<symbol id="i-arrow-left" viewBox="0 0 24 24"><line x1="19" y1="12" x2="5" y2="12"/><polyline points="12 19 5 12 12 5"/></symbol>
<symbol id="i-eye" viewBox="0 0 24 24"><path d="M1 12s4-8 11-8 11 8 11 8-4 8-11 8-11-8-11-8z"/><circle cx="12" cy="12" r="3"/></symbol>
<symbol id="i-key" viewBox="0 0 24 24"><path d="M21 2l-2 2m-7.61 7.61a5.5 5.5 0 1 1-7.778 7.778 5.5 5.5 0 0 1 7.777-7.777zm0 0L15.5 7.5m0 0 3 3L22 7l-3-3"/></symbol>
<symbol id="i-server" viewBox="0 0 24 24"><rect x="2" y="3" width="20" height="7" rx="2"/><rect x="2" y="14" width="20" height="7" rx="2"/><line x1="6" y1="6.5" x2="6.01" y2="6.5"/><line x1="6" y1="17.5" x2="6.01" y2="17.5"/></symbol>
<symbol id="i-layers" viewBox="0 0 24 24"><polygon points="12 2 2 7 12 12 22 7 12 2"/><polyline points="2 17 12 22 22 17"/><polyline points="2 12 12 17 22 12"/></symbol>
<symbol id="i-target" viewBox="0 0 24 24"><circle cx="12" cy="12" r="10"/><circle cx="12" cy="12" r="6"/><circle cx="12" cy="12" r="2"/></symbol>
<symbol id="i-clipboard" viewBox="0 0 24 24"><path d="M16 4h2a2 2 0 0 1 2 2v14a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2h2"/><rect x="8" y="2" width="8" height="4" rx="1"/><path d="m9 14 2 2 4-4"/></symbol>
<symbol id="i-message" viewBox="0 0 24 24"><path d="M21 15a2 2 0 0 1-2 2H7l-4 4V5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2z"/></symbol>
<symbol id="i-save" viewBox="0 0 24 24"><path d="M19 21H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h11l5 5v11a2 2 0 0 1-2 2z"/><polyline points="17 21 17 13 7 13 7 21"/><polyline points="7 3 7 8 15 8"/></symbol>
<symbol id="i-paperclip" viewBox="0 0 24 24"><path d="M21.44 11.05l-9.19 9.19a6 6 0 0 1-8.49-8.49l9.19-9.19a4 4 0 0 1 5.66 5.66l-9.2 9.19a2 2 0 0 1-2.83-2.83l8.49-8.48"/></symbol>
<symbol id="i-history" viewBox="0 0 24 24"><path d="M3 3v5h5"/><path d="M3.05 13A9 9 0 1 0 6 5.3L3 8"/><polyline points="12 7 12 12 15 15"/></symbol>
</defs></svg>
```

## LISTA EKRANÓW (ID → plik → treść)
Patrz `SPEC_MOCKUPY.md` w tym katalogu — pełny opis każdego ekranu i jego zawartości.
