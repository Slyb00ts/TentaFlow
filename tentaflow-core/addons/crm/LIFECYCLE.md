# CRM — cykl życia projektu (research)

Notatka projektowa dla nowego addonu CRM w TentaFlow. Bazuje na reverse-engineeringu modułu Projekty ze starego IntrApp (`~/repos/dotnet/IntrApp`). Cel: zachować to, co naprawdę pracowało w starym systemie i odrzucić martwe pola.

## Założenia podstawowe

- Cały lifecycle handlowy żyje w **jednym rekordzie projektu** sterowanym statusami + flagami. Brak osobnych encji `Chance`/`Offer` — szansa i oferta to fazy tego samego rekordu.
- Pola atrybutowe wersjonujemy SCD2 (`valid_from`/`valid_to` + `active`). Edycja = INSERT nowej wersji, stary wiersz dostaje `valid_to=now()` i `active=false`.
- Tabela "core" (Project) i tabela "attributes" (ProjectAttributes) były osobno tylko po to, by trzymać wersjonowanie. W nowym rozwiązaniu można zostawić to rozdzielenie albo schować je za jednym widokiem — decyzja implementacyjna, nie domenowa.

## Pola zachowane (używane w realnym flow)

### Identyfikacja
- `id` — PK
- `code` — wewnętrzny kod (auto-gen, sekwencja per właściciel)
- `description` — nazwa
- `full_description` — długi opis
- `contract_number` — numer umowy (zewnętrzny)

### Strony
- `principal_contact_id` (stare `ContactId`) — mocodawca / kontrahent główny
- `end_client_contact_id` (stare `ContactId2`/`EndClientId`) — klient końcowy
- `approver_contact_person_id` — osoba zatwierdzająca kartę akceptacji
- `account_holder_contact_person_id` — pełnomocnik / odpowiedzialny u klienta

### Organizacja
- `section_id` — sekcja
- `leading_section_id` — sekcja wiodąca
- `leading_section_person_id` — osoba prowadząca z sekcji wiodącej
- `responsible_persons[]` (osobna tabela: id pracownika, rola/job, share, is_leader, send_notifications)

### Klasyfikacja
- `project_type_id` — typ z katalogu `ProjectTypes` (osobny katalog z polem `type ∈ {2 oferta/szansa, 3 realizacja}` — to ten przełącznik faz)
- `scope_id` (stare `ProjectType` int) — zakres z katalogu `ProjectScope`
- `status_list_id` + `status_list_status` — magiczna wartość **100 = realizacja** (przełącza wszystko na widok realizacyjny)
- `status` — `0=otwarty | 1=aktywny | 2=zamknięty`
- `is_trade_project` — flaga "to projekt handlowy" (włącza CRM-owe widoki)
- `is_active_project` — soft-zamknięcie (zachowane na liście "zamknięte")
- `contract_kind` — rodzaj umowy (jeśli mamy katalog rodzajów)

### Daty
- `sign_date` — data podpisu umowy
- `end_date` — planowane zakończenie
- `realization_date` — faktyczne zakończenie (ustawiana przy `Done`)
- `duration` — czas trwania (dni/miesiące — zależy od `contract_kind`)
- `budget_card_valid_date` — ważność karty budżetowej

### Finanse
- `currency_id` + `course` + `rate_date` — waluta + kurs + data kursu
- `value`, `value_rated` — wartość w walucie projektu i przeliczona
- `vat_rate_id`
- `account_id` — konto księgowe
- `margin_predicted`, `margin_predicted_percent` — marża prognozowana
- `planned_budget` — budżet planowany
- `billing_status_id` — status fakturowania
- `interval_id` — interwał (na razie informacyjny, automatu nie było — patrz "Czego nie było")

### Workflow zatwierdzania
- `commit` — budżet zatwierdzony przez kierownictwo (decyduje o widoczności w prognozach przed realizacją)
- `acceptance_card_file_id` — PDF karty akceptacji (wygenerowany z szablonu)
- `acceptance_card_send` — czy wysłana
- `acceptance_card_accepted` / `acceptance_card_declined` — wynik kliknięcia z maila

