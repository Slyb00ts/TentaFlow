# Projekty — praca zespołu, roadmapa, bezpieczeństwo, struktura (mockupy 2026-09-29) — kontrakt budowy

Zestaw ekranów dla planów:
- `docs/PROJECT_STUDIO_WORKFLOW_PLAN.md` (praca zespołu, workflow, tablica, sprinty, wersje,
  roadmapa, zasoby, changelog, plan z dokumentów),
- `docs/PROJECT_STUDIO_SECURITY_PLAN.md` (moduł Bezpieczeństwo),
- `docs/ORG_STRUCTURE_PLAN.md` (struktura organizacyjna, prywatność).

Kontrakt wizualny jest ten sam co `mockups/projekty-20260723/shared/BUILD_CONTRACT.md` (styles.css
+ projekty.css), rozszerzony o `shared/praca.css` (komponenty i ruch) i `shared/praca.js`
(drobne interakcje).

**Punkt startu każdego ekranu: skopiuj `_szablon.html`** (ma head, ikony, sidebar, nagłówek
projektu NextApp, zakładki, przykłady komponentów). Nie wymyślaj innego szkieletu.

## 0. Mapa nawigacji — WIĄŻĄCA (rewizja 3: oryginalny układ Projektów + rozszerzenia)

**Zasada nadrzędna: te mockupy ROZSZERZAJĄ istniejące Projekty (`mockups/projekty-20260723`), a nie
projektują ich od nowa.** Nagłówek projektu, kolejność i wygląd zakładek, pasek widoków Zadań, lista
i kanban mają wygląd oryginału. Nowe rzeczy dochodzą w tym samym stylu i w miejscach, gdzie użytkownik
ich szuka. Fragmenty są w `shared/nav.html`, a style oryginalnych Zadań w `shared/zadania.css`.

| Poziom | Co |
|---|---|
| Menu boczne | jak w TentaFlow + Flow Builder, Struktura organizacyjna, Prywatność; „Projekty” → lista projektów (`p01-lista.html`); stopka → globalny profil (`g01`) |
| Poziom aplikacji Projekty | zakładki **Projekty · Moja praca · Zasoby**, pod nimi oryginalny `.page-head` (tytuł, opis, akcje) — `p01`, `a01`, `c04` |
| Projekt | oryginalny `detail-header` (chipy + Członkowie · Eksport · Ustawienia) i **oryginalne zakładki** Przegląd · Wiedza · Testy · Dokumentacja · Chat · Zadania **+ nowe: Roadmapa · Wydania · Bezpieczeństwo** · Połączenia (tylko funkcje włączone w projekcie) |
| Podprojekty (rewizja 4) | w `detail-header` przy nazwie przycisk `.sub-switch` („8 podprojektów” / „Zmień podprojekt”) otwiera drzewo (`praca.js`, dane `TREE`); okruszki pokazują pełną ścieżkę; w podprojekcie linia `.inherit-line` „dziedziczy / własne”; w Liście i Tablicy przełącznik `.scope-toggle` **Ten projekt / Z podprojektami** na początku rzędu filtrów, zadania z podprojektów mają `.proj-tag` i własny status („u siebie: …”) |
| Zadania | oryginalny pasek: przełącznik **Lista · Tablica** rozszerzony o **Backlog i sprinty · Do weryfikacji · Daily · Raport tygodniowy · Czas pracy**, po prawej „Nowe zadanie”; pod spodem oryginalny rząd filtrów |
| Inne zakładki z podwidokami | przełącznik `segmented` pod zakładkami (ten sam komponent co w Wiedzy i Testach oryginału) |
| Ekrany spoza tego zestawu | zakładki/podwidoki, których tu nie ma (np. Przegląd, Chat), mają `href="#"` z podpowiedzią „istniejący ekran” — **nigdy nie przenoszą do mockupów innego projektu** |

Przypisanie ekranów do nawigacji:

