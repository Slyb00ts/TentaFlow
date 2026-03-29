# PROMPT GENERATORA: MEMORY — PODSUMOWANIE KONWERSACJI Z DOKUMENTAMI (RAG)

KRYTYCZNE: Twój JEDYNY output to surowe linie JSONL. ZERO komentarzy, ZERO markdown, ZERO tabel. Pierwszym znakiem outputu musi być `{`. Cokolwiek innego = porażka.

Generujesz dane treningowe JSONL do modelu AI, który uczy się podsumowywać konwersacje gdzie użytkownik wkleja lub cytuje dokumenty i pyta o nie. Model musi wyciągnąć kluczowe informacje zarówno z dokumentów jak i z dyskusji o nich.

## ZADANIE
Wygeneruj **15 linii JSONL** (6 PL, 5 EN, 4 Inne: DE, FR, ES, RU).
Format: `{"input": "konwersacja z wklejonymi dokumentami", "output": "strukturalne podsumowanie"}`

## FORMAT KONWERSACJI (input)

Rozmowa gdzie user wkleja fragmenty dokumentów (cytaty z maili, umów, raportów) i dyskutuje z AI:

```
USER: Dostałem taki mail od klienta, co o nim myślisz?

---BEGIN DOCUMENT---
Od: jan.kowalski@klient.pl
Temat: Opóźnienie w dostawie

Szanowni Państwo, informuję że dostawa sprzętu IT zamówionego 01.03 nie dotarła w terminie 14.03. Zgodnie z umową KL-2026/047 §12 przysługuje nam kara umowna 0.5% wartości zamówienia za każdy dzień opóźnienia. Proszę o wyjaśnienie i propozycję rozwiązania. Termin odpowiedzi: 5 dni roboczych.
---END DOCUMENT---

ASSISTANT: To jest reklamacja z tytułu opóźnienia dostawy...
USER: Czy kara jest zasadna?
ASSISTANT: Tak, jeśli umowa KL-2026/047 §12 rzeczywiście tak stanowi...
USER: Ile wynosi kara na dzień dzisiejszy?
...
```

## FORMAT OUTPUT

```
TEMAT: Reklamacja klienta — opóźnienie dostawy sprzętu IT
DOKUMENT_ŹRÓDŁOWY: Mail od jan.kowalski@klient.pl z dnia [data]
KLUCZOWE_FAKTY_Z_DOKUMENTU:
- Zamówienie z 01.03, termin dostawy 14.03
- Umowa: KL-2026/047, §12 — kara umowna
- Kara: 0.5% wartości za każdy dzień opóźnienia
- Termin odpowiedzi: 5 dni roboczych
USTALENIA_Z_ROZMOWY:
- Kara jest zasadna (potwierdzone przez AI)
- Obliczona kara na dzień rozmowy: [kwota]
FEEDBACK_UŻYTKOWNIKA: [cytaty]
ACTION_ITEMS:
- Odpowiedzieć klientowi w ciągu 5 dni
- Sprawdzić przyczynę opóźnienia
DECYZJE: [jeśli podjęte]
```

## TYPY KONWERSACJI Z RAG (wylosuj 4-5 na batch)

### Mail + dyskusja:
- Reklamacja klienta → analiza → propozycja odpowiedzi
- Oferta od dostawcy → porównanie z wymaganiami
- Mail od prawnika → analiza ryzyka
- Zaproszenie do przetargu → analiza warunków
- Raport z audytu → omówienie wyników

### Dokument + pytania:
- Fragment umowy → pytania o konkretne paragrafy
- Regulamin → sprawdzenie czy spełnia RODO
- Specyfikacja techniczna → porównanie z aktualnym systemem
- Raport finansowy → analiza trendów
- CV kandydata → ocena dopasowania do stanowiska

### Wiele dokumentów w rozmowie:
- User wkleja 2 maile i pyta o sprzeczności
- Fragment umowy + faktura → weryfikacja kwot
- Specyfikacja + oferta → porównanie

### Konwersacja z poprawkami:
- AI źle interpretuje dokument → user poprawia
- User wkleja nową wersję dokumentu → zmiany
- AI pomija ważny szczegół → user wskazuje

## KLUCZOWE REGUŁY

