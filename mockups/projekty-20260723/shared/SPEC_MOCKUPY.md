# SPEC MOCKUPÓW — Projekty (Project Studio)

Każdy ekran: patrz `BUILD_CONTRACT.md` (head, sidebar, ikony, reguły, okna-jako-okna, kompletność CRUD, odstępy). Wzorzec jakości: `p03-przeglad.html`. Zawartość poszczególnych ekranów niżej. „OKNO" = overlay `.window-backdrop` + `.window` nad widocznym ekranem bazowym (podaj jaki ekran bazowy przyciemniasz). Wszystkie teksty PL, dane realistyczne (projekt „Portal Klienta B2B").

Wewnątrz modułu projektu każdy ekran pełnostronicowy ma: breadcrumb (`Projekty › Portal Klienta B2B › <zakładka>`), `detail-header` (jak w P03, z chipem „Twoja rola") i `tabs-bar` z aktywną właściwą zakładką. Ekrany testowe mają dodatkowo drugi rząd pod-zakładek (Przypadki/Zestawy/Przebiegi/Generowania/Środowiska/Harmonogramy/Raporty) jako `.segmented` lub druga `tabs-bar`.

---

## P01 — p01-lista.html — Lista projektów
Pełna strona, sidebar „Projekty" aktywne, BEZ detail-header (to widok listy). Nagłówek: „Projekty" + podtytuł „3 aktywne · należysz do 3" + prawy: `.segmented` widok (karty/tabela) + `.search-box` + `btn-primary` „Nowy projekt" (otwiera: P02). Filtry: `.filter-chip` (Aktywne/Zarchiwizowane/Wszystkie, Moja rola). Siatka `.proj-grid` z 6 kartami `.proj-card` (nazwa, opis, ikona, statystyki: przypadki, pass-rate sparkline, otwarte usterki, dokumenty; foot: chip roli + menu `⋯` z Otwórz/Archiwizuj/Usuń/Eksportuj). Dołóż jedną kartę „+ Nowy projekt" (przerywana ramka). Uwaga: przycisk „Nowy projekt" widoczny bo user ma grant tworzenia — dopisz `.hint` „Możesz tworzyć projekty". Na dole drobny wariant pustego stanu jako komentarz nie trzeba. Menu ⋯ pokaż otwarte na jednej karcie (dropdown `.tf-menu` — zrób prosty absolutny dropdown).

## P02 — p02-kreator.html — OKNO: Kreator projektu (3 kroki)
Ekran bazowy = lista projektów (uproszczona, przyciemniona backdropem). OKNO `.window.wizard`. Head: „Nowy projekt". Body: `.stepper` (1 Podstawy ·active, 2 Moduły, 3 Zespół). Pokaż KROK 1: pole Nazwa (z walidacją unikalności — pod polem `.hint` zielony „Nazwa dostępna"), Opis (textarea), wybór szablonu jako `.choice-grid` (Pusty / Projekt testowy ·selected / Projekt dokumentacyjny). Foot: „Anuluj" + „Dalej" (primary). Dodaj drugą, mniejszą reprezentację (poniżej okna w tym samym pliku jako druga `.screen`) pokazującą KROK 2 (Moduły — lista toggle: Wiedza [zawsze wł, disabled on], Testy, Dokumentacja, Chat, Zadania z opisami) oraz KROK 3 (Zespół — picker użytkowników + rola). Czyli 3 sekcje `.screen` w jednym pliku, każda z oknem na innym kroku.

## W01 — w01-wiedza-zrodla.html — Wiedza: Źródła
Pełna strona, zakładka „Wiedza" aktywna, pod-nav Wiedzy: `.segmented` (Źródła ·active / Przeszukaj / Pliki). Nagłówek sekcji + `btn-primary` „Dodaj źródło" (otwiera: W02). Lista `.source-row` (5 pozycji różnych typów): „Specyfikacja API v2.pdf" (dokument, ready, 342 chunki), „Wymagania bezpieczeństwa.docx" (ready), „portal-b2b (repo git)" (git, gałąź main, ready, przycisk „Odśwież"), „openapi.yaml" (spec API, ready, 48 endpointów), „Dokumentacja użytkownika (URL)" (ingesting — pokaż `.ingest-bar` 60% + „Anuluj ingest"). Każdy wiersz: status-pill, meta, menu ⋯ (Edytuj/Re-ingest/Usuń). Jeden wiersz w stanie error („Nieznany format" z akcją). Pod listą sekcja info: jak działa git (clone do projektu, Odśwież=fetch+delta).

