# PROMPT GENERATORA: MEMORY — PODSUMOWANIE DOKUMENTÓW

KRYTYCZNE: Twój JEDYNY output to surowe linie JSONL. ZERO komentarzy, ZERO markdown, ZERO tabel. Pierwszym znakiem outputu musi być `{`. Cokolwiek innego = porażka.

Jesteś ekspertem od analizy dokumentów biznesowych i prawnych. Generujesz dane treningowe JSONL do modelu AI, który uczy się wyciągać kluczowe informacje z dokumentów i tworzyć strukturalne podsumowania.

## ZADANIE
Wygeneruj **20 linii JSONL** (8 PL, 6 EN, 6 Inne: DE, FR, ES, IT, NL, PT).
Format: `{"input": "pełny tekst dokumentu", "output": "strukturalne podsumowanie"}`

## FORMAT OUTPUT (podsumowania)

Model ma generować podsumowanie w formacie klucz-wartość, wyciągając WSZYSTKO co istotne:

```
RODZAJ: Umowa sprzedaży nieruchomości
STRONY: Jan Kowalski (kupujący), Anna Nowak (sprzedająca)
DATA: 15.03.2026
PRZEDMIOT: Lokal mieszkalny przy ul. Długiej 15/3, Warszawa, 65m²
KWOTA: 450 000 PLN
TERMIN_PŁATNOŚCI: 30 dni od podpisania aktu notarialnego
WARUNKI: Wpłata zadatku 45 000 PLN w dniu podpisania
KARY: 5% wartości umowy za opóźnienie płatności powyżej 14 dni
UWAGI: Wymaga aktu notarialnego. Sprzedająca oświadcza brak obciążeń hipotecznych.
```

Klucze dostosowane do typu dokumentu (nie zawsze te same):

### Dla umów/kontraktów:
RODZAJ, STRONY, DATA, PRZEDMIOT, KWOTA, TERMIN, WARUNKI, KARY, OKRES_OBOWIĄZYWANIA, WYPOWIEDZENIE, UWAGI

### Dla faktur/rachunków:
RODZAJ, WYSTAWCA, NABYWCA, NR_FAKTURY, DATA_WYSTAWIENIA, TERMIN_PŁATNOŚCI, KWOTA_NETTO, VAT, KWOTA_BRUTTO, POZYCJE, NR_KONTA

### Dla certyfikatów/zaświadczeń:
RODZAJ, WYDANY_DLA, WYDANY_PRZEZ, DATA_WYDANIA, WAŻNY_DO, ZAKRES, NR_CERTYFIKATU

### Dla regulaminów/polityk:
RODZAJ, PODMIOT, DATA_OBOWIĄZYWANIA, KLUCZOWE_POSTANOWIENIA, OGRANICZENIA, KARY, KONTAKT

### Dla raportów:
RODZAJ, AUTOR, DATA, OKRES, KLUCZOWE_WYNIKI, PROBLEMY, REKOMENDACJE, DANE_LICZBOWE

### Dla korespondencji urzędowej:
RODZAJ, NADAWCA, ADRESAT, DATA, SPRAWA, ŻĄDANIE, TERMIN, PODSTAWA_PRAWNA

## TYPY DOKUMENTÓW (wylosuj 4-5 na batch)

- Umowa najmu mieszkania/lokalu
- Umowa o pracę / zlecenie / B2B
- Umowa sprzedaży (nieruchomość, samochód, sprzęt)
- NDA / umowa o poufności
- SLA (Service Level Agreement)
- Faktura VAT / rachunek / nota korygująca
- Certyfikat ISO / audyt / zaświadczenie
- Polisa ubezpieczeniowa
- Regulamin usługi / sklepu / serwisu
- Polityka prywatności / RODO
- Akt notarialny
- Wezwanie do zapłaty / pismo przedsądowe
- Protokół zdawczo-odbiorczy
- Specyfikacja techniczna / RFP / oferta
- Raport kwartalny / audyt finansowy
- Decyzja administracyjna / urzędowa
- Umowa licencyjna (software)
- Warranty / gwarancja produktu
- Term sheet / letter of intent
- Pismo od komornika / nakaz zapłaty

## KLUCZOWE REGUŁY

1. **Dokument musi być REALISTYCZNY** — prawdziwie wyglądające dane (ale fikcyjne), paragrafy, numeracja, podpisy, pieczątki
2. **Dokument musi mieć 300-800 tokenów** — wystarczająco długi żeby było co podsumować
3. **Podsumowanie wyciąga KAŻDY istotny fakt** — kwoty, daty, strony, warunki, kary, terminy
4. **Podsumowanie jest KRÓTSZE niż dokument** — zwięzłe, ale kompletne
5. **Używaj realistycznych danych** — polskie imiona/firmy/adresy dla PL, angielskie dla EN, prawdziwie wyglądające numery NIP/REGON/KRS
6. **Każdy dokument ma INNY kontekst** — różne branże, kwoty, strony
7. **Podsumowanie w tym samym języku co dokument**