1. **Konwersacja 500-1200 tokenów** z min. 1 wklejonym dokumentem
2. **Dokument oznaczony** `---BEGIN DOCUMENT---` / `---END DOCUMENT---`
3. **Podsumowanie ROZDZIELA** informacje z dokumentu od ustaleń z dyskusji
4. **Min. 30% konwersacji zawiera KOREKTY** — user mówi "nie, tu chodzi o coś innego", "pominąłeś kluczowy fragment"
5. **Min. 20% zawiera WIELE dokumentów** w jednej rozmowie
6. **Fakty z dokumentów muszą być DOKŁADNE** — kwoty, daty, nazwiska, paragrafy
7. **Podsumowanie w języku konwersacji** (nie dokumentu, jeśli różne)
8. **Realistyczne dokumenty** — prawnicze, biznesowe, techniczne, HR

## PRZYKŁAD

{"input": "USER: Sprawdź tę ofertę od dostawcy chmury:\n\n---BEGIN DOCUMENT---\nOFERTA nr OFF/2026/0392\nCloudMax Sp. z o.o.\nul. Techniczna 44, Wrocław\n\nDla: TechVentures Sp. z o.o.\n\nPrzedmiot: Hosting dedykowany — pakiet Enterprise\n- 4x serwer bare metal (2x Xeon Gold 6338, 512GB RAM, 4TB NVMe)\n- Load balancer F5 (HA pair)\n- Backup: 30-dniowa retencja, RPO 1h\n- SLA: 99.95% (max. 22 min downtime/miesiąc)\n- Support: 24/7 z czasem reakcji 15 min (P1)\n\nCena: 18 400 PLN netto/miesiąc\nUmowa: 24 miesiące z automatycznym przedłużeniem\nWypowiedzenie: 6 miesięcy przed końcem okresu\nKara za wcześniejsze rozwiązanie: 40% pozostałej wartości\n---END DOCUMENT---\n\nCzy to dobra oferta?\nASSISTANT: Parametry wyglądają solidnie. SLA 99.95% to standard enterprise. Zwróciłbym uwagę na dwie rzeczy: kara 40% za wcześniejsze rozwiązanie jest wysoka, a 6-miesięczny okres wypowiedzenia jest długi.\nUSER: A jak to się ma do naszego budżetu? Mamy max 15k miesięcznie.\nASSISTANT: Oferta jest o 3 400 PLN powyżej budżetu. Możecie negocjować cenę lub zmniejszyć specyfikację — np. 2 serwery zamiast 4 na start.\nUSER: Nie, potrzebujemy 4 serwerów. Ale ta kara za wcześniejsze rozwiązanie jest nie do przyjęcia. Negocjuj to do max 20%.\nASSISTANT: Rozumiem. Proponuję kontr-ofertę: akceptacja ceny 18 400 PLN, ale zmniejszenie kary do 20% i skrócenie wypowiedzenia do 3 miesięcy.\nUSER: Dobry plan. Przygotuj draft maila do nich.", "output": "TEMAT: Analiza oferty hostingu dedykowanego od CloudMax\nDOKUMENT_ŹRÓDŁOWY: Oferta nr OFF/2026/0392 od CloudMax Sp. z o.o.\nKLUCZOWE_FAKTY_Z_DOKUMENTU:\n- Przedmiot: 4x bare metal (2x Xeon Gold 6338, 512GB RAM, 4TB NVMe) + LB F5 HA\n- SLA: 99.95% (max 22 min downtime/miesiąc)\n- Support: 24/7, czas reakcji 15 min P1\n- Backup: 30 dni retencji, RPO 1h\n- Cena: 18 400 PLN netto/miesiąc\n- Umowa: 24 miesiące, auto-przedłużenie\n- Wypowiedzenie: 6 miesięcy\n- Kara za wcześniejsze rozwiązanie: 40% pozostałej wartości\nUSTALENIA_Z_ROZMOWY:\n- Oferta przekracza budżet o 3 400 PLN (budżet: 15 000 PLN/mies)\n- 4 serwery są wymagane — nie można zmniejszyć\n- Kara 40% nieakceptowalna — negocjować do max 20%\n- Propozycja kontr-oferty: cena 18 400 OK, kara 20%, wypowiedzenie 3 mies.\nFEEDBACK_UŻYTKOWNIKA:\n- \"Nie, potrzebujemy 4 serwerów\" (korekta — AI zaproponował zmniejszenie)\n- \"ta kara za wcześniejsze rozwiązanie jest nie do przyjęcia\" (negatywny wobec oferty)\n- \"Dobry plan\" (pozytywny wobec AI)\nACTION_ITEMS:\n- Przygotować draft maila z kontr-ofertą do CloudMax\nDECYZJE: Akceptacja ceny 18 400, negocjacja kary do 20% i wypowiedzenia do 3 mies."}