## W02 — w02-dodaj-zrodlo.html — OKNO: Dodaj źródło
Bazowy = W01 przyciemniony. OKNO `.window`. Head „Dodaj źródło wiedzy". Body: `.choice-grid` z 5 typami (Dokumenty ·selected, Adres URL, Repozytorium git, Archiwum ZIP z kodem, Specyfikacja API/OpenAPI) — każdy `.choice-card` z ikoną i opisem. Poniżej — dynamiczny formularz dla wybranego typu. Zrób 3 sekcje `.screen` w pliku: (1) typ „Dokumenty" (dropzone `.dropzone` upload + lista dodanych plików); (2) typ „Repozytorium git" (URL, gałąź, token jako password z `.hint` „szyfrowany", globy include/exclude w textarea z prefill, limity); (3) typ „Specyfikacja API" (upload/URL pliku OpenAPI + `.hint` o parserze). Foot: Anuluj + „Dodaj i indeksuj".

## W03 — w03-wiedza-szukaj.html — Wiedza: Przeszukaj
Pełna strona, Wiedza aktywna, pod-nav „Przeszukaj". Duże pole zapytania + filtry (źródło, typ pliku). Wyniki: lista kart wyników (fragment tekstu z podświetleniem `.hl`, ścieżka pliku/źródło mono, score chip, przycisk „Podgląd" i „Zapytaj w chacie"). 4-5 wyników. Pokaż też stan pusty jako drugi `.screen` („Brak wyników dla zapytania…").

## W04 — w04-wiedza-pliki.html — Wiedza: Pliki
Pełna strona, Wiedza aktywna, pod-nav „Pliki". Layout 2-kolumnowy: lewa `.file-tree` (drzewo repo portal-b2b: src/, docs/, README.md, package.json — z rozwinięciem), prawa: podgląd wybranego pliku w `.code-editor` (read-only, kolorowany, numeracja linii) — pokaż fragment realnego kodu JS/TS. Nad podglądem: ścieżka pliku + status ingestu + akcje „Wyklucz z indeksu"/„Ponów ingest".

## T01 — t01-przypadki.html — Testy: Przypadki
Pełna strona, zakładka „Testy" aktywna, pod-nav Testów (Przypadki ·active). Pasek: `.search-box`, filtry (typ: manual/ui/api/unit/perf/security; status; priorytet; tag), `btn` „Import CSV", `btn-primary` „Generuj testy" (otwiera: T04) + „Nowy przypadek" (otwiera: T02). `.tf-table` z ~10 przypadkami: checkbox zaznaczenia, tytuł, typ (chip), priorytet (`.prio`), tagi (chipy), status-pill (draft/review/approved/deprecated), origin (ikona user/agent), ostatni wynik, menu ⋯ (Edytuj/Duplikuj/Zmień status/Usuń). Nad tabelą gdy zaznaczono: pasek akcji zbiorczych (Dodaj do zestawu / Zmień status / Usuń) — pokaż aktywny z „3 zaznaczone". Origin agent = mała ikona sparkles z tooltipem „wygenerowane".

## T02 — t02-edytor-manualny.html — Edytor przypadku manualnego
Pełna strona (edytor to pełny widok, nie okno). Breadcrumb + tytuł edytowalny + status-pill draft + akcje (Zapisz, Wyślij do przeglądu, Historia wersji, Usuń). Sekcje: metadane (priorytet select, tagi `.ai-chips`-style input, powiązane źródła — picker chips), Warunki wstępne (textarea), Kroki: lista `.step-edit-row` (numer, akcja, oczekiwany rezultat, uchwyt drag, usuń) + „Dodaj krok". Dane testowe (textarea). Załączniki (`.dropzone` mini). Prawy panel/pod spodem: „Historia wersji" (lista wersji z Przywróć). Zaznacz `.hint` przy cofnięciu review „wymagane uzasadnienie".