## Pola **odrzucone** (martwe lub redundantne w starym kodzie)

| Stare pole | Powód |
|---|---|
| `IsAcc` | duplikat `IsTradeProject` |
| `OfferChanceOnly` | zawsze ustawiane na `false`, brak kodu zmieniającego — pole zdeprecjonowane |
| `ApproverId2`, `AccountHolderId2`, `IsAccountHolder2` | drugie pełnomocnictwo nigdzie nie sprawdzane |
| `OTNStatus` | nie znaleziono użycia w `Set/Get/List/Done` |
| `OfferType`, `OfferDate` | faza oferty jest sterowana przez `status_list_status`, dedykowane pola redundantne |
| `RealizationStatus` | redundantne ze `status` + `status_list_status==100` |
| `ExchangeDate` | duplikat `rate_date` |
| `ModifyBy`, `ModifyDate` | mamy SCD2 (`valid_from`/`valid_to`) — historia z definicji |
| `Fold` | UI-only (zwinięcie w drzewku) — przeniesione do stanu UI, nie do bazy |
| `Comment` | mamy `full_description` — jedno pole na długi tekst wystarczy |
| `InvoiceIssued` | wyliczalne z istnienia powiązanych przychodów |
| `RemainsBudget` | wyliczane (`planned_budget − suma kosztów`), nie trzymamy |
| `IACode` (z `Projects`) | wewnętrzny artefakt starego systemu |

Jeśli któreś z odrzuconych pól okaże się potrzebne — dodajemy świadomie, nie kopiujemy hurtem.

## Encje powiązane (zachowane)

- **`trade_costs`** (plan) i **`trade_realization_costs`** (realizacja) — koszty rozbite po typie:
  `1=dodatki | 2=podwykonawcy | 3=inne | 4=materiały | 5=usługi wewnętrzne`
- **`project_incomes`** (plan przychodów) i **`project_realization_incomes`** (faktyczne):
  - `code` (auto), `value`, `value_rated`, `currency_id`, `course`, `rate_date`
  - `invoice_number` — **punkt styku z e-dokumentami** (numer e-faktury / KSeF wpisywany ręcznie albo przez integrację zewnętrzną)
  - `date`, `vat_rate_id`, `realization_days`, `to_pay_days`
  - `linked_costs[]` — refakturowanie wielu kosztów jednym przychodem
  - `document_cost_id` — powiązanie z kosztem z e-dokumentu
- **`document_cost_projects`** (N:M koszt e-dokument ↔ projekt):
  - `amount`, `amount_type` (kwota lub %)
  - `is_accepted`, `acceptor_id` — workflow akceptacji kosztu na projekcie
- **`project_financial_conditions`** — warunki płatności (Type=1 itd.)
- **`project_contact_persons`** — osoby kontaktowe po stronie klienta (z udziałem czasu `working_time`)
- **`trade_responsible_persons`** — handlowcy / odpowiedzialni po naszej stronie (`is_leader`, `can_edit_project`, `send_notifications`, `job_id`, `share`)
- **`trade_categories`** — przypisanie do kategorii (widoczność / filtrowanie list)

Z czego REZYGNUJEMY w nowym addonie:
- **Czas pracy** (`Timesheets`, `NewTimesheet`) — poza scope CRM.
- **Zadania** (`Tasks`) — poza scope CRM.
- **Dziennik operatora** (`ProductNotes`) — TentaFlow ma własny `audit_log`.
- **Subprojekty Presales/Realizacja/Utrzymanie** — sztywno tworzone w starym `Set()`, w praktyce niewykorzystywane jako oddzielne rekordy (zastępujemy fazami statusu).

## Cykl życia (skrót)