| Zakładka / miejsce | Podwidok → plik |
|---|---|
| Aplikacja | Projekty → p01 · Moja praca → a01 · Zasoby → c04 |
| Zadania (NextApp) | Lista → a06 · Tablica → a02 (okno Nowe zadanie → a04, karta zadania → a03) · Backlog i sprinty → b01 · Do weryfikacji → a05 · Daily → b02 · Raport tygodniowy → b03 · Czas pracy → b04 |
| Roadmapa (**projekt Wdrożenie DCIM** — wszystkie ekrany roadmapy są w tym projekcie) | Oś czasu → c01 (okno przesunięcia → c05) · Teraz / Następnie / Później → c02 · Alokacje → c03 · Propozycje planu → e02 |
| Wydania (NextApp) | Wersje → b05 · Changelog → b06 |
| Testy (NextApp) | Środowiska → d06 (reszta podwidoków → istniejące ekrany Testów) |
| Bezpieczeństwo (NextApp) | Pulpit → e03 · Znaleziska → e04 · Wydania i SBOM → e07 · PSIRT → e05 · Pentesty → e06 · Wyjątki i obowiązki → e08 |
| Wiedza (**Wdrożenie DCIM**) | Źródła i pliki → **e01** (pliki projektu używane przez Chat; zaznaczenie plików → „Przygotuj plan projektu” → postęp analizy w panelu bocznym) |
| Ustawienia projektu (NextApp) | Funkcje → d03 · Workflow → d01 (Flow Builder) · SLA i kalendarz → d04 · Moduły → d05 · Członkowie → d02 · **Podprojekty → d09** · Integracje git → d08 |
| Podprojekt Energetyka Centrum · Utrzymanie | **pełne zakładki i pełny pasek Zadań jak w NextApp**; Zadania → Tablica → **a07** (pozostałe podwidoki `href="#"`); okna: Nowy podprojekt → **d10**, Zakończ podprojekt → **d11**; Wdrożenie DCIM (c01–c03, e01–e02) to też podprojekt: NextApp › Energetyka Centrum |
| Globalne | Mój profil → g01 · Struktura: Drzewo → f01, Lista i import → f03, Widoczność → f06, Historia → f04 (edycja f02 z przycisku „Edytuj strukturę”) · Prywatność → f05 |

## 0a. Akcje na elementach — WIĄŻĄCE (rewizja 5)

- Każdy element, który da się zmienić, ma „⋯” (`icon-btn` z `#i-more`). Menu wybiera `data-menu` na przycisku, na wierszu/karcie albo na `<body>`.
  Menu i okna są wspólne: `shared/akcje.js` (MENUS, FIELDS, TARGETS) + `shared/akcje.css`. Nie robić menu ani okien lokalnie.
- Operacje: **Edytuj** (pola per typ) · **Przypisz / Zmień osobę** (okno wyboru osoby z obciążeniem, nieobecnością, podpowiedzią zastępcy; agenci tam, gdzie mogą wykonywać) ·
  **Przekaż z komentarzem** (komentarz wymagany) · **Przenieś** (projekt, sprint, wersja, jednostka…) · **Archiwizuj / Usuń** (skutki opisane w oknie, toast z „Cofnij”).
- Bezpośrednio przy wartości: `data-act="assign|handover|edit[:wariant]|move:<cel>|archive|delete|link:<url>|toast:<tekst>"` (np. klikalny awatar `ak-pick`, przycisk `ak-edit` „Zmień”).
- Zbiorczo: tabela `data-bulk` z checkboxami `ak-sel` i paskiem `ak-bulkbar`. Hurtowe przekazanie osoby: **f07** (wejścia z d02, f02, f03, c04, e08, g01).
- Kontrola: `scratchpad/clickall.cjs` klika każde „⋯” i każde `data-act` na każdym ekranie — wynik musi mieć 0 martwych.

## 1. Zasady UX — „chce się używać, nie przytłacza”

1. **Jedna główna akcja na ekran** (przycisk `btn-primary`), reszta drugorzędna (`btn` / `icon-btn`
   / menu `⋯`). Nigdy dwa niebieskie przyciski obok siebie.
2. **Stopniowe ujawnianie:** szczegóły w rozwinięciu, panelu bocznym albo po najechaniu. Ekran
   w stanie domyślnym pokazuje tylko to, co potrzebne do decyzji. Zaawansowane ustawienia
   są za „Więcej opcji”.
3. **Spokojna gęstość:** dużo powietrza (section-card 18–20 px, odstępy 16 px), maks. ~7 kolumn
   w tabeli, ważne liczby duże, reszta 12–13 px w `--text-2/3`.