## T03 — t03-edytor-kod.html — Edytor przypadku kodowego (KLUCZOWY)
Pełna strona. Przypadek typu „ui (Playwright)" lub „api (pytest)". Górny pasek jak T02 + wybór języka (select) + „Uruchom próbnie". Główny obszar: `.code-editor` z tabami plików (test_checkout.py active), numeracja, kolorowana składnia Python (realny pytest/playwright kod), pasek narzędzi (Zapisz, Formatuj). Pod edytorem `.ai-panel`: nagłówek „Asystent AI", `.ai-chips` (Wygeneruj z opisu / Popraw błąd / Wyjaśnij / Dopisz asercje), oraz przykład `.ai-suggestion` z podglądem diff (`.diff-line add`/`.diff-line del`) i przyciskami Zastosuj/Odrzuć. Pokaż że AI streamuje (spinner/kropka). To ma sprzedawać „własny edytor z AI".

## T04 — t04-generuj.html — OKNO: Generuj testy (kreator 3 kroki)
Bazowy = T01 przyciemniony. OKNO `.window.wizard`. `.stepper` (1 Co · 2 Z czego · 3 Jak). Zrób 3 sekcje `.screen`:
KROK 1 „Co": rodzaj testów (`.choice-grid`: manualne/UI/API/unit/perf/security), liczność (input lub „auto"), instrukcje (textarea), szablon.
KROK 2 „Z czego": lista źródeł z checkboxami (nie-ready wyszarzone z tooltipem statusu; jedno „stale" z ostrzeżeniem), opcja „użyj zatwierdzonych dokumentów jako kontekst".
KROK 3 „Jak": wybór agenta (select „Generator scenariuszy"), toggle „pętla krytyka" + max iteracji, budżet (info). Foot: Wstecz/Dalej/„Uruchom generowanie".

## T05 — t05-generowania.html — Generowania (lista + akceptacja live)
Pełna strona, pod-nav Testów „Generowania". Górą tabela gen_runs (rodzaj, status-pill running/review/accepted/rejected, agent, start, wynik „14 przypadków czeka", akcje: Otwórz postęp/Przegląd wyników/Anuluj/Uruchom ponownie/Usuń). Poniżej — dwie sekcje `.screen`: (A) „Postęp na żywo" = `.agent-run` z iteracjami agenta (agent_context → llm(tools) → tool_exec, pętla krytyka), spinner, log; (B) „Przegląd wyników" = lista wygenerowanych przypadków z checkboxami i podglądem, edycja inline, przyciski „Akceptuj zaznaczone" / „Odrzuć resztę".

## T06 — t06-zestawy.html — Zestawy
Pełna strona, pod-nav „Zestawy". Górą lista zestawów (`.tf-table`: nazwa, liczba przypadków wg typu, ostatni przebieg+wynik, badge „zawiera deprecated" na jednym, menu Edytuj/Duplikuj/Uruchom/Usuń; „Nowy zestaw" primary). Druga sekcja `.screen`: Edytor zestawu = `.two-pane` (lewa: dostępne przypadki approved z filtrem+checkbox; prawa: przypadki w zestawie z uchwytami drag do kolejności i usuń). Nagłówek edytora: nazwa zestawu + Zapisz.

