# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build & Run

No workspace Cargo.toml — each crate builds independently. The main binary is `tentaflow`.

```bash
# Build main binary (from tentaflow/)
cd tentaflow && cargo build

# Build core library (from tentaflow-core/)
cd tentaflow-core && cargo build

# Run
./tentaflow/target/release/tentaflow --config config.toml

# WASM addons require this target
rustup target add wasm32-wasip1

# Browser protocol glue (tentaflow-protocol-wasm) requires these two.
# Without them build.rs skips generating www/js/protocol/wasm_glue.{js,wasm}
# and the dashboard fails to load in the browser.
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.108 --locked

# Or one-shot: ./scripts/setup.sh (Linux + macOS)
```

Feature flags on `tentaflow-core`:

| Flag | Purpose |
|------|---------|
| `docker` | Docker management (bollard) |
| `inference-llamacpp` | llama.cpp backend |
| `inference-mlx` | Apple MLX (macOS only) |
| `dashboard-api` | Axum HTTP dashboard + API |

## Configuration

`config.toml` at project root. Key sections: `[server]`, `[server.mtls]`, `[protocols.quic]`, `[mesh]`, `[load_balancing]`, `[monitoring]`. Default HTTPS/QUIC port: 8090.

`[server.mtls]` (optional) — Service-to-Core mTLS pinning for `/core/frame/pickup`:

```toml
[server.mtls]
pickup_required = false              # default off (F1a/F1b compat)
client_cert_fingerprints = []        # SHA-256 hex of allowed client leaf certs
```

Production must flip `pickup_required = true` and list at least one fingerprint.

## Dashboard Settings

Zakładka `Settings -> Dostępy zewnętrzne` grupuje sekrety do usług
zewnętrznych: `hf_token` dla Hugging Face, `ngc_api_key` dla NVIDIA NGC oraz
listę rejestrów kontenerów. Sekrety są zapisywane przez binary protocol jako
settings z flagą `is_secret`; listing settings zwraca tylko marker
`<redacted>` dla niepustych sekretów. Recommender vLLM i generyczny
`EngineRecommendRequest` używają zapisanego `hf_token`, jeśli request z UI nie
podaje tokena jawnie, więc wizard może pobierać `config.json` z gated repo bez
utrwalania tokena w konfiguracji deploymentu.

## Bundled Addons

`tentaflow-core/build.rs` osadza bundled addony z `tentaflow-core/addons/` w
binarce core. Payload zawiera `addon.wasm`, `manifest.toml`, opcjonalne
dokumenty/bloki oraz wszystkie pliki `migrations/*.sql`. `bundle_hash` liczy
tez tresc migracji, wiec zmiana schematu SQL wymusza reconciliation bundled
addona nawet bez podbijania wersji manifestu.

Przy starcie `addon::bundled` zapisuje osadzone pliki do rozpakowanego katalogu
addona przed install/upgrade. Jesli zainstalowany addon jest juz aktualny, dalej
wywoluje `lifecycle::ensure_sql_storage`, wiec brakujace tabele per-addon SQLite
sa odtwarzane przez idempotentne migracje zamiast konczyc sie runtime warningami
`no such table`.

## Mesh Pairing

First-contact pairing nad iroh uzywa osobnego ALPN `tentaflow-pairing/v2`.
Request/response sa len-prefixed CBOR (`PairingFirstContactRequest`,
`PairingFirstContactResponse`) i handler weryfikuje, ze deklarowany
`sender_node_id` jest rowny iroh `remote_id`, a Ed25519 czesc
`sender_public_key_hex` zgadza sie z tym node id. Aktywne hinty transportowe
pending/trusted nie sa zapisywane jako JSON w `settings`; trafiaja do
`peer_persisted` i `peer_hints` ze stanem `TRUST_PENDING_PAIRING` albo
`TRUST_TRUSTED`. Legacy `settings.pending_contact:*` i
`settings.trusted_contact:*` sa tylko sciezka migracji/cleanup przy starcie.

## Sync Ledger Plan

