# Plan: Compliance Core

## Podsumowanie

Compliance Core ma scalić obecny `audit_log`, generator dokumentów RODO,
policy claims, retencję, AI audit i przyszłe procesy DSAR/DPIA/breach w jeden
spójny system core. Addony nie decydują, co jest logowane ani jak działa RODO:
każdy dostęp do danych, każde wywołanie AI i każda operacja wysokiego ryzyka
przechodzi przez TentaFlow Core.

Dokument bazuje na `docs/COMPLIANCE_CORE_INVENTORY.md`.

## Wymagania

- Core wymusza logowanie i polityki zgodności, addon nie może ich wyłączyć.
- Wszystkie payloady protokołu pozostają binarne przez CBOR.
- `audit_log` zostaje technicznym audit trail z hash-chain.
- Prompt, odpowiedź, źródła RAG i tool calls trafiają do osobnych tabel AI audit.
- Minimum retencji dla AI audit i krytycznych logów zgodności wynosi 6 miesięcy.
- Retencję można tylko wydłużyć, nie skrócić poniżej minimum.
- Dokumenty RODO stają się częścią Document Center, a nie osobną wyspą.
- Dane compliance są objęte core sync zgodnie z permission engine i policy epoch.
- UI ma jedną sekcję `Compliance`, zamiast osobnych niespójnych ekranów.
- Brak fallbacków, równoległych starych ścieżek i „tymczasowych” modeli.
- Wszystkie teksty widoczne w UI są wielojęzyczne przez pola `*_translations`
  walidowane przez `json_valid`; seed musi mieć co najmniej `pl` i `en`.

## Poza Zakresem Pierwszego Etapu

- Automatyczna porada prawna.
- Publiczny blockchain / token / consensus.
- PostgreSQL backend.
- Pełne GUI dla każdego workflow w pierwszym commicie.
- Automatyczne wykrywanie wszystkich danych osobowych w dowolnym pliku.

## Architektura

```text
Addon / GUI / API / Flow / Mesh
        |
        v
Core Gateways
  |-- Data Access Gateway
  |-- AI Gateway
  |-- Compliance Policy Engine
        |
        v
Compliance Core
  |-- Audit Trail references
  |-- AI Audit
  |-- Data Inventory / ROPA
  |-- Legal Basis
  |-- Consent Ledger
  |-- DSAR
  |-- DPIA / FRIA
  |-- Breach Register
  |-- Retention Engine
  |-- Document Center
        |
        v
SQLite materialized state + Sync Ledger + Blob/KV/File storage
```

## Komponenty

| Komponent | Odpowiedzialność |
|-----------|------------------|
| `compliance::audit_ref` | Łączy `audit_log` z konkretnym compliance eventem bez wkładania dużych payloadów do `audit_log` |
| `compliance::ai_audit` | Rejestr promptów, odpowiedzi, modelu, źródeł, tool calls i wyniku AI |
| `compliance::data_inventory` | Kategorie danych, czynności przetwarzania, odbiorcy, transfery, pola osobowe |
| `compliance::legal_basis` | Podstawy prawne per organizacja, kategoria danych i cel |
| `compliance::consent` | Historia zgód, wersje, zakres, wycofanie |
| `compliance::dsar` | Żądania eksportu/usunięcia/sprostowania/ograniczenia/przenoszenia |
| `compliance::dpia` | Rejestr DPIA/FRIA, statusy, właściciele, decyzje |
| `compliance::breach` | Rejestr incydentów i naruszeń |
| `compliance::retention` | Jedna decyzja kiedy dane mogą być usunięte lub zanonimizowane |
| `compliance::documents` | Warstwa nad obecnym `legal_documents` i przyszłymi dokumentami |
| `CompliancePolicyEngine` | Decyzja allow/deny/log/redact/retain/legal_hold |
| `DataAccessGateway` | Centralny punkt opisu i audytu operacji na danych core/addonów |
| `AiGateway` | Centralny punkt zapisu AI audit dla chat, flow, addonów i tool calling |

## Model Danych

### Tabele Podstawowe

| Tabela | Cel |
|--------|-----|
| `compliance_data_categories` | Słownik kategorii danych i klasyfikacji ryzyka |
| `compliance_processing_activities` | ROPA: czynności przetwarzania |
| `compliance_activity_categories` | Relacja czynność -> kategorie danych |
| `compliance_legal_basis` | Podstawa prawna dla celu i kategorii danych |
| `compliance_retention_policies` | Centralne polityki retencji |
| `compliance_legal_holds` | Blokady usunięcia danych |
| `compliance_documents` | Docelowy rejestr dokumentów, nadbudowa nad `legal_documents` |

