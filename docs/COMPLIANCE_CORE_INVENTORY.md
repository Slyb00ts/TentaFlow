# Inventory: Compliance Core

## Cel

Ten dokument zbiera obecne elementy TentaFlow związane z audytem, RODO/GDPR,
retencją, claimami prawnymi i UI zgodności. Inventory ma pokazać, co już
istnieje, czego brakuje i które elementy trzeba scalić w jeden spójny
`Compliance Core`.

## Podsumowanie

W repo istnieją trzy częściowe systemy:

| Obszar | Status | Wniosek |
|--------|--------|---------|
| `audit_log` | Działa jako techniczny dziennik audytu z hash-chain | Zostaje jako niski audit trail, ale nie wystarcza dla AI Act ani RODO |
| `legal_documents` | Działa jako generator dokumentów RODO/PDF | Zostaje jako Document Center, ale jest zbyt wąski i kamerowy |
| `policy_claims` | Działa jako claim engine dla DPIA/FRIA/consent/gates | Zostaje jako mechanizm decyzji/gate, ale nie jest rejestrem RODO |
| retencja | Rozproszona po organizacjach, sync policy, kamerach i cleanupach | Wymaga jednego retention engine |
| AI audit | Brak centralnego logu prompt/response/source/tool | Trzeba dodać od podstaw w core |
| DSAR / data subjects / breach / ROPA | Brak | Trzeba dodać od podstaw |

Najważniejsze ryzyko obecnego stanu: UI pokazuje „Dokumenty RODO”, ale to nie
oznacza pełnego systemu RODO. To jest tylko generator wybranych dokumentów.

## Istniejące Moduły Backend

### Audit

| Plik | Rola |
|------|------|
| `tentaflow-core/src/audit/mod.rs` | Typy audytu, `RiskClass`, buforowany `AuditLogger` |
| `tentaflow-core/src/audit/chain.rs` | Wyliczanie hash-chain dla `audit_log` |
| `tentaflow-core/src/audit/verify.rs` | Weryfikacja integralności chaina |
| `tentaflow-core/src/db/repository.rs` | `log_audit`, `list_audit_logs`, `count_audit_logs`, `cleanup_audit_logs` |
| `tentaflow-core/src/dispatch/handlers.rs` | Binary handlers: list/export/cleanup |
| `tentaflow-core/src/dispatch/audit_broadcast.rs` | Broadcast nowych eventów do UI |
| `tentaflow-cli/src/commands/audit.rs` | CLI verify chain |

Obecny schemat `audit_log` powstaje w migracjach i ma m.in.:

- `id`
- `timestamp`
- `user_id`
- `addon_id`
- `action`
- `resource`
- `details`
- `ip_address`
- `node_id`
- `instance_id`
- `resource_type`
- `resource_id`
- `result`
- `error_message`
- `severity`
- `risk_class`
- `related_claim_id`
- `request_id`
- `prev_hash`
- `hash`

Wniosek:

- `audit_log` jest dobrym technicznym dziennikiem zdarzeń.
- Nie powinien przechowywać pełnych promptów, odpowiedzi ani dużych payloadów.
- Powinien dostać referencje do przyszłych tabel compliance/AI audit.
- `cleanup_audit_logs` obecnie przyjmuje `keep_days >= 1`, co jest sprzeczne z
  wymaganiem minimum 6 miesięcy dla logów AI i części logów zgodnościowych.

### Legal Documents

| Plik | Rola |
|------|------|
| `tentaflow-core/src/db/legal_documents.rs` | Repozytorium tabeli `legal_documents` |
| `tentaflow-core/src/services/legal/mod.rs` | Moduł legal documents |
| `tentaflow-core/src/services/legal/types.rs` | `RodoVariant`: `short`, `standard`, `full` |
| `tentaflow-core/src/services/legal/rodo_generator.rs` | Generator PDF RODO |
| `tentaflow-core/src/services/legal/revoke.rs` | Soft revoke + audit |
| `tentaflow-core/src/services/legal/signed_url.rs` | HMAC signed URLs do pobrania PDF |
| `tentaflow-core/src/api/legal.rs` | HTTP endpoint `/legal/<doc_id>` |
| `tentaflow-core/src/dispatch/legal_admin.rs` | Binary RPC: list/generate/revoke |
| `tentaflow-core/templates/legal/*.hbs` | Szablony dokumentów RODO |

Tabela `legal_documents` ma:

- `id`
- `org_id`
- `variant`
- `generated_at`
- `generated_by_user_id`
- `content_hash`
- `pdf_path`
- `signed_url_ref`
- `revoked_at`

Wniosek:

- Mechanizm jest sensownie izolowany per `org_id`.
- Dokumenty mają content hash i soft revoke.
- Generowanie emituje `legal.generate`, revoke emituje `legal.revoke`.
- Obecny generator ma domyślne kategorie i retencję mocno pod monitoring/kamery.
- To nie jest pełny rejestr RODO, tylko generator dokumentów.

### Policy Claims / Gates

| Plik | Rola |
|------|------|
| `tentaflow-core/src/services/policy/mod.rs` | Moduł policy claims |
| `tentaflow-core/src/services/policy/repo.rs` | CRUD `policy_claims` i podpisów |
| `tentaflow-core/src/services/policy/engine.rs` | Weryfikacja claimów |
| `tentaflow-core/src/addon/host_functions/gate.rs` | Addon host function `gate_check_v1` |
| `tentaflow-cli/src/commands/policy.rs` | CLI issue/list/show/verify/revoke |

Migracja `policy_claims` deklaruje claimy typu:

- `dpia`
- `fria`
- `legal_grant`
- `consent`
- `approval`
- `grant`
- `deployment_profile`

Wniosek:

- To jest użyteczny mechanizm do gatingu operacji i claimów.
- Nie jest to pełny consent ledger ani DPIA register.
- Trzeba go wpiąć w Compliance Core jako mechanizm decyzji, nie jako główny
  model RODO.

### Organizacje I Retencja

| Plik | Rola |
|------|------|
| `tentaflow-core/src/services/org/repo.rs` | CRUD organizacji, w tym `retention_policy_json` |
| `tentaflow-core/src/db/migrations.rs` | `organizations.retention_policy_json` |
| `tentaflow-core/src/db/repository.rs` | `sync_policies.retention_days` |
| `tentaflow-core/src/sync/core_materializer.rs` | Materializacja org/sync policy z ledgera |

Obecne miejsca retencji:

- `organizations.retention_policy_json`
- `sync_policies.retention_days`
- kamera/nagrania w generatorze RODO
- `audit_log_cleanup(keep_days)`
- dokumentacja sync ledger z osobnymi założeniami retencji

Wniosek:

- Retencja jest rozproszona.
- Brakuje centralnej tabeli i centralnej decyzji: co można usunąć, kiedy,
  dlaczego i czy jest legal hold.

## Istniejące UI

### Audit UI

| Plik | Rola |
|------|------|
| `tentaflow-core/www/js/modules/audit.js` | Ekran dziennika audytu |
| `tentaflow-core/www/i18n/pl.json` | Tłumaczenia `audit.*` |

Funkcje:

- lista audytu;
- filtry;
- polling;
- server-push `AuditEvent`;
- export CSV;
- cleanup starych logów.

Problemy:

- `severity` jest wyliczane w UI z nazwy akcji, a nie z pełnych danych DB.
- cleanup jest dostępny bez centralnej polityki retencji.
- ekran nie pokazuje integralności chaina, retention class ani compliance scope.

### Legal UI

| Plik | Rola |
|------|------|
| `tentaflow-core/www/js/modules/legal/index.js` | Ekran „Dokumenty RODO” |
| `tentaflow-core/www/i18n/pl.json` | Menu `legal: Dokumenty RODO` |

Funkcje:

- lista dokumentów;
- generowanie wariantu `short`, `standard`, `full`;
- soft revoke;
- pobranie przez signed URL.

Problemy:

- UI gating używa roli `admin/dpo`, bo `authMe` nie zwraca permission listy.
- signed URL jest cache’owany tylko w sesji przeglądarki.
- ekran sugeruje „RODO”, ale pokrywa tylko dokumenty PDF.
- brak powiązania z data inventory, DSAR, DPIA, consent, breach i AI audit.

## Istniejący Protokół

| Plik | Rola |
|------|------|
| `tentaflow-protocol/src/message_body.rs` | Typy `AuditLog*` |
| `tentaflow-protocol/src/legal.rs` | Typy `LegalAdminPayload` |
| `tentaflow-protocol/src/lib.rs` | Eksport typów |
| `tentaflow-core/www/js/protocol/codec.js` | Klient JS dla binary protocol |

Istniejące requesty:

- `AuditLogListRequest`
- `AuditLogExportRequest`
- `AuditLogCleanupRequest`
- `LegalDocumentsListRequest`
- `LegalDocumentGenerateRequest`
- `LegalDocumentRevokeRequest`

Wniosek:

- Jest binary protocol dla audytu i dokumentów.
- Nie ma protokołu dla Compliance Core jako całości.
- Nie ma requestów dla AI audit, ROPA, DSAR, DPIA, breach, consent ledger ani
  retention policy.