Docelowa architektura synchronizacji zdecentralizowanych danych TentaFlow jest
opisana w `docs/SYNC_LEDGER_PLAN.md`. Plan zakłada SQLite jako bazę aktualnego
stanu addonów oraz wbudowany Fjall jako storage techniczny dla Sync Ledger,
outbox/inbox, ACK, cursorów, hash-chain, snapshotów i kompaktowania. Stary
mechanizm CRDT został usunięty z aktywnego runtime: moduły `mesh/crdt.rs`,
`mesh/crdt_store.rs` i `mesh/peer_manager.rs` nie są już częścią
`tentaflow-core`, `MESH_MSG_CRDT_DELTA` i warianty CRDT zostały usunięte z
`tentaflow-protocol`, a tabele `crdt_*` zostały usunięte z bazowego schematu
SQLite. Jedyną aktywną ścieżką synchronizacji danych w runtime jest
`sync/ledger`.
Wynik spike'a build matrix RocksDB jest w `docs/ROCKSDB_BUILD_MATRIX_SPIKE.md`:
Linux i Android (`aarch64`, `x86_64`) przechodzą, Android wymaga jawnego NDK
CC/CXX/AR/linkera, a iOS/macOS/Windows wymagają docelowych runnerów. Ten spike
zostaje historycznym punktem odniesienia, nie aktualnym wyborem domyślnym.
Porownanie KV jest w `docs/KV_STORE_BENCHMARK_REDB_FJALL.md`: `redb` ma mocny
profil read-heavy, `fjall` wygrywa update-heavy path i jest pure Rust, a
`rocksdb` ma bardzo mocne wyniki ogolne oraz najmniejszy rozmiar na dysku kosztem
C++ toolchaina. Decyzja docelowa: Sync Ledger implementuje `FjallSyncLedgerStore`;
`fjall` jest normalną zależnością `tentaflow-core`.
Sync Ledger ma byc crypto-ready, ale nie jest teraz projektem krypto: operacje
maja canonical encoding, stabilny hash, podpisy, hash-chain per partycja i
Merkle summaries, zeby w przyszlosci mozna bylo dodac Block Builder / Consensus
Layer bez zmiany API addonow, SQLite ani Core Storage API.
Pierwszy kod produkcyjny jest w `tentaflow-core/src/sync/ledger/`: typy
`NewSyncOperation` / `SyncOperation`, trait `SyncLedgerStore` oraz
`FjallSyncLedgerStore`. Implementacja zapisuje operation log, indeks po
`OperationId`, outbox, inbox, peer cursors, snapshot metadata i partition heads w
osobnych keyspace'ach Fjall. `append_operation` przydziela sekwencje per
partycja, buduje hash-chain przez `prev_partition_hash` i podpisuje operacje
dopiero po wyliczeniu canonical hash. `put_verified_in_inbox` zapisuje operacje
przychodzace tylko po walidacji integralnosci i podpisu. Modul walidacji
udostepnia signer/verifier Ed25519, verifier dla hex node id, sprawdzanie
hash-chain oraz Merkle summary dla ciaglych zakresow partycji.
`sync::runtime::init()` zawsze zwraca autorytatywna globalna instancje
`SyncRuntime` z `OnceLock`, wiec handlery dispatch i caller startowy nie moga
rozjechac sie na dwoch roznych runtime'ach po ponownej inicjalizacji.
Sekrety zewnetrznych integracji zapisane w `settings` sa synchronizowane tylko
przez jawna allowliste `core.shared_setting_secret` (`hf_token`, `ngc_api_key`).
Nadawca zapisuje lokalnie wartosc zaszyfrowana swoim `SettingsCipher`, ledger
niesie wartosc operacji, a odbiorca materializuje ja ponownie lokalnym szyfrem.
Identity Registry dla syncu jest w migracji `sync_identity_registry` i
repozytorium `db::repository`: `sync_nodes` przechowuje techniczna tozsamosc
node/device, `user_identity_keys` przechowuje kryptograficzne klucze usera, a
`node_user_assignments` mapuje userow do nodow. Node identity i user identity sa
rozdzielone: node podpisuje transport/sync delivery, user identity bedzie uzyta
do wlascicielstwa danych, approvali, audytu i pozniejszych operacji wysokiego
ryzyka.
Permission Engine foundation jest w migracji `sync_permission_engine` i
repozytorium `db::repository`: `sync_user_org_profiles` opisuje dzial i managera
usera, `sync_resource_acl` opisuje owner/assigned/department/manager dla zasobu,
a `sync_explicit_shares` trzyma jawne udostepnienia userowi albo nodowi. Funkcje
`can_user_access_sync_resource` i `can_node_receive_sync_resource` podejmuja
decyzje dla owner, assigned, department, manager_subtree, explicit share, admin
oraz authority node. Sync Policy foundation jest w migracji `sync_policy`:
`sync_policies` trzyma tryb synchronizacji per organizacja, addon, typ zasobu
albo konkretny zasob, a repozytorium udostepnia `upsert_sync_policy`,
`get_effective_sync_policy` i `list_sync_targets_for_resource`. Targety sa
wybierane wedlug trybu i filtrowane przez Permission Engine. Per-addon SQL
storage tworzy wewnetrzna tabele `__tentaflow_sync_captures` i zapisuje capture
kazdego DML w tej samej transakcji co zapis addonu. `sync::runtime` startuje z
nodem, appenduje capture do Fjall Sync Ledger, podpisuje operacje kluczem
MeshSecurity i wrzuca `OperationId` do outbox wedlug Sync Policy. Capture
przechowuje `operation_id`, a startup nodu odpala drainer dla zainstalowanych
addonow SQL i ponawia wpisy ze statusem `pending` albo `error`. Po migracji
UFP/2 sync wire idzie przez `channel=0x06 SyncLedger`; payloady `SyncPush`,
`SyncAck`, `SyncPull`, `SyncPullResponse` i snapshot nadal niosa binarne
CBOR/CBOR body potrzebne obecnemu runtime, a nie JSON. Po polaczeniu z
trusted peerem pipeline wysyla pending outbox, odbiorca zapisuje zweryfikowane
operacje do inbox i odsyla ACK.
Incoming operations z inbox sa aplikowane do lokalnego SQLite addona przez
`apply_replicated_write`, bez tworzenia nowego capture; replay jest
idempotentny przez `__tentaflow_sync_applied`, a inbox entry dostaje marker
`applied`. Addonowe KV `storage_set` i `storage_delete` zapisuje trwaly capture
`__tentaflow_kv_sync_captures` w tej samej transakcji SQLite co `addon_storage`;
operacje `addon.kv` ida do Sync Ledger jako binarne `FieldValue::Bytes`, a
odbiorca materializuje je do `addon_storage` bez JSON i bez dodatkowego capture
loop. `FileBlobStore` zapisuje trwaly capture `__tentaflow_blob_sync_captures`
po zapisie content-addressed pliku; runtime dzieli blob na operacje
`blob_store_chunks` po 1 MiB z binarnym `FieldValue::Bytes`, a `core.blob` jest
manifestem bez bajtow pliku. Odbiorca sortuje pending outbox wedlug
`partition_id + partition_sequence`, aplikuje chunki przed manifestem,
materializuje finalny plik strumieniowo pod
`<TENTAFLOW_HOME>/blobs/<sha-prefix>/<sha>.bin`, waliduje hash/rozmiar i sprzata
tymczasowe chunki spod `<TENTAFLOW_HOME>/sync/blob-chunks`. Startup runtime
odpala drainery pending/error dla addon SQL, core captures, KV captures i blob
captures przed apply inbox.
`sync::storage_monitor` liczy rozmiar SQLite, Fjall ledgera, snapshot blobow,
finalnych blobow i pending chunkow oraz wyznacza progi wolnego miejsca
20/10/5%. Przy stanie `critical` runtime blokuje nowe duze bloby powyzej 1 MiB,
zostawiajac male operacje SQL/KV/metadata bez sztucznej blokady.
Raport storage idzie przez binary protocol jako `SyncStorageBody` z handlerem
admin-only i bazowa zakladka `Settings -> Storage` pokazuje pressure, wolne/uzyte
miejsce, limit blokady duzych blobow oraz rozmiary sciezek storage. Conflict foundation
zapisuje nieaplikowalne incoming operations do
`__tentaflow_sync_conflicts`, oznacza inbox entry jako `conflicted` i przerywa
retry loop bez utraty operacji. Snapshot Manager (`sync::snapshot`) buduje i
zapisuje podpisane snapshoty partycji na podstawie ciaglego hash-chain, Merkle
root, state hash, policy epoch i ostatniego operation hash. Restore-plan
snapshotu pobiera podpisany checkpoint, weryfikuje podpis autora i zwraca
operacje po snapshocie z walidacja ciaglosci hash-chain. Materializowany restore
do SQLite addona odtwarza stan z lokalnej historii ledgera, najpierw walidujac
snapshot jako checkpoint prefiksu, a potem aplikujac operacje SQL w kolejnosci
bez capture loop. SQL snapshot package zapisuje podpisany hash i rozmiar bloba w
`SyncSnapshot`; blob niesie zweryfikowany prefiks operacji SQL i pozwala
odtworzyc SQLite po usunieciu prefiksu z glownych rekordow ledgera.
`SnapshotPackageStore` zapisuje te bloby content-addressed pod
`<TENTAFLOW_HOME>/sync/snapshot-blobs`, waliduje hash/rozmiar przy zapisie i
odczycie oraz umozliwia restore z utrwalonego bloba. Mesh Sync Protocol ma
snapshot package pull/response (`0x4B..0x4C`): responder wysyla podpisane
metadane snapshotu, blob SQL package i opcjonalny tail operacji, a receiver
weryfikuje podpis, zgodnosc metadanych oraz hash/rozmiar bloba przed zapisem.
`SyncPull` automatycznie przechodzi na `SyncSnapshotResponse`, gdy responder nie
moze juz wyslac ciaglego zakresu operacji po kompaktowaniu. Receiver odrzuca
pull response i snapshot tail z luka sekwencji albo z innej partycji oraz
odtwarza SQLite z przyjetego bloba przed utrwaleniem peer cursora snapshotu.
Mesh pipeline uruchamia aktywny scheduler repair: dla connected trusted peerow
ponawia niepotwierdzony outbox i wysyla `SyncPull` dla luk sekwencji wykrytych
przy odbiorze operacji, z exponential backoff per `peer+partition`. Repair queue
jest utrwalona w osobnym keyspace Fjall `repair_queue`; wpisy sa listowane po
terminie `next_attempt_ms`, retry aktualizuje backoff, a udany pull/snapshot
usuwa wpis. `CompactionManager` kompaktuje operacje tylko per partycja i tylko po
podpisanym snapshot package obecnym w `SnapshotPackageStore`; kompaktowanie
blokuje sie, gdy prefiks ma niepotwierdzony outbox. `sync::runtime` ma testy
runtime dla offline outbox -> reconnect push -> ACK, luki sekwencji -> repair
pull od brakujacego prefiksu oraz skompaktowanego prefiksu ->
`SyncSnapshotResponse`. `CompactionFinalityPolicy` obsluguje finality przez
`AllOutboxTargets`, `RequiredTargets` i `Quorum`; ledger zwraca statusy outbox
dla prefiksu partycji, a manager blokuje kompaktowanie przy brakujacych ACK albo
brakujacych wymaganych targetach. Conflict Manager ma backendowe API list/resolve
dla `__tentaflow_sync_conflicts`, strategie `keep_local`, `ignore` i
`accept_remote`; `accept_remote` transakcyjnie usuwa lokalny konflikt primary-key
dla INSERT, aplikuje remote write i oznacza inbox jako applied przez
`sync::runtime`. `keep_local` i `ignore` zamykaja konflikt bez nadpisywania
lokalnego stanu i terminalnie oznaczaja inbox jako handled, gdy odpowiadajacy
wpis inbox nadal istnieje. Handler konfliktow waliduje `org_id`, `addon_id`,
status i `operation_id`, a list/resolve sa rejestrowane jako Admin-only. GUI uzywa binary protocol przez
`MessageBody::SyncConflictBody(SyncConflictPayload::{ListRequest,ResolveRequest})`
i handler `sync_conflict_dispatch` z polityka `Admin`, bez endpointow REST.
Zakladka `Settings -> Sync` listuje konflikty i wykonuje `keep_local`, `ignore`,
`accept_remote` przez binary WS. Core Data Sync zaczyna sie od
`tentaflow-core/src/sync/core_registry.rs`: rejestr mapuje synchronizowane dane
platformowe (`flows`, `flow_versions`, `flow_model_bindings`, `user_accounts`,
legacy `users`, `user_groups`, `group_members`, `roles`, `org_memberships`,
`organizations`) na `resource_type` i partycje `core/org/{org_id}/...`.
Runtime tables (`flow_executions`, `flow_invocations`, `audit_log`) nie sa
domyslnie synchronizowane. Migracja `core_sync_captures` tworzy
`__tentaflow_core_sync_captures`, a `sync::core_capture` zapisuje binary
`changed_fields_blob` przez CBOR w tej samej transakcji co przyszly zapis
core. Repozytorium Flow Buildera zapisuje core capture dla `create_flow`,
`update_flow`, `delete_flow`, `update_flow_with_snapshot` i CRUD
`flow_model_bindings`; snapshot poprzedniej wersji flow tworzy
`core.flow_version`. Repozytoria identity/RBAC zapisuja capture dla
`user_accounts`, `user_groups`, `group_members`, `organizations`,
`org_memberships`, `sync_nodes`, `user_identity_keys`, `node_user_assignments`,
`sync_user_org_profiles`, `sync_policies`, `sync_resource_acl` i
`sync_explicit_shares`; zmiana hasla jest redagowana do
`password_changed=true`, bez synchronizacji hasha. `sync::runtime` zapisuje core capture jako binarny payload
CBOR z `addon_id=core`, partycjami `core/org/...` i domenowymi
`changed_fields`, bez addonowego `params_json`; `sync::core_capture` ma drainer
pending/error do ledgera. `sync::core_materializer` aplikuje incoming `core.*`
przez allowlist tabel i pol oraz parametryzowane zapytania; `apply_unapplied_inbox`
rozdziela operacje core od addonowych, bez wykonywania dowolnego SQL z payloadu.
Duplicate `INSERT` po primary key jest scalany field-level przez
`ON CONFLICT ... DO UPDATE` na allowlistowanych polach, a test
`core_push_materializes_flow_on_receiver` sprawdza przeplyw source runtime ->
outbox -> push -> receiver inbox -> materializer SQLite -> ACK dla `core.flow`.
Testy `core_outbox_targets_only_nodes_with_resource_access` i
`core_outbox_targets_org_admin_node_without_resource_acl` sprawdzaja selektywne
targetowanie `replicated_by_permission`: node przypisany do usera dostaje tylko
zasob z ACL, a node admina org dostaje core resource bez ACL zasobu.
Policy epoch dla permissions sync jest w `settings` jako
`sync.permission_epoch:{org_id}`. Epoka rosnie przy zmianach node
identity/assignment, roli/is_active usera, profilu org, ACL zasobu, explicit
shares, policies i org membership; `SyncOperation.policy_epoch` niesie aktualna
wartosc epoki. Pending outbox jest rewalidowany przez Permission Engine tuz przed
wyslaniem; jezeli target node stracil dostep po zakolejkowaniu operacji, wpis
outbox jest zamykany lokalnie i operacja nie jest wysylana. Incoming operations
sa aplikowane deterministycznie wedlug priorytetu blobow, `partition_id` i
`partition_sequence`, zeby wielooperacyjny repair response nie mogl wykonac
update przed insertem.
Test `repair_pull_response_materializes_missing_core_flow_operations` sprawdza
pelny repair path dla `core.flow`: odbiorca wykrywa luke sekwencji, kolejkuje
pull od brakujacego prefiksu, source odsyla operacje, receiver materializuje flow
i ACK zamyka outbox.
Test `compacted_prefix_is_served_as_snapshot_response` sprawdza pelny snapshot
response path: po kompaktowaniu source wysyla snapshot package + tail, receiver
waliduje i odtwarza SQLite z bloba, aplikuje tail, czysci repair queue oraz ACK
zamyka outbox.
Test `multi_node_mesh_sync_push_materializes_core_flow_and_acks` uruchamia dwa
prawdziwe `IrohMeshManager` na loopback, wysyla `SyncPush` i `SyncAck` przez
UFP/2 `SyncLedger`, materializuje `core.flow` na receiverze i potwierdza outbox
na source. Test `multi_node_mesh_repair_pull_materializes_missing_core_flow_operations`
wysyla przez UFP/2 niepelny `SyncPullResponse`, receiver wykrywa luke, wysyla
`SyncPull`, source odsyla pelny `SyncPullResponse`, receiver materializuje
insert+update i `SyncAck` zamyka outbox obu operacji. Test
`multi_node_mesh_snapshot_response_restores_compacted_sql_prefix` wysyla przez
UFP/2 `SyncPull` dla skompaktowanego prefiksu, source odsyla
`SyncSnapshotResponse` z SQL snapshot package i tail, receiver odtwarza SQLite,
czysci repair queue i ACK zamyka outbox prefiksu oraz tail. Test
`multi_node_mesh_permission_revoke_stops_future_core_flow_push` wysyla pierwszy
`core.flow` do receivera z dostepem, potem przepina ACL na innego usera/node i
potwierdza, ze kolejna aktualizacja nie buduje juz push payloadu dla starego
receivera. Test `multi_node_mesh_kv_push_materializes_storage_on_receiver`
wysyla `addon.kv` przez UFP/2 `SyncPush`, receiver materializuje
`addon_storage`, a ACK zamyka outbox. Test
`multi_node_mesh_chunked_blob_push_materializes_file_on_receiver` wysyla
wielochunkowy `core.blob` przez UFP/2, receiver sklada finalny plik
content-addressed, waliduje hash przez runtime i sprzata tymczasowe chunki.
Test `multi_node_mesh_four_node_fanout_syncs_core_flow_to_all_targets` uruchamia
cztery prawdziwe nody loopback, replikuje jeden `core.flow` z source do trzech
targetow przez `replicated_by_permission`, materializuje dane w trzech DB i
potwierdza ACK/outbox dla kazdego targetu.
Integracyjny test `process_four_node_sync_fanout_survives_restart`
(`tentaflow-core/tests/process_four_node_sync.rs`) uruchamia cztery osobne
procesy lokalnie. Kazdy proces ma wlasny `TENTAFLOW_HOME`, SQLite, Fjall ledger,
klucz noda i `IrohMeshManager`; parent laczy je przez stdin/stdout, wykonuje
trust/connect, replikuje `core.flow` do trzech receiverow, czeka na ACK z outbox
Fjall, restartuje wszystkie procesy z tymi samymi katalogami i potwierdza trwala
materializacje SQLite oraz ACK po restarcie. Uruchomienie:
`cargo test --manifest-path tentaflow-core/Cargo.toml --test process_four_node_sync --features dashboard-api process_four_node_sync_fanout_survives_restart -- --nocapture`.
Pelny plik `process_four_node_sync` ma 9 scenariuszy: fanout po restarcie,
offline catch-up, permission gating, central-only bez materializacji,
central-only read/write-through, core suite po pelnym restarcie source i trzech
receiverow, snapshot tail z ACL, konflikt oraz powtarzany fanout.
`mesh::pipeline` ma wydzielony jeden tick schedulera repair, uzywany przez
produkcyjna petle co 5 sekund oraz przez testy bez dublowania logiki. Test
`multi_node_mesh_repair_scheduler_recovers_gap_after_reconnect` wykrywa luke,
zapisuje repair queue, rozlacza i laczy nody ponownie, odpala produkcyjny tick
schedulera, wysyla `SyncPull`, materializuje brakujacy prefiks i czysci repair
queue. Test `multi_node_mesh_full_restart_persists_fanout_and_acks` pokrywa
pelny restart source + trzech receiverow: operacja fanout i pending outbox sa
tworzone przed restartem, po restarcie dostarczane przez mesh, a po kolejnym
restarcie SQLite receiverow i ACK outbox source dalej sa trwale. Test
`addon_conflict_survives_receiver_restart_and_accepts_remote` potwierdza, ze
konflikt addon SQL pozostaje otwarty po restarcie receivera, `accept_remote`
materializuje remote write i oznacza inbox jako applied.