### AI Audit

| Tabela | Cel |
|--------|-----|
| `compliance_ai_events` | Jedno wywołanie AI albo streaming session |
| `compliance_ai_payloads` | Prompt/response jako payload z hashem i opcjonalną redakcją |
| `compliance_ai_sources` | Źródła RAG, pliki, wektory, fragmenty kontekstu |
| `compliance_ai_tool_calls` | Tool calling, wejście, wynik, status, powiązany addon |
| `compliance_ai_policy_decisions` | Decyzje PII, legal basis, blokady, redakcje |

Minimalne pola `compliance_ai_events`:

- `event_id`
- `org_id`
- `user_id`
- `node_id`
- `addon_id`
- `flow_id`
- `request_id`
- `model_id`
- `backend`
- `started_at`
- `finished_at`
- `status`
- `risk_class`
- `legal_basis_id`
- `retention_policy_id`
- `prompt_hash`
- `response_hash`
- `audit_log_id`

### DSAR / Consent / DPIA / Breach

| Tabela | Cel |
|--------|-----|
| `compliance_data_subjects` | Osoby, których dane dotyczą |
| `compliance_data_subject_links` | Link subject -> user/contact/person/resource |
| `compliance_dsar_requests` | Żądania osoby i statusy |
| `compliance_dsar_exports` | Artefakty eksportu i hash |
| `compliance_consent_records` | Zgody, zakres, wersja, wycofanie |
| `compliance_dpia_records` | Rejestr DPIA/FRIA |
| `compliance_breach_incidents` | Rejestr naruszeń |
| `compliance_processors` | Procesorzy/podprocesorzy i transfery |

## Relacja Z Istniejącymi Elementami

| Istniejący element | Decyzja |
|--------------------|---------|
| `audit_log` | Zostaje, dostaje referencje do compliance eventów |
| `audit_log.prev_hash/hash` | Zostaje jako integralność audit trail |
| `AuditLog*` protocol | Zostaje, ekran trafia pod `Compliance -> Audit Trail` |
| `legal_documents` | Migrowane logicznie do Document Center |
| `services/legal/*` | Zostaje, źródła dokumentów mają pochodzić z Data Inventory |
| `policy_claims` | Zostaje jako claim/gate engine |
| `gate_check_v1` | Zostaje jako addon enforcement hook |
| `organizations.retention_policy_json` | Zastępowane centralnymi retention policies |
| `sync_policies.retention_days` | Powiązane z retention policy, nie jako osobna prawda |
| `audit_log_cleanup` | Zastąpione decyzją Retention Engine |

## Przepływy

### Chat / API

```text
/v1/chat/completions
  -> Router
  -> AiGateway.start_event
  -> Flow albo backend LLM
  -> AiGateway.record_sources/tool_calls
  -> AiGateway.finish_event
  -> audit_log row z referencją do ai_event_id
```

### Flow Engine

```text
trigger -> ... -> llm node
  -> LlmNodeAdapter buduje messages
  -> AiGateway rejestruje event z flow_id/node_id
  -> response wraca do envelope
  -> policy decision i usage zapisane w AI audit
```

### Addon LLM

```text
llm_generate / llm_generate_stream_start
  -> host function permission check
  -> AiGateway z addon_id i instance_id
  -> Router
  -> AI audit + audit_log reference
```

### RAG / Vector

```text
vector_search_v1 / RAG connector
  -> policy/gate check
  -> source fragments zapisane jako compliance_ai_sources
  -> downstream LLM event wskazuje użyte sources
```

### DSAR Export

```text
Admin/DPO tworzy DSAR
  -> Compliance Core buduje plan danych
  -> core repositories eksportują swoje dane
  -> addony dostają obowiązkowe query przez Core Data Access Gateway
  -> wynik trafia do signed artifact + audit trail
```

## Zadania

### Faza 1: Fundament Danych

