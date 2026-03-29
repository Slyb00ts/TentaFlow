# PROMPT GENERATORA: EXTRACT — WYCIĄGANIE PÓL Z DOKUMENTÓW

KRYTYCZNE: Twój JEDYNY output to surowe linie JSONL. ZERO komentarzy, ZERO markdown, ZERO tabel. Pierwszym znakiem outputu musi być `{`. Cokolwiek innego = porażka.

Generujesz dane treningowe JSONL do modelu AI, który uczy się wyciągać KONKRETNE pola z dokumentów. User podaje listę pól jakie chce — model wyciąga wartości z tekstu. Jeśli pole wymaga wnioskowania (nie jest wprost napisane), model wnioskuje z kontekstu.

## ZADANIE
Wygeneruj **15 linii JSONL** (6 PL, 5 EN, 4 Inne: DE, FR, ES, RU).
Format: `{"input": "<|extract|>\nFIELDS: ...\nDOCUMENT:\n...", "output": "pole=wartość\npole2=wartość2"}`

## FORMAT

### Input
```
<|extract|>
FIELDS: zamawiający, wykonawca, kwota_netto, data_umowy, przedmiot, kary, termin_realizacji
DOCUMENT:
[pełny tekst dokumentu]
```

### Output
```
zamawiający=TechVision Sp. z o.o.
wykonawca=CloudMax Sp. z o.o.
kwota_netto=18 400 PLN/miesiąc
data_umowy=15.03.2026
przedmiot=Hosting dedykowany 4x bare metal
kary=Niedostępność powyżej SLA: 0.5%/dzień (max 20%); Opóźnienie: 1000 PLN/dzień
termin_realizacji=30 dni od podpisania
```

### Reguły outputu
- Jedna linia per pole: `nazwa_pola=wartość`
- Jeśli pole nie występuje → `nazwa_pola=BRAK`
- Jeśli wartość wymaga wnioskowania → wnioskuj z kontekstu dokumentu
- Jeśli pole ma wiele wartości (np. lista kar) → oddziel średnikiem
- Zachowaj precyzję: kwoty z walutą, daty w formacie z dokumentu, pełne nazwy firm
- Output w języku dokumentu

## TYPY DOKUMENTÓW I PÓL (wylosuj 3-4 na batch)

### Umowa o pracę / zlecenie / B2B
Pola: pracodawca, pracownik, stanowisko, wynagrodzenie_brutto, data_zawarcia, okres_zatrudnienia, wymiar_czasu, miejsce_pracy, okres_wypowiedzenia, zakaz_konkurencji
Wnioskowanie: np. "wymiar_czasu" może nie być wprost ale wynika z "pełny etat"

### Umowa handlowa / sprzedaży / dostaw
Pola: sprzedający, kupujący, przedmiot, kwota_netto, kwota_brutto, vat, data_umowy, termin_dostawy, warunki_płatności, kary, gwarancja, reklamacje
Wnioskowanie: np. "kwota_brutto" wyliczana z netto + VAT jeśli podane osobno

### Umowa najmu / dzierżawy
Pola: wynajmujący, najemca, adres_lokalu, powierzchnia, czynsz, kaucja, termin_płatności, okres_najmu, wypowiedzenie, opłaty_dodatkowe
Wnioskowanie: np. "kaucja" opisana jako "dwukrotność czynszu" → musisz wyliczyć

### Faktura
Pola: wystawca, nabywca, numer_faktury, data_wystawienia, termin_płatności, kwota_netto, vat, kwota_brutto, pozycje, numer_konta
Wnioskowanie: kwota_brutto = netto + VAT; pozycje to lista z cenami jednostkowymi

### SLA / umowa serwisowa
Pola: dostawca, klient, usługa, sla_poziom, czas_reakcji, czas_naprawy, kary_sla, okres_umowy, wynagrodzenie, limity
Wnioskowanie: np. "max downtime" wyliczany z SLA% (99.95% = ~22min/miesiąc)

### Certyfikat / zaświadczenie
Pola: wydany_dla, wydany_przez, numer, data_wydania, ważny_do, zakres, norma
Wnioskowanie: "ważny_do" może wynikać z "ważny 3 lata od daty wydania"

### Protokół odbioru
Pola: zamawiający, wykonawca, przedmiot_odbioru, data_odbioru, uwagi, zastrzeżenia, status_odbioru, podpisy
Wnioskowanie: "status_odbioru" może nie być wprost — wnioskuj z uwag (brak zastrzeżeń = przyjęty)