## Transport architecture (2-tier)

TentaFlow runs two transport tiers and every change must respect this split:

### Tier 1: Binary primary (default)

WebTransport `/wt/api` + WebSocket `/ws/api` fallback, binary `MessageBody` protocol.
- Frontend ↔ Core: all admin UI, all data fetching
- Addons ↔ Core (via wasmtime): host functions ABI via addon-sdk wrappers
- Services in mesh ↔ Core: QUIC tunnel (mesh control plane)
- Sub-second response, low overhead, full request/response binary serialization

## Admin Scheduler

Scheduler administracyjny działa w `tentaflow-core/src/scheduler/` i trzyma
stan w SQLite (`scheduled_jobs`, `scheduled_runs`). Uruchamia wyłącznie funkcje
addonów przez `AddonManager::call_tool`, a dashboard komunikuje się z nim przez
binary protocol (`SchedulerBody(SchedulerPayload)`), nie przez REST.

Ekran admina jest w `www/js/modules/scheduler.js` i jest podpięty do menu tylko
dla administratorów. Obsługiwane tryby harmonogramu: `once` (RFC3339),
`interval` (`30m`, `1h`, `1d`) oraz prosty dzienny `cron` w formacie
`minute hour * * *`. Scheduler startuje raz procesowo z dashboard/unified server
i jest odporny na restart przez wyliczanie `next_run_at` z trwałej bazy.
Zapis joba waliduje, że wskazany addon jest zainstalowany, włączony i ma
deklarowane narzędzie w manifeście. UI jest uniwersalne: admin najpierw wybiera
addon, potem jedną z funkcji zadeklarowanych przez ten addon, a payload JSON jest
generowany z parametrów narzędzia. Przed wykonaniem joba scheduler uruchamia
instancję addonu, jeśli wybrany addon nie ma aktywnej instancji WASM; samo
wywołanie nadal idzie przez standardowe sprawdzenie uprawnień `call_tool`.

