# Contacts

Contacts jest centralnym addonem dla firm, osob, zatrudnien i map relacji. CRM,
Calendar, Billing, Documents, Email i Activity powinny referowac rekordy Contacts
przez `company_id` / `person_id`, a nie kopiowac dane kontaktowe do swoich tabel.

## Zakres pierwszej wersji

- `companies` - firmy z NIP, REGON, KRS, adresem, www i relacja do spolki matki.
- `persons` - osoby z podstawowymi danymi, zgoda RODO, obecna firma i stanowisko.
- `company_persons` - historia zatrudnien osoby w firmach.
- `person_relations` - relacje miedzy osobami, np. `reports_to`, `communicates_with`, `influences`, `blocks`.
- `sales_roles` - role osoby w sprzedazy dla firmy, np. decydent, influencer, user, bloker.
- `smart_lists` - model danych pod zapisane listy, bez gotowego UI zapisu filtrow.

## Narzedzia dla LLM i flow

- `search_contacts` - globalne wyszukiwanie firm i osob.
- `get_company`, `get_person` - szczegoly rekordu.
- `create_company`, `create_person` - zapis po potwierdzeniu przez uzytkownika.
- `attach_person_to_company` - przypiecie osoby do firmy z aktualnym stanowiskiem.
- `list_persons_in_company` - osoby kontaktowe firmy.
- `get_relationship_map` - graf do widoku K4.
- `lookup_company_online` - aktualny lookup MF po NIP/REGON, bez cache.
- `extract_from_text` - tryb `draft`: LLM wyciaga propozycje z maila, notatki lub transkrypcji.
- `compute_person_insights` - tryb `suggest`: LLM generuje wnioski tylko z danych Contacts.

Mutacje z LLM powinny isc przez broker akcji platformy i confirmation dialog. Addon
zwraca dane gotowe do diffu, ale nie powinien sam podejmowac decyzji za uzytkownika.

## Wyszukiwanie online

`lookup_company_online` i `create_company` z `online_lookup=true` uzywaja
oficjalnego Wykazu VAT MF:

```text
https://wl-api.mf.gov.pl/api/search/nip/{nip}?date={YYYY-MM-DD}
https://wl-api.mf.gov.pl/api/search/regon/{regon}?date={YYYY-MM-DD}
```

Addon nie cache'uje odpowiedzi MF. Zapis w `companies` powstaje dopiero po akcji
utworzenia firmy. Sam lookup zwraca `online=true` i `cached=false`.

## Integracje z przyszlymi addonami

Mockupy K2/K3 pokazuja dane, ktorych Contacts nie posiada:

- CRM: aktywne deale, historia wygranych, wartosc pipeline, budzet, status deala.
- Calendar: spotkania, proponowane terminy, preferencje godzin.
- Activity: timeline, zadania, przypomnienia, ostatni kontakt.
- Email: watki, drafty follow-up, liczba interakcji mailowych.
- Billing: faktury, platnosci, ryzyko rozliczeniowe.
- Documents: pliki i zalaczniki przypiete do firmy, osoby albo deala.

Te dane powinny wejsc przez deklaratywne `PanelContribution`, `RelationProvider`,
`SearchProvider` i `ActionProvider` opisane w planie CRM. Contacts dostarcza rdzen:
resource ids, podstawowe dane, relacje i graf. Shell renderuje kontribucje przez
komponenty `tf-*`, nie przez HTML zwracany przez inne addony.

## Budowanie relacji

Relacje powstaja z kilku zrodel:

- recznie: uzytkownik przypina osobe do firmy i ustawia stanowisko lub role sprzedazowa;
- import: CSV/vCard/legacy CRM mapuje firmy po NIP/REGON, osoby po emailu;
- online: lookup MF uzupelnia firme po NIP/REGON;
- AI draft: `extract_from_text` proponuje osoby, firmy, zatrudnienia i relacje z tekstu;
- przyszle addony: Activity/Email/Calendar moga publikowac sygnaly komunikacji jako relacje.

K4 rozroznia dwie warstwy:

- hierarchia formalna: `reports_to`, `manages`, zwykle z danych recznych/importu;
- wplyw sprzedazowy: `decision_maker`, `influencer`, `user`, `blocker` w `sales_roles`;
- komunikacja: docelowo `communicates_with` z Activity/Email, z `strength` liczonym z czestotliwosci.

AI moze sugerowac relacje i role, ale zapis relacji powinien wymagac potwierdzenia,
bo bledny graf decyzyjny moze prowadzic do zlych dzialan handlowych.