| ID | Zadanie | Złożoność | Zależności | Agent | Status |
|----|---------|-----------|------------|-------|--------|
| C1.1 | Dodać migracje tabel Compliance Core | L | - | programista-bazy-danych | DONE |
| C1.2 | Dodać `tentaflow-core/src/compliance/` z typami domenowymi | M | C1.1 | programista-rust | DONE |
| C1.3 | Dodać repozytoria dla data inventory, legal basis, retention, AI events | L | C1.1, C1.2 | programista-rust | PARTIAL: kategorie, retencja, AI events |
| C1.4 | Dodać seed minimalnych kategorii danych i domyślnych polityk retencji | M | C1.3 | programista-rust | DONE |
| C1.5 | Dodać testy migracji i repozytoriów | M | C1.3, C1.4 | tester-jednostkowy | PARTIAL |

### Faza 2: Retention Engine

| ID | Zadanie | Złożoność | Zależności | Agent | Status |
|----|---------|-----------|------------|-------|--------|
| C2.1 | Dodać `compliance::retention::resolve_policy` | M | C1.3 | programista-rust | DONE dla AI audit |
| C2.2 | Zablokować cleanup poniżej minimum 6 miesięcy dla AI/compliance | M | C2.1 | programista-rust | TODO |
| C2.3 | Przepiąć `audit_log_cleanup` przez Retention Engine | M | C2.1 | programista-rust | TODO |
| C2.4 | Dodać legal hold i testy blokady usunięcia | M | C2.1 | tester-jednostkowy | TODO |
| C2.5 | Powiązać `sync_policies.retention_days` z centralną polityką | L | C2.1 | programista-rust | TODO |

### Faza 3: AI Audit Gateway

| ID | Zadanie | Złożoność | Zależności | Agent | Status |
|----|---------|-----------|------------|-------|--------|
| C3.1 | Dodać `AiGateway` start/finish/fail event | L | C1.3 | programista-rust | DONE |
| C3.2 | Wpiąć `routing/chat.rs` blocking chat | M | C3.1 | programista-rust | DONE |
| C3.3 | Wpiąć `routing/streaming.rs` streaming chat | L | C3.1 | programista-rust | DONE |
| C3.4 | Wpiąć `flow_engine/node_adapters/llm.rs` | L | C3.1 | programista-rust | TODO |
| C3.5 | Wpiąć addonowe `llm_generate` i streaming start | M | C3.1 | programista-rust | PARTIAL: `llm_generate` |
| C3.6 | Wpiąć `vector_search_v1` jako źródła RAG | L | C3.1 | programista-rust | TODO |
| C3.7 | Dodać testy prompt/response/source/tool capture | L | C3.2-C3.6 | tester-jednostkowy | PARTIAL: prompt/response/tool |

### Faza 4: Data Inventory I ROPA

| ID | Zadanie | Złożoność | Zależności | Agent | Status |
|----|---------|-----------|------------|-------|--------|
| C4.1 | Zdefiniować core data categories dla users/org/flows/settings/secrets/addons | M | C1.3 | planer | PARTIAL: seed core/addon |
| C4.2 | Dodać rejestr processing activities | M | C1.3 | programista-rust | PARTIAL: seed podstawowy |
| C4.3 | Dodać legal basis resolver per activity/category | M | C4.2 | programista-rust | PARTIAL: AI legal basis |
| C4.4 | Dodać admin read/write handlers dla inventory i activities | L | C4.2, C4.3 | programista-rust | PARTIAL: read-only kategorie |
| C4.5 | Dodać testy walidacji brakującej podstawy prawnej | M | C4.3 | tester-jednostkowy | TODO |

### Faza 5: Consent, DPIA, Breach, DSAR

| ID | Zadanie | Złożoność | Zależności | Agent | Status |
|----|---------|-----------|------------|-------|--------|
| C5.1 | Dodać Consent Ledger jako osobny model od `policy_claims` | L | C1.3 | programista-rust | TODO |
| C5.2 | Dodać DPIA/FRIA register i powiązanie z `policy_claims` | L | C1.3 | programista-rust | TODO |
| C5.3 | Dodać Breach Register | M | C1.3 | programista-rust | TODO |
| C5.4 | Dodać DSAR requests i export plan | XL | C4.2 | programista-rust | TODO |
| C5.5 | Dodać DSAR export dla core tables | L | C5.4 | programista-rust | TODO |
| C5.6 | Dodać obowiązkowy addon DSAR interface przez Core Storage API | XL | C5.4 | programista-rust | TODO |
| C5.7 | Testy DSAR export/delete/restrict/legal hold | XL | C5.4-C5.6 | tester-jednostkowy | TODO |

### Faza 6: Document Center