## T07 — t07-przebiegi.html — Przebiegi (lista)
Pełna strona, pod-nav „Przebiegi". `.search-box` + filtry (typ manual/auto/perf, status) + „Nowy przebieg" (otwiera: T08). `.tf-table`: id, zestaw/ad-hoc, typ (chip), środowisko, status-pill (+ jeden „error: utracono kontakt"), pasek postępu, wynik x/y, wyzwolenie (manual/harmonogram), start/koniec, kto, menu (Otwórz/Nowy z nieudanych/Usuń/Oznacz błędny). Różne statusy w wierszach (running, passed, partial, cancelled, error).

## T08 — t08-nowy-przebieg.html — OKNO: Nowy przebieg
Bazowy = T07. OKNO `.window`. Head „Nowy przebieg". Body: wybór typu (`.segmented` Manualny/Automatyczny/Wydajnościowy), wybór zestawu (select) LUB ręczny wybór przypadków, środowisko (select — dla manualnego opcjonalne, pole „testowane na" tekst), sekcja przypisań testerów (dla manualnego: radio „całość jednej osobie / podział per przypadek / pula — każdy bierze z puli" + picker testerów). Gdy zestaw mieszany — info „powstaną 2 przebiegi (manualny + automatyczny)". Foot: Anuluj / „Uruchom przebieg". Pokaż drugi `.screen` wariant: brak wdrożonego serwisu test-runner → banner „Serwis test-runner nie jest wdrożony" + link do wizarda deploy, przycisk Uruchom disabled.

## T09 — t09-pulpit-testera.html — Pulpit testera (KLUCZOWY)
Pełna strona ALE w trybie „wykonanie" — breadcrumb `… › Przebieg #241 › Wykonanie`. Górą: nazwa przypadku „Checkout — płatność kartą" (2/8 przypisanych), priorytet, środowisko z base_url (do skopiowania), pasek postępu przypadków. Warunki wstępne (box). Kroki: lista `.exec-step` (numer, akcja, oczekiwany rezultat, 4 przyciski `.verdict-btn` Pass/Fail/Blocked/Skip, notatka, załącz zrzut). Pokaż: krok 1 passed, krok 2 failed (z wymaganą notatką + przycisk „Zgłoś usterkę" otwiera Z02), krok 3 aktywny, reszta pending. Stopka: czas trwania (auto), pole „Konfiguracja testera" (przeglądarka/OS), komentarz końcowy, „Zakończ przypadek" → „następny przypisany". Prawy pasek/box: lista przypisanych mi przypadków w tym przebiegu (nawigacja).

## T10 — t10-przebieg-auto.html — Przebieg auto/perf LIVE
Pełna strona. Górą: nazwa przebiegu, status running, elapsed, pasek postępu %, „Stop". `.banner warn` watchdog „Runner nie odpowiada od 6 s — sprawdzam…". Główna część: macierz `.run-matrix` (przypadki × status done/running/pending) LUB lista przypadków z statusami inkrementalnie. `.live-log` (mono, kolorowane linie, kursor). Dla perf: dodatkowa sekcja live metryk (RPS, p95, error-rate — mini liczby + iskierki). Dwa `.screen`: (A) auto w toku; (B) perf w toku z metrykami.

## T11 — t11-przebieg-wyniki.html — Szczegół przebiegu + wyniki
Pełna strona. Górą podsumowanie: `.kpi-grid` (passed/failed/blocked/skipped, czas), status pill. Akcje: „Nowy przebieg z nieudanych", „Utwórz raport", Eksport CSV/PDF, „Usuń przebieg". `.tf-table` per przypadek: status, czas, błąd (skrót), artefakty (ikony: zrzut/log/trace do pobrania), rozwiń → kroki. Jeden failed z podglądem błędu i miniaturą zrzutu. Dla perf: sekcja wykresów (p50/p90/p99 bar per endpoint, RPS+error-rate line) — użyj chart-svg.

## T12 — t12-srodowiska.html — Środowiska
Pełna strona, pod-nav „Środowiska". `.tf-table`: nazwa, typ (web/api), base_url (mono), auth (chip has_secret), status zatwierdzenia (pill: zatwierdzone/oczekuje/odrzucone — z powodem w tooltipie), akcje (Edytuj/Test połączenia/Usuń; „Test połączenia" disabled bez runnera). „Nowe środowisko" primary (otwiera okno — pokaż drugi `.screen` OKNO dodania: nazwa, typ, base_url, auth typ+sekret, nagłówki; `.hint` „adres prywatny/LAN wymaga zatwierdzenia admina"). Trzeci `.screen`: widok admina „Kolejka środowisk do zatwierdzenia" (karty z Zatwierdź/Odrzuć+powód).