## TRUDNOŚĆ
- 30% proste: faktura, zaświadczenie (mało pól)
- 40% średnie: umowa z kilkoma warunkami
- 30% trudne: złożona umowa z wieloma stronami, karami, warunkami, wyjątkami, załącznikami

## PRZYKŁADY

{"input": "UMOWA NAJMU LOKALU MIESZKALNEGO\n\nzawarta w Krakowie dnia 01.03.2026 r.\n\npomiędzy:\nMarią Kowalską, zam. ul. Floriańska 15, 31-021 Kraków, PESEL 75032112345\nzwaną dalej Wynajmującą\na\nTomaszem Nowakiem, zam. ul. Długa 7/3, 30-001 Kraków, PESEL 90061567890\nzwanym dalej Najemcą\n\n§1. Wynajmująca oddaje Najemcy do użytkowania lokal mieszkalny przy ul. Karmelickiej 23/5, Kraków, o powierzchni 48 m², składający się z 2 pokoi, kuchni, łazienki i przedpokoju.\n\n§2. Umowa zostaje zawarta na czas określony od 01.04.2026 do 31.03.2027.\n\n§3. Czynsz najmu wynosi 2 800 PLN miesięcznie, płatny do 10-go dnia każdego miesiąca na konto: PL61 1090 1014 0000 0712 1234 5678.\n\n§4. Najemca wpłaca kaucję w wysokości 5 600 PLN (dwukrotność czynszu).\n\n§5. Opłaty eksploatacyjne (media, internet) ponosi Najemca.\n\n§6. Wypowiedzenie umowy z zachowaniem 2-miesięcznego okresu wypowiedzenia.\n\n§7. Kara za opóźnienie w płatności: odsetki ustawowe.\n\nPodpisy:\nMaria Kowalska — Wynajmująca\nTomasz Nowak — Najemca", "output": "RODZAJ: Umowa najmu lokalu mieszkalnego\nSTRONY: Maria Kowalska (wynajmująca), Tomasz Nowak (najemca)\nDATA: 01.03.2026\nPRZEDMIOT: Lokal mieszkalny, ul. Karmelicka 23/5, Kraków, 48m², 2 pokoje + kuchnia + łazienka + przedpokój\nOKRES: 01.04.2026 — 31.03.2027 (czas określony, 12 miesięcy)\nCZYNSZ: 2 800 PLN/miesiąc, płatny do 10-go dnia miesiąca\nKONTO: PL61 1090 1014 0000 0712 1234 5678\nKAUCJA: 5 600 PLN (2x czynsz)\nMEDIA: po stronie najemcy\nWYPOWIEDZENIE: 2 miesiące\nKARY: odsetki ustawowe za opóźnienie płatności"}
{"input": "INVOICE\n\nInvoice #: INV-2026-0847\nDate: March 15, 2026\nDue Date: April 14, 2026\n\nFrom:\nAcme Solutions Ltd.\n123 Business Park, London EC2A 4NE\nVAT: GB123456789\n\nTo:\nGlobal Tech Inc.\n456 Innovation Drive, Manchester M1 2AB\n\nDescription                          Qty    Unit Price    Total\n-----------------------------------------------------------\nCloud Hosting (March)                 1      £2,400.00    £2,400.00\nAPI Integration Service               40h    £95.00/h     £3,800.00\nSSL Certificate (annual)              3      £49.99       £149.97\n-----------------------------------------------------------\nSubtotal:                                                 £6,349.97\nVAT (20%):                                                £1,270.00\nTotal:                                                    £7,619.97\n\nPayment: Bank transfer to Barclays, Sort: 20-45-67, Acc: 12345678\nRef: INV-2026-0847\n\nThank you for your business!", "output": "TYPE: Invoice\nISSUER: Acme Solutions Ltd., 123 Business Park, London EC2A 4NE, VAT GB123456789\nCLIENT: Global Tech Inc., 456 Innovation Drive, Manchester M1 2AB\nINVOICE_NR: INV-2026-0847\nDATE: March 15, 2026\nDUE_DATE: April 14, 2026\nITEMS: Cloud Hosting March (£2,400), API Integration 40h×£95 (£3,800), SSL Certificate 3× (£149.97)\nSUBTOTAL: £6,349.97\nVAT: £1,270.00 (20%)\nTOTAL: £7,619.97\nPAYMENT: Bank transfer, Barclays, Sort 20-45-67, Acc 12345678, Ref INV-2026-0847"}
