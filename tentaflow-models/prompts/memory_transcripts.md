# PROMPT GENERATORA: MEMORY — PODSUMOWANIE TRANSKRYPCJI SPOTKAŃ

KRYTYCZNE: Twój JEDYNY output to surowe linie JSONL. ZERO komentarzy, ZERO markdown, ZERO tabel. Pierwszym znakiem outputu musi być `{`. Cokolwiek innego = porażka.

Generujesz dane treningowe JSONL do modelu AI, który uczy się podsumowywać transkrypcje spotkań — wyciągać ustalenia, decyzje, zadania, problemy i kto co ma zrobić.

## ZADANIE
Wygeneruj **10 linii JSONL** (4 PL, 3 EN, 3 Inne: DE/FR/ES).
Format: `{"input": "transkrypcja spotkania", "output": "strukturalne podsumowanie"}`

## FORMAT TRANSKRYPCJI (input)

Transkrypcja ma wyglądać jak PRAWDZIWA rozmowa — z:
- Błędami transkrypcji (źle rozpoznane słowa, brak interpunkcji, powtórzenia)
- Naturalnym językiem mówionym ("no", "kurde", "wiesz", "znaczy", "jakby", "eee")
- Przerywaniem sobie nawzajem
- Odchodzeniem od tematu i wracaniem
- Fragmentami niezrozumiałymi
- Timestampami i nazwami mówców

Format:
```
Transkrypcja
[data], [godzina]

[Imię Nazwisko] [timestamp]
[wypowiedź — naturalny język mówiony z błędami transkrypcji]

[Imię Nazwisko] [timestamp]
[wypowiedź]
...
```

## FORMAT OUTPUT (podsumowania)

```
TEMAT: [1 zdanie — o czym było spotkanie]
UCZESTNICY: [lista z rolami jeśli znane]
DATA: [z transkrypcji]

OBECNY_STAN:
- [co już jest zrobione / jak wygląda sytuacja]
- [kluczowe fakty o obecnym stanie]

USTALENIA:
- [konkretne ustalenia techniczne/biznesowe]
- [co zostało zdecydowane]

PROBLEMY_I_OBAWY:
- [co nie działa / co jest trudne / co wymaga przemyślenia]
- [wątpliwości zgłoszone przez uczestników]

DO_ZROBIENIA:
- [Imię]: [co ma zrobić] — [termin jeśli ustalony]
- [Imię]: [co ma zrobić]

DECYZJE:
- [podjęte decyzje — kto zdecydował co]

ODŁOŻONE:
- [tematy odłożone na później / świadomie pominięte]
```

## KLUCZOWE REGUŁY

### Transkrypcja musi być REALISTYCZNA
1. **Język mówiony** — nie literacki. Ludzie mówią chaotycznie, przerywają, wracają do tematu
2. **Błędy transkrypcji** — AI speech-to-text nie jest idealne:
   - Źle rozpoznane słowa techniczne (np. "prox moks" zamiast "Proxmox", "klik House" zamiast "ClickHouse")
   - Brakujące znaki interpunkcji
   - Połączone lub rozbite słowa
   - Wypełniacze: "no", "eee", "znaczy", "jakby", "wiesz co"
3. **Naturalne przejścia** — temat zmienia się płynnie, ludzie wracają do wcześniejszych wątków
4. **800-2000 tokenów** per transkrypcja — to są długie rozmowy
5. **2-4 uczestników** — z wyraźnymi rolami (programista, PM, tester, klient)

### Podsumowanie musi być PRECYZYJNE
1. **Wyciąga WSZYSTKIE ustalenia** — nawet drobne
2. **Przypisuje zadania do KONKRETNYCH osób** z terminami jeśli padły
3. **Rozróżnia** ustalenia od pomysłów/propozycji od decyzji
4. **Zaznacza rzeczy ODŁOŻONE** — "na razie olewamy", "w drugim rzucie", "potem się zastanowimy"
5. **NIE wymyśla** — jeśli coś nie padło w rozmowie, nie ma tego w podsumowaniu

## BRANŻE I KONTEKSTY (wylosuj 2-3 na batch)

### IT / Software:
- Sprint planning / daily standup
- Code review / architektura systemu
- Konfiguracja infrastruktury (Docker, K8s, CI/CD)
- Integracja systemów (API, protokoły, bazy danych)
- Debugowanie produkcyjnego buga
- Demo dla klienta + feedback
- Retrospektywa sprintu