## Eureka MF Addon

Bundled addon `tentaflow-core/addons/eureka/` indeksuje publiczne informacje z
`https://eureka.mf.gov.pl/api/public/v1/informacje/{id}` do własnego SQLite.
Nie używa żadnego endpointu REST TentaFlow: LLM wywołuje narzędzia addonu przez
standardowy mechanizm addon tools, a przyszłe uruchomienia cykliczne powinny iść
przez admin scheduler.
Manifest addonu deklaruje wymagany cel zewnętrzny przez `[[network_rule]]`:
`tcp://eureka.mf.gov.pl:443`. Host function `http.request` działa fail-closed:
samo uprawnienie `http.request` nie pozwala na ruch wychodzący bez zgodnej i
zatwierdzonej reguły w `addon_network_rules`. Deklaracje manifestu nie są
zatwierdzane automatycznie przy instalacji, także gdy `required=true`; admin
zatwierdza je w zakładce Network addona, a UI zapisuje realne pole `approved`
używane przez host functions.

Narzędzia: `search` (lokalne wyszukiwanie po SQLite), `get_entry` (pobranie lub
odświeżenie pojedynczego wpisu), `sync_new` (dzienny skan nowych ID), `full_dump`
(wznawialny zrzut zakresu ID w batchach), `retry_failed` (ponowienie wpisów ze
statusem `error`), `recent` (najnowsze lokalne wpisy) oraz `stats`. Checkpointy
są w tabeli `eureka_sync_state`, wpisy w `eureka_entries`, a status każdego
sprawdzonego identyfikatora w `eureka_fetch_status`.