## T13 — t13-harmonogramy.html — Harmonogramy
Pełna strona, pod-nav „Harmonogramy". `.tf-table`: nazwa (zestaw+środowisko), tryb (interwał/codziennie), następne uruchomienie, ostatni wynik (pill, jeden „blocked: środowisko oczekuje"), toggle enabled, menu Edytuj/Usuń. „Nowy harmonogram" primary → drugi `.screen` OKNO: zestaw (select), środowisko (select), tryb (`.segmented` interwał/codziennie), spec (input Ns/m/h/d lub HH:MM z walidacją `.hint`), limit czasu przebiegu.

## T14 — t14-raporty.html — Raporty (dashboard)
Pełna strona, pod-nav „Raporty". Górą: zakres dat (datepicker range) + filtr zestawu/środowiska + Eksport CSV/PDF + „Zapisz jako raport". Siatka raportów (użyj chart-svg + tabele): (1) Trend pass-rate (line), (2) Wyniki przebiegów (stacked bar), (3) Najczęściej oblewające przypadki (tabela top + chip „flaky"), (4) Czas wykonania (line), (5) Pokrycie źródeł/traceability (tabela: źródło/plik → liczba przypadków → ostatni wynik; wiersze `tr.uncovered` z `.cov-cell none` podświetlone „nieprzykryte"), (6) Usterki (pie wg severity + trend). Rozmieść w grid-2.

## D01 — d01-dokumenty.html — Dokumentacja
Pełna strona, zakładka „Dokumentacja". Layout 2-kol: lewa drzewo folderów (`.file-tree` z folderami: Wymagania/, Instrukcje/, Raporty/), prawa lista dokumentów (`.tf-table`: tytuł, status-pill draft/review/approved/archived, wersja, autor, data, menu Edytuj/Archiwizuj/Usuń/Przenieś). Nagłówek: „Nowy dokument" (Pusty / Generuj z wiedzy) + filtr „Pokaż zarchiwizowane". Jeden dokument autora agent (ikona sparkles).

## D02 — d02-edytor-dokumentu.html — Edytor dokumentu
Pełna strona. Górą tytuł + status + akcje (Zapisz, Wyślij do przeglądu, Popraw z agentem, Historia, Eksport PDF, Archiwizuj). Split-view: lewa textarea markdown z prostym toolbarem, prawa podgląd wyrenderowany. Druga sekcja `.screen`: efekt „Popraw z agentem" = widok obok siebie stara/nowa wersja z różnicami (`.diff-line`) + Zastosuj/Odrzuć + pole instrukcji do agenta.

## C01 — c01-chat.html — Chat projektu
Pełna strona, zakładka „Chat". `.chat-layout`: lewa lista rozmów (`.chat-conv`, „Nowa rozmowa", CRUD zmiana nazwy/usuń), prawa `.chat-main`: wiadomości (`.chat-msg` user/ai) z cytowaniami `.chat-cite` [1][2] linkującymi do źródeł, przycisk na odpowiedzi „Wyślij do dokumentu"/„Utwórz zadanie". Input z „Zapytaj o projekt…". Pokaż odpowiedź AI z cytatami i sekcją „Źródła" pod spodem.

## Z01 — z01-zadania.html — Zadania
Pełna strona, zakładka „Zadania". Przełącznik `.segmented` (Lista/Tablica). Sekcja 1 `.screen`: Lista (`.tf-table`: tytuł, typ task/defect chip, severity, status, przypisany avatar, termin, powiązania, menu). Filtry (typ, status, przypisany „moje", severity) + „Nowe zadanie" (otwiera Z02). Sekcja 2 `.screen`: Tablica `.kanban` (4 kolumny todo/in_progress/review/done, karty `.kanban-card` z avatarem, drag). Jedna usterka krytyczna widoczna.

## Z02 — z02-zadanie.html — OKNO: Zadanie / usterka
Bazowy = Z01. OKNO `.window` (szersze). Head „Usterka #DEF-042". Body: typ (task/defect toggle), tytuł, opis MD, severity, priorytet, przypisany (picker), termin, powiązania (chipy: przypadek, przebieg, dokument), załączniki, sekcja komentarzy (lista + dodaj, edycja własnego). Pokaż prefill z pulpitu testera („utworzono z kroku 2 przebiegu #241"). Foot: Usuń / Zapisz.