| ID | Zadanie | Złożoność | Zależności | Agent | Status |
|----|---------|-----------|------------|-------|--------|
| C6.1 | Dodać `compliance_documents` jako docelowy rejestr dokumentów | M | C1.1 | programista-bazy-danych | TODO |
| C6.2 | Przenieść logikę `legal_documents` pod `compliance::documents` | L | C6.1 | programista-rust | TODO |
| C6.3 | Zasilać szablony RODO z Data Inventory i ROPA | L | C4.2, C6.2 | programista-rust | TODO |
| C6.4 | Dodać dokumenty AI audit/retention/processing summary | M | C6.3 | programista-rust | TODO |
| C6.5 | Testy generowania i revoke dokumentów po migracji | M | C6.2-C6.4 | tester-jednostkowy | TODO |

### Faza 7: Protokół CBOR I Handlery

| ID | Zadanie | Złożoność | Zależności | Agent | Status |
|----|---------|-----------|------------|-------|--------|
| C7.1 | Dodać `tentaflow-protocol/src/compliance.rs` | M | C1.3 | programista-rust | DONE |
| C7.2 | Dodać `ComplianceAdminPayload` jako inner enum | M | C7.1 | programista-rust | DONE |
| C7.3 | Dodać handlery admin read-only dla overview/inventory/AI audit | L | C7.2 | programista-rust | PARTIAL: kategorie, retencje, AI events |
| C7.4 | Dodać handlery mutujące dla legal basis/retention/DSAR/DPIA | XL | C7.3, C4-C5 | programista-rust | TODO |
| C7.5 | Dodać testy CBOR round-trip | M | C7.2 | tester-jednostkowy | DONE |

### Faza 8: UI

| ID | Zadanie | Złożoność | Zależności | Agent | Status |
|----|---------|-----------|------------|-------|--------|
| C8.1 | Dodać menu `Compliance` i przenieść `Audit`/`Dokumenty RODO` jako tabs | M | C7.3 | programista-frontend | TODO |
| C8.2 | Dodać Overview z alertami retencji, AI audit i dokumentów | M | C7.3 | programista-frontend | TODO |
| C8.3 | Dodać AI Audit list/detail | L | C7.3 | programista-frontend | TODO |
| C8.4 | Dodać Data Inventory/ROPA/Legal Basis UI | XL | C7.4 | programista-frontend | TODO |
| C8.5 | Dodać DSAR/DPIA/Breach UI | XL | C7.4 | programista-frontend | TODO |
| C8.6 | Usunąć stare osobne wejścia menu po migracji | S | C8.1-C8.5 | programista-frontend | TODO |
| C8.7 | E2E Playwright dla podstawowych workflow admina | L | C8.1-C8.5 | tester-e2e | TODO |

### Faza 9: Sync I Multi-node

| ID | Zadanie | Złożoność | Zależności | Agent | Status |
|----|---------|-----------|------------|-------|--------|
| C9.1 | Dodać compliance tables do Core Sync Registry | L | C1.1 | programista-rust | TODO |
| C9.2 | Zdefiniować partycje `core/org/{org_id}/compliance/*` | M | C9.1 | programista-rust | TODO |
| C9.3 | Dodać permission policy dla compliance danych | L | C9.1 | programista-rust | TODO |
| C9.4 | Test 4 nodów: AI audit i retention policy sync | XL | C9.1-C9.3 | tester-e2e | TODO |
| C9.5 | Test odmowy syncu dla noda bez uprawnień compliance | L | C9.3 | tester-e2e | TODO |

### Faza 10: Dokumentacja I Review

| ID | Zadanie | Złożoność | Zależności | Agent | Status |
|----|---------|-----------|------------|-------|--------|
| C10.1 | Zaktualizować Addon SDK docs: addon nie decyduje o compliance logging | M | C3-C5 | dokumentator | TODO |
| C10.2 | Zaktualizować CLAUDE.md o Compliance Core | S | C1-C9 | dokumentator | DONE |
| C10.3 | Dodać dokument operacyjny: retencja, DSAR, eksport, legal hold | M | C5-C8 | dokumentator | TODO |
| C10.4 | Code review security/OWASP/RODO risk | L | C1-C9 | code-reviewer | PARTIAL |

## Kolejność Wykonania

1. C1.1 -> C1.5
2. C2.1 -> C2.4
3. C3.1 -> C3.7
4. C4.1 -> C4.5
5. C7.1 -> C7.3
6. C8.1 -> C8.3
7. C5.1 -> C5.7
8. C6.1 -> C6.5
9. C7.4 -> C8.7
10. C9.1 -> C9.5
11. C10.1 -> C10.4