## Company Lookup PL Addon

Bundled addon `tentaflow-core/addons/company-lookup/` wykonuje wyłącznie online
lookup firm w oficjalnym API Wykazu podatników VAT MF:
`https://wl-api.mf.gov.pl/api/search`. Addon jest stateless: nie deklaruje
storage, nie ma migracji i nie cache'uje odpowiedzi. Każde wywołanie narzędzia
robi świeży request HTTP przez host function `http.request`.

Manifest deklaruje `[[network_rule]]` `tcp://wl-api.mf.gov.pl:443`, więc ruch
wychodzący nadal wymaga zatwierdzenia reguły Network przez admina. Narzędzia:
`lookup_by_nip`, `lookup_by_regon`, `lookup_many_by_nip` oraz `lookup_company`.
Odpowiedzi mają znormalizowane pola (`name`, `nip`, `regon`, `krs`,
`vat_status`, `address`, `working_address`, `residence_address`) oraz surowe
`raw` z API MF dla przypadków, gdy integracja potrzebuje pełnego payloadu.

Addon udostępnia też `blocks.json` dla Flow Buildera: `lookup_by_nip` i
`lookup_by_regon`. Flow blocki przyjmują identyfikator w `payload.nip`,
`payload.regon` albo `payload.Text` i zwracają wynik w `payload`, dzięki czemu
inne addony mogą korzystać z lookupu przez standardowy mechanizm flow.

## Contacts Addon