4. **Język ludzki:** „Masz 2 zadania z terminem dziś”, a nie „SLA breach count: 2”. Podpowiedzi
   `.hint-card` tam, gdzie użytkownik robi coś pierwszy raz. Puste stany z ilustracją `.empty-illu`
   i jedną jasną akcją.
5. **Kolor ma znaczenie, zawsze to samo** (zmienne w praca.css):
   - typ zadania: funkcjonalność = indygo, błąd = czerwony (kółko), techniczne = szary,
     bezpieczeństwo = różowy (romb), epik = fiolet,
   - waga: krytyczny / istotny / mało istotny = `.weight-critical/-major/-minor`,
   - SLA: pierścień `.sla` (zielony > 50%, bursztyn < 50%, czerwony pulsujący po terminie
     lub < 10%, szary przerywany = pauza),
   - **agent = morski (`--agent`)** wszędzie: awatar `.av-agent`, karta `.tcard.is-agent`,
     kropka `.live-dot.agent`,
   - poufne = `.conf-lock` (różowa kłódka).
6. **Agent zawsze rozpoznawalny i nigdy „udający człowieka”:** ikona robota, podpis „Agent
   testujący (model lokalny: qwen3-coder)”, decyzje końcowe przy człowieku (przyciski
   „Przyjmij / Odrzuć” przy propozycjach LLM).
7. **Skróty klawiszowe** pokazane dyskretnie (`.kbd`): N = nowe zadanie, / = szukaj, ⌘K = paleta.
8. **Informacja zwrotna po akcji:** toast `.toast` z „Cofnij” (na dole po prawej, w `.toast-stack`)
   zamiast okien „Czy na pewno?” dla akcji odwracalnych. Potwierdzenie tylko dla nieodwracalnych.
9. **Role kontekstowo:** akcja niedostępna dla funkcji użytkownika jest przygaszona z tooltipem
   (`title="Wymaga funkcji Release Manager"`), a nie ukryta — użytkownik wie, że istnieje.
10. **Dostępność:** kontrast tokenów, focus widoczny, ikony z `aria-hidden`, przyciski z tekstem
    albo `title`, tabele z nagłówkami.

## 2. Ruch — atrakcyjny, ale nie męczący

- Tylko `opacity` i `transform` (design/foundations/motion.md). Nie animuj szerokości — paski
  postępu to `.meter > span` ze `--v` (scaleX).
- **Wejście ekranu:** `.pr-enter` na nagłówku, `.pr-stagger` na siatkach kart i listach (kaskada
  30–350 ms). Maksymalnie 2–3 kaskady na ekran.
- **Liczby KPI** liczą się od zera: `<span data-count="48">48</span>` (praca.js; tekst w HTML
  = wartość końcowa, więc bez JS wszystko jest poprawne).
- **Stany na żywo:** `.live-dot` (pulsuje), `.stage.active` (kręcący się znak), `.tcard.is-agent`
  (delikatny połysk, gdy agent pracuje), `.sla-danger` (puls), `.bar` roadmapy rośnie od lewej,
  linie struktury rysują się (`.org-lines path`), oś czasu historii rośnie w dół.
- **Mikro-interakcje:** `.pr-lift` (unoszenie kart na hover), `.tcard.dragging` (karta przechyla
  się i unosi przy przeciąganiu — tablica ma działające przeciąganie z praca.js:
  `draggable="true"` na `.tcard`, kolumna `.board-col` z `.board-col-head .name`), `.copy-btn`
  („Skopiowano”), `.stage.done .st-dot` (sprężyste „pyk”).
- **Okna:** `.window-backdrop` + `.window` z `animation: pr-scale-in var(--dur-slow) var(--ease-emphasized)`,
  panele boczne `pr-slide-left`.
- Wszystko wyłącza `prefers-reduced-motion` (już w praca.css) — nic nie może od animacji zależeć.
- **Umiar:** żadnych ciągłych animacji dekoracyjnych poza `.live-dot`, `.hello .wave`,
  `.empty-illu` i stanami „trwa”.

## 3. Wspólna historia danych (używaj dokładnie tych nazw)

- **Organizacja:** Solutio sp. z o.o. Zalogowana (domyślnie): **Anna Kowalska (AK, av-1)** — PM,
  Dział Realizacji. Na ekranach „z perspektywy developera” (A01 w wariancie, D07): **Marek Nowak**.