```
TWORZENIE
  insert Project + ProjectAttributes (active=true, valid_from=now, valid_to=∞)
  code = generate_code()
  status_list_status < 100, is_trade_project=true, is_active_project=true
  commit=false, acceptance_card_*=false

FAZA SZANSA/OFERTA  (status_list_status < 100, project_type.type=2)
  widok finansowy: trade_costs + project_incomes  (plan)
  edycja: SCD2 (stary wiersz active=false + valid_to=now, nowy insert)
  commit=true  → projekt wchodzi do prognoz
  acceptance_card_send=true → mail z 3 linkami (accept / decline / view)
                              → kliknięcie zwrotne ustawia accepted lub declined

PROMOCJA DO REALIZACJI  (warunek: escalation_complete=true)
  1. wygaszenie poprzednich realization_costs i realization_incomes
  2. KLON trade_costs → trade_realization_costs (nowe kody)
  3. KLON project_incomes → project_realization_incomes
     z remapowaniem linked_costs (stare id → nowe id realizacyjne)
  4. project_type_id ← typ "Realizacja" (type=3)
  5. status_list_status = 100

FAZA REALIZACJI  (status_list_status == 100)
  widok finansowy: trade_realization_costs + project_realization_incomes
  set_income():
    - insert/update project_realization_incomes
      (code, value, invoice_number, date, vat_rate_id, currency_id, course,
       linked_costs[], document_cost_id, realization_days, to_pay_days)
    - jeśli document_cost_id > 0:
        document_cost_projects.acceptor_id = current_user
        document_cost_projects.is_accepted = true

ZAMKNIĘCIE  (Done)
  is_active_project = false
  status = 2
  realization_date = now() (jeśli nie ustawione wcześniej)

USUWANIE  (soft)
  Project.active = false
  ProjectAttributes.active = false, valid_to = now()
```

## Co od czego zależy — najważniejsze sprzężenia

| Zmiana | Skutek |
|---|---|
| `id == 0` przy zapisie | nowy `code`, insert obu tabel, audit log |
| Każda edycja | wersjonowanie SCD2 atrybutów, reinsert `responsible_persons` |
| `commit = true` | widoczność w prognozach przed realizacją |
| `acceptance_card_send = true` | mail z linkami do osoby zatwierdzającej |
| Kliknięcie linku z maila | ustawienie `accepted` lub `declined` |
| `status_list_status` rośnie do 100 + `escalation_complete` | klonowanie planu na realizację, przełącznik widoków na realizacyjne, zmiana `project_type` na "Realizacja" |
| `status_list_status` spada | mail do board membera z linkiem do usunięcia |
| `status_list_status == 100` (stan trwały) | wszystkie raporty/karty używają tabel realizacyjnych |
| `set_income` z `document_cost_id > 0` | akceptacja kosztu z e-dokumentu na poziomie tego projektu |
| `Done(true)` | soft-zamknięcie, `is_active_project=false` |

## Integracja z elektronicznymi dokumentami (numery faktur)

Punkt styku w starym systemie:

1. **`project_realization_incomes.invoice_number`** — numer dokumentu sprzedażowego (nasza faktura sprzedaży / e-faktura wystawiona przez nas). W starym kodzie wpisywany ręcznie albo zasilany przez zewnętrzny serwis.
2. **`document_cost_attributes.invoice_number`** — numer faktury dostawcy (e-dokument kosztowy przyjmowany do systemu).
3. **`document_cost_projects`** — rozksięgowanie jednego e-dokumentu kosztowego na wiele projektów (`amount` / `amount_type` + workflow `is_accepted` / `acceptor_id`).
4. **`linked_costs[]`** w przychodach — refakturowanie wielu kosztów jednym przychodem.

W starym IntrApp nie było klienta KSeF wbudowanego — wymiana numerów odbywała się tekstowo. W nowym addonie zostawiamy te same punkty styku jako pola, a sam transport e-faktury (KSeF / inne) będzie osobnym addonem/serwisem, który będzie:
- wpisywał numer KSeF do `invoice_number` po wystawieniu,
- tworzył wpisy w `document_costs` po odebraniu UPO/dokumentu kosztowego.

## Widoczność na liście projektów