Bundled addon `tentaflow-core/addons/contacts/` jest źródłem prawdy dla firm,
osób, zatrudnień i map relacji dla przyszłych addonów CRM, Calendar, Billing,
Activity, Email i Documents. Dane są trzymane w per-addon SQLite z migracją
`migrations/001_init.sql`: `companies`, `persons`, `company_persons`,
`person_relations`, `sales_roles`, tagi i smart listy.
Manifest deklaruje platformy `linux`, `macos`, `windows`, `ios` i `android`.
Na desktopie/routerze addon działa przez Wasmtime, a w buildach mobilnych przez
ten sam abstrakcyjny runtime addona przełączony na `wasmi`.

Contacts udostępnia narzędzia LLM/tool-calling: `search_contacts`,
`get_company`, `get_person`, `create_company`, `create_person`,
`attach_person_to_company`, `list_persons_in_company`,
`get_relationship_map`, `lookup_company_online`, `extract_from_text` oraz
`compute_person_insights`. Flow Builder dostaje bloki `contacts.search_contacts`,
`contacts.find_or_create_company`, `contacts.find_or_create_person` i
`contacts.lookup_company_online`.

Manifest Contacts ma sekcję `[application]` z `entry_panel = "main"`, więc
addon rejestruje się jako aplikacja użytkownika w "Moje aplikacje" i sidebarze.
Panel `main` jest renderowany przy starcie addona, a dodatkowe panele
`companies`, `persons`, `company-detail`, `person-detail`, `relationship-map`
i `smart-lists` są budowane przez akcję UI `panel-navigate`. UI używa
natywnych komponentów SDK (`nav_tabs`, `toolbar`, `table_v2`, `canvas`,
`stat`, `key_value`, `timeline`), żeby odwzorować mockupy K1-K4 bez własnego
HTML ani lewego menu shellowego. Widoki nie zawierają rekordów demo: lista,
szczegóły osoby, szczegóły firmy, smart listy i mapa relacji czytają dane z
per-addon SQLite. Pusta baza renderuje empty state zamiast przykładowych firm
lub osób. Panele `companies` i `persons` mają formularze zapisu, które wywołują
te same ścieżki SQL co tool-calling (`save_company` / `save_person`).

Lookup online po NIP/REGON używa oficjalnego Wykazu VAT MF
`https://wl-api.mf.gov.pl/api/search` i nie cache'uje odpowiedzi. Sam lookup
zwraca `online=true` i `cached=false`; zapis do `companies` następuje dopiero
przez `create_company` lub `contacts.find_or_create_company` z potwierdzoną
mutacją. Manifest deklaruje `tcp://wl-api.mf.gov.pl:443`, więc admin musi
zatwierdzić network rule addona.

Mockupy CRM K1-K4 są traktowane jako docelowe powierzchnie Contacts. Sekcje
pokazujące deale, spotkania, maile, faktury, dokumenty i timeline nie należą do
Contacts; mają przyjść później przez platformowe kontrakty `PanelContribution`,
`RelationProvider`, `SearchProvider` i `ActionProvider`, a shell renderuje je
przez komponenty `tf-*`.

### Tier 2: HTTP REST secondary

Reserved for external integrations that cannot use the binary protocol:

1. `POST /core/frame/pickup` — Service-to-Core for backend service integrations
   (yolo, whisper inference). Authentication: HMAC `X-Pickup-Token` (one-shot,
   30 s TTL). Production REQUIRES mTLS client cert pinning (`[server.mtls]`).
2. `GET /recordings/<ref>?token=&exp=&ref=` — Browser-friendly signed URL for
   addon-issued recording downloads (PNG snapshots, MP4 segments). HMAC,
   multi-use, 60–3600 s TTL.
3. `GET /frames/<ref>?token=&exp=&ref=` — Same pattern, frame_url for raw RGB24
   bytes from frame_storage LRU. HMAC, multi-use, 60–600 s TTL.

### Security boundary

Both tiers share:
- HMAC SHA-256 token verification (constant-time via `subtle::ConstantTimeEq`)
- Audit log per outcome (`audit_log` + `frame_pickup_log`)
- Rate limit per IP + global (token bucket, 429 + `Retry-After`)
- Path traversal containment (canonicalize + `base_dir.starts_with` check)
- Security response headers: `Cross-Origin-Resource-Policy: same-site`,
  `Referrer-Policy: no-referrer`, `Cache-Control: private, no-store`,
  `X-Content-Type-Options: nosniff`, `Strict-Transport-Security: max-age=63072000;
  includeSubDomains` (HSTS applied unconditionally to every response).

Production TLS profile (enforced in `api::unified_server`):
- TLS 1.3 only (legacy clients explicitly unsupported in F1b)
- AEAD cipher suites only (no CBC, no RC4 — implied by TLS 1.3 lockout)
- HSTS header on every response (200, 401, 403, 404, 429 — no exception)

### Cluster constraint

HMAC signing keys (PickupToken + frame_url + recording_url + cameras AES-GCM)
and the pickup mTLS allowlist are process-local OR file-based per node.

Single-node (F1b P3.A): HMAC keys persist on disk at
`<tentaflow_home>/keys/{pickup_token,frame_url,recording_url}.key` (mode
0600 on Unix). Restart no longer invalidates outstanding URLs or pickup
tokens. Rotation: `tentaflow-cli keys rotate <name>` — running issuers
keep the previous key as a verify-only secondary for `max_ttl + 5 s` so a
rotation does not invalidate tokens already in flight.

Cross-node frame pickup (F1b P3.C — done): when a pickup token mesh-verifies
against a peer's HMAC key, the verifying node fetches the frame bytes from
the issuing peer over the mesh stream (`MESH_MSG_FRAME_PROXY_REQUEST = 0x45`
/ `MESH_MSG_FRAME_PROXY_RESPONSE = 0x46`, 5 s timeout) and serves them to
the calling service. B-side replay protection lives in
`PickupTokenIssuer::mesh_inflight_consume`. `frame_pickup_log` gains a
nullable `source_node_id` column (DB v24) — local pickups leave it NULL,
cross-node pickups record the peer's NodeId. 503 responses always carry
`Retry-After: 5`.