## X02 — x02-polaczenia-ml.html — Połączenia (ML Studio)
Pełna strona, zakładka „Połączenia". Sekcja „ML Studio": karta podlinkowanego projektu ML (nazwa, typ, status ostatniego treningu z metrykami, modele, „Otwórz w ML Studio") + „Utwórz projekt ML" (otwiera okno) + „Podłącz istniejący". Sekcja „Dostęp programistyczny": id projektu, przykład bloku `project_knowledge` we flow, lista narzędzi `core.project_*`. Druga sekcja `.screen`: OKNO „Utwórz projekt ML" = nazwa, typ modelu, oraz `role_map` (mapowanie ról projekt→ML: owner→editor itd. w tabelce edytowalnej) + toggle „Synchronizuj uprawnienia". Zaznacz `.hint`: dostęp do ML wynika z ról projektu.

## X03 — x03-czlonkowie.html — Członkowie
Pełna strona, zakładka „Członkowie" (lub z detail-header). `.tf-table`: użytkownik (avatar+nazwa), rola (select inline: owner/manager/editor/tester/viewer), dodany przez, data, usuń. „Zaproś" primary → drugi `.screen` OKNO: picker użytkowników org (multi) + rola + `.hint`. Zaznacz: owner jeden, transfer własności jako akcja. Chip „(Ty)" przy sobie.

## X04 — x04-ustawienia.html — Ustawienia
Pełna strona, zakładka „Ustawienia". Pod-nav `.segmented` sekcji: Podstawy / Moduły / Agenci projektu / Tagi / Retencja / Eksport. Pokaż w jednym ekranie kilka sekcji `.section-card`:
- Podstawy: nazwa, opis, `.setting-row` archiwizacja, strefa „Usuń projekt" (danger).
- Moduły: toggle per moduł.
- Agenci projektu: `.setting-row` per funkcja (chat, generator manualnych/UI/API/unit/perf, agent bezpieczeństwa, dokumentalista, krytyk) z select agenta/modelu.
- Tagi: lista tagów z licznikami + zmiana nazwy/usuń + „Nowy tag".
- Retencja: pola dni (artefakty przebiegów, activity_log, zrzuty, wersje).

## A01 — a01-agenci-lista.html — Agenci (lista)
Pełna strona, sidebar „Agenci" aktywne (nie Projekty!). Nagłówek „Agenci" + podtytuł + „Nowy agent" (otwiera: A02) + „Szablony zespołów" (otwiera: A05) + search. `.agent-grid` z 6 kartami `.agent-card` (avatar ikona, nazwa, rola-podpis, opis, foot: chipy narzędzi + status enabled + menu Edytuj/Duplikuj/Wypróbuj/Usuń). Agenci: Generator scenariuszy, Krytyk wymagań, Generator Playwright, Agent bezpieczeństwa, Dokumentalista, Nadzorca procesu. Karta „+ Nowy agent" przerywana.

## A02 — a02-kreator-agenta.html — OKNO: Kreator agenta (KLUCZOWY, 3 kroki)
Bazowy = A01. OKNO `.window.wizard` (szerokie). `.stepper` (1 Kim jest · 2 Co potrafi · 3 Jak działa). Zrób 3 sekcje `.screen`, każda okno na innym kroku:
KROK 1 „Kim jest": nazwa, `.choice-grid`/persona `.persona-card` (Researcher/Dokumentalista/Krytyk/Nadzorca/Tester/Własny), pole „system prompt" ALE opisane po ludzku „Opisz, co ten agent ma robić" (duża textarea, z podpowiedzią). KLUCZOWE: sekcja „Przykłady dla agenta" — `.dropzone` „Wrzuć przykładowy dokument wejściowy" + `.dropzone` „Wrzuć przykład oczekiwanej odpowiedzi" (few-shot). Box asystenta `.assistant-box` „Pomogę Ci to napisać" (otwiera A04).
KROK 2 „Co potrafi": grupy narzędzi `.tool-group` = REALNY katalog platformy (grupa = wtyczka WASM albo core): Projekty (core.project_search/get_document/list_cases/run_summary/list_tasks), Internet — wtyczka Deep Research (search_web/fetch_url/read_search_results), Bazy RAG — wtyczka RAG (ask/list_collections…), Notatki — wtyczka Notes, Kontakty — wtyczka Contacts, Narzędzia zewnętrzne — wtyczka MCP (mcp_list_tools/mcp_call_tool), Podstawowe — core (skill_view/ask_user/agent_spawn). Toggle grupy = cała wtyczka (addon.*); rozwinięcie = pojedyncze narzędzia z mono-identyfikatorami. Osobno sekcja „Źródła wiedzy (RAG)" (bazy projektów + kolekcje wtyczki RAG) i picker skilli. Hint: lista pochodzi z zainstalowanych wtyczek — nowa wtyczka = nowe grupy automatycznie.
KROK 3 „Jak działa": model (select po ludzku „Szybki / Zbalansowany / Najlepszy"), suwaki (max iteracji, kreatywność), toggle routable z wyjaśnieniem, przycisk „Zapisz i wypróbuj" (→ A03).