- **DZIŚ = wtorek 29.09.2026, ok. 10:30** (jedna data dla wszystkich ekranów).
- **Projekt programistyczny:** **NextApp**, klucz zadań `NA`, Scrum, **Sprint 42** (22.09–05.10, dziś
  dzień 6 z 10), **Sprint 43** (06.10–19.10, w planowaniu),
  **wersja 2.5.0** w stabilizacji (wydanie planowane 14.10), 2.4.3 = bieżąca wspierana,
  **rc: `v2.5.0-rc.2`**, środowisko testowe `test-2.5.0-rc2`.
- **Moduły NextApp** (ręczne): Uprawnienia i hasła · OMS / Widok szafy · Import OPC ·
  Procedury zmiany w szafie · Integracja SDP · Dashboard EMI.
- **Ludzie (inicjały, kolor awatara, funkcje):**
  - Anna Kowalska — AK, av-1 — PM
  - Marek Nowak — MN, av-2 — Developer + Security
  - Piotr Zieliński — PZ, av-3 — Developer (główny dev: OMS)
  - Ewa Wiśniewska — EW, av-4 — Tester (główna testerka: Uprawnienia, SDP)
  - Tomasz Lewandowski — TL, av-5 — Designer UI/UX
  - Karolina Dąbrowska — KD, av-6 — Release Manager
  - Jan Wójcik — JW, av-7 — DevOps
  - Magdalena Kamińska — MK, av-8 — Dyrektor Działu Realizacji (przełożona Anny)
  - Paweł Szymański — PS, av-2 — Developer (zastępca kierownika zespołu)
- **Agenci:** Agent testujący (Playwright, model lokalny `qwen3-coder`) · Agent security ·
  Agent review · Agent programista (drobne poprawki) · Agent eksplorator (testy swobodne,
  „klika po aplikacji”).
- **Przykładowe zadania (spójne numery):**
  - `NA-231` Błąd krytyczny SLA: „Import OPC zawiesza się przy arkuszu > 100 tys. wierszy” (PZ,
    runda 3, 20 min po terminie)
  - `NA-228` Funkcjonalność: „Nakładka Góra szafy — kafelki mocy” (MN, w code review, moduł OMS)
  - `NA-226` Funkcjonalność z projektem UI: „Nowy widok struktury przełożonych” (TL → projekt UI)
  - `NA-224` Błąd istotny: „Zegar SLA nie pauzuje w statusie Czeka na informacje” (Agent
    programista, MR !482)
  - `NA-220` Techniczne: „Aktualizacja jQuery 3.7.0 → 3.7.1 w wwwroot/lib” (blokuje NA-231
    w przykładzie zależności)
  - `NA-219` Bezpieczeństwo (poufne): „Poprawka bezpieczeństwa SEC-2026-014”
  - `NA-215` Błąd mało istotny bez SLA: „Literówka w oknie Procedury zmiany”
  - `NA-233` zgłoszony przez **Agenta eksploratora**: „Błąd 500 po dwukrotnym kliknięciu Zapisz
    w Ustawieniach systemowych” (3 wystąpienia)
- **Projekt nieprogramistyczny:** **Wdrożenie DCIM — Klient Energetyka** (roadmapa z datami,
  strumienie: Infrastruktura, Migracja danych, Integracje, Szkolenia, Odbiory; kamienie: Odbiór
  środowiska testowego 17.11, Szkolenia administratorów 08.12, Go-live 12.01.2027).
- **Git:** GitLab `gitlab.solutio.pl/dcim/nextapp` (powiązane automatycznie przez domenę),
  GitHub `github.com/solutio/nextapp-portal` (Marek: „Połącz konto”).
- Daty w formacie `29.09`, godziny `14:20`, czas pracy „6 h pracy”, „2 dni rob.”.

## 4. Lista ekranów (ID → plik → treść)

