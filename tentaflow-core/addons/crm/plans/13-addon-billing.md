# 13 · Addon — Billing

**Mockupy:** [B1 faktury sprzedaży](../../../../../.gstack/projects/Slyb00ts-TentaFlow/designs/crm-v1/b01-invoices.html) · [B2 inbox kosztów](../../../../../.gstack/projects/Slyb00ts-TentaFlow/designs/crm-v1/b02-cost-inbox.html) · [C10 refakturowanie](../../../../../.gstack/projects/Slyb00ts-TentaFlow/designs/crm-v1/c10-cost-refactor.html)

## Cel

Finansowe zdarzenia z **lifecyclem**: faktury sprzedaży (przychody) i e-dokumenty kosztowe (zakupy) — z rozksięgowywaniem kosztów na deale, marżą, integracją z KSeF/e-Faktura.

Osobny od `documents` bo faktura ≠ plik. Faktura ma stan (wystawiona/zapłacona/przeterminowana), numer KSeF, powiązanie z dealem, refakturowanie. Plik PDF faktury żyje w documents.

## Domeny i encje

### `sales_invoices` (faktury sprzedaży — wystawione przez nas)

| Pole | Typ | Opis |
|---|---|---|
| `id` | UUID PK | |
| `invoice_number` | TEXT UNIQUE | `FV/2026/05/0042` (nasza numeracja) |
| `ksef_number` | TEXT UNIQUE | `87412-2026` (z KSeF po wysłaniu) |
| `client_company_id` | UUID FK → companies | |
| `client_person_id` | UUID FK | Opcj. (osoba kontaktowa) |
| `linked_deal_id` | UUID FK → crm.deals | Z którego deala wynika |
| `issue_date` | DATE | |
| `due_date` | DATE | |
| `paid_date` | DATE | Null = nie zapłacone |
| `net_amount` | DECIMAL | |
| `vat_amount` | DECIMAL | |
| `gross_amount` | DECIMAL | |
| `currency` | TEXT | „PLN" / „EUR" / „USD" |
| `exchange_rate` | DECIMAL | Wobec waluty bazowej |
| `status` | ENUM | `draft | issued | sent | paid | overdue | canceled` |
| `payment_terms_days` | INT | |
| `pdf_file_id` | UUID FK → documents.files | |
| `ksef_sent_at` | TIMESTAMP | |
| `ksef_accepted_at` | TIMESTAMP | |
| `ksef_response` | JSONB | Dla audit |
| `notes` | TEXT | |
| `created_by`, `created_at`, `updated_at` | | |

### `sales_invoice_items`

| Pole | Typ | Opis |
|---|---|---|
| `id` | UUID PK | |
| `invoice_id` | UUID FK | |
| `description` | TEXT | „Wdrożenie modułu fleet" |
| `quantity` | DECIMAL | |
| `unit_price_net` | DECIMAL | |
| `vat_rate` | DECIMAL | 0.23 / 0.08 / 0 |
| `position_order` | INT | |

### `cost_documents` (e-dokumenty kosztowe — przyjęte od dostawców, np. z KSeF)

| Pole | Typ | Opis |
|---|---|---|
| `id` | UUID PK | |
| `supplier_number` | TEXT | Numer nadany przez dostawcę (`FV/2026/05/00187` Allegro) |
| `our_internal_number` | TEXT | Numer w naszej księdze przyjęć |
| `ksef_number` | TEXT | |
| `supplier_company_id` | UUID FK → contacts.companies | Dostawca (też trzymany w Contacts) |
| `issue_date` | DATE | |
| `due_date` | DATE | |
| `paid_date` | DATE | |
| `net_amount` | DECIMAL | |
| `vat_amount` | DECIMAL | |
| `gross_amount` | DECIMAL | |
| `currency` | TEXT | |
| `subject` | TEXT | „Subskrypcja API Allegro · maj 2026" (z AI extract jeśli były skany) |
| `category` | TEXT | „Software/SaaS", „Hardware", „Services", „Subcontractor" (z katalogu) |
| `status` | ENUM | `pending_review | reviewed | refactured | own_cost | paid | rejected` |
| `accountant_approved_by`, `accountant_approved_at` | | Akceptacja księgowości |
| `pdf_file_id` | UUID FK | |
| `ksef_received_at` | TIMESTAMP | |
| `ai_extracted` | BOOL | Czy AI wyciągnął przedmiot |
| `created_at` | | |

