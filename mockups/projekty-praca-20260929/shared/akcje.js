// ===== File: akcje.js — shared actions for the projekty-praca mockups =====
// One mechanism for every screen:
//   * "⋯" buttons open a menu chosen by type: data-menu on the button, or the nearest [data-menu]
//     ancestor, or <body data-menu>. The subject (what the menu acts on) comes from data-subject
//     or is read from the nearest row/card.
//   * Any element with data-act="assign|handover|edit|move|archive|delete" opens the matching window
//     directly (data-type, data-subject, data-role optional).
// Windows: assign / hand over (person or agent picker with load and absences), edit (fields per type),
// move (target list per type), archive / delete (consequences per type). Every action ends with a toast
// that has "Cofnij".
(function () {
  var PEOPLE = [
    { ini: 'AK', av: 'av-1', name: 'Anna Kowalska', fn: 'PM', load: 70 },
    { ini: 'MN', av: 'av-2', name: 'Marek Nowak', fn: 'Programista · lider Dashboard EMI', load: 95 },
    { ini: 'PZ', av: 'av-3', name: 'Piotr Zieliński', fn: 'Programista', load: 130 },
    { ini: 'PS', av: 'av-2', name: 'Paweł Szymański', fn: 'Programista · zastępca modułów', load: 80, hint: 'zastępca modułu' },
    { ini: 'EW', av: 'av-4', name: 'Ewa Wiśniewska', fn: 'Tester', load: 85, away: 'urlop 20–24.10 → zastępuje Paweł S.' },
    { ini: 'JW', av: 'av-7', name: 'Jan Wójcik', fn: 'Programista · DevOps', load: 60 },
    { ini: 'TL', av: 'av-5', name: 'Tomasz Lewandowski', fn: 'Projektant UI', load: 50 },
    { ini: 'KD', av: 'av-6', name: 'Karolina Dąbrowska', fn: 'Release manager · tester', load: 75 }
  ];
  var AGENTS = [
    { name: 'Agent programista', fn: 'model lokalny qwen3-coder · MR do przeglądu człowieka' },
    { name: 'Agent testujący', fn: 'testy według przypadków · werdykt + dowody' },
    { name: 'Agent eksplorator', fn: 'testy swobodne · zgłasza do weryfikacji' },
    { name: 'Agent security', fn: 'triage znalezisk · propozycje VEX' }
  ];

  // Move targets per kind
  var TARGETS = {
    projects: ['NextApp', 'Energetyka Centrum · Wdrożenie DCIM', 'Energetyka Centrum · Utrzymanie', 'Port Lotniczy Wschód · Utrzymanie', 'Bank Regionalny · Wdrożenie', 'Bank Regionalny · Utrzymanie'],
    sprints: ['Sprint 42 (bieżący, do 06.10)', 'Sprint 43 (07.10–20.10)', 'Backlog'],
    versions: ['2.5.0 (stabilizacja)', '2.5.1 (poprawki)', '2.6.0 (planowana)', 'bez wersji'],
    units: ['Dział Realizacji', 'Zespół Produktu NextApp', 'Zespół Wdrożeń', 'Zespół Utrzymania', 'Dział Bezpieczeństwa'],
    horizons: ['Teraz', 'Następnie', 'Później'],
    tasks: ['NA-231 Import OPC zawiesza się…', 'NA-224 Zegar SLA nie pauzuje…', 'NA-226 Nowy widok struktury przełożonych', 'ECU-12 Alarmy EMI nie odświeżają licznika'],
    duplicates: ['NA-198 Podwójne wysłanie formularza ustawień', 'NA-230 Brak odświeżenia wykresu w Dashboard EMI'],
    streams: ['Infrastruktura', 'Migracja danych', 'Szkolenia i odbiór']
  };

  function it(label, icon, act, extra) { var o = { label: label, icon: icon, act: act }; for (var k in extra || {}) o[k] = extra[k]; return o; }
  var SEP = { sep: true };

  // act: assign | handover | edit | move:<targets> | archive | delete | link:<url> | toast:<text>
  var MENUS = {
    task: [it('Otwórz kartę', 'i-external', 'link:a03-zadanie.html'), it('Edytuj…', 'i-edit', 'edit'), it('Przypisz do…', 'i-user-plus', 'assign'),
      it('Przekaż z komentarzem…', 'i-send', 'handover'), SEP, it('Przenieś do projektu…', 'i-sitemap', 'move:projects'), it('Do sprintu…', 'i-layers', 'move:sprints'),
      it('Ustaw wersję…', 'i-package', 'move:versions'), it('Dodaj powiązanie…', 'i-link', 'edit:link'), it('Kopiuj link', 'i-copy', 'toast:Skopiowano link do zadania'), SEP,
      it('Archiwizuj', 'i-history', 'archive'), it('Usuń', 'i-trash', 'delete', { danger: true })],
    'verify-item': [it('Przypisz po potwierdzeniu…', 'i-user-plus', 'assign'), it('Przekaż innej osobie do oceny…', 'i-send', 'handover'),
      it('Oznacz jako duplikat…', 'i-copy', 'move:duplicates'), SEP, it('Odrzuć z powodem…', 'i-x-circle', 'delete', { danger: true })],
    'today-item': [it('Otwórz', 'i-external', 'link:a03-zadanie.html'), it('Odłóż na jutro', 'i-clock', 'toast:Odłożono na jutro'), it('Przekaż…', 'i-send', 'handover'),
      SEP, it('Usuń z „Na dziś”', 'i-x', 'toast:Usunięto z listy „Na dziś” — zadanie zostaje')],
    comment: [it('Edytuj komentarz', 'i-edit', 'edit'), SEP, it('Usuń komentarz', 'i-trash', 'delete', { danger: true })],
    attachment: [it('Pobierz', 'i-download', 'toast:Pobieranie rozpoczęte'), it('Zmień nazwę…', 'i-edit', 'edit'), SEP, it('Usuń załącznik', 'i-trash', 'delete', { danger: true })],
    link: [it('Otwórz', 'i-external', 'link:a03-zadanie.html'), it('Zmień typ powiązania…', 'i-edit', 'edit:link'), SEP, it('Usuń powiązanie', 'i-trash', 'delete', { danger: true })],
    project: [it('Otwórz', 'i-external', 'link:a02-tablica.html'), it('Ustawienia', 'i-settings', 'link:d03-funkcje-projektu.html'), it('Członkowie', 'i-users', 'link:d02-czlonkowie.html'),
      it('Nowy podprojekt…', 'i-plus', 'link:d10-nowy-podprojekt.html'), it('Przekaż własność…', 'i-send', 'handover'), it('Eksportuj', 'i-download', 'toast:Eksport projektu przygotowywany'), SEP,
      it('Zakończ projekt…', 'i-flag', 'link:d11-zakoncz-podprojekt.html'), it('Archiwizuj', 'i-history', 'archive'),
      it('Usuń', 'i-trash', 'delete', { danger: true, disabled: 'Usunąć można tylko zakończony projekt bez podprojektów' })],
    subproject: [it('Otwórz', 'i-external', 'link:a07-tablica-utrzymanie.html'), it('Ustawienia podprojektu', 'i-settings', 'link:d09-podprojekty.html'),
      it('Zmień PM podprojektu…', 'i-user-plus', 'assign'), it('Przenieś pod inny projekt…', 'i-move', 'move:projects'), it('Eksportuj (z podprojektami)', 'i-download', 'toast:Eksport podprojektu przygotowywany'), SEP,
      it('Zakończ podprojekt…', 'i-flag', 'link:d11-zakoncz-podprojekt.html'), it('Usuń', 'i-trash', 'delete', { danger: true, disabled: 'Najpierw zakończ podprojekt' })],
    'subproject-ended': [it('Otwórz (tylko odczyt)', 'i-eye', 'toast:Archiwum otwarte tylko do odczytu'), it('Wznów', 'i-refresh', 'toast:Podprojekt wznowiony — znów aktywny'),
      it('Eksportuj dla klienta', 'i-download', 'toast:Eksport dla klienta przygotowywany'), SEP, it('Usuń na stałe…', 'i-trash', 'delete', { danger: true })],
    sprint: [it('Edytuj sprint…', 'i-edit', 'edit'), it('Zamknij sprint…', 'i-flag', 'edit:close-sprint'), SEP, it('Usuń sprint', 'i-trash', 'delete', { danger: true })],
    version: [it('Edytuj wersję…', 'i-edit', 'edit'), it('Zmień odpowiedzialnego za wydanie…', 'i-user-plus', 'assign'), it('Przekaż wydanie…', 'i-send', 'handover'), SEP,
      it('Archiwizuj wersję', 'i-history', 'archive')],
    'version-row': [it('Otwórz zadanie', 'i-external', 'link:a03-zadanie.html'), it('Przesuń do wersji…', 'i-package', 'move:versions'), it('Przypisz…', 'i-user-plus', 'assign'), SEP,
      it('Usuń z wersji', 'i-x', 'toast:Zadanie usunięte z wersji 2.5.0')],
    'gate-item': [it('Przypisz osobę…', 'i-user-plus', 'assign'), it('Oznacz jako spełnione', 'i-check', 'toast:Pozycja checklisty spełniona')],
    'changelog-line': [it('Edytuj zdanie', 'i-edit', 'edit'), it('Pokaż źródła', 'i-link', 'toast:Źródła: NA-224, MR !482'), SEP, it('Pomiń w changelogu', 'i-x', 'toast:Zdanie pominięte — zadanie zostaje w wersji')],
    'time-row': [it('Edytuj wpisy…', 'i-edit', 'edit'), it('Przenieś na inne zadanie…', 'i-move', 'move:tasks'), SEP, it('Usuń wpisy', 'i-trash', 'delete', { danger: true })],
    'daily-item': [it('Edytuj notatkę', 'i-edit', 'edit'), it('Zgłoś blokadę…', 'i-lock', 'edit:block'), it('Przekaż…', 'i-send', 'handover'), SEP, it('Usuń', 'i-trash', 'delete', { danger: true })],
    'roadmap-item': [it('Edytuj pozycję…', 'i-edit', 'edit'), it('Zmień właściciela…', 'i-user-plus', 'assign'), it('Przekaż właścicielstwo…', 'i-send', 'handover'),
      it('Dodaj zależność…', 'i-link', 'edit:dependency'), it('Przenieś do strumienia…', 'i-move', 'move:streams'), it('Zaplanuj przesunięcie…', 'i-calendar', 'link:c05-przesuniecie.html'), SEP,
      it('Wstrzymaj / porzuć…', 'i-pause', 'edit:status'), it('Archiwizuj', 'i-history', 'archive')],
    milestone: [it('Edytuj kamień…', 'i-edit', 'edit'), it('Zmień właściciela…', 'i-user-plus', 'assign'), SEP, it('Usuń kamień', 'i-trash', 'delete', { danger: true })],
    stream: [it('Zmień nazwę…', 'i-edit', 'edit'), it('Dodaj pozycję w strumieniu', 'i-plus', 'edit:new-item'), it('Zmień właściciela…', 'i-user-plus', 'assign'), SEP,
      it('Usuń strumień', 'i-trash', 'delete', { danger: true })],
    dependency: [it('Edytuj zależność…', 'i-edit', 'edit:dependency'), SEP, it('Usuń zależność', 'i-trash', 'delete', { danger: true })],
    'nnl-card': [it('Edytuj…', 'i-edit', 'edit'), it('Zmień właściciela…', 'i-user-plus', 'assign'), it('Przenieś do…', 'i-move', 'move:horizons'), SEP,
      it('Usuń', 'i-trash', 'delete', { danger: true })],
    allocation: [it('Zmień % i okres…', 'i-edit', 'edit'), it('Przepnij na inną osobę…', 'i-user-plus', 'assign'), it('Przekaż z notatką…', 'i-send', 'handover'), SEP,
      it('Usuń przydział', 'i-trash', 'delete', { danger: true })],
    demand: [it('Obsadź…', 'i-user-plus', 'assign'), it('Edytuj zapotrzebowanie…', 'i-edit', 'edit'), SEP, it('Usuń zapotrzebowanie', 'i-trash', 'delete', { danger: true })],
    'person-load': [it('Przenieś alokacje osoby…', 'i-move', 'link:f07-do-przekazania.html'), it('Nieobecności i zastępstwa', 'i-calendar', 'link:g01-profil.html'),
      it('Zaproponuj wyrównanie', 'i-wand', 'toast:Propozycja wyrównania gotowa do przeglądu')],
    'knowledge-file': [it('Otwórz', 'i-external', 'toast:Podgląd pliku'), it('Zmień nazwę…', 'i-edit', 'edit'), it('Zastąp nową wersją…', 'i-upload', 'toast:Wybierz plik nowej wersji'), SEP,
      it('Usuń ze źródeł', 'i-trash', 'delete', { danger: true })],
    question: [it('Edytuj pytanie…', 'i-edit', 'edit'), it('Przypisz osobę…', 'i-user-plus', 'assign'), it('Oznacz jako odpowiedziane…', 'i-check', 'edit:answer'), SEP,
      it('Usuń pytanie', 'i-trash', 'delete', { danger: true })],
    requirement: [it('Popraw…', 'i-edit', 'edit'), it('Pokaż cytat w dokumencie', 'i-eye', 'toast:Otworzono dokument w miejscu cytatu'), SEP, it('Odrzuć', 'i-x-circle', 'delete', { danger: true })],
    member: [it('Edytuj funkcje…', 'i-edit', 'edit'), it('Przekaż administrację projektu…', 'i-send', 'handover'), it('Zastępstwa osoby', 'i-calendar', 'link:g01-profil.html'), SEP,
      it('Usuń z projektu i przekaż pracę…', 'i-user-x', 'link:f07-do-przekazania.html', { danger: true })],
    'agent-member': [it('Edytuj konfigurację…', 'i-edit', 'edit'), it('Wstrzymaj agenta', 'i-pause', 'toast:Agent wstrzymany — jego kroki czekają na człowieka'),
      it('Przekaż jego zadania człowiekowi…', 'i-send', 'handover'), SEP, it('Usuń z projektu', 'i-trash', 'delete', { danger: true })],
    'temp-account': [it('Przedłuż dostęp…', 'i-clock', 'edit:extend'), it('Dodaj funkcję…', 'i-plus', 'edit'), SEP, it('Odbierz dostęp teraz', 'i-user-x', 'delete', { danger: true })],
    module: [it('Edytuj moduł…', 'i-edit', 'edit'), it('Zmień głównego developera…', 'i-user-plus', 'assign'), it('Zmień głównego testera…', 'i-user-plus', 'assign'),
      it('Zastępcy…', 'i-users', 'assign'), it('Dodaj ścieżkę repo…', 'i-folder', 'edit:path'), SEP, it('Archiwizuj moduł', 'i-history', 'archive')],
    'sla-policy': [it('Edytuj politykę…', 'i-edit', 'edit'), it('Ustaw jako domyślną', 'i-check', 'toast:Polityka ustawiona jako domyślna dla błędów'), SEP,
      it('Archiwizuj', 'i-history', 'archive', { disabled: 'Używana przez 38 otwartych zadań — najpierw przepnij je na inną politykę' })],
    'day-off': [it('Edytuj…', 'i-edit', 'edit'), SEP, it('Usuń dzień wolny', 'i-trash', 'delete', { danger: true })],
    env: [it('Przedłuż o 3 dni', 'i-clock', 'toast:Środowisko przedłużone do 05.10'), it('Przekaż innej osobie…', 'i-send', 'handover'), SEP,
      it('Zatrzymaj i usuń', 'i-trash', 'delete', { danger: true })],
    'env-template': [it('Edytuj szablon…', 'i-edit', 'edit'), it('Duplikuj', 'i-copy', 'toast:Utworzono kopię szablonu'), SEP, it('Usuń szablon', 'i-trash', 'delete', { danger: true })],
    repo: [it('Tryb integracji…', 'i-settings', 'edit'), it('Sprawdź webhook', 'i-refresh', 'toast:Webhook odpowiada — 200 OK'), SEP, it('Odłącz repozytorium', 'i-trash', 'delete', { danger: true })],
    'git-account': [it('Powiąż ręcznie z osobą…', 'i-user-plus', 'assign'), SEP, it('Odłącz konto', 'i-trash', 'delete', { danger: true })],
    'wf-step': [it('Zmień, kogo przydzielić…', 'i-user-plus', 'assign'), it('Zastępstwo w kroku…', 'i-users', 'edit:step-deputy'), SEP, it('Usuń krok', 'i-trash', 'delete', { danger: true })],
    'task-type': [it('Edytuj typ…', 'i-edit', 'edit'), it('Zmień workflow typu…', 'i-branch', 'edit'), SEP,
      it('Archiwizuj typ', 'i-history', 'archive', { disabled: 'Typ ma otwarte zadania — najpierw zmień ich typ' })],
    finding: [it('Przypisz…', 'i-user-plus', 'assign'), it('Przekaż do triage…', 'i-send', 'handover'), it('Utwórz zadanie bezpieczeństwa', 'i-plus', 'toast:Utworzono zadanie NA-240 (poufne)'),
      it('Oznacz jako fałszywy alarm…', 'i-x-circle', 'edit:false-positive'), it('Wniosek o wyjątek…', 'i-scale', 'link:e08-wyjatki-obowiazki.html')],
    psirt: [it('Zmień prowadzącego…', 'i-user-plus', 'assign'), it('Przekaż sprawę…', 'i-send', 'handover'), it('Edytuj sprawę…', 'i-edit', 'edit'), SEP,
      it('Zamknij sprawę…', 'i-flag', 'archive')],
    contact: [it('Edytuj kontakt…', 'i-edit', 'edit'), SEP, it('Usuń z listy', 'i-trash', 'delete', { danger: true })],
    pentest: [it('Edytuj zlecenie…', 'i-edit', 'edit'), it('Zmień nadzorującego…', 'i-user-plus', 'assign'), SEP, it('Anuluj zlecenie', 'i-x-circle', 'archive')],
    'pentest-finding': [it('Przypisz developera…', 'i-user-plus', 'assign'), it('Zaplanuj retest…', 'i-calendar', 'edit:retest'), it('Oznacz jako naprawione', 'i-check', 'toast:Oznaczono — czeka na retest'), SEP,
      it('Odrzuć z powodem…', 'i-x-circle', 'delete', { danger: true })],
    tester: [it('Przedłuż dostęp…', 'i-clock', 'edit:extend'), SEP, it('Odbierz dostęp teraz', 'i-user-x', 'delete', { danger: true })],
    'sbom-comp': [it('Edytuj wpis…', 'i-edit', 'edit'), SEP, it('Usuń z manifestu', 'i-trash', 'delete', { danger: true })],
    'supported-version': [it('Zmień koniec wsparcia…', 'i-calendar', 'edit'), SEP, it('Zakończ wsparcie…', 'i-flag', 'archive')],
    exception: [it('Edytuj…', 'i-edit', 'edit'), it('Przedłuż…', 'i-clock', 'edit:extend'), it('Zmień właściciela…', 'i-user-plus', 'assign'),
      it('Zmień zatwierdzającego…', 'i-user-plus', 'assign'), SEP, it('Zamknij wcześniej', 'i-flag', 'archive')],
    obligation: [it('Edytuj regułę…', 'i-edit', 'edit'), it('Zmień właściciela…', 'i-user-plus', 'assign'), it('Przekaż…', 'i-send', 'handover'), SEP,
      it('Usuń regułę', 'i-trash', 'delete', { danger: true })],
    'org-person': [it('Edytuj przypisanie…', 'i-edit', 'edit'), it('Przenieś do jednostki…', 'i-move', 'move:units'), it('Zastępstwo…', 'i-users', 'edit:deputy'), SEP,
      it('Zakończ przypisanie i przekaż…', 'i-user-x', 'link:f07-do-przekazania.html', { danger: true })],
    unit: [it('Edytuj jednostkę…', 'i-edit', 'edit'), it('Zmień kierownika…', 'i-user-plus', 'assign'), it('Przenieś pod…', 'i-move', 'move:units'), SEP,
      it('Zlikwiduj od daty…', 'i-history', 'archive')],
    'privacy-request': [it('Zmień prowadzącego…', 'i-user-plus', 'assign'), it('Edytuj…', 'i-edit', 'edit'), SEP, it('Odrzuć z uzasadnieniem…', 'i-x-circle', 'delete', { danger: true })],
    'legal-hold': [it('Edytuj zakres…', 'i-edit', 'edit'), it('Zmień właściciela…', 'i-user-plus', 'assign'), SEP, it('Zdejmij blokadę', 'i-x', 'archive')],
    absence: [it('Edytuj…', 'i-edit', 'edit'), it('Co przejmą zastępcy', 'i-users', 'link:f07-do-przekazania.html'), SEP, it('Usuń nieobecność', 'i-trash', 'delete', { danger: true })],
    deputy: [it('Edytuj zastępstwo…', 'i-edit', 'edit'), it('Zmień zastępcę…', 'i-user-plus', 'assign'), it('Zakończ wcześniej', 'i-flag', 'toast:Zastępstwo zakończone dziś'), SEP,
      it('Usuń', 'i-trash', 'delete', { danger: true })]
  };

  // Edit windows: fields per type (kind: text | area | select:<a|b> | date | person | number)
  var F = function (label, kind, value) { return { label: label, kind: kind || 'text', value: value || '' }; };
  var FIELDS = {
    task: [F('Tytuł', 'text', '@subject'), F('Opis', 'area', 'Kroki odtworzenia, oczekiwany i faktyczny wynik…'), F('Typ', 'select:Błąd|Funkcjonalność|Techniczne|Bezpieczeństwo|Epik|Podzadanie'),
      F('Waga', 'select:krytyczny|istotny|mało istotny'), F('Uzasadnienie zmiany wagi', 'text', 'wymagane przy zmianie wagi'), F('Termin', 'date', '2026-10-02'), F('Moduł', 'select:Import OPC|Dashboard EMI|Integracja SDP|Procedury zmiany|OMS / Widok szafy|Uprawnienia i hasła')],
    link: [F('Typ powiązania', 'select:blokuje|jest blokowane przez|powiązane z|duplikat|koniec → start (zależność)'), F('Zadanie', 'text', 'NA-220 Aktualizacja jQuery…'), F('Opóźnienie (dni rob.)', 'number', '0')],
    sprint: [F('Nazwa', 'text', 'Sprint 43'), F('Cel sprintu', 'area', 'Stabilizacja 2.5.0 i nakładka „Góra” szafy'), F('Od', 'date', '2026-10-07'), F('Do', 'date', '2026-10-20')],
    'close-sprint': [F('Niedokończone zadania (4) przenieś do', 'select:Sprint 43|Backlog'), F('Notatka do przeglądu sprintu', 'area', '')],
    version: [F('Numer', 'text', '2.5.0'), F('Planowana data wydania', 'date', '2026-10-08'), F('Stan', 'select:planowana|w realizacji|stabilizacja|wydana|archiwalna'), F('Opis', 'area', '')],
    'roadmap-item': [F('Nazwa', 'text', '@subject'), F('Strumień', 'select:Infrastruktura|Migracja danych|Szkolenia i odbiór'), F('Od', 'date', '2026-10-12'), F('Do', 'date', '2026-11-20'),
      F('Postęp ręczny (%)', 'number', ''), F('Opis', 'area', '')],
    dependency: [F('Poprzednik', 'text', 'Inwentaryzacja i mapowanie'), F('Typ', 'select:koniec → start|start → start|koniec → koniec|start → koniec'), F('Opóźnienie (dni rob.)', 'number', '0')],
    status: [F('Stan pozycji', 'select:w planie|w realizacji|wstrzymana|porzucona'), F('Powód', 'area', 'wymagany przy wstrzymaniu i porzuceniu')],
    'new-item': [F('Nazwa pozycji', 'text', ''), F('Od', 'date', ''), F('Do', 'date', ''), F('Właściciel', 'person', '')],
    milestone: [F('Nazwa', 'text', '@subject'), F('Data planowana', 'date', '2027-01-12'), F('Kryterium osiągnięcia', 'area', 'Protokół odbioru podpisany przez klienta')],
    stream: [F('Nazwa strumienia', 'text', '@subject'), F('Kolor', 'select:indygo|turkus|pomarańcz|róż')],
    'nnl-card': [F('Nazwa', 'text', '@subject'), F('Opis dla odbiorcy', 'area', ''), F('Horyzont', 'select:Teraz|Następnie|Później')],
    allocation: [F('Wymiar', 'select:25%|50%|60%|80%|100%'), F('Od', 'date', '2026-10-12'), F('Do', 'date', '2026-11-20'), F('Uwagi', 'area', '')],
    demand: [F('Funkcja', 'select:Tester|Programista|Wdrożeniowiec|Projektant UI'), F('Wymiar', 'select:25%|50%|100%'), F('Od', 'date', '2026-10-19'), F('Do', 'date', '2026-11-27')],
    'knowledge-file': [F('Nazwa', 'text', '@subject')],
    question: [F('Treść pytania', 'area', '@subject'), F('Do kogo', 'text', 'zamawiający — dział IT')],
    answer: [F('Odpowiedź zamawiającego', 'area', ''), F('Data odpowiedzi', 'date', '2026-09-29'), F('Źródło', 'text', 'e-mail / protokół spotkania')],
    requirement: [F('Treść wymagania', 'area', '@subject'), F('Siła', 'select:MUSI|POWINNO|MOŻE')],
    comment: [F('Treść', 'area', '@subject'), F('Uwaga', 'text', 'zmiana zostaje w historii komentarza')],
    attachment: [F('Nazwa pliku', 'text', '@subject')],
    member: [F('Funkcje', 'select:Programista|Tester|PM|Release manager|Projektant UI|Security'), F('Administrator projektu', 'select:nie|tak'), F('Dostęp do', 'date', '')],
    'agent-member': [F('Model', 'select:qwen3-coder (lokalny)|model z bramki AI'), F('Limit rund bez człowieka', 'number', '3'), F('Wyłącznik po', 'select:3 nieudanych rundach|2 h pracy|nigdy')],
    extend: [F('Nowa data końca', 'date', '2026-10-15'), F('Uzasadnienie', 'area', '')],
    module: [F('Nazwa', 'text', '@subject'), F('Opis dla użytkownika', 'area', '')],
    path: [F('Ścieżka w repozytorium', 'text', 'NextApp/Modules/…')],
    'sla-policy': [F('Nazwa', 'text', '@subject'), F('Podstawa czasu', 'select:robocza|kalendarzowa'), F('Krytyczny (h)', 'number', '24'), F('Istotny (h)', 'number', '48'), F('Mało istotny (h)', 'number', '72')],
    'day-off': [F('Data', 'date', '2026-12-24'), F('Nazwa', 'text', '@subject')],
    env: [F('Wygasa', 'date', '2026-10-05')], 'env-template': [F('Nazwa', 'text', '@subject'), F('Sterownik', 'select:Docker|Podman|Jenkins|stały adres')],
    repo: [F('Tryb', 'select:A — webhook|B — odpytywanie|C — CI wysyła wyniki'), F('Gałąź główna', 'text', 'master')],
    'step-deputy': [F('Gdy wykonawca jest nieobecny', 'select:zastępstwo z profilu|zastępca modułu|PM projektu|czekaj')],
    'task-type': [F('Nazwa', 'text', '@subject'), F('Workflow', 'select:Wytwarzanie z UI i security|Utrzymanie klienta|Prosty'), F('Polityka SLA', 'select:SLA robocze|SLA kalendarzowe|bez SLA')],
    'false-positive': [F('Uzasadnienie', 'area', 'np. kod nieosiągalny — biblioteka tylko w testach'), F('Dowód', 'text', 'link do analizy / przebiegu')],
    psirt: [F('Tytuł sprawy', 'text', '@subject'), F('Wersje dotknięte', 'text', '2.4.x, 2.5.0'), F('Aktywnie wykorzystywana', 'select:nie|tak — zegar CRA 24 h')],
    contact: [F('Klient', 'text', '@subject'), F('Osoba kontaktowa', 'text', ''), F('E-mail', 'text', '')],
    pentest: [F('Zakres', 'area', 'NextApp 2.5.0-rc2 · środowisko test-2.5.0-rc2'), F('Od', 'date', '2026-10-05'), F('Do', 'date', '2026-10-16')],
    retest: [F('Data retestu', 'date', '2026-10-20'), F('Tester', 'person', '')],
    'sbom-comp': [F('Nazwa', 'text', '@subject'), F('Wersja', 'text', ''), F('Licencja', 'text', ''), F('Źródło', 'text', '')],
    'supported-version': [F('Koniec wsparcia', 'date', '2031-10-08')],
    exception: [F('Uzasadnienie', 'area', ''), F('Środki kompensujące', 'area', ''), F('Wygasa', 'date', '2026-12-31')],
    obligation: [F('Nazwa', 'text', '@subject'), F('Cykl', 'select:co miesiąc|co kwartał|co rok'), F('Przypomnienie przed terminem', 'select:7 dni|14 dni|30 dni'), F('Eskalacja', 'select:zastępca → kierownik|kierownik|PM projektu')],
    'org-person': [F('Stanowisko', 'text', ''), F('Część etatu', 'select:1,0|0,75|0,5|0,25'), F('Typ', 'select:stałe|p.o.|kontraktor'), F('Stanowisko główne', 'select:tak|nie'), F('Od', 'date', '2026-10-01'), F('Do', 'date', '')],
    deputy: [F('Zakres', 'select:wszystko|zatwierdzenia|eskalacje'), F('Od', 'date', '2026-10-20'), F('Do', 'date', '2026-10-24')],
    unit: [F('Nazwa', 'text', '@subject'), F('Typ', 'select:dział|zespół|sekcja'), F('Kod', 'text', '')],
    'privacy-request': [F('Rodzaj', 'select:dostęp|usunięcie|sprostowanie|ograniczenie'), F('Termin odpowiedzi', 'date', '2026-10-29')],
    'legal-hold': [F('Zakres', 'area', '@subject'), F('Do', 'date', '')],
    absence: [F('Od', 'date', '2026-10-20'), F('Do', 'date', '2026-10-24'), F('Rodzaj', 'select:urlop|L4|szkolenie|inne')],
    'time-row': [F('Pon', 'number', '2'), F('Wt', 'number', '3'), F('Śr', 'number', '1,5'), F('Czw', 'number', ''), F('Pt', 'number', '')],
    'daily-item': [F('Notatka', 'area', '@subject')], block: [F('Powód blokady', 'area', ''), F('Czeka na', 'text', 'NA-220 / osoba / klient')],
    'changelog-line': [F('Zdanie dla użytkownika', 'area', '@subject')],
    'new-case': [F('Tytuł sprawy', 'text', ''), F('Źródło zgłoszenia', 'select:zgłoszenie badacza|klient|skaner|pentest|własne wykrycie'), F('Wersje dotknięte', 'text', ''),
      F('Aktywnie wykorzystywana', 'select:nie wiadomo|nie|tak — zegar CRA 24 h'), F('Prowadzi', 'person', '')],
    'cra-field': [F('Treść pola', 'area', '@subject'), F('Uwaga', 'text', 'zmiana zostaje w historii szkicu; wysyłka nadal ręczna przez portal ENISA')],
    'new-pentest': [F('Nazwa zlecenia', 'text', 'PT-2026-03'), F('Zakres', 'area', ''), F('Od', 'date', ''), F('Do', 'date', ''), F('Nadzorujący', 'person', '')],
    'new-obligation': [F('Nazwa', 'text', ''), F('Cykl', 'select:co miesiąc|co kwartał|co rok'), F('Pierwszy termin', 'date', ''), F('Właściciel', 'person', ''),
      F('Eskalacja', 'select:zastępca → kierownik|kierownik|PM projektu')],
    'new-deputy': [F('Zastępca', 'person', ''), F('Kolejność', 'select:1|2|3'), F('Zakres', 'select:wszystko|zatwierdzenia|eskalacje')],
    'new-position': [F('Nazwa stanowiska', 'text', ''), F('Jednostka', 'select:Dział Realizacji|Zespół Produktu NextApp|Zespół Wdrożeń|Zespół Utrzymania|Dział Bezpieczeństwa|Zespół Jakości'),
      F('Raportuje do', 'text', ''), F('Osoba', 'person', ''), F('Część etatu', 'select:1,0|0,75|0,5|0,25'), F('Od', 'date', '2026-10-01')],
    'function-catalog': [F('Nazwa funkcji', 'text', ''), F('Skrót na karcie', 'text', ''), F('Domyślny poziom w macierzy', 'select:czyta|edytuje|zarządza'),
      F('Istniejące funkcje', 'area', 'PM · Programista · Tester · Release manager · Projektant UI · Security · Wdrożeniowiec — zmiana nazwy nie zmienia uprawnień')],
    'version-row': [], 'gate-item': []
  };

  var ARCHIVE_TEXT = {
    task: 'Zadanie znika z tablicy i list, zostaje w historii, raportach i wyszukiwarce. Możesz je przywrócić.',
    project: 'Projekt przechodzi do zarchiwizowanych: tylko do odczytu, poza licznikami. Możesz go przywrócić.',
    version: 'Wersja znika z list wyboru; wydane zadania i changelog zostają.',
    module: 'Moduł znika z list wyboru; zadania zachowują przypisanie do niego w historii.',
    exception: 'Wyjątek wygasa dziś. Znalezisko wraca do pulpitu jako otwarte.',
    unit: 'Jednostka zostanie zlikwidowana od wybranej daty. Osoby bez innej jednostki trafią na listę „Do przekazania”.',
    reorganization: 'Reorganizacja zostaje wycofana — zaplanowane zmiany nie wejdą w życie. Historia i wersja robocza zostają.',
    'legal-hold': 'Blokada zostaje zdjęta. Dane wracają pod zwykłe okresy retencji — najbliższe usunięcie według polityki.',
    psirt: 'Sprawa zostaje zamknięta. Zegar CRA musi być zakończony raportem końcowym; sprawa zostaje poufna w archiwum.',
    pentest: 'Zlecenie zostaje anulowane. Konta testerów wygasają od razu, zebrane znaleziska zostają.',
    'privacy-request': 'Żądanie zostaje zamknięte.',
    default: 'Pozycja przechodzi do archiwum i znika z bieżących widoków. Historia zostaje.'
  };
  var DELETE_TEXT = {
    task: 'Usunięcie jest dostępne tylko dla zadań bez historii pracy (bez commitów, czasu i przejść). To zadanie ma historię — użyj „Archiwizuj”.',
    'verify-item': 'Zgłoszenie automatu zostanie odrzucone z podanym powodem. Agent nie zgłosi ponownie tego samego wystąpienia.',
    'temp-account': 'Dostęp wygaśnie natychmiast. Wpisy i znaleziska osoby zostają.',
    tester: 'Dostęp testera wygaśnie natychmiast. Jego znaleziska zostają w zleceniu.',
    'privacy-request': 'Żądanie zostaje odrzucone z uzasadnieniem. Osoba dostaje odpowiedź, wpis trafia do rejestru żądań.',
    deputy: 'Zastępca zostaje zdjęty. Od teraz eskalacje idą do kolejnego zastępcy albo kierownika.',
    'subproject-ended': 'Usunięcie na stałe kasuje bazę podprojektu. Pozostaje tylko wpis w audycie. Tej operacji nie da się cofnąć.',
    default: 'Pozycja zostanie usunięta. Tuż po usunięciu możesz jeszcze kliknąć „Cofnij”.'
  };

  // ---------- helpers ----------
  function esc(s) { return String(s).replace(/[&<>"]/g, function (c) { return { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]; }); }
  function icon(id) { return '<svg class="icon"><use href="#' + id + '"/></svg>'; }
  // Joins child texts with spaces so "<span>NA-233</span>Błąd…" reads "NA-233 Błąd…".
  function spaced(node) {
    return Array.prototype.map.call(node.childNodes, function (n) { return n.nodeType === 1 ? spaced(n) : n.textContent; }).join(' ').replace(/\s+/g, ' ').trim();
  }
  function typeOf(el) {
    if (el.dataset.menu) return el.dataset.menu;
    var host = el.closest('[data-menu]');
    return host ? host.dataset.menu : (document.body.dataset.menu || 'task');
  }
  function subjectOf(el) {
    if (el.dataset.subject) return el.dataset.subject;
    var host = el.closest('[data-subject]');
    if (host) return host.dataset.subject;
    var row = el.closest('tr, .kanban-card, .tree-row, .mod-card, .proj-card, .tcard, .bl-row, .src, .q-item, li, .cf-card, .card, .section-card, [data-row]');
    if (!row) return document.title.split('·').pop().trim();
    var t = row.querySelector('.kk-title, .t-title .tt, .tt, .pc-name, .mc-n, .tn b, h3, .t-title, .name, b, strong');
    var txt = spaced(t || row);
    return txt.length > 70 ? txt.slice(0, 68) + '…' : txt;
  }
  function toast(text) {
    var stack = document.querySelector('.ak-toasts');
    if (!stack) { stack = document.createElement('div'); stack.className = 'ak-toasts'; document.body.appendChild(stack); }
    var t = document.createElement('div'); t.className = 'toast';
    t.innerHTML = icon('i-check-circle') + '<span>' + esc(text) + '</span><span class="undo">Cofnij</span>';
    stack.appendChild(t); setTimeout(function () { t.remove(); }, 3600);
  }

  // ---------- menu ----------
  var openMenu = null;
  function closeMenu() { if (openMenu) { openMenu.remove(); openMenu = null; } }
  function showMenu(btn) {
    closeMenu();
    var type = typeOf(btn), items = MENUS[type] || MENUS.task, subject = subjectOf(btn);
    var m = document.createElement('div'); m.className = 'ak-menu'; m.setAttribute('role', 'menu');
    m.innerHTML = '<div class="ak-menu-sub">' + esc(subject) + '</div>' + items.map(function (x, i) {
      if (x.sep) return '<div class="mn-sep"></div>';
      var dis = x.disabled ? ' aria-disabled="true" title="' + esc(x.disabled) + '"' : '';
      return '<button type="button" role="menuitem" class="ak-mi' + (x.danger ? ' danger' : '') + (x.disabled ? ' dis' : '') + '" data-i="' + i + '"' + dis + '>' +
        icon(x.icon) + '<span>' + esc(x.label) + (x.disabled ? '<small>' + esc(x.disabled) + '</small>' : '') + '</span></button>';
    }).join('');
    document.body.appendChild(m);
    var r = btn.getBoundingClientRect(), mw = 260;
    var left = Math.min(window.scrollX + r.right - mw, window.scrollX + document.documentElement.clientWidth - mw - 12);
    m.style.left = Math.max(8, left) + 'px'; m.style.top = (window.scrollY + r.bottom + 6) + 'px';
    openMenu = m;
    m.addEventListener('click', function (e) {
      e.stopPropagation();
      var b = e.target.closest('.ak-mi'); if (!b || b.classList.contains('dis')) return;
      var x = items[+b.dataset.i]; closeMenu(); run(x.act, type, subject, x.label, btn);
    });
    var first = m.querySelector('.ak-mi:not(.dis)'); if (first) first.focus();
  }

  // ---------- windows ----------
  function openWin(title, iconId, body, primary, onOk, wide) {
    var bd = document.createElement('div'); bd.className = 'ak-backdrop';
    bd.innerHTML = '<div class="window ak-win' + (wide ? ' wide' : '') + '" role="dialog" aria-modal="true" aria-label="' + esc(title) + '">' +
      '<div class="window-head"><div class="window-title">' + icon(iconId) + esc(title) + '</div><button class="window-close" title="Zamknij (Esc)">' + icon('i-x') + '</button></div>' +
      '<div class="window-body">' + body + '</div>' +
      '<div class="window-foot"><button class="btn ak-cancel">Anuluj</button><button class="btn ' + (primary.danger ? 'btn-danger' : 'btn-primary') + ' ak-ok">' + esc(primary.label) + '</button></div></div>';
    document.body.appendChild(bd);
    function close() { bd.remove(); document.removeEventListener('keydown', key); }
    function key(e) { if (e.key === 'Escape') close(); }
    document.addEventListener('keydown', key);
    bd.addEventListener('click', function (e) { if (e.target === bd) close(); });
    bd.querySelector('.window-close').onclick = close; bd.querySelector('.ak-cancel').onclick = close;
    bd.querySelector('.ak-ok').onclick = function () { if (onOk(bd) !== false) close(); };
    var f = bd.querySelector('input, textarea, select'); if (f) f.focus();
    return bd;
  }
  function personRow(p, i, agent) {
    var load = agent ? '' : '<span class="ak-load' + (p.load > 100 ? ' over' : p.load >= 90 ? ' high' : '') + '">' + p.load + '%</span>';
    var av = agent ? '<span class="av av-sm ak-agent">' + icon('i-bot') + '</span>' : '<span class="av av-sm ' + p.av + '">' + p.ini + '</span>';
    var tags = (p.hint ? '<span class="ak-tag ok">' + esc(p.hint) + '</span>' : '') + (p.away ? '<span class="ak-tag warn">' + esc(p.away) + '</span>' : '') +
      (p.load > 100 ? '<span class="ak-tag bad">przeciążony</span>' : '');
    return '<label class="ak-person"><input type="radio" name="ak-p" value="' + esc(p.name) + '"' + (i === 0 ? ' checked' : '') + '>' + av +
      '<span class="ak-pn"><b>' + esc(p.name) + '</b><small>' + esc(p.fn) + '</small>' + tags + '</span>' + load + '</label>';
  }
  function peoplePicker(withAgents) {
    var ordered = PEOPLE.slice().sort(function (a, b) { return (b.hint ? 1 : 0) - (a.hint ? 1 : 0); });
    return '<div class="ak-search">' + icon('i-search') + '<input placeholder="Szukaj osoby, funkcji lub agenta…" aria-label="Szukaj osoby"></div>' +
      '<div class="ak-people"><div class="ak-grp">Osoby · obciążenie w tym tygodniu</div>' + ordered.map(function (p, i) { return personRow(p, i, false); }).join('') +
      (withAgents ? '<div class="ak-grp">Agenci</div>' + AGENTS.map(function (a) { return personRow(a, 1, true); }).join('') : '') + '</div>';
  }
  function wireSearch(bd) {
    var inp = bd.querySelector('.ak-search input'); if (!inp) return;
    inp.addEventListener('input', function () {
      var q = inp.value.trim().toLowerCase();
      bd.querySelectorAll('.ak-person').forEach(function (r) { r.classList.toggle('scope-hidden', !!q && r.textContent.toLowerCase().indexOf(q) < 0); });
    });
  }
  function chosen(bd) { var r = bd.querySelector('input[name="ak-p"]:checked'); return r ? r.value : ''; }

  function assign(type, subject, label) {
    var role = /testera/.test(label) ? 'Główny tester' : /developera/.test(label) ? 'Główny developer' : /Zastępcy/.test(label) ? 'Zastępcy' : '';
    var agents = /^(task|verify-item|version-row|finding|pentest-finding|wf-step|allocation)$/.test(type) || /agent/i.test(label);
    var bd = openWin(label.replace('…', '') + ': ' + subject, 'i-user-plus',
      (role ? '<div class="ak-note">Rola: <b>' + role + '</b>. Poprzednia osoba dostanie powiadomienie o zmianie.</div>' : '') + peoplePicker(agents) +
      (/^(allocation|demand)$/.test(type) ? '<div class="ak-fields">' + fieldHtml(F('Ile przenieść', /zastępcę/.test(label) ? 'select:całość|50%|25%' : 'select:25%|50%|całość'), subject) +
        fieldHtml(F('Od', 'date', /zastępcę/.test(label) ? '2026-10-20' : '2026-10-05'), subject) + fieldHtml(F('Do', 'date', /zastępcę/.test(label) ? '2026-10-24' : '2026-11-20'), subject) +
        '</div><div class="ak-hint" style="margin-bottom:10px">Po okresie przydział wraca do poprzedniej osoby, jeśli ustawisz „Do”.</div>' : '') +
      '<label class="ak-check"><input type="checkbox" checked> Powiadom obie osoby (czat + e-mail)</label>',
      { label: 'Przypisz' }, function (b) { toast('Przypisano: ' + chosen(b)); });
    wireSearch(bd);
  }
  function handover(type, subject, label) {
    var bd = openWin(label.replace('…', '') + ': ' + subject, 'i-send',
      '<div class="ak-note">Przekazanie zmienia osobę odpowiedzialną i zostawia w historii <b>kto, komu, kiedy i dlaczego</b>. Przejmujący widzi komentarz na górze karty.</div>' +
      peoplePicker(/^(task|finding|allocation|agent-member)$/.test(type)) +
      '<div class="field"><label>Komentarz dla przejmującego <span class="ak-req">wymagany</span></label><textarea class="input" rows="3" placeholder="Co jest zrobione, co zostało, gdzie szukać (MR, gałąź, notatki)…"></textarea></div>' +
      '<div class="ak-checks"><label class="ak-check"><input type="checkbox" checked> Zostaw mnie jako obserwatora</label>' +
      (type === 'task' ? '<label class="ak-check"><input type="checkbox"> Przenieś mój licznik czasu</label>' : '') +
      '<label class="ak-check"><input type="checkbox" checked> Powiadom (czat + e-mail)</label></div>',
      { label: 'Przekaż' }, function (b) {
        var ta = b.querySelector('textarea');
        if (!ta.value.trim()) { ta.classList.add('ak-err'); ta.placeholder = 'Napisz choć jedno zdanie — bez tego przejmujący zaczyna od zera.'; ta.focus(); return false; }
        toast('Przekazano: ' + chosen(b));
      });
    wireSearch(bd);
  }
  function fieldHtml(f, subject) {
    var v = f.value === '@subject' ? subject : f.value, id = 'ak-f-' + Math.random().toString(36).slice(2, 8);
    var ctl;
    if (f.kind === 'area') ctl = '<textarea class="input" id="' + id + '" rows="3">' + esc(v) + '</textarea>';
    else if (f.kind.indexOf('select:') === 0) ctl = '<select class="select" id="' + id + '">' + f.kind.slice(7).split('|').map(function (o) { return '<option>' + esc(o) + '</option>'; }).join('') + '</select>';
    else if (f.kind === 'person') ctl = '<select class="select" id="' + id + '">' + PEOPLE.map(function (p) { return '<option>' + esc(p.name) + '</option>'; }).join('') + '</select>';
    else ctl = '<input class="input" id="' + id + '" type="' + (f.kind === 'date' ? 'date' : 'text') + '" value="' + esc(v) + '"' + (f.kind === 'number' ? ' inputmode="decimal"' : '') + '>';
    return '<div class="field"><label for="' + id + '">' + esc(f.label) + '</label>' + ctl + '</div>';
  }
  function edit(type, subject, label, variant) {
    var fields = FIELDS[variant || type] || FIELDS[type] || [F('Nazwa', 'text', '@subject'), F('Opis', 'area', '')];
    openWin(label.replace('…', '') + ': ' + subject, 'i-edit', '<div class="ak-fields">' + fields.map(function (f) { return fieldHtml(f, subject); }).join('') + '</div>' +
      '<div class="ak-hint">Zmiana trafia do historii z Twoim nazwiskiem i godziną.</div>', { label: 'Zapisz' }, function () { toast('Zapisano zmiany'); });
  }
  function move(kind, subject, label) {
    var opts = TARGETS[kind] || [];
    openWin(label.replace('…', '') + ': ' + subject, 'i-move',
      '<div class="ak-people">' + opts.map(function (o, i) { return '<label class="ak-person"><input type="radio" name="ak-p" value="' + esc(o) + '"' + (i === 0 ? ' checked' : '') + '><span class="ak-pn"><b>' + esc(o) + '</b></span></label>'; }).join('') + '</div>' +
      (kind === 'projects' ? '<div class="ak-note">Zadanie dostanie klucz docelowego projektu. Stary klucz zostaje jako alias — commity i linki dalej działają. Typy i SLA przełączą się na tamtejsze.</div>' : ''),
      { label: 'Przenieś' }, function (b) { toast('Przeniesiono do: ' + chosen(b)); });
  }
  function confirmAct(type, subject, label, del) {
    var text = (del ? DELETE_TEXT : ARCHIVE_TEXT)[type] || (del ? DELETE_TEXT : ARCHIVE_TEXT).default;
    var needReason = /powod|uzasadn/i.test(label);
    openWin(label.replace('…', '') + ': ' + subject, del ? 'i-trash' : 'i-history', '<div class="ak-note' + (del ? ' danger' : '') + '">' + esc(text) + '</div>' +
      (needReason ? '<div class="field"><label>Powód</label><textarea class="input" rows="2"></textarea></div>' : ''),
      { label: label.replace('…', ''), danger: del }, function () { toast((del ? 'Usunięto: ' : 'Zarchiwizowano: ') + subject); });
  }
  function run(act, type, subject, label, el) {
    var parts = act.split(':'), a = parts[0], arg = parts.slice(1).join(':');
    if (a === 'link') { window.location.href = arg; return; }
    if (a === 'toast') { toast(arg); return; }
    if (a === 'assign') return assign(type, subject, label);
    if (a === 'handover') return handover(type, subject, label);
    if (a === 'edit') return edit(type, subject, label, arg);
    if (a === 'move') return move(arg, subject, label);
    if (a === 'archive') return confirmAct(type, subject, label, false);
    if (a === 'delete') return confirmAct(type, subject, label, true);
  }

  // ---------- wiring ----------
  var MORE = 'button[title="Akcje"], button[title="Więcej"], button[title="Więcej opcji"], button[title="Akcje podprojektu"], .card-more, [data-menu-btn]';
  document.addEventListener('click', function (e) {
    var direct = e.target.closest('[data-act]');
    if (direct) {
      e.preventDefault(); e.stopPropagation(); closeMenu();
      run(direct.dataset.act, typeOf(direct), subjectOf(direct), direct.dataset.label || direct.textContent.trim() || 'Zmień', direct);
      return;
    }
    var btn = e.target.closest(MORE);
    if (btn && btn.querySelector('use[href="#i-more"]')) {
      e.preventDefault(); e.stopPropagation();
      if (openMenu && openMenu._for === btn) { closeMenu(); return; }
      showMenu(btn); if (openMenu) openMenu._for = btn; return;
    }
    if (openMenu && !e.target.closest('.ak-menu')) closeMenu();
  }, true);
  document.addEventListener('keydown', function (e) { if (e.key === 'Escape') closeMenu(); });

  // Bulk selection: <table data-bulk> with .ak-sel checkboxes and a .ak-bulkbar
  document.querySelectorAll('[data-bulk]').forEach(function (host) {
    var bar = host.parentNode.querySelector('.ak-bulkbar') || document.querySelector('.ak-bulkbar');
    function upd() {
      var n = host.querySelectorAll('.ak-sel:checked').length;
      if (bar) { bar.classList.toggle('on', n > 0); var c = bar.querySelector('.ak-n'); if (c) c.textContent = n; bar.dataset.subject = 'zaznaczone zadania (' + n + ')'; }
    }
    host.addEventListener('change', function (e) {
      if (e.target.classList.contains('ak-sel-all')) host.querySelectorAll('.ak-sel').forEach(function (c) { c.checked = e.target.checked; });
      upd();
    });
    upd();
  });

  window.akcje = { run: run, toast: toast, MENUS: MENUS };
})();