## Istniejące Testy

| Plik | Zakres |
|------|--------|
| `tentaflow-core/tests/security_audit_chain.rs` | Integralność chaina audytu |
| `tentaflow-core/tests/audit_user_attribution.rs` | Przypisanie audytu do usera |
| `tentaflow-core/tests/profiling_phase1_audit.rs` | Audyt profilowania |
| testy w `services/legal/*` | Legal document generation/revoke/signed URL |
| testy w `services/policy/*` | Policy claims i gate verification |
| `tentaflow-cli/tests/cli_policy.rs` | CLI claimów |

Wniosek:

- Pokrycie istniejących elementów jest częściowe.
- Nie ma testów pełnego compliance workflow.
- Nie ma multi-node testów retencji/compliance.
- Nie ma testów AI audit capture.

## Braki Funkcjonalne

### Brak AI Audit

Nie znaleziono centralnego systemu zapisującego:

- pełny prompt;
- pełną odpowiedź;
- model/backend/node;
- flow/addon;
- tool calls;
- źródła RAG;
- klasyfikację danych;
- decyzję PII;
- retention class.

Istniejące ścieżki LLM/flow/router mają usage i routing, ale nie tworzą
kompletnego compliance eventu.

### Brak Data Inventory / ROPA

Nie ma tabel ani UI dla:

- kategorii danych;
- celów przetwarzania;
- podstaw prawnych;
- odbiorców;
- okresów retencji;
- transferów poza EOG;
- kategorii osób, których dane dotyczą.

### Brak DSAR

Nie ma systemu:

- eksportu danych osoby;
- usunięcia;
- sprostowania;
- ograniczenia;
- sprzeciwu;
- przenoszenia.

### Brak Consent Ledger

`policy_claims` potrafi przechowywać claim typu `consent`, ale nie zastępuje
pełnego ledgeru zgód. Brakuje:

- wersji zgody;
- scope zgody;
- sposobu zebrania;
- historii wycofania;
- powiązania z data subject.

### Brak DPIA / Breach Register

`policy_claims` potrafi reprezentować claim typu `dpia` lub `fria`, ale nie ma:

- rejestru DPIA;
- statusów;
- oceny ryzyka;
- właściciela;
- decyzji DPO;
- rejestru naruszeń;
- terminów i statusu zgłoszenia.

## Proponowana Klasyfikacja Istniejących Elementów

| Element | Decyzja |
|---------|---------|
| `audit_log` | Zachować, nie rozszerzać do dużych payloadów; dodać referencje do compliance eventów |
| `audit/chain.rs` i `audit/verify.rs` | Zachować jako integralność audit trail |
| `AuditLog*` protocol | Zachować, docelowo przenieść pod sekcję Compliance UI |
| `legal_documents` | Zmigrować logicznie do Document Center |
| `services/legal/*` | Zachować, ale zasilać z data inventory zamiast sztywnych defaultów |
| `policy_claims` | Zachować jako claim/gate engine |
| `gate_check_v1` | Zachować jako enforcement hook dla addonów |
| `organizations.retention_policy_json` | Zastąpić centralnymi retention policies |
| `sync_policies.retention_days` | Powiązać z centralnymi retention policies |
| `audit_log_cleanup` | Przerobić przez retention engine |
| menu `Audit` i `Dokumenty RODO` | Scalić w jedną sekcję `Compliance` |

## Docelowy Podział Modułów

```text
tentaflow-core/src/compliance/
├── mod.rs
├── ai_audit.rs
├── data_inventory.rs
├── legal_basis.rs
├── retention.rs
├── dsar.rs
├── consent.rs
├── dpia.rs
├── breach.rs
├── documents.rs
└── policy_decision.rs
```

Docelowe UI:

```text
Compliance
├── Overview
├── AI Audit
├── Audit Trail
├── Data Inventory
├── Processing Activities
├── Legal Basis
├── Consents
├── DSAR
├── DPIA / FRIA
├── Breaches
├── Retention
└── Documents
```

## Następny Krok Implementacyjny

Pierwszy commit implementacyjny powinien dodać sam model Compliance Core bez
przepinania całej aplikacji:

1. migracje tabel compliance z wielojęzycznymi polami `*_translations`;
2. repozytoria Rust;
3. podstawowe typy protokołu CBOR;
4. minimalne handlery admin read-only;
5. testy migracji, tłumaczeń seedów i repozytoriów.

Dopiero drugi etap powinien przepinać AI gateway, audit cleanup i UI. Dzięki
temu nie mieszamy zmian schematu, routingu AI i przebudowy widoków w jednym
dużym commicie.