### `cost_allocations` (rozksięgowanie kosztu na deale — kluczowa tabela)

| Pole | Typ | Opis |
|---|---|---|
| `id` | UUID PK | |
| `cost_document_id` | UUID FK | |
| `deal_id` | UUID FK → crm.deals | |
| `amount_net` | DECIMAL | Część kwoty netto na ten deal |
| `amount_pct` | DECIMAL | % całości (computed) |
| `refacture_markup_pct` | DECIMAL | Narzut przy refakturze (default 15%) |
| `refactured_amount` | DECIMAL | Computed: `amount_net * (1 + markup_pct)` |
| `is_accepted_by_owner` | BOOL | Owner deala zaakceptował |
| `accepted_by_owner_at` | TIMESTAMP | |
| `created_at`, `created_by` | | |

Jeden cost może być rozksięgowany na wiele deali (proporcjonalnie). Jeden deal może mieć wiele cost_allocations (różne koszty).

### `margin_snapshots` (snapshoty marży per deal — dla raportów)

| Pole | Typ | Opis |
|---|---|---|
| `deal_id` | UUID FK | |
| `snapshot_date` | DATE | |
| `value_pln` | DECIMAL | Wartość deala |
| `costs_planned_pln` | DECIMAL | Suma kosztów planowych |
| `costs_actual_pln` | DECIMAL | Suma kosztów faktycznych (refactured) |
| `margin_pln` | DECIMAL | |
| `margin_pct` | DECIMAL | |

Daily job.

## Lifecycle

### Faktura sprzedaży

```
draft → issued (numer FV nadany) → sent (PDF + email do klienta + KSeF) 
  → KSeF accepted | rejected
  → paid (gdy data zaksięgowana) OR overdue (po due_date bez paid)
  → optional: canceled (korekta)
```

### Cost document (e-dokument kosztowy)

```
received (z KSeF / OCR / manual upload)
  → AI auto-extract subject, category
  → pending_review (czeka na operatora)
  → reviewed: allocate to deals (cost_allocations created) | own_cost (overhead, nie refakturujemy)
  → owner deala accepts cost_allocation (per deal)
  → refactured (gdy klient zapłaci refakturę — appears in sales_invoices indirectly)
  → paid (gdy my zapłacimy dostawcy)
```

## UI surfaces

### B1 — Faktury sprzedaży

KPIs top: wystawione, zapłacone (%), w terminie, przeterminowane. Filtry, tabela.

**Akcje:**

1. **„Nowa faktura"** → wizard (klient → deal → pozycje → preview PDF → wystawić)
2. **Klik wiersza** → modal detail (z PDF viewer i status timeline)
3. **„Wyślij do KSeF"** (na fakturze issued) → `billing.send_to_ksef(invoice_id)`. Background job, status updates.
4. **„Oznacz jako zapłaconą"** (manual jeśli nie ma integracji bankowej) → update status + dispatch event
5. **„Korekta"** (na issued/sent) → tworzy fakturę korygującą + linked do oryginału
6. **Filtry status** — Wszystkie/Zapłacone/W terminie/Przeterminowane/Wysłane KSeF
7. **AI alert** banner — „Polskie LNG zalega 27 dni, historyczna średnia 18d — eskalacja zalecana" — klik otwiera dialog z propozycją: mail eskalacyjny do CFO / telefon scheduled / windykacja workflow

### B2 — Inbox kosztów

Tabela e-dokumentów `status=pending_review`. KPIs: wszystkie wartościowo, do akceptacji.

**Akcje:**

1. **Klik wiersza** → otwiera C10 (Cost refactor screen)
2. **„Pomiń (koszt własny)"** → status = own_cost, brak refactury
3. **„Refakturuj"** → otwiera modal rozksięgowania (1 deal) lub C10 (multi-deal split)
4. **„Auto-rozksięguj wszystkie (AI)"** (top) → AI proponuje dla każdego pendingu sugerowane allocations, batch confirm