Pełny zestaw filtrów (do zachowania):
- `active = true` na obu tabelach + okienko `valid_from <= now() < valid_to`
- filtr po `is_active_project` (otwarte / zamknięte / wszystkie)
- filtr po kategorii (`trade_categories`)
- filtr "tylko handlowe" (`project_type.type IN (2,3)`)
- filtr po `commit` (jeśli chcemy tylko zatwierdzone)
- **uprawnienia / widoczność**:
  - tryb klasyczny: pokaż jeśli user należy do kategorii projektu LUB jest osobą kontaktową LUB twórcą LUB ma `all_read`
  - tryb CRM (w starym systemie `Config.UseProject > 0`): pokaż jeśli user jest na liście `responsible_persons` LUB pozostałe warunki

## Co warto przemyśleć (decyzje otwarte)

1. **`status_list_status == 100` to dziś magic number**. W nowym kodzie → enum `ProjectPhase { Lead, Offer, InRealization, Closed }` zamiast int-a.
2. **Klon planu → realizacji**: w starym kodzie to imperatywna procedura w `Set()`. Można zrobić to czyściej jako dedykowane przejście stanu (event `PromoteToRealization`), z atomowym kopiowaniem i remapem `linked_costs`.
3. **SCD2 na atrybutach**: dla TentaFlow (SQLite) to działa, ale dla tabel finansowych (`realization_incomes`) — czy potrzebujemy historii zmian per pole, czy wystarczy `updated_at`? W starym kodzie wersjonowane były tylko `ProjectAttributes`.
4. **Subprojekty Presales/Realizacja/Utrzymanie** — czy w ogóle? W starym kodzie sztywno generowane, ale brak śladu rzeczywistego użycia. Rekomendacja: nie tworzyć.
5. **Karta budżetowa / karta akceptacji**: szablony PDF (3 szablony: budżetowa, akceptacji, harmonogram). Decyzja: czy renderujemy w addonie (np. przez Typst/HTML→PDF), czy przez host functions TentaFlow?
6. **Refakturowanie wielu kosztów jednym przychodem (`linked_costs[]`)**: w SQLite zamiast tablicy → tabela pośrednia `income_linked_costs (income_id, cost_id)`.

## Mapowanie tabel — propozycja (do iteracji)

```
projects                    — core, identyfikator + audit
project_attributes          — wersjonowane atrybuty biznesowe (SCD2)
project_contact_persons     — osoby kontaktowe po stronie klienta
project_responsible_persons — handlowcy / odpowiedzialni po naszej stronie
project_categories          — przypisania do kategorii (M:N)
project_financial_conditions— warunki płatności

trade_costs                 — plan kosztów
trade_realization_costs     — koszty realizacyjne (klon przy promocji)

project_incomes             — plan przychodów
project_realization_incomes — przychody realne (klon przy promocji)
income_linked_costs         — N:M przychód ↔ koszt (refakturowanie)

document_costs              — e-dokumenty kosztowe
document_cost_attributes    — wersjonowane atrybuty e-dokumentu
document_cost_projects      — N:M e-dokument ↔ projekt z workflow akceptacji

project_types               — katalog (z polem `type` 2/3 sterującym fazą)
project_scopes              — katalog zakresu
status_list                 — katalog statusów per właściciel
billing_statuses            — katalog statusów fakturowania
intervals                   — katalog interwałów fakturowania
contract_kinds              — katalog rodzajów umów
```

## Czego **nie było** w starym IntrApp (a może warto dodać)

- Automatyczne generowanie planu `project_incomes` z `billing_status_id` + `interval_id`. Pola istniały, generator nie istniał — wszystkie przychody wpisywane ręcznie.
- Wbudowany klient KSeF / e-Faktur (numery wpisywane ręcznie lub przez zewnętrzny serwis).
- Powiadomienia o nowym przychodzie do handlowców (kod był, ale **zakomentowany**).
- Audyt statusów z poziomu bazy (brak triggerów — wszystko w warstwie aplikacji).

---

*Plik roboczy. Aktualizujemy w trakcie projektowania addonu, zanim zacznie powstawać kod.*