Grupa **A — Moja praca i zadania**
| ID | Plik | Treść |
|---|---|---|
| A01 | `a01-moja-praca.html` | Start dnia: powitanie („Dzień dobry, Anno”), **„Na dziś” max 5 pozycji** (termin SLA, review czekające na mnie, spotkanie daily 9:30), moje zadania w kolumnach „Teraz / Następne / Czeka na innych”, mini-oś sprintu, podpowiedź `.hint-card`, sekcja „Agenci pracują dla Ciebie” (2 agentów w toku z postępem). Spokojnie, bez tabel. |
| A02 | `a02-tablica.html` | **Tablica kanban** sprintu: kolumny ze statusów workflow (Do weryfikacji · Do zrobienia · W realizacji · Code review · Testy · Gotowe do wydania), limity WIP, swimlane „Po SLA / pozostałe”, karty z typem, kluczem, wagą, SLA, awatarem (człowiek/agent), rundą, blokadą, modułem; kolumna Testy pokazuje „2/3 werdyktów”. **Działające przeciąganie** (praca.js) + toast z Cofnij. Filtry w jednym wierszu (pill-tabs: Tablica/Lista/Backlog/Daily, „Moje”, moduł, wersja). |
| A03 | `a03-zadanie.html` | **Karta zadania NA-231 (pełny ekran)**: lewa kolumna — tytuł, opis z wklejonym zrzutem, **odtwarzacz wideo** (nagranie agenta), kroki odtworzenia, załączniki (galeria), komentarze z @wzmianką; prawa — status z przyciskami przejść („Przekaż do testów”), SLA z pierścieniem i kalendarzem („termin: dziś 14:00, robocze”), pola, moduł, wersja, powiązania (zależność od NA-220, gałąź `fix/NA-231-opc-import`, MR !481 z pipeline'em), **oś historii z rundami testów** (Runda 1/2/3, werdykty testera i agenta, zmiana wagi z uzasadnieniem, przejścia), ewidencja czasu (licznik start/stop, 5 h 20 min / szac. 4 h). |
| A04 | `a04-nowe-zadanie.html` | Okno **Nowe zadanie** nad tablicą: krok 1 wybór typu (5 kart), dla Błędu: waga (3 duże przyciski z opisem), SLA (polityka „SLA robocze” → „termin: czwartek 10:00 · 24 h pracy”), obszar wklejenia zrzutu (Ctrl+V), moduł, „Kto dalej: Do weryfikacji → PM (reguła workflow)”. Minimum pól, reszta pod „Więcej”. |
| A05 | `a05-do-weryfikacji.html` | **Zgłoszenia automatów do weryfikacji**: NA-233 od Agenta eksploratora — zrzut, nagranie, log konsoli, „3 wystąpienia” (lista z datami), podobne zadania (deduplikacja), przyciski „Potwierdź błąd” (wybór wagi → start SLA) / „Duplikat NA-…” / „Odrzuć”; licznik limitu przebiegu agenta. |

Grupa **B — Planowanie i wydania**
| B01 | `b01-backlog-sprint.html` | Backlog po lewej (priorytety, szacunki, gotowość), **Sprint 43 (planowanie)** po prawej: pojemność zespołu (osoby × h z nieobecnościami), pasek wypełnienia, przeciąganie zadań, ostrzeżenie „przekroczono o 12 h”. |
| B02 | `b02-daily.html` | **Daily** (generowane z historii): osoby w kartach — „od wczoraj” (przejścia, commity, MR), „dziś w toku”, blokady; u góry 5-zdaniowe podsumowanie LLM z przyciskiem „Przyjmij/Edytuj”; tryb „Prowadź daily” (jedna osoba naraz, timer 2 min). |
| B03 | `b03-weekly.html` | **Raport tygodniowy**: dowiezione, cycle time, zadania stojące, SLA (1 naruszenie), ping-pong (NA-231 3 rundy), ryzyka dla 2.5.0, stan bezpieczeństwa; szkic LLM do zatwierdzenia i przyciski kopiowania (tekst / Markdown / sformatowany). |
| B04 | `b04-ewidencja-czasu.html` | **Karta czasu tygodnia** Marka (dni × zadania, licznik aktywny przy NA-228), zatwierdzenie tygodnia; widok „Szacunek vs wykonanie” (per typ i moduł, wykres słupkowy), bez rankingu osób. |
| B05 | `b05-wersja-wydanie.html` | **Wersja 2.5.0**: zawartość z git vs plan (listy „weszło zgodnie z planem”, „oznaczone na 2.5, niescalone”, „weszło bez planu”), kandydat `rc.2` z linkiem do środowiska `test-2.5.0-rc2`, bramki wydania (testy, bezpieczeństwo, changelog, checklista) jako etapy `.stages`, przycisk „Zatwierdź wydanie” (Release Manager — dla Anny przygaszony z tooltipem). |
| B06 | `b06-changelog.html` | **Changelog 2.5.0**: przełącznik wariantu (Użytkownik / Administrator / Wewnętrzny), język (PL/EN), szkic LLM w sekcjach (Wymaga uwagi, Nowości, Ulepszenia, Poprawione błędy, Poprawki bezpieczeństwa) — przy każdym zdaniu dyskretny znacznik źródeł (NA-…) i fragment zadania po najechaniu; zdania wymagające potwierdzenia oznaczone; lista „zadania bez zdania”; przyciski **Kopiuj jako tekst / Markdown / sformatowany / Pobierz** z animacją. |

Grupa **C — Roadmapa i zasoby**
| C01 | `c01-roadmapa.html` | **Roadmapa projektu Wdrożenie DCIM — Klient Energetyka** (daty): strumienie jako wiersze, paski plan (przerywany obrys) vs realizacja/prognoza, kamienie milowe (romb), linie zależności (SVG), linia „dziś”, **ścieżka krytyczna** podświetlona, przełącznik „porównaj z: Plan v1 (zatw. 01.09) / Plan v2 (po aneksie, 22.09)”, odchylenie w dniach przy pozycjach. |
| C02 | `c02-teraz-nastepnie.html` | Roadmapa **NextApp** w widoku Teraz / Następnie / Później (bez dat), karty inicjatyw z postępem z zadań, modułem, wersją docelową. |
| C03 | `c03-alokacje.html` | Roadmapa z **alokacjami**: pod pozycjami osoby i zapotrzebowanie bez osoby („Tester 50% — nieobsadzone”), wiersz zespołu z obciążeniem tygodniowym (w tym pasek „inne projekty”), konflikty (Piotr 130% w tyg. 44, urlop Ewy 20–24.10). |
| C04 | `c04-zasoby.html` | **Zasoby (organizacja)**: osoby × tygodnie (mapa `.heat`), filtry (zespół, funkcja), rozwinięcie osoby → projekty, panel „Brakuje w listopadzie: 1,5 etatu testera”, nieobecności jako paski skośne bez powodu. |
| C05 | `c05-przesuniecie.html` | Okno **przesunięcia** nad C01: pozycja „Migracja danych” opóźniona o 6 dni rob. → wybór przyczyny (klient / my / zewnętrzna / zmiana zakresu) + uzasadnienie, **propozycja przeplanowania następników** (lista z nowymi datami, zaznaczanie), wpływ na kamień „Odbiór środowiska testowego”. |

Grupa **D — Konfiguracja projektu i workflow**
| D01 | `d01-edytor-workflow.html` | **Edytor workflow** (płótno w stylu Flow Buildera, ale spokojniejsze): węzły Status / Krok ludzki (z funkcją i regułą przydziału) / Krok agenta / Decyzja („Wymaga projektu UI?”) / Bramka równoległa (Tester + Agent testujący + Agent security) / Zdarzenie git („MR scalony”); paleta po lewej, inspektor po prawej, walidacja „wszystko osiągalne ✓”, przyciski Import/Eksport BPMN, „Opublikuj wersję 4” → mapa statusów. |
| D02 | `d02-czlonkowie.html` | **Członkowie i funkcje**: osoby i agenci, chipy funkcji (wiele na osobę), konto czasowe (pentester zewn. do 30.10), macierz uprawnień funkcji (obszary × poziomy) w rozwijanym panelu, powiązanie git (GitLab ✓ domena / GitHub „nie połączono”). |
| D03 | `d03-funkcje-projektu.html` | **Ustawienia → Funkcje projektu**: kafelki przełączników (Zadania, Workflow, Sprinty, Roadmapa, Moduły, Wersje i changelog, Git, Testy, Środowiska, Bezpieczeństwo, SLA, Ewidencja czasu, Wiedza, Czat) z zależnościami („Git wymaga Wersji”), szablony startowe (Programistyczny / Wdrożenie / Prosty). |
| D04 | `d04-sla-kalendarz.html` | **Polityki SLA i kalendarz roboczy**: polityki (Kalendarzowe 24/48/72 h, Robocze 24/48/72 h pracy, Bez SLA) w kartach z edycją; kalendarz: godziny 8:00–16:00, strefa Europe/Warsaw, święta 2026 (w tym Wigilia 24.12), dni wolne projektu; **podgląd przeliczenia**: „zgłoszenie pt 15:00 → termin roboczy śr 15:00 / kalendarzowy sob 15:00”. |
| D05 | `d05-moduly.html` | **Moduły NextApp**: karty modułów z głównym devem/testerem i zastępcami, opcjonalne ścieżki repo, zdrowie modułu (otwarte błędy, SLA, średnio rund), dodawanie modułu ręcznie. |
| D06 | `d06-srodowiska.html` | **Środowiska**: uruchomione (`test-2.5.0-rc2` z TTL, konta testowe per rola, link), `mr-482` (24 h), szablony, sterowniki (Docker lokalny, Portainer, Jenkins, GitLab CI, GitHub Actions), okno „Zamów środowisko” z wyborem refa i TTL. |
| D07 | `d07-profil.html` | **Profil użytkownika (Marek)**: połączone konta (GitLab ✓ automatycznie przez domenę, GitHub → „Połącz konto” z animowanym stanem), moje nieobecności, zastępstwo na czas urlopu, zdjęcie (na razie inicjały + „Dodaj zdjęcie” z informacją o widoczności). |

Grupa **E — Plan z dokumentów i bezpieczeństwo**
| E01 | `e01-przygotuj-plan.html` | **Przygotuj plan z dokumentów** (projekt Wdrożenie DCIM): wybrane dokumenty (OPZ.pdf 184 str., Umowa.docx, Załącznik nr 3 — wymagania.xlsx), etapy `.stages` (Przygotowanie ✓ → Ekstrakcja w toku 62% → Scalanie → Planowanie → Krytyk), subagenci w siatce (8 równolegle, każdy z fragmentem „Rozdz. 4.2–4.6”), licznik wymagań rosnący na żywo, szacowany czas, „możesz zamknąć okno — dokończymy w tle”. |
| E02 | `e02-propozycja-planu.html` | **Propozycja planu**: zakładki Wymagania (z cytatem i odnośnikiem „OPZ s. 42, pkt 4.3.2”, MUSI/POWINIEN/MOŻE, przyjmij/popraw/odrzuć) · Pytania do zamawiającego (lista do skopiowania) · Roadmapa i zadania (podgląd na osi, zaznaczanie) · Macierz pokrycia (wymaganie → zadania, 2 braki podświetlone) · przycisk „Zatwierdź wybrane”. Przy szacunkach: przedziały „16–24 h (wstępne)”. |
| E03 | `e03-bezpieczenstwo.html` | **Bezpieczeństwo · Pulpit** NextApp: KPI (Critical/High we wspieranych wersjach, SLA podatności, obowiązki po terminie), „Do decyzji” (propozycje VEX, wyjątek do zatwierdzenia), wersje wspierane z datą ostatniego sprawdzenia SBOM, oś zdarzeń. |
| E04 | `e04-znaleziska.html` | **Znaleziska**: tabela (źródło: Trivy/Semgrep/OSV/pentest, komponent, wersje, ważność, stan), panel boczny wybranego znaleziska z **propozycją LLM VEX** („not_affected — code_not_reachable: funkcja `jQuery.htmlPrefilter` nie jest wywoływana”, dowód: wyniki wyszukiwania w repo), przyciski Przyjmij/Odrzuć, utworzenie zadania bezpieczeństwa. |
| E05 | `e05-psirt.html` | **PSIRT — sprawa SEC-2026-014** (poufna, widok PSO): **zegar CRA** jako oś 24 h / 72 h / 14 dni z odliczaniem (wczesne ostrzeżenie wysłane ✓ 10:12, zgłoszenie za 41 h), szkic treści pól do ENISA SRP z przyciskami kopiowania, checklista, zadanie poufne powiązane, lista klientów do powiadomienia. |
| E06 | `e06-pentest.html` | **Zlecenie pentestu** 2.5.0: zakres, okno 13–24.10, testerzy (zewnętrzny z kontem czasowym), plan testów jako zestaw przypadków WSTG/ASVS (postęp), znaleziska z retestem, import raportu (PDF → propozycje znalezisk). |

Grupa **F — Struktura organizacyjna i prywatność** (sidebar: aktywne „Struktura organizacyjna”
albo „Prywatność”, bez nagłówka projektu — własny nagłówek modułu)
| F01 | `f01-struktura.html` | **Struktura — tryb prezentacji**: płótno `.org-canvas` z drzewem (Zarząd → piony → działy → zespoły), karty `.org-card` z inicjałami, kolorem jednostki, wakatem, stanowiskiem sztabowym z boku (Asystentka Zarządu), zastępca kierownika, „+12” zwinięte; wyszukiwarka z **podświetloną ścieżką** do Marka Nowaka, przycisk „Moja pozycja”, minimapa, narzędzia (zoom, dopasuj, osoby/jednostki, pełny ekran, eksport). Animowane rysowanie linii. |
| F02 | `f02-struktura-edycja.html` | **Tryb edycji** (administrator): ta sama scena, karta w trakcie przeciągania na nowego przełożonego (podświetlony cel), panel inspektora (stanowisko, jednostka, kierownik + zastępcy kierownika w kolejności, sztabowe tak/nie, data obowiązywania zmiany „od 01.11.2026”), pasek „3 zmiany zaplanowane na 01.11 · Podgląd stanu na 01.11”, cofnij/ponów. |
| F03 | `f03-struktura-lista-import.html` | **Lista + Import**: tabela osób (stanowisko, jednostka, przełożony, od kiedy, źródło) i okno **importu CSV/XLSX — tryb próbny**: podsumowanie (dodane 184, zmienione 12, błędy 3 z opisem: nieznany login, cykl, dwa stanowiska główne), podgląd drzewa, „Zapisz wszystko” zablokowane do usunięcia błędów. |
| F04 | `f04-struktura-historia.html` | **Historia i reorganizacja**: suwak „stan na dzień” (01.01.2026 ↔ dziś ↔ 01.11.2026), zmiany z diffem, reorganizacja planowana jako zestaw zmian z datą i zatwierdzeniem, porównanie przed/po (podświetlone przeniesienia). |
| F05 | `f05-prywatnosc.html` | **Prywatność**: zakładki Żądania osób (żądanie usunięcia byłego kontraktora: raport „co o nim jest” z aplikacji — Projekty, Struktura, Bezpieczeństwo — z operacją per kategoria: usuń / pseudonimizuj / zostaw z podstawą prawną), Polityki retencji (kategorie, okresy „nie ustalono — wymaga decyzji prawnej”), Blokady prawne, Rejestr kategorii. |

Plus **`index.html`** (spis ekranów w kartach z grupami) — przygotowuje koordynator.

## 5. Kompletność i jakość (sprawdź przed oddaniem)

- Każdy przycisk otwierający okno ma komentarz `<!-- otwiera: ID -->`.
- Listy/tabele: akcja dodawania w nagłówku, menu `⋯` przy wierszach.
- Brak poziomego przewijania strony (szerokie rzeczy w kontenerze `overflow:auto`).
- Tekst po polsku, bez literówek, dane z §3.
- Brak emoji (tylko ikony SVG z `shared/icons.html`; nowe ikony dodawaj w pliku ekranu jako
  `<symbol>` w tym samym sprite, nie zmieniaj `shared/icons.html`).
- **Nie zmieniaj plików w `shared/`.** Style jednego ekranu idą do `<style>` w tym pliku (krótko).
  Jeśli potrzebny jest styl wspólny dla kilku ekranów — zgłoś w raporcie, koordynator dopisze.
- Zrzut ekranu i obejrzenie wyniku jest obowiązkowe (§6).

## 6. Weryfikacja wizualna

```bash
node /private/tmp/claude-501/-Users-critix-repos-dotnet-nextapp/12ac9948-e6f6-45e7-aea5-04ebed1f0d34/scratchpad/shotmock.cjs \
  /Users/critix/repos/rust/TentaFlow/mockups/projekty-praca-20260929/a02-tablica.html
# zapisuje PNG całej strony (szer. 1600) do .../scratchpad/shots/a02-tablica.png i wypisuje ścieżkę — obejrzyj narzędziem Read; W=1280 sprawdza węższy ekran; wypisuje też błędy JS
```

Jeśli przeglądarka nie startuje w piaskownicy, uruchom polecenie z wyłączoną piaskownicą.
Ekran ma dobrze wyglądać w 1600×1000 i nie łamać się przy 1280 px szerokości.