### C10 — Refakturowanie pojedynczego e-dokumentu (drill-down z B2)

**Layout:** lewa kolumna = dane dokumentu źródłowego + AI rozpoznanie przedmiotu. Prawa kolumna = builder allocations (lista dealów + kwoty/procenty + narzut).

**Akcje:**

1. **„Sugestia AI"** (top-right) → AI proponuje allocations z confidence (np. „cały koszt na deal Allegro Marketplace, confidence 92%")
2. **„Akceptuj propozycję"** → wypełnia allocations
3. **„Rozdziel inaczej"** → ręczna edycja
4. **Edycja per deal:** kwota (PLN lub %), narzut (% markup), notatka
5. **„+ Dodaj kolejny deal"** → kolejny wiersz w allocations
6. **„Zatwierdź rozksięgowanie"** → INSERT cost_allocations, dispatch event `cost.allocated`
7. Po zatwierdzeniu: każdy owner deala dostaje task `accept_cost_allocation` w activity addon

### Wewnątrz innych addonów (contributions)

**deal.detail.sidebar** → sekcja „Koszty & refaktury" z:
- Plan kosztów (z trade_costs w CRM)
- Marża planowa
- E-dokumenty podpięte (z `cost_allocations`)
- „z czego do akceptacji" badge

**contact.detail.sidebar / company.detail.sidebar** → „Faktury / rozliczenia" KPIs (wystawione, zapłacone %, avg opóźnienie).

## Provided contracts

**Resources:** `sales_invoice`, `cost_document`, `cost_allocation`

**Queries:**
- `billing.list_invoices(filter)`
- `billing.get_invoice(id)`
- `billing.list_cost_documents(filter)`
- `billing.list_cost_allocations_for_deal(deal_id)`
- `billing.compute_margin_for_deal(deal_id, at_date?)`
- `billing.compute_margin_for_section(section_id, period)` — dla dashboards
- `billing.invoices_for_company(company_id) → list<Invoice>` — view contribution

**Actions:**
- `billing.create_invoice(input) → Invoice` (act, required)
- `billing.send_to_ksef(invoice_id)` (act, required — koszt prawdziwy, integracja gov)
- `billing.mark_paid(invoice_id, paid_date)` (act, required)
- `billing.create_correction(invoice_id, ...)` (act, required)
- `billing.ingest_cost_document(file_id | ksef_id)` (act — przyjmuje plik / pobiera z KSeF)
- `billing.allocate_cost(cost_id, deal_id, amount, markup_pct)` (act, required)
- `billing.accept_cost_allocation(allocation_id)` (act, required — tylko owner deala)
- `billing.reject_cost_allocation(allocation_id, reason)` (act)

**Views:**
- `deal.detail.sidebar` → „Koszty & refaktury"
- `contact.detail.sidebar`, `company.detail.sidebar` → „Faktury / rozliczenia"
- `dashboard.handlowiec.sidebar` → „Refaktury do uzgodnienia"
- `dashboard.zarzad` → „Marża per sekcja", „Ryzyko płatności"

**Events:**
- `billing.invoice_issued / sent / paid / overdue`
- `billing.cost_received`
- `billing.cost_allocated`
- `billing.cost_allocation_accepted / rejected`
- `billing.ksef_response_received`

**AI Tools:**
- `billing.attach_cost_to_deal(cost_id, deal_id, amount?, markup_pct?)` (act, required)
- `billing.suggest_allocations(cost_id) → list<SuggestedAllocation>` (read, AI proposes)
- `billing.summarize_financial_health(client_company_id)` (read)
- `billing.detect_payment_risk(invoice_id) → RiskAssessment` (read)

## Consumed contracts