### Polisa ubezpieczeniowa
Pola: ubezpieczyciel, ubezpieczony, numer_polisy, przedmiot, suma_ubezpieczenia, składka, okres, wyłączenia, franszyza
Wnioskowanie: franszyza może być opisana jako "udział własny" lub "redukcyjna"

### Raport finansowy / kwartalny
Pola: firma, okres, przychody, koszty, zysk_netto, ebitda, zmiana_rr, zatrudnienie, prognoza
Wnioskowanie: "zmiana_rr" wyliczana z porównania z poprzednim okresem

### Pismo urzędowe / decyzja
Pola: organ, strona, numer_decyzji, data, sprawa, rozstrzygnięcie, podstawa_prawna, termin_odwołania
Wnioskowanie: termin odwołania może wynikać z "14 dni od doręczenia"

## KLUCZOWE REGUŁY

1. **Dokument 300-800 tokenów** — realistyczny, z numeracją paragrafów, podpisami
2. **Pola 5-10 per rekord** — nie za mało, nie za dużo
3. **Min. 30% pól wymaga WNIOSKOWANIA** — wartość nie jest wprost w tekście
4. **Min. 20% ma pola z BRAK** — pole którego w dokumencie nie ma
5. **Kwoty ZAWSZE z walutą** — "18 400 PLN" nie "18400"
6. **Daty w formacie z dokumentu** — nie normalizuj
7. **Każdy dokument z INNEJ branży** — różnorodność
8. **Realistyczne dane** — fikcyjne ale prawdziwie wyglądające

## TRUDNOŚĆ
- 30% proste: faktura (pola wprost), certyfikat (mało pól)
- 40% średnie: umowa z kilkoma polami do wnioskowania
- 30% trudne: złożona umowa gdzie np. kary wynikają z kombinacji paragrafów, kwota brutto trzeba wyliczyć, termin zależy od warunku

## PRZYKŁADY

{"input": "<|extract|>\nFIELDS: wynajmujący, najemca, adres, powierzchnia, czynsz, kaucja, okres, wypowiedzenie, media\nDOCUMENT:\nUMOWA NAJMU LOKALU MIESZKALNEGO\nzawarta w Krakowie dnia 01.03.2026\npomiędzy: Marią Kowalską, zam. ul. Floriańska 15, Kraków (Wynajmująca)\na Tomaszem Nowakiem, zam. ul. Długa 7/3, Kraków (Najemca)\n\n§1. Wynajmująca oddaje lokal przy ul. Karmelickiej 23/5, Kraków, 48m².\n§2. Umowa od 01.04.2026 do 31.03.2027.\n§3. Czynsz 2 800 PLN/mies, płatny do 10-go. Kaucja: dwukrotność czynszu.\n§4. Media ponosi Najemca.\n§5. Wypowiedzenie z 2-miesięcznym okresem.", "output": "wynajmujący=Maria Kowalska, ul. Floriańska 15, Kraków\nnajemca=Tomasz Nowak, ul. Długa 7/3, Kraków\nadres=ul. Karmelicka 23/5, Kraków\npowierzchnia=48 m²\nczynsz=2 800 PLN/miesiąc, płatny do 10-go dnia miesiąca\nkaucja=5 600 PLN\nokres=01.04.2026 — 31.03.2027 (12 miesięcy)\nwypowiedzenie=2 miesiące\nmedia=po stronie najemcy"}
{"input": "<|extract|>\nFIELDS: provider, client, sla_level, max_downtime, response_time, penalty, contract_duration, monthly_cost\nDOCUMENT:\nSERVICE LEVEL AGREEMENT\nBetween: CloudPeak Ltd (Provider) and DataFlow Inc (Client)\nEffective: April 1, 2026\n\n1. Service: Managed Kubernetes Cluster (3 nodes, 64GB RAM each)\n2. Availability: 99.95% monthly uptime guarantee\n3. Support: 24/7 with 15-minute response for P1 incidents\n4. Penalties: 5% monthly fee credit per 0.1% below SLA target\n5. Term: 24 months, auto-renewal\n6. Pricing: $4,200/month\n7. Exclusions: Scheduled maintenance windows (max 4h/month) not counted against SLA", "output": "provider=CloudPeak Ltd\nclient=DataFlow Inc\nsla_level=99.95% monthly uptime\nmax_downtime=~22 minutes/month (calculated from 99.95%)\nresponse_time=15 minutes for P1 incidents (24/7)\npenalty=5% monthly fee credit per 0.1% below SLA\ncontract_duration=24 months, auto-renewal\nmonthly_cost=$4,200/month"}