Multi-node (F1b P3.B): each peer mirrors its three HMAC issuer keys to
every trust-paired peer over the existing mTLS mesh stream
(`MESH_MSG_HMAC_KEYS_SYNC = 0x44`,
`tentaflow_protocol::mesh::HmacKeysSyncPayload`). Tokens minted on node A
verify on node B for the lifetime of the trust pairing. State is held in
`services::mesh_keys::MeshKeyPool` (in-memory only, never persisted to
disk — a revoked peer cannot leave stale verifiers behind). Disconnect /
trust-revoke drops the peer's pool entries; reconnect re-advertises.
One-shot pickup-token semantics are owned by the issuing node — mesh
fallback verifies HMAC + expiry but does not enforce one-shot on the
verifying side (the 30 s pickup TTL keeps the replay window tight). An
explicit broadcast-on-rotate hop (push new keys without waiting for the
next `PeerConnected`) is deferred; today rotation propagates lazily on
the next connect cycle. Unlike `TrustedKeysSync`, the HMAC advertise is
**not** gated by the 30 s `last_sync_sent` cooldown — every trusted
`PeerConnected` re-advertises so a rotated key reaches peers on the
first reconnect.

### service_call rate limit (F1b P5)

`service_call_v1` (host fn `service_request`) is rate-limited per addon:
burst 100, sustain ~16.67 req/s (1000 req/min). Implementation in
`src/services/service_call_rate_limit.rs`; limiter is a process-wide
singleton sharing the `TokenBucket` primitive from `src/util/token_bucket.rs`
with `api::rate_limit`. Denials return `AbiError::QuotaExceeded` (11) and
emit at most one collapsed `audit_log` row per addon per 60 s window
(`risk_class='C'`, `result='denied'`, `details.denied_count` carries the
in-window total) — prevents an addon DoS from turning into an audit-log DoS.

### Logging warning

NEVER enable hyper access logging (`RUST_LOG=hyper=debug`) in production without
a query-string scrubber. URLs `/recordings/<ref>?token=<hmac>` would log the
HMAC token wire in plain text via Hyper's request line.

### Default development command

```bash
cargo build --features dashboard-api
```

`camera` lives in `tentaflow-core`'s default features (GStreamer is mandatory
for the video-surveillance pipeline), so it no longer needs an explicit flag.
`dashboard-api` is still opt-in because some headless deployments skip the
HTTP/dashboard stack.

### Production deploy checklist

- [ ] TLS 1.3 enforced (default since E2; do not weaken)
- [ ] HSTS header observed in all responses (verify with `curl -k -I https://.../`)
- [ ] `[server.mtls] pickup_required = true` with at least one fingerprint
- [ ] HMAC token soak test passed (no 429 storms, no token leakage in logs)
- [ ] `RUST_LOG` scoped to crate-level (no `hyper=debug`)

## Conventions

- Comments in code: English only
- Variable/function names: English
- Commit messages: English, format `[type]: description`
- Rust: `rustfmt` defaults, `snake_case` functions, `PascalCase` types
- JS/HTML/CSS: 2-space indent, `camelCase` JS, `kebab-case` CSS
- C#: 4-space indent, `PascalCase` public, `_camelCase` private fields

## Code quality rules (MANDATORY — apply to every change)

These rules apply to humans AND to every AI agent working on this repo. No exceptions unless the user explicitly overrides a specific rule for a specific task.

### 1. No stubs, placeholders, or TODOs
- Every commit must be production-ready. If you cannot finish a feature in this pass, do not ship a partial implementation that pretends to work.
- Forbidden: `todo!()`, `unimplemented!()`, `// TODO: implement`, empty function bodies that return defaults, mock responses, "we'll wire this up later" scaffolding.
- If a dependency is missing, say so and stop. Do not fake it.

### 2. No backward-compatibility shims, no fallbacks
- When you change a function, change it in place. Do not keep the old version around "just in case".
- No alias exports, no deprecated wrappers, no feature flags for old behavior, no `if let Some(old) = ... else { new_path }` fallback chains.
- Exception: only when the user explicitly asks for compat (rare — assume never).

### 3. No versioned function names
- Forbidden: `process_request_v2`, `do_thing_new`, `calculate_ultrafast`, `handle_event_improved`, `user_check_permission_fixed`.
- If you are improving an existing function, **edit it in place**. The git history is the version record; the code should have one name per concept.
- If the signature change breaks callers, update the callers. That is the work.

### 4. Check for existing functions before writing new ones
- Before adding a new function, search the crate (or the relevant module) for something that already does this. Use Grep/ripgrep on likely names, likely signatures, and likely call sites.
- If a similar function exists and almost fits, extend it (new parameter, new enum variant) rather than forking a parallel one.
- This applies to Rust, JS, CSS, DB helpers — everywhere.

### 5. Delete unused code as you go
- When a refactor removes the last caller of a function, delete the function in the same commit. Do not leave dead code "in case we need it".
- Same for unused imports, unused struct fields, unused CSS classes, unused i18n keys, unused SQL helpers.
- `cargo check` warnings about unused items are bugs, not noise.

### 6. Comments describe WHY, not WHAT
- English only.
- File headers stay: `// ============ File: <name> — <1-sentence purpose> ============`.
- Inline comments only when the code's intent is not obvious from names — e.g. a workaround for a known bug, a non-obvious invariant, a performance trick. Do not narrate what the next line does.
- Forbidden: meta-comments like `// CRITICAL:`, `// OPT-001`, `// Fixed in this PR`, `// Changed from X to Y`, `// OWASP-xxx`. Git blame carries history; comments carry intent.

### 8. Always use project web components — never roll your own UI primitive

Project components live under `tentaflow-core/www/js/components/` — currently: `tf-button`, `tf-chip`, `tf-input`, `tf-menu`, `tf-searchbox`, `tf-select`, `tf-table`, `tf-tabs`, `tf-toggle`, `tf-window`.