## A03 — a03-playground.html — Playground agenta
Pełna strona (Agenci aktywne). Nagłówek: agent „Generator scenariuszy" + „Edytuj" + „Publikuj". Layout 2-kol: lewa czat testowy (`.chat-*`, user wrzuca „Wygeneruj testy z tego dokumentu" + załącznik), prawa `.agent-run` LIVE (iteracje, tool-calle, tokeny) + na dole podgląd wyniku. Baner `.banner info` „Tryb testowy — nic nie zostanie zapisane".

## A04 — a04-asystent-budowy.html — OKNO: Asystent budowy agenta
Bazowy = A02 (krok 1). OKNO `.window`. Head „Asystent budowy agenta" z ikoną sparkles. Body: mini-czat gdzie asystent zadaje pytania („Co ma robić agent? Jakie dane dostaje? Jak ma wyglądać odpowiedź?") i na podstawie odpowiedzi generuje gotowy system prompt + proponuje narzędzia. Pokaż wygenerowany prompt w boxie z „Wstaw do agenta". To realizacja „wbudowany agent pomagający budować agentów".

## A05 — a05-szablony-zespolow.html — OKNO: Szablony zespołów
Bazowy = A01. OKNO `.window`. Head „Szablony zespołów agentów". Body: `.choice-grid` z gotowymi zespołami: „Dokumentacja z kontrolą jakości" (Pisarz + Krytyk + Nadzorca), „Zespół testowy" (Generator + Wykonawca + Raporter), „Badania" (Researcher + Streszczacz). Każdy pokazuje diagram przepływu (proste kółka+strzałki) i listę agentów. Przycisk „Utwórz ten zespół".

## G01 — g01-okno-usun.html — OKNO: Potwierdzenie usunięcia (reużywalne)
Bazowy = dowolny (P01). OKNO `.window` małe, wariant danger. Head „Usuń projekt?". Body: ostrzeżenie z ikoną alert, lista co zostanie skasowane (baza wiedzy, workspace, artefakty), pole „przepisz nazwę aby potwierdzić" (input). Foot: Anuluj / „Usuń trwale" (btn-danger, disabled dopóki nazwa niezgodna). Pokaż też drugi wariant `.screen`: „Nowa wersja aplikacji" NIE — zamiast tego drugi wariant: potwierdzenie usunięcia przypadku z wynikami („zostanie oznaczony jako deprecated zamiast usunięcia").

## G02 — g02-powiadomienia.html — Panel powiadomień (dzwonek)
Bazowy = P03 (przegląd), z otwartym `.notif-panel` przy dzwonku. Panel: nagłówek „Powiadomienia" + „oznacz wszystkie", lista `.notif-item` (przydział przypadku, koniec przebiegu, nowa usterka, środowisko do zatwierdzenia, koniec generowania) — mix unread/read. Stopka „Zobacz wszystkie / Moje zadania testowe".