### Przemysł / IoT / BMS:
- System alarmów i monitoringu
- Konfiguracja czujników i sterowników
- Wymiana danych między systemami (Modbus, BACnet, OPC UA)
- Wizualizacja danych na dashboardzie
- Integracja z SCADA / PLC

### Biznes:
- Negocjacje umowy z klientem
- Planowanie budżetu
- Przegląd KPI / wyników kwartalnych
- Rekrutacja — omówienie kandydata
- Onboarding nowego pracownika

### Inne:
- Spotkanie projektowe (budowlane, architektoniczne)
- Konsultacja prawna
- Spotkanie zarządu
- Planowanie eventu / konferencji

## TRUDNOŚĆ
- 20% proste: krótkie spotkanie, jasne ustalenia, 2 osoby
- 50% średnie: dłuższa rozmowa, kilka tematów, pewne odchodzenie od tematu
- 30% trudne: chaotyczna rozmowa, dużo przerywania, zmiany zdania, tematy techniczne z błędami transkrypcji, niejasne ustalenia

## PRZYKŁAD

{"input": "Transkrypcja\n22 marca 2026, 14:00\n\nAnna Wiśniewska 0:00\nDobra, zaczynamy. Mamy review tego co się zmieniło w API od ostatniego tygodnia.\n\nMarcin Kowalski 0:05\nNo to ja zrobiłem ten endpoint do listowania zamówień, jest na branchu feature/orders-list. Zwraca paginację, filtrowanie po statusie i dacie.\n\nAnna Wiśniewska 0:15\nOk, a testy są?\n\nMarcin Kowalski 0:17\nUnit testy tak, ale integracyjne jeszcze nie, bo nie mam mocka dla serwisu płatności.\n\nAnna Wiśniewska 0:23\nTo jest problem, bo bez integracyjnych nie możemy tego mergować. Paweł, możesz przygotować mocka?\n\nPaweł Nowak 0:28\nJasne, ale potrzebuję specyfikacji tego serwisu. Marcin, masz dokumentację API płatności?\n\nMarcin Kowalski 0:33\nMam Swaggera, podeślę ci po spotkaniu.\n\nPaweł Nowak 0:36\nOk, to do jutro powinienem mieć mocka gotowego.\n\nAnna Wiśniewska 0:40\nSuper. Marcin, jak będzie mock to od razu dopisz integracyjne i robimy review. Cel to merge do środy.\n\nMarcin Kowalski 0:45\nJasne.\n\nAnna Wiśniewska 0:47\nDobra, jeszcze jedno — klient zgłosił że filtrowanie po dacie nie działa na produkcji. Marcin, wiesz coś o tym?\n\nMarcin Kowalski 0:52\nA to pewnie ten bug z formatem daty, już wiem o co chodzi. ISO 8601 vs nasz format. Dam fixa dzisiaj.\n\nAnna Wiśniewska 0:58\nOk, to priorytet — najpierw fix, potem reszta. Coś jeszcze?\n\nPaweł Nowak 1:02\nNie, chyba tyle.\n\nAnna Wiśniewska 1:04\nDobra, kończymy.", "output": "TEMAT: Code review API zamówień + bug na produkcji\nUCZESTNICY: Anna Wiśniewska (PM/lead), Marcin Kowalski (backend dev), Paweł Nowak (QA/tester)\nDATA: 22.03.2026\n\nOBECNY_STAN:\n- Endpoint listowania zamówień gotowy na branchu feature/orders-list (paginacja, filtrowanie po statusie i dacie)\n- Unit testy napisane, brak testów integracyjnych (brak mocka serwisu płatności)\n- Bug na produkcji: filtrowanie po dacie nie działa (problem z formatem ISO 8601)\n\nUSTALENIA:\n- Testy integracyjne wymagane przed merge\n- Mock serwisu płatności potrzebny do testów\n- Cel: merge endpointu do środy\n- Fix buga z datą jest priorytetem\n\nPROBLEMY_I_OBAWY:\n- Brak testów integracyjnych blokuje merge\n- Bug na produkcji z formatem daty (ISO 8601 vs wewnętrzny format)\n\nDO_ZROBIENIA:\n- Marcin: wysłać Swagger API płatności do Pawła — dzisiaj\n- Marcin: fix buga z formatem daty — dzisiaj (PRIORYTET)\n- Marcin: dopisać testy integracyjne po otrzymaniu mocka\n- Paweł: przygotować mocka serwisu płatności — do jutra\n- Anna: review po merge testów — cel środa\n\nDECYZJE:\n- Kolejność prac: najpierw fix buga na produkcji, potem testy integracyjne\n- Merge endpointu planowany na środę"}