```toml
[needs.platform]
permissions, ai_broker, ksef_client  # ksef_client = osobny serwis (przyszłość) lub direct API
[needs.contacts]
get_company = []
search = []
[needs.documents]
render_from_template = []  # do generowania PDF faktury
upload = []
get_file = []
[needs.crm]
read_deal = []
list_deals_for_company = []
subscribe = ["deal.won", "deal.promoted_to_realization"]  # automatyczne propozycje faktury po wygranej
[needs.activity]
create_task = []  # task „akceptuj koszt" dla ownera deala
publish_event = []
```

## Permissions

- Faktury sprzedaży — visibility z deala (jeśli widzisz deal, widzisz fakturę)
- Cost documents — accountant + admin (cost.read)
- Cost allocations — owner deala (z [02 permissions](./02-platform-permissions.md))
- Margins — `flag: see_all_in_section/dept` + sales-manager
- KSeF integration — admin tylko

## KSeF Integration (na razie out-of-scope, ale planujemy)

Punkty styku w schema (pola `ksef_number`, `ksef_sent_at`, `ksef_response`) są **gotowe**. Realna integracja z KSeF API to:
1. Osobny serwis `ksef-client` (Rust binary lub jako addon)
2. Wystawia tooly: `ksef.send_invoice(xml)`, `ksef.receive_invoice(ksef_id)`, `ksef.list_received_since(date)`
3. Billing addon konsumuje przez kontrakt

Migracja: na MVP **bez KSeF** — handlowcy wpisują ręcznie numer KSeF gdy dostaną z zewnątrz. v2 → realna integracja.

## Migracja z IntrApp

| IntrApp | TentaFlow Billing |
|---|---|
| `Invoices` + `InvoiceAttributes` | `sales_invoices` |
| `InvoiceItems` | `sales_invoice_items` |
| `DocumentCosts` + `DocumentCostAttributes` | `cost_documents` |
| `DocumentCostProjects` | `cost_allocations` (z konwersją amount/% + dodaniem refacture_markup_pct = default 0 jeśli IntrApp nie miał) |
| `ProjectRealizationIncomes` | wpisywane w `sales_invoices.linked_deal_id` + `invoice_number` |
| `LinkedCosts[]` (array IDs) | konwertowane do M:N przez `invoice_cost_links` (osobna tabela jeśli potrzebne) |

## Implementation order

1. Schema (wszystkie tabele).
2. Host fn read.
3. UI B1 lista faktur (read-only).
4. Host fn create_invoice + UI wizard.
5. PDF rendering faktury (przez documents.render_from_template).
6. UI B2 inbox kosztów (read-only).
7. Host fn ingest_cost_document + AI extraction (przez documents.extract_metadata + custom prompts).
8. UI C10 refactor — single cost split.
9. Host fn allocate_cost + accept/reject workflow.
10. View contributions w CRM/Contacts.
11. Margin calculations + snapshots (daily job).
12. AI tools (suggest_allocations, summarize_financial_health, detect_payment_risk).
13. KSeF stubs (pola gotowe, integration v2).
14. Migracja z IntrApp.

## Otwarte decyzje

1. **Numeracja faktur** — szablon konfigurowalny per tenant? Rekomendacja: **tak, `invoice_number_format = "FV/{YYYY}/{MM}/{NNNN}"` w settings tenanta**.

2. **Walutowe** — wszystkie kwoty w PLN czy multi-currency? Rekomendacja: **multi-currency w schema, w UI default PLN z exchange_rate snapshot przy issue date**.

3. **Korekty** — pełne czy uproszczone? Rekomendacja: **pełne (osobna faktura linked do oryginału)**.

4. **Auto-marking paid** — integracja z bankiem (mt940/import wyciągów) czy ręczne? Rekomendacja: **MVP ręczne, v2 mt940 import + matching po numerze faktury**.

5. **Cost categories** — predefiniowane czy admin CRUD? Rekomendacja: **admin CRUD z seedowanymi kategoriami (SaaS/Hardware/Subcontractor/Office/Marketing)**.

6. **Recurring invoices** (fakturowanie cykliczne) — czy w MVP? Rekomendacja: **nie**. Plan kosztów cyklicznych istnieje w deal (interval_id) ale realnie generowanie cykliczne to v2 — wymaga workflow „wystaw fakturę co miesiąc".
