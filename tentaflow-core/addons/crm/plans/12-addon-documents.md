# 12 · Addon — Documents

**Mockup:** [D1 Documents](../../../../../.gstack/projects/Slyb00ts-TentaFlow/designs/crm-v1/d01-documents.html)

## Cel

Pliki + metadane + AI auto-tagging + **renderer PDF** dla wewnętrznych szablonów (głównie karty akceptacji budżetu — patrz CRM plan).

**NIE jest** to:
- Billing (faktury → osobny addon, [13](./13-addon-billing.md))
- Komunikator / chat — to nie miejsce na inline conversations

Documents = single source of truth dla plików: oferty PDF, harmonogramy XLS, NDA, karty akceptacji, transkrypcje, załączniki z maili.

## Domeny i encje

### `files`

| Pole | Typ | Opis |
|---|---|---|
| `id` | UUID PK | |
| `name` | TEXT | Original filename |
| `display_name` | TEXT | Edytowalna nazwa w UI |
| `mime_type` | TEXT | |
| `size_bytes` | BIGINT | |
| `storage_key` | TEXT | Klucz do object storage (S3 / TentaFlow file storage) |
| `storage_backend` | TEXT | `local` / `s3` / `azure_blob` |
| `hash_sha256` | TEXT | Do dedup (jeden plik = jeden storage record nawet jeśli 5 zasobów go używa) |
| `uploaded_by` | UUID FK | User |
| `uploaded_at` | TIMESTAMP | |
| `is_active` | BOOL | Soft delete |
| `is_signed` | BOOL | Czy podpisany cyfrowo |

### `file_links` (M:N — plik ↔ wiele zasobów)

| Pole | Typ | Opis |
|---|---|---|
| `file_id` | UUID FK | |
| `linked_resource` | JSONB | `{type, id}` (np. deal/contact/company) |
| `relation` | TEXT | „oferta", „nda", „karta_akceptacji", „transkrypcja_spotkania", „załącznik_maila" |
| `linked_at`, `linked_by` | | |

Plik może być powiązany z wieloma zasobami (np. ta sama oferta podpięta i do dealu i do firmy).

### `file_tags`

`file_id`, `tag` (np. `signed`, `tracking_enabled`, `confidential`, `template`)

### `file_metadata` (AI-extracted + manual)

| Pole | Typ | Opis |
|---|---|---|
| `file_id` | UUID FK | |
| `key` | TEXT | Np. `document_type`, `value_pln`, `client_name`, `expiry_date` |
| `value` | TEXT | |
| `source` | ENUM | `manual | ai_extract | ocr | template_field` |
| `confidence` | DECIMAL | dla AI |

### `templates` (szablony PDF / dokumentów do generowania)

| Pole | Typ | Opis |
|---|---|---|
| `id` | UUID PK | |
| `name` | TEXT | „Karta akceptacji budżetu v3" |
| `slug` | TEXT | `acceptance_card_v3` |
| `engine` | ENUM | `typst | html_to_pdf | markdown` |
| `source` | TEXT | Treść szablonu |
| `field_schema` | JSONB | Pola wymagane: deal_id, client_name, budget_pln, items[], approver_name |
| `created_at`, `updated_at` | | |

### `file_access_log` (RODO — kto kiedy otwierał wrażliwe pliki)

| Pole | Typ | Opis |
|---|---|---|
| `file_id` | UUID FK | |
| `user_id` | UUID FK | |
| `action` | ENUM | `viewed | downloaded | shared` |
| `accessed_at` | TIMESTAMP | |
| `ip_address` | TEXT | |
| `user_agent` | TEXT | |

Aktywne dla plików z tagiem `confidential` lub `signed`.

## Lifecycle pliku

```
uploaded → AI auto-tagging (background job) → metadata extracted
  → linked to resources (manual or auto from email/meeting)
  → optional: signed (digital signature) → is_signed=true
  → optional: tracking enabled (email opens/views logged) → file_access_log
  → soft-deleted (90 day retention, then hard delete unless legal hold)
```

## UI surfaces

### D1 — Documents lista