## Minimalny Pierwszy Commit

Pierwszy commit powinien zawierać tylko fundament:

- migracje tabel Compliance Core;
- moduł `tentaflow-core/src/compliance/`;
- repozytoria dla inventory, legal basis, retention i AI events;
- seed minimalnych kategorii danych;
- testy migracji i repozytoriów.

Nie powinien jeszcze przepinać chat, flow, UI ani sync. To ogranicza ryzyko i
daje stabilny model, do którego można wpinać kolejne warstwy.

## Kryteria Akceptacji

- `cargo test --lib` przechodzi dla nowych repozytoriów i migracji.
- Każde wywołanie AI przez chat/flow/addon tworzy `compliance_ai_events`.
- Prompt i odpowiedź mają hash, retencję i powiązanie z `audit_log`.
- RAG/vector źródła są zapisane jako `compliance_ai_sources`.
- Tool calls są zapisane jako `compliance_ai_tool_calls`.
- `audit_log_cleanup` nie może skasować danych chronionych minimum retencji.
- Legal hold blokuje usunięcie i anonimizację.
- `Documents` generuje RODO z Data Inventory, nie z kamerowych defaultów.
- UI ma jedną sekcję `Compliance`.
- Dane compliance synchronizują się zgodnie z permission engine.
- Node bez uprawnień nie otrzymuje compliance danych.
- DSAR export zawiera dane core, AI audit i deklarowane dane addonów.
- Wszystkie nowe payloady admin protokołu są CBOR.

## Test Matrix

| Test | Zakres |
|------|--------|
| `compliance_migrations_create_all_tables` | Schemat i indeksy |
| `retention_rejects_below_minimum_ai_audit` | Minimum 6 miesięcy |
| `ai_gateway_records_blocking_chat` | Chat blocking |
| `ai_gateway_records_streaming_chat` | Chat streaming |
| `ai_gateway_records_flow_llm_node` | Flow LLM |
| `ai_gateway_records_addon_llm` | Addon LLM host fn |
| `ai_gateway_records_vector_sources` | RAG/vector provenance |
| `audit_cleanup_uses_retention_engine` | Cleanup przez policy |
| `legal_hold_blocks_delete` | Legal hold |
| `dsar_export_contains_core_and_ai_events` | DSAR export |
| `consent_withdrawal_preserves_history` | Consent ledger |
| `dpia_record_links_policy_claim` | DPIA/claim relation |
| `compliance_cbor_round_trip` | Protocol CBOR |
| `multi_node_compliance_sync_authorized` | 4-node sync allow |
| `multi_node_compliance_sync_denied` | 4-node sync deny |
| `compliance_ui_admin_workflow` | E2E UI |

## Ryzyka

| Ryzyko | Prawdopodobieństwo | Wpływ | Mitygacja |
|--------|-------------------|-------|-----------|
| Logowanie pełnych promptów przechowa dane wrażliwe | Wysokie | Wysoki | Retencja, legal basis, redakcja, dostęp tylko admin/DPO |
| `audit_log` urośnie zbyt szybko | Średnie | Średni | Duże payloady poza `audit_log`, hash i referencje |
| DSAR dla addonów będzie niespójny | Wysokie | Wysoki | Obowiązkowy addon DSAR interface przez Core Storage API |
| Sync wyśle compliance dane na niewłaściwy node | Średnie | Wysoki | Permission Engine + policy epoch + testy 4 nodów |
| UI będzie sugerować pełną zgodność zanim backend ją ma | Średnie | Wysoki | Najpierw backend, potem UI z realnymi statusami |
| Stare `legal_documents` zostaną równoległym systemem | Średnie | Średni | Przenieść pod Document Center i usunąć osobny ekran |

## Decyzje Otwarte

1. Czy prompt/response przechowywać zawsze w całości, czy dopuszczamy redakcję
   z zachowaniem hasha oryginału?
2. Czy AI audit ma mieć osobną politykę syncu domyślnie `replicated_by_permission`
   czy `authority_write` dla organizacji enterprise?
3. Czy DSAR export addonów ma być wymagany od każdego addonu od razu, czy tylko
   od addonów deklarujących dane osobowe?
4. Czy `policy_claims` ma zostać jako osobny moduł `services::policy`, czy
   docelowo przenieść namespace pod `compliance::policy_claims`?