**Rules:**
- For every UI primitive (button, input, select, toggle, chip, tabs, window/modal, searchbox, menu, table) use the `tf-*` component. Zero `<button>`, `<input>`, `<select>`, hand-rolled `.tabs-bar`, hand-rolled modal overlays in feature modules. The only permitted raw `<input>` is `type="file"` (no tf-file-input exists yet).
- If a `tf-*` component is missing a feature you need (animation, slot, event, variant, prop) — **extend the component**, don't build a one-off. Add the prop to the component's API, update its CSS, bump the demo if one exists.
- If a pattern is repeated in feature code (e.g. an oauth-mode radio card pattern, or a permission matrix cell), consider adding a new `tf-*` component. Add it when the pattern appears in 2+ places OR the feature module exceeds ~30 lines of markup for the same element.
- If a component's existing behavior is broken (no animation, wrong focus ring, missing keyboard handler), fix the component rather than working around it in the feature module.
- Code review rejects any diff that renders a custom tab strip, custom toggle, custom select dropdown, custom modal, etc., when a `tf-*` component exists. "Slight visual difference" is not justification — change the component's CSS variant.

**Why:** one-off UI primitives drift in look, accessibility, animation timing, and keyboard behavior. Users notice inconsistency. Components centralize the fixes.

## gstack

For all web browsing, use the `/browse` skill from gstack. Never use `mcp__claude-in-chrome__*` tools.

Available gstack skills:

| Skill | Purpose |
|-------|---------|
| `/browse` | Headless browser for web browsing, QA testing, screenshots |
| `/connect-chrome` | Launch real Chrome controlled by gstack with Side Panel |
| `/qa` | Systematic QA testing + fix bugs found |
| `/qa-only` | QA testing report only (no fixes) |
| `/design-review` | Visual QA: find and fix spacing, hierarchy, AI slop issues |
| `/design-consultation` | Product design system creation |
| `/design-shotgun` | Generate multiple design variants for comparison |
| `/review` | Pre-landing PR review |
| `/ship` | Ship workflow: tests, review, changelog, PR |
| `/land-and-deploy` | Merge PR, wait for CI, verify production |
| `/canary` | Post-deploy canary monitoring |
| `/benchmark` | Performance regression detection |
| `/investigate` | Systematic debugging with root cause analysis |
| `/office-hours` | YC-style forcing questions for startups/builders |
| `/plan-ceo-review` | CEO/founder-mode plan review |
| `/plan-eng-review` | Eng manager plan review |
| `/plan-design-review` | Designer's eye plan review |
| `/autoplan` | Auto-review pipeline (CEO + design + eng) |
| `/retro` | Weekly engineering retrospective |
| `/document-release` | Post-ship documentation update |
| `/codex` | OpenAI Codex CLI: review, challenge, consult |
| `/cso` | Chief Security Officer audit |
| `/setup-browser-cookies` | Import browser cookies for authenticated testing |
| `/setup-deploy` | Configure deployment settings |
| `/careful` | Safety guardrails for destructive commands |
| `/freeze` | Restrict edits to a specific directory |
| `/unfreeze` | Clear freeze boundary |
| `/guard` | Full safety: careful + freeze combined |
| `/gstack-upgrade` | Upgrade gstack to latest version |

## Skill routing

When the user's request matches an available skill, ALWAYS invoke it using the Skill
tool as your FIRST action. Do NOT answer directly, do NOT use other tools first.
The skill has specialized workflows that produce better results than ad-hoc answers.

Key routing rules:
- Product ideas, "is this worth building", brainstorming → invoke office-hours
- Bugs, errors, "why is this broken", 500 errors → invoke investigate
- Ship, deploy, push, create PR → invoke ship
- QA, test the site, find bugs → invoke qa
- Code review, check my diff → invoke review
- Update docs after shipping → invoke document-release
- Weekly retro → invoke retro
- Design system, brand → invoke design-consultation
- Visual audit, design polish → invoke design-review
- Architecture review → invoke plan-eng-review
- Save progress, checkpoint, resume → invoke checkpoint
- Code quality, health check → invoke health

## Sync Storage Proxy

- Central-only addon SQL/KV/Blob uses binary CBOR mesh messages `MESH_MSG_STORAGE_PROXY_REQUEST` (`0x34`) and `MESH_MSG_STORAGE_PROXY_RESPONSE` (`0x35`) on UFP/2 Mesh channel.
- Authority-backed policies may use `authority_readthrough`, `authority_write`, or `replicated_by_permission`/`sharded` with `authority_node_id`. Nodes with `sync_receive` materialize locally; nodes without it read/write through the authority without storing addon rows locally.
- Blob central-only proxy is chunked: clients call `BlobPutChunk`/`BlobGetChunk`, authority validates chunk hash and final sha256, writes the content-addressed blob, records blob capture and returns range bytes without local client materialization.

## Compliance Core

- `tentaflow-core/src/compliance/` to wspólna warstwa core dla RODO/GDPR, AI audit, retencji, ROPA, DSAR, zgód, DPIA i rejestru naruszeń.
- Migracja `compliance_core_foundation` tworzy kanoniczne tabele compliance i provisionuje domyślne rekordy dla każdej organizacji. Teksty widoczne w UI używają pól `*_translations` walidowanych przez `json_valid`; seed musi zawierać co najmniej `pl` i `en`.
- `compliance_ai_events` przechowuje jedno wywołanie/sesję AI i łączy się z istniejącym chainem `audit_log` przez `audit_log_id`; prompty, odpowiedzi, źródła i tool calls zostają w dedykowanych tabelach compliance AI.
- Retencja AI audit jest rozwiązywana przez `compliance_retention_policies` i nie może być krótsza niż 183 dni.
- Protokół administracyjny używa `MessageBody::ComplianceAdminBody` i `tentaflow-protocol/src/compliance.rs`; przez CBOR przechodzą skróty kategorii, retencji i eventów AI, bez treści promptów/odpowiedzi.
- Dostęp administracyjny wymaga `compliance.read`; role `org_admin` i `dpo` dostają także `compliance.write` na potrzeby dalszych operacji zarządzania.