**Sidebar:** foldery (logiczne, nie fizyczne — to filtry):
- Wszystkie
- Oferty (auto: files z metadata `document_type = offer`)
- Umowy
- Karty akceptacji (z `is_signed=true` + tag `acceptance_card`)
- NDA
- Transkrypcje
- Załączniki z maili

**Akcje w głównym widoku:**

1. **„Wgraj"** (top right) → multi-file upload (drag-drop też). Po uploadzie automatyczne AI auto-tagging (`documents.extract_metadata` tool).
2. **Klik wiersza** → modal podglądu (PDF inline viewer + sidebar z metadata i powiązanymi zasobami)
3. **Bulk select + akcje:** masowy tag, masowe linkowanie do deala (np. „te 5 plików dla VW Poznań"), masowy download as zip
4. **Sortowanie:** ostatnia aktywność, rozmiar, nazwa
5. **Filtry:** powiązany z (filter po `linked_resource`), z podpisem, typ dokumentu
6. **„AI · auto-organizacja"** (banner) → otwiera widok 38 plików bez metadata + proponuje typ/powiązanie dla każdego (1 click approve all / approve selected)

### Wewnątrz innych addonów (contributions)

**deal.detail.sidebar** → sekcja „Dokumenty" pokazuje pliki z `file_links` dla tego deala. Lista z miniatkami + akcje: open / download / unlink.

**contact.detail.sidebar** → analogicznie dla osoby.

**company.detail.sidebar** → dla firmy.

### Modal podglądu pliku

- PDF viewer (PDF.js)
- Metadata sidebar (key-value list, editable)
- Powiązane zasoby (z możliwością add/remove linków)
- Access log (kto kiedy otwierał)
- „Wyślij" (otwiera mail composer z prefilled attachment)

## Renderer PDF (kluczowa funkcja dla CRM)

Wbudowany generator PDF z szablonów. Engine: **Typst** (rekomendacja — precyzyjny layout, czytelny syntax, mały overhead) jako default. Alternatywa: `html_to_pdf` (Chromium headless) — bardziej elastyczna ale ciężka.

**Workflow generowania:**

```
1. CRM woła documents.render_from_template(template_slug, field_values)
2. Documents wczytuje template (z `templates` table)
3. Renderuje z field_values → PDF binary
4. Zapisuje do `files` z linked_resource = deal_id
5. Zwraca file_id
6. CRM linkuje file do deal (jako relation="acceptance_card")
```

**Szablony preinstalowane (seed):**
- `acceptance_card_v3` — karta akceptacji budżetu (z C9 mockupu)
- `offer_template_basic` — szablon oferty (jeśli chcemy generator ofert)
- `schedule_template` — harmonogram wdrożenia
- `nda_template` — NDA do podpisu

**Field schema dla `acceptance_card_v3`:**
```json
{
  "deal_id": "uuid",
  "deal_code": "string",
  "deal_name": "string",
  "client_name": "string",
  "owner_name": "string",
  "section_name": "string",
  "value_pln": "decimal",
  "costs_total_pln": "decimal",
  "costs_breakdown": [
    {"category": "string", "value_pln": "decimal", "count": "int"}
  ],
  "margin_pln": "decimal",
  "margin_pct": "decimal",
  "approver_name": "string",
  "valid_days": "int"
}
```

## Provided contracts

**Resources:** `file`, `template`

**Queries:**
- `documents.list(filter)`
- `documents.get_file(id)` — z signed-URL do download
- `documents.list_for_resource(linked_resource)` — wszystkie pliki dla deala/contact/company
- `documents.search(query)` — fuzzy po nazwie + full-text po OCR'owanej treści

**Actions:**
- `documents.upload(file_bytes, name, mime_type, linked_resources?)`
- `documents.link_to_resource(file_id, linked_resource, relation)`
- `documents.unlink(file_id, linked_resource)`
- `documents.update_metadata(file_id, metadata)`
- `documents.delete(file_id)` — soft
- `documents.render_from_template(template_slug, field_values) → file_id` — **kluczowa dla CRM**
- `documents.sign_file(file_id, signature_data)` — digital signature (PAdES standard)
- `documents.enable_tracking(file_id)` — włącza tracking otwierań

**Views:**
- contribution dla `*.detail.sidebar` → sekcja „Dokumenty"

**Events:**
- `documents.uploaded`
- `documents.linked / unlinked`
- `documents.viewed` (dla tracked)
- `documents.signed`
- `documents.rendered_from_template`

**AI Tools:**
- `documents.extract_metadata(file_id)` — read, AI extrakcja typu/dat/wartości z PDF
- `documents.suggest_links(file_id)` — read, AI proponuje powiązanie do deal/contact (np. „ten PDF wygląda jak oferta dla mBank")
- `documents.summarize(file_id)` — read, streszczenie zawartości
- `documents.find_similar(file_id)` — read, znajduje podobne pliki (dedup)
- `documents.compare(file_id_a, file_id_b)` — read, porównanie wersji

## Consumed contracts

```toml
[needs.platform]
permissions = ["can"]
ai_broker = ["request_call"]
[needs.contacts]
get_person = []
get_company = []
[needs.activity]
publish_event = []  # documents publikuje też do timeline (np. „dodano plik X")
```

## Permissions

- **Read pliku** — visibility z linked_resource (jeśli user widzi deala, widzi też jego pliki)
- **Upload** — wszyscy zalogowani
- **Delete** — autor + admin
- **Sign** — `flag: can_sign_documents` (rzadko)
- **Confidential files** (tag) — wymaga explicit grant per user

## Migracja z IntrApp

IntrApp miał:
- `Files` (pliki PDF + bytea content)
- `ProjectAttributes.AcceptanceCardBytes` (PDF embed bytea — coś okropnego)
- `ProjectAttributes.AcceptanceCardId` → FK do Files

Mapowanie:
- `Files` → `files` (storage z bytea → externalized do object storage)
- `AcceptanceCardBytes` → upload jako oddzielny `file` z relation=`acceptance_card`, link do deal
- Pliki bez metadata → AI auto-tag po migracji (`documents.extract_metadata` na każdym)

## Implementation order

1. Schema: files, file_links, file_tags, file_metadata, templates, file_access_log.
2. Storage adapter (initially local filesystem, S3 jako opcja).
3. Host fn upload + get + list + link.
4. UI D1 (read-only lista + modal viewer).
5. UI D1 — upload (drag-drop) + linking + bulk actions.
6. Templates table + seed templates (Typst).
7. PDF renderer (Typst integration) — `documents.render_from_template`.
8. View contributions dla sidebars w CRM/Contacts/Billing.
9. AI tools (extract_metadata, suggest_links, summarize).
10. Tracking otwierań (email tracking — przez signed URLs).
11. Signed files (PAdES — opcj. v2).
12. Migracja z IntrApp.

## Otwarte decyzje

1. **Typst vs HTML→PDF** — Typst (mniejszy, szybszy, deterministyczny) vs Chromium (większy, bardziej elastyczny, można reuse'ować CSS z UI). Rekomendacja: **Typst dla 3 wbudowanych templatek (karta akceptacji etc), HTML→PDF opcjonalnie dla custom templatów stworzonych przez addony**.

2. **Storage backend domyślny** — local FS, S3, Azure? Rekomendacja: **local FS dla self-hosted MVP, S3 jako pluggable adapter v2**.

3. **Dedup po hash** — automatyczne (jeden storage_key dla wielu file records z tym samym hash)? Rekomendacja: **tak — oszczędność storage, ale czasem ten sam plik chce mieć dwie różne metadata** (np. oferta v1 dla mBank vs oferta v1 dla PKO, ten sam plik źródłowy). Rozwiązanie: storage dedupowany, ale `files` records osobne.

4. **Wersjonowanie plików** — czy oferta v1, v2, v3 to 3 osobne records czy jeden z historią? Rekomendacja: **3 osobne records z tagiem `version_chain_id`** wspólnym — można pokazać historię w UI.

5. **OCR dla scanów** — czy automatycznie OCR'ować PDFy z obrazem (skany kart akceptacji wracające z drukarki)? Rekomendacja: **tak, integration z `tesseract` lub external service**. Wynik trafia do `file_metadata` jako ocr_text dla full-text search.

6. **Rich preview vs prosty download** — czy embed PDF viewer (PDF.js) czy tylko download? Rekomendacja: **PDF viewer inline dla PDF/obrazów, download dla XLS/DOCX**.
