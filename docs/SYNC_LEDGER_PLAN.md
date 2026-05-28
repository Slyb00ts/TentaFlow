# Plan: TentaFlow Sync Ledger

## Podsumowanie

TentaFlow ma synchronizowac dane miedzy zdecentralizowanymi nodami bez centralnego punktu awarii, ale bez kopiowania calej bazy na kazde urzadzenie. Addony zapisują dane przez TentaFlow Core, a Core decyduje o uprawnieniach, polityce syncu, dystrybucji, szyfrowaniu, retencji i kompaktowaniu.

SQLite pozostaje obowiazkowa baza aktualnego stanu danych addonow. Fjall zostaje wybrany jako wbudowany storage techniczny dla append-only sync ledgera, outbox/inbox, ACK, cursorow, hash-chain, snapshotow i metadanych kompaktowania.

## Decyzje projektowe

- SQLite nie jest zastepowany.
- Fjall jest docelowym embedded storage dla Sync Ledger i jest dodany jako normalna zaleznosc `tentaflow-core`.
- RocksDB nie jest domyslnym wyborem dla Sync Ledger; zostaje tylko wynikiem historycznego spike'a i moze wrocic jako osobna opcja server-only, jezeli bedzie jawnie otwarta taka decyzja.
- Addon nie deklaruje trybow synchronizacji i nie zarzadza syncem.
- Kazdy zapis addonowy musi przejsc przez TentaFlow Core.
- Sync policy jest konfigurowana w GUI TentaFlow.
- Contacts jest pierwszym addonem pilotazowym syncu end-to-end.
- Domyslna retencja ledgera to brak limitu.
- System musi ostrzegac o zajetosci dysku i pokazywac, ktore partycje oraz nody blokuja kompaktowanie.
- Stary mechanizm CRDT nie zostaje jako drugi rownolegly system. Po audycie zostanie rozbudowany w miejscu albo usuniety i zastapiony nowym `sync/ledger`.

## Stan Implementacji

- Zrobione: `fjall` jest zaleznoscia `tentaflow-core`.
- Zrobione: istnieje modul `tentaflow-core/src/sync/ledger/` z `SyncLedgerStore`, `FjallSyncLedgerStore`, formatem operacji, outbox, inbox, peer cursorami, metadata snapshotow i partition heads.
- Zrobione: lokalny `append_operation` przydziela sekwencje per partycja, laczy hash-chain przez `prev_partition_hash`, liczy canonical hash i podpisuje operacje po wyliczeniu finalnej tresci.
- Zrobione: inbox zapisuje tylko operacje zweryfikowane przez `SyncOperationVerifier`; verifier Ed25519 sprawdza canonical hash, `OperationId`, dlugosc podpisu i podpis aktora.
- Zrobione: `validate_hash_chain_from` obsluguje sprawdzanie zakresu od znanego cursora, bez pobierania calej historii partycji.
- Zrobione: `build_merkle_summary` buduje Merkle root tylko dla ciaglego zakresu jednej partycji.
- Zrobione: testy jednostkowe ledgera obejmuja append, hash-chain, lookup po `OperationId`, outbox, inbox, odrzucenie zlego podpisu, cursor, snapshot metadata, podpisy i Merkle summary.
- Zrobione: migracja `sync_identity_registry` dodaje `sync_nodes`, `user_identity_keys` i `node_user_assignments`, czyli rozdziela techniczna tozsamosc node/device od kryptograficznej tozsamosci usera.
- Zrobione: repository ma operacje rejestracji node/device, przypisywania usera do node, zapisu/revoke kluczy usera i listowania aktywnych relacji.
- Zrobione: migracja `sync_permission_engine` dodaje `sync_user_org_profiles`, `sync_resource_acl` i `sync_explicit_shares`.
- Zrobione: repository ma decyzje `can_user_access_sync_resource` i `can_node_receive_sync_resource` dla owner, assigned, department, manager_subtree, explicit share, admin oraz authority node.
- Zrobione: migracja `sync_policy` dodaje `sync_policies`, czyli konfiguracje trybu synchronizacji per organizacja, addon, typ zasobu albo konkretny zasob.
- Zrobione: repository ma `upsert_sync_policy`, `get_effective_sync_policy` i `list_sync_targets_for_resource`.
- Zrobione: Sync Policy wybiera targety wedlug trybu: `local_only` i `ephemeral` nic nie wysylaja, `authority_readthrough` i `authority_write` kieruja do authority node, a `replicated_by_permission` i `sharded` filtrują trusted nody przez Permission Engine.
- Zrobione: per-addon SQL storage tworzy wewnetrzna tabele `__tentaflow_sync_captures` i zapisuje capture kazdego DML w tej samej transakcji co zapis addonu.
- Zrobione: `sync::runtime` startuje razem z nodem, appenduje capture do Fjall Sync Ledger, podpisuje operacje kluczem MeshSecurity i wrzuca je do outbox wedlug Sync Policy + Permission Engine.
- Zrobione: capture zapisuje `operation_id`, a startup nodu uruchamia drainer zainstalowanych addonow SQL i ponawia wpisy ze statusem `pending` albo `error`.
- Zrobione: po migracji UFP/2 sync wire idzie przez `channel=0x06 SyncLedger`; payloady `SyncPush`, `SyncAck`, `SyncPull`, `SyncPullResponse` i snapshot niosa binarne CBOR/CBOR body potrzebne obecnemu runtime, a nie JSON. Pipeline wysyla pending outbox po polaczeniu z trusted peerem, zapisuje zweryfikowane operacje do inbox i ACK-uje przyjete `OperationId`.
- Zrobione: incoming operations z inbox sa aplikowane do lokalnego SQLite addona przez `apply_replicated_write`, bez tworzenia nowego capture; replay jest idempotentny przez `__tentaflow_sync_applied`, a inbox entry dostaje marker `applied`.
- Zrobione: conflict foundation zapisuje nieaplikowalne incoming operations do `__tentaflow_sync_conflicts`, oznacza inbox entry jako `conflicted` i przerywa retry loop bez utraty operacji.
- Zrobione: Snapshot Manager (`sync::snapshot`) buduje i zapisuje podpisane snapshoty partycji na podstawie ciaglego hash-chain, Merkle root, state hash, policy epoch i ostatniego operation hash.
- Zrobione: restore-plan snapshotu pobiera podpisany checkpoint, weryfikuje podpis autora i zwraca operacje po snapshocie z walidacja ciaglosci hash-chain.
- Zrobione: materializowany restore do SQLite addona odtwarza stan z lokalnej historii ledgera, najpierw walidujac snapshot jako checkpoint prefiksu, a potem aplikujac operacje SQL w kolejnosci bez capture loop.
- Zrobione: SQL snapshot package zapisuje podpisany hash i rozmiar bloba w `SyncSnapshot`; blob niesie zweryfikowany prefiks operacji SQL i pozwala odtworzyc SQLite po usunieciu prefiksu z glownych rekordow ledgera.
- Zrobione: `SnapshotPackageStore` zapisuje snapshot package content-addressed na dysku pod `<TENTAFLOW_HOME>/sync/snapshot-blobs`, waliduje hash/rozmiar przy zapisie i odczycie oraz umozliwia restore z utrwalonego bloba.
- Zrobione: Mesh Sync Protocol ma wire payloady `SyncSnapshotPull` i `SyncSnapshotResponse` (`0x4B..0x4C`), ktore przenosza metadane snapshotu, blob SQL package i opcjonalne operacje po snapshocie. Odbiorca weryfikuje podpis snapshotu, zgodnosc metadanych i hash/rozmiar bloba przed zapisem do `SnapshotPackageStore`.
- Zrobione: `SyncPull` automatycznie przechodzi na `SyncSnapshotResponse`, gdy responder nie moze juz wyslac ciaglego zakresu operacji po kompaktowaniu. Receiver odrzuca pull response i snapshot tail z luka sekwencji albo z innej partycji, odtwarza SQLite z przyjetego bloba oraz utrwala peer cursor dopiero po walidacji i restore.
- Zrobione: mesh pipeline uruchamia aktywny scheduler repair. Scheduler co kilka sekund dla connected trusted peerow ponawia niepotwierdzony outbox i wysyla `SyncPull` dla luk sekwencji wykrytych przy odbiorze operacji, z exponential backoff per `peer+partition`.
- Zrobione: repair queue jest utrwalona w osobnym keyspace Fjall `repair_queue`; wpisy sa listowane po terminie `next_attempt_ms`, retry aktualizuje backoff, a udany pull/snapshot usuwa wpis.
- Zrobione: `CompactionManager` kompaktuje operacje tylko per partycja i tylko po podpisanym snapshot package obecnym w `SnapshotPackageStore`; kompaktowanie blokuje sie, gdy prefiks ma niepotwierdzony outbox.
- Zrobione: `sync::runtime` ma testy runtime dla offline outbox -> reconnect push -> ACK, luki sekwencji -> repair pull od brakujacego prefiksu oraz skompaktowanego prefiksu -> `SyncSnapshotResponse`.
- Zrobione: `CompactionFinalityPolicy` obsluguje finality kompaktowania przez `AllOutboxTargets`, `RequiredTargets` i `Quorum`; ledger zwraca statusy outbox dla prefiksu partycji, a manager blokuje kompaktowanie przy brakujacych ACK albo brakujacych wymaganych targetach.
- Zrobione: Conflict Manager ma backendowe API list/resolve dla `__tentaflow_sync_conflicts`, strategie `keep_local`, `ignore` i `accept_remote`; `accept_remote` transakcyjnie usuwa lokalny konflikt primary-key dla INSERT, aplikuje remote write i oznacza inbox jako applied przez `sync::runtime`.
- Zrobione: Core Sync Registry mapuje `organizations`, `user_accounts`, legacy `users`, `user_groups`, `group_members`, `roles`, `org_memberships`, `flows`, `flow_versions` i `flow_model_bindings` na `resource_type` oraz partycje `core/org/{org_id}/...`.
- Zrobione: migracja `core_sync_captures` tworzy `__tentaflow_core_sync_captures`, a `sync::core_capture` zapisuje `CoreWriteCapture` w transakcji core SQLite. `changed_fields` jest BLOB-em CBOR, nie JSON-em.
- Zrobione: repozytorium Flow Buildera zapisuje core capture dla `create_flow`, `update_flow`, `delete_flow`, `update_flow_with_snapshot` oraz CRUD `flow_model_bindings`; snapshot poprzedniej wersji flow tworzy capture `core.flow_version`.
- Zrobione: repozytoria identity/RBAC zapisuja core capture dla `user_accounts`, `user_groups`, `group_members`, `organizations` i `org_memberships`; zmiana hasla jest redagowana do `password_changed=true`, bez synchronizacji hasha.
- Zrobione: `sync::runtime` zapisuje core capture jako binarny payload CBOR z `addon_id=core`, partycjami `core/org/...` i domenowymi `changed_fields`, bez addonowego `params_json`; `sync::core_capture` ma drainer pending/error do ledgera.
- Zrobione: `sync::core_materializer` aplikuje incoming `core.*` przez allowlist tabel i pol, bez wykonywania SQL z payloadu; `apply_unapplied_inbox` rozdziela operacje core od addonowych.
- Zrobione: materializer ma field-level merge dla duplicate `INSERT` po primary key przez `ON CONFLICT ... DO UPDATE`; retry/pull/snapshot dla istniejacego rekordu scala znane pola zamiast tworzyc konflikt.
- Zrobione: addonowe KV `storage_set` i `storage_delete` zapisuje trwaly capture `__tentaflow_kv_sync_captures` w tej samej transakcji SQLite co `addon_storage`; operacje `addon.kv` ida do Sync Ledger jako binarne `FieldValue::Bytes`, materializowane po stronie odbiorcy do `addon_storage` bez JSON i bez dodatkowego capture loop.
- Zrobione: `FileBlobStore` zapisuje trwaly capture `__tentaflow_blob_sync_captures` po zapisie content-addressed pliku; `sync::runtime` dzieli blob na operacje `blob_store_chunks` po 1 MiB z binarnym `FieldValue::Bytes`, a `core.blob` jest manifestem bez bajtow pliku.
- Zrobione: odbiorca sortuje pending outbox wedlug `partition_id + partition_sequence`, aplikuje chunki przed manifestem, materializuje finalny plik strumieniowo pod `<TENTAFLOW_HOME>/blobs/<sha-prefix>/<sha>.bin`, waliduje hash/rozmiar i sprzata tymczasowe chunki spod `<TENTAFLOW_HOME>/sync/blob-chunks`.
- Zrobione: startup runtime odpala drainery pending/error dla addon SQL, core captures, KV captures i blob captures przed apply inbox.
- Zrobione: `sync::storage_monitor` liczy zajetosc SQLite, Fjall ledgera, snapshot blobow, finalnych blobow i pending chunkow oraz wyznacza progi wolnego miejsca 20/10/5%.
- Zrobione: przy stanie `critical` runtime blokuje nowe duze bloby powyzej 1 MiB, zostawiajac male operacje SQL/KV/metadata bez sztucznej blokady.
- Zrobione: raport storage jest wystawiony przez binary protocol jako `SyncStorageBody(SyncStoragePayload::ReportRequest/ReportResponse)` z handlerem admin-only.
- Zrobione: `Settings -> Storage` pokazuje status pressure, wolne/uzyte miejsce, limit blokady duzych blobow oraz rozmiary sciezek storage przez binary WS.
- Zrobione: test `core_push_materializes_flow_on_receiver` sprawdza przeplyw source runtime -> outbox -> push -> receiver inbox -> materializer SQLite -> ACK dla `core.flow`.
- Zrobione: test `core_blob_push_materializes_file_on_receiver` sprawdza source runtime -> outbox -> push -> receiver inbox -> content-addressed file dla `core.blob`.
- Zrobione: test `core_blob_push_materializes_chunked_file_on_receiver` sprawdza wielochunkowy plik, manifest bez `bytes`, trzy operacje chunkow i cleanup tymczasowego katalogu chunkow.
- Zrobione: testy `storage_monitor` sprawdzaja progi pressure, sumowanie storage paths i blokade tylko dla duzych blobow przy `critical`.
- Zrobione: testy selektywnego core outboxa sprawdzaja, ze `replicated_by_permission` wysyla `core.flow` tylko do node'a przypisanego do usera z ACL oraz do node'a admina org bez ACL zasobu.
- Zrobione: `sync.permission_epoch:{org_id}` w `settings` jest podbijany przy zmianach decyzji sync: node identity/assignment, role/is_active usera, profile org, ACL zasobu, explicit shares, policies i org membership. `SyncOperation.policy_epoch` niesie aktualna epoke.
- Zrobione: przed wyslaniem pending outbox `sync::runtime` ponownie liczy targety przez Permission Engine. Jezeli node stracil dostep po zakolejkowaniu operacji, wpis outbox jest zamykany lokalnie i nie trafia do mesh push.
- Zrobione: `apply_unapplied_inbox` aplikuje incoming operations deterministycznie wedlug priorytetu blobow, `partition_id` i `partition_sequence`, zeby repair response z wieloma operacjami nie mogl wykonac update przed insertem.
- Zrobione: test `repair_pull_response_materializes_missing_core_flow_operations` sprawdza pelny repair path dla `core.flow`: odbiorca dostaje operacje z luka, kolejkuje pull od brakujacego prefiksu, source odsyla brakujace operacje, receiver materializuje flow i ACK zamyka outbox.
- Zrobione: test `compacted_prefix_is_served_as_snapshot_response` sprawdza teraz pelny snapshot response path: po kompaktowaniu source wysyla snapshot package + tail, receiver waliduje i odtwarza SQLite z bloba, aplikuje tail, czysci repair queue oraz ACK zamyka outbox.
- Zrobione: test `multi_node_mesh_sync_push_materializes_core_flow_and_acks` uruchamia dwa prawdziwe `IrohMeshManager` na loopback, wysyla `SyncPush` i `SyncAck` przez UFP/2 `SyncLedger`, materializuje `core.flow` na receiverze i potwierdza outbox na source.
- Zrobione: test `multi_node_mesh_repair_pull_materializes_missing_core_flow_operations` wysyla przez UFP/2 niepelny `SyncPullResponse`, receiver wykrywa luke, wysyla `SyncPull`, source odsyla pelny `SyncPullResponse`, receiver materializuje insert+update i `SyncAck` zamyka outbox obu operacji.
- Zrobione: test `multi_node_mesh_snapshot_response_restores_compacted_sql_prefix` wysyla przez UFP/2 `SyncPull` dla skompaktowanego prefiksu, source odsyla `SyncSnapshotResponse` z SQL snapshot package i tail, receiver odtwarza SQLite, czysci repair queue i ACK zamyka outbox prefiksu oraz tail.
- Zrobione: test `multi_node_mesh_permission_revoke_stops_future_core_flow_push` wysyla pierwszy `core.flow` do receivera z dostepem, potem przepina ACL na innego usera/node i potwierdza, ze kolejna aktualizacja nie buduje juz push payloadu dla starego receivera.
- Zrobione: test `multi_node_mesh_kv_push_materializes_storage_on_receiver` wysyla `addon.kv` przez UFP/2 `SyncPush`, receiver materializuje `addon_storage`, a ACK zamyka outbox.
- Zrobione: test `multi_node_mesh_chunked_blob_push_materializes_file_on_receiver` wysyla wielochunkowy `core.blob` przez UFP/2, receiver sklada finalny plik content-addressed, waliduje hash przez runtime i sprzata tymczasowe chunki.
- Zrobione: test `multi_node_mesh_four_node_fanout_syncs_core_flow_to_all_targets` uruchamia cztery prawdziwe nody loopback, replikuje jeden `core.flow` z source do trzech targetow przez `replicated_by_permission`, materializuje dane w trzech DB i potwierdza ACK/outbox dla kazdego targetu.
- Zrobione: `mesh::pipeline` ma wydzielony jeden tick schedulera repair, uzywany przez produkcyjna petle co 5 sekund oraz przez testy bez dublowania logiki.
- Zrobione: test `multi_node_mesh_repair_scheduler_recovers_gap_after_reconnect` wykrywa luke, zapisuje repair queue, rozlacza i laczy nody ponownie, odpala produkcyjny tick schedulera, wysyla `SyncPull`, materializuje brakujacy prefiks i czysci repair queue.
- Zrobione: `process_four_node_core_suite_materializes_after_restart` restartuje source i trzech receiverow, po czym potwierdza materializacje core suite oraz brak pending outbox na source.
- Zrobione: `process_four_node_sync` uruchamia 9 scenariuszy procesowych: fanout po restarcie, offline catch-up, permission gating, central-only bez materializacji, central-only read/write-through, core suite po pelnym restarcie, snapshot tail ACL, konflikt i powtarzany fanout.
- Zrobione: core sync obejmuje teraz rowniez control-plane sync: `sync_nodes`, `user_identity_keys`, `node_user_assignments`, `sync_user_org_profiles`, `sync_policies`, `sync_resource_acl` i `sync_explicit_shares`.
- Nie zrobione: nie ma jeszcze GUI resolve/ignore dla core conflict, GUI konfiguracji Sync/Permissions/Devices ani PostgreSQL storage backend.

## Cele

- Offline-first synchronizacja dla danych, ktore maja byc replikowane.
- Selektywna replikacja wedlug uprawnien, urzadzen, profili i polityk zasobow.
- Mozliwosc pracy bez centralnego serwera aplikacyjnego.
- Mozliwosc ustawienia zasobu jako lokalnego, replikowanego, authority-only, sharded albo ephemeral.
- Integralnosc operacji przez podpisy, hash-chain i Merkle summaries.
- Niezawodne dostarczanie operacji przez outbox, inbox, retry, ACK i peer cursors.
- Snapshoty i kompaktowanie, zeby ledger nie rosl bez kontroli.
- Docelowy consensus dla polityk organizacyjnych i bezpieczenstwa.
- Kompatybilnosc architektury z przyszla warstwa Block Builder / Consensus, bez zmiany API addonow.
- Rozdzielenie podpisu technicznego node/device od podpisu usera: node podpisuje transport i sync delivery, user identity sluzy do wlascicielstwa, approvali, audytu i operacji wysokiego ryzyka.
- Crypto-ready App Registry i zero-trust model ownerow addonow sa opisane w `docs/CRYPTO_READY_APP_REGISTRY_AND_CONSENSUS.md`.

## Poza Zakresem

- Nie budujemy publicznego blockchaina.
- Nie robimy mining, tokenow ani globalnego consensusu dla kazdej zmiany kontaktu.
- Nie synchronizujemy automatycznie calej bazy CRM na kazdy node.
- Nie pozwalamy addonom omijac Core Storage API przy zapisach danych.

## Crypto-ready Architecture

Sync Ledger ma byc projektowany tak, zeby TentaFlow mogl w przyszlosci dostac
osobna warstwe blockchain/krypto bez przepisywania addonow, UI, SQLite ani Core
Storage API. Nie oznacza to budowania tokenow, miningu ani publicznego chaina w
obecnym zakresie. Obecny zakres to ledger zdarzen, ktory ma kryptograficzne
wlasciwosci potrzebne do pozniejszego grupowania operacji w bloki.

Szczegolowy model App Registry, ownerow addonow, maintainerow, policy ledgera,
block buildera, finality certificate i zero-trust walidacji jest w
`docs/CRYPTO_READY_APP_REGISTRY_AND_CONSENSUS.md`.

Wymagane wlasciwosci:

```text
deterministyczna serializacja operacji
stabilny OperationId liczony z kanonicznej tresci
hash kazdej operacji
hash-chain per partycja
Merkle root / Merkle summary per zakres operacji
podpis autora operacji
podpisane snapshoty z root hash
jawne identity actor_user, actor_device i actor_node
replay protection
walidacja operacji przed apply do SQLite
oddzielny Policy Ledger dla zmian organizacyjnych i bezpieczenstwa
```

Docelowa sciezka rozbudowy:

```text
Sync Ledger
  -> Block Builder
  -> Consensus Layer
  -> Finality
  -> opcjonalny Token / Economic Layer
```

Zasady projektowe:

- Operacja musi miec ten sam hash na kazdym nodzie.
- Timestamp, autor, epoch polityk i poprzedni hash musza byc jawnie zapisane w operacji.
- SQLite przechowuje aktualny stan, a Fjall Sync Ledger przechowuje historie, dowody, cursory, ACK i metadata snapshotow.
- Consensus nie jest wymagany dla kazdej zmiany CRM w obecnym zakresie; ma byc mozliwy jako modul finalizujacy paczki operacji.
- Partycje ledgera musza byc mozliwe do pozniejszego mapowania na chain, shard albo rollup.
- Addony nie moga zalezec od przyszlej warstwy krypto. Addon widzi Core Storage API, a nie blok, validator ani token.
- Warstwa tokenow nie jest czescia Sync Ledger i moze powstac tylko jako osobny modul po osobnej decyzji.

## Architektura

```text
Addon
  |
  v
Core Storage API
  |
  +--> Permission Engine
  |
  +--> SQLite materialized state
  |
  +--> Sync Ledger / Outbox
          |
          v
      Mesh Sync Protocol
          |
          v
      Remote Inbox
          |
          v
      Verify -> Permission Recheck -> Apply to SQLite
```

## Komponenty

| Komponent | Odpowiedzialnosc |
|-----------|------------------|
| Device Registry | Rejestr nodow, urzadzen, profili, przypisan uzytkownikow i revocation |
| Org / Users / Roles | Uzytkownicy, dzialy, role, managerowie i czlonkostwa |
| Permission Engine | Effective access do zasobow, rekordow i pol |
| Core Storage API | Jedyna kontrolowana droga zapisu dla addonow |
| Sync Policy | Konfiguracja tego, co i jak sie synchronizuje |
| Sync Ledger | Append-only log podpisanych operacji |
| Outbox / Inbox | Kolejki dostarczania, odbioru, retry i deduplikacji |
| Peer Cursors / ACK | Wiedza o tym, co dany peer odebral i potwierdzil |
| Mesh Sync Protocol | Wymiana operacji, snapshotow i Merkle summaries |
| Snapshot Manager | Snapshoty, checkpointy i kompaktowanie |
| Storage Monitor | Monitorowanie dysku, alerty i blokady bezpieczenstwa |
| Conflict Manager | Wykrywanie, zapis i rozwiazywanie konfliktow |
| Admin GUI | Konfiguracja urzadzen, uprawnien, syncu, retencji i audytu |

## Storage

```text
SQLite:
- dane addonow
- aktualny stan
- relacje
- indeksy SQL
- SELECT-y dla UI
- tool calling

Fjall:
- operation log
- outbox
- inbox
- peer cursors
- ACK
- hash-chain
- Merkle summaries
- snapshot metadata
- retry queue
- compaction metadata
```

Stan implementacji:

```text
tentaflow-core/src/sync/ledger/types.rs
tentaflow-core/src/sync/ledger/fjall_store.rs
```

Pierwsza implementacja zawiera `SyncLedgerStore`, `FjallSyncLedgerStore`,
`NewSyncOperation`, `SyncOperation`, canonical CBOR encoding, hash operacji
przez BLAKE3, sekwencje per partycja, hash-chain
przez `prev_partition_hash`, operation index po `OperationId`, outbox, inbox,
peer cursors, partition heads i snapshot metadata. Podpisy sa polem operacji;
walidacja kryptograficzna podpisu jest osobnym zadaniem security.

Docelowy trait:

```rust
trait SyncLedgerStore {
    fn append_operation(&self, operation: NewSyncOperation) -> Result<AppendResult>;
    fn get_operations(&self, query: OperationQuery) -> Result<Vec<SyncOperation>>;
    fn get_operation(&self, op_id: OperationId) -> Result<SyncOperation>;
    fn put_in_outbox(&self, target: SyncTarget, op_id: OperationId) -> Result<()>;
    fn get_outbox_entry(&self, target: SyncTarget, op_id: OperationId) -> Result<OutboxEntry>;
    fn put_in_inbox(&self, source: PeerId, operation: SyncOperation) -> Result<()>;
    fn get_inbox_entry(&self, source: PeerId, op_id: OperationId) -> Result<InboxEntry>;
    fn mark_delivered(&self, target: SyncTarget, op_id: OperationId) -> Result<()>;
    fn mark_acknowledged(&self, target: SyncTarget, op_id: OperationId) -> Result<()>;
    fn get_peer_cursor(&self, peer: PeerId, partition: PartitionId) -> Result<Option<PeerCursor>>;
    fn save_peer_cursor(&self, cursor: PeerCursor) -> Result<()>;
    fn save_snapshot(&self, snapshot: SyncSnapshot) -> Result<()>;
    fn get_snapshot(&self, partition: PartitionId, up_to_sequence: u64, snapshot_id: SnapshotId) -> Result<SyncSnapshot>;
    fn get_partition_head(&self, partition: PartitionId) -> Result<Option<PartitionHead>>;
    fn compact(&self, policy: CompactionPolicy) -> Result<()>;
}
```

## Format Operacji

Kazdy zapis przez Core tworzy operacje:

```text
op_id
org_id
partition_id
addon_id
resource_type
resource_id
table_name
primary_key
action: insert/update/delete
changed_fields / patch
before_hash
after_hash
actor_user_id
actor_device_id
actor_node_id
hlc_timestamp
prev_partition_hash
payload_hash
acl_snapshot_hash
policy_epoch
signature
encryption_info
```

Operacja zawiera roznice, nie pelny stan bazy. Reprezentacja na drucie i w
ledgerze jest binarna/canonical, a nie JSON. Logiczny payload operacji sklada
sie z: `table`, `primary_key`, `action`, mapy `changed_fields`, `before_hash`,
`after_hash`, `actor_user_id`, `actor_device_id`, `actor_node_id`, podpisu i
metadata polityki.

Delete tworzy tombstone. Tombstone nie moze byc usuniety przed minimalnym TTL i przed bezpiecznym snapshotem.

## Partycje

Nie ma jednego globalnego chaina. Ledger jest dzielony na partycje:

```text
org/policy
org/users
org/devices
org/roles
core/org/{org_id}/flows
core/org/{org_id}/users
core/org/{org_id}/groups
core/org/{org_id}/roles
addon/contacts/companies
addon/contacts/persons
addon/calendar/user/{user_id}
addon/notes/user/{user_id}
addon/rag/index/{authority_node_id}
```

Kazda partycja ma wlasny hash-chain, Merkle root, cursory i retencje. Node pobiera tylko te partycje, ktore sa mu potrzebne i do ktorych ma prawo.

## Sync Policy

Polityka jest zapisywana w bazie TentaFlow i konfigurowana w GUI, nie w manifestach addonow.

Tryby core:

| Tryb | Znaczenie |
|------|-----------|
| local_only | Dane istnieja tylko na tym nodzie |
| replicated_by_permission | Dane sa synchronizowane offline-first do uprawnionych nodow |
| authority_readthrough | Dane mieszkaja na authority node, inne nody odpytuja online |
| authority_write | Zapis idzie przez authority node |
| sharded | Dane sa dzielone miedzy nody wedlug klucza |
| ephemeral | Brak trwalego zapisu i brak syncu |

Przyklad dla Contacts:

```text
Companies: replicated_by_permission
Persons: replicated_by_permission
Private notes: local_only albo replicated_by_permission
Attachments: authority_readthrough
Activity log: authority_write
```

Przyklad dla RAG:

```text
Corpus: authority_readthrough
Embeddings index: authority_write
Query results: ephemeral
```

## Uprawnienia

Permission check jest wykonywany w trzech miejscach:

```text
przed zapisem:
- czy actor moze wykonac operacje?

przed wyslaniem:
- czy docelowy node/device/profile moze dostac operacje?

po odebraniu:
- czy operacja jest poprawna dla lokalnie znanej polityki i epoki?
```

Minimalne reguly:

```text
owner
department
manager_subtree
explicit_share
admin
authority_node
```

Potrzebne modele:

```text
users
org_units
org_memberships
manager_edges
roles
role_permissions
resource_acl
device_assignments
effective_access_cache
policy_epochs
```

## Device Registry

Node nie jest uzytkownikiem.

```text
node_id
device_id
device_name
device_kind: personal/shared/server/kiosk
assigned_user_ids
active_profile_id
trust_level
approved_by
approved_at
revoked_at
last_seen_at
```

Dla urzadzen wspoldzielonych:

```text
osobny profil lokalny per user
osobny klucz szyfrujacy per profil
brak wspolnej bazy z danymi wielu userow bez jawnej polityki
```

## Core Data Sync

Sync addonow nie wystarcza, bo Flow Builder, uzytkownicy, role, grupy i
czlonkostwa sa danymi platformowymi w core SQLite. Te dane musza wejsc do tego
samego Sync Ledgera, ale przez osobna warstwe `Core Sync`, a nie przez udawanie
addona ani przez raw SQL trigger na calej bazie.

Pierwszy fundament kodowy jest w `tentaflow-core/src/sync/core_registry.rs`.
Rejestr okresla, ktore tabele core wolno synchronizowac, jaki maja
`resource_type`, klucz glowny i partycje ledgera. Kanoniczny techniczny
`addon_id` dla operacji core to `core`, bo obecny format `SyncOperation` ma pole
`addon_id`; semantycznie oznacza to platforme TentaFlow, nie addon uzytkownika.

Zakres synchronizowany:

| Obszar | Tabele | Partycja | Uwagi |
|--------|--------|----------|-------|
| Organizacje | `organizations` | `core/org/{org_id}/organizations` | Konfiguracja tenantow i slugow |
| Userzy | `user_accounts`, legacy `users` | `core/org/{org_id}/users` | Hashe hasel i sekrety wymagaja szyfrowania/allowlisty pol |
| Grupy | `user_groups`, `group_members` | `core/org/{org_id}/groups` | Czlonkostwa musza byc idempotentne |
| Role | `roles`, `org_memberships` | `core/org/{org_id}/roles` | Zmiany rol powinny podbijac policy epoch |
| Flow Builder | `flows`, `flow_versions`, `flow_model_bindings` | `core/org/{org_id}/flows` | Syncujemy definicje, nie runtime wykonania |
| Identity Registry | `sync_nodes`, `user_identity_keys`, `node_user_assignments`, `sync_user_org_profiles` | `core/org/{org_id}/identity` | Mapowanie node/user i struktura org sa warunkiem selektywnej replikacji |
| Sync Control Plane | `sync_policies`, `sync_resource_acl`, `sync_explicit_shares` | `core/org/{org_id}/sync-control` | Polityki i ACL musza dojsc do nodow przed decyzjami materializacji |

Zakres celowo niesynchronizowany domyslnie:

| Tabela | Powod |
|--------|-------|
| `flow_executions` | Historia/runtime, moze byc lokalna albo audit-only |
| `flow_invocations` | Telemetria wywolan, bardzo duzy wolumen |
| `audit_log` | Osobny tryb audit export/replication, nie zwykly data sync |
| cache/runtime/status tables | Stan lokalny noda |

Core Sync musi byc wdrazany przez jawne wrappery repozytorium, nie przez trigger
na calej core DB. Kazda funkcja zapisu core powinna tworzyc `CoreWriteCapture`
w tej samej transakcji co zmiana danych, a potem runtime mapuje capture na
`NewSyncOperation` przez `core_registry`. To daje stabilny `resource_type`,
`resource_id`, actor user, org, partycje i allowliste pol.

Kolejnosc implementacji Core Sync:

1. Zrobione: `core_registry` z descriptorami tabel, partycji i testami.
2. Zrobione: wewnetrzna tabela core capture w glownej SQLite:
   `__tentaflow_core_sync_captures`, analogiczna do addonowego
   `__tentaflow_sync_captures`.
3. Zrobione: `CoreWriteCapture` i helper `record_core_write_capture(tx, ...)`.
4. Zrobione: integracja z repozytorium Flow Buildera: `flows`, `flow_versions`,
   `flow_model_bindings`.
5. Zrobione: integracja z repozytorium userow/grup/org membership:
   `user_accounts`, `user_groups`, `group_members`, `organizations`,
   `org_memberships`. Hashe hasel nie trafiaja do `changed_fields`.
6. Zrobione: drainer runtime dla core captures zapisuje operacje z
   `addon_id = core` i partycjami `core/org/...`, oznacza capture jako
   `ledgered` albo `error` i nie zapisuje sukcesu, gdy runtime nie jest gotowy.
7. Zrobione: incoming core operations ida przez dedykowany materializer z
   allowlista tabel/pol i parametryzowanymi zapytaniami. Konflikty trafiaja w
   stan inbox conflict; nie ma wykonywania dowolnego SQL z payloadu.
8. Zrobione: field-level merge dla duplicate `INSERT` po primary key uzywa
   `ON CONFLICT ... DO UPDATE` i scala tylko allowlistowane pola z operacji.
9. Field allowlist i redakcja wartosci wrazliwych: hashe hasel, sekrety, tokeny i
   klucze nie moga byc synchronizowane plaintextem.
10. Zrobione: policy epoch dla permissions sync jest trzymany w `settings`
   jako `sync.permission_epoch:{org_id}`. Epoka rosnie przy zmianach node
   identity/assignment, roli/is_active usera, profilu org, ACL zasobu, explicit
   shares, policies i org membership; `SyncOperation.policy_epoch` dostaje
   aktualna wartosc.
11. Zrobione: pending outbox jest rewalidowany tuz przed wyslaniem do target node.
    Cofniecie ACL/roli po zakolejkowaniu operacji blokuje push i zamyka lokalny
    wpis outbox bez wyslania danych do cofniętego targetu.
12. Testy multi-node: flow utworzony na nodzie A pojawia sie na B; rola zmieniona
    na A zmienia effective permissions na B; user przypisany do noda dostaje
    tylko swoje/dozwolone zasoby. Czesc zrobiona: testy runtime sprawdzaja
    push/materializacje `core.flow`, targetowanie po ACL usera oraz targetowanie
    admina org bez ACL zasobu.

## Dostarczanie Operacji

```text
1. Node A zapisuje operacje.
2. Operacja trafia do ledgera i outbox.
3. A wysyla operacje do dostepnych trusted peers.
4. Node C odbiera, weryfikuje i zapisuje w inbox.
5. C moze pozniej przekazac operacje do B.
6. B wraca online i pyta A albo C o brakujace operacje.
7. B weryfikuje podpis, hash-chain, policy epoch i uprawnienia.
8. B aplikuje patch do SQLite.
9. B wysyla ACK.
```

Operacja moze dojsc przez dowolny zaufany node. Nie wymaga centralnego serwera.

## Consensus i Policy Ledger

Nie robimy globalnego consensusu dla kazdej zmiany kontaktu, notatki czy kalendarza.

Dla zwyklych danych:

```text
signed operation log
hash-chain
HLC ordering
merge rules
eventual consistency
```

Dla polityk i bezpieczenstwa powstaje `org_policy_ledger`.

Obejmuje:

```text
zmiany adminow
revocation user/device/node
role i permission policies
sync policies
wybor authority nodes
threshold/quorum config
emergency lock/recovery
```

Operacja polityki:

```text
policy_op_id
org_id
epoch
action
payload
required_threshold
signatures[]
prev_policy_hash
new_policy_hash
effective_at
```

Tryby zatwierdzania:

```text
1-of-1 authority
M-of-N admin quorum
threshold by role
emergency revoke
```

Kod i schema musza obslugiwac model docelowy od poczatku. Mala instalacja moze uzyc `1-of-1`, ale nie wolno projektowac systemu tak, jakby quorum mialo byc dopisane pozniej osobnym mechanizmem.

## Konflikty

Strategie konfliktow:

| Typ danych | Strategia |
|------------|-----------|
| Scalar field | HLC + actor priority albo explicit conflict |
| Tags / sets | OR-Set |
| Counters | PN-Counter |
| Relacje CRM | Add/remove edge jako osobne operacje |
| Text notes | Na start field-level merge, docelowo CRDT tekstowy |
| Delete | Tombstone z TTL |

Konflikt nierozwiazywalny trafia do `sync_conflicts` i jest widoczny w GUI:

```text
Settings -> Sync -> Conflicts
```

Backend konfliktow:

```text
list_sync_conflicts(org_id, addon_id, status, limit)
resolve_sync_conflict(org_id, addon_id, operation_id, resolution)
resolve_addon_sync_conflict(...) przez sync::runtime aktualizuje tez inbox ledgera
```

Binary protocol dla GUI:

```text
MessageBody::SyncConflictBody(SyncConflictPayload::ListRequest)
MessageBody::SyncConflictBody(SyncConflictPayload::ResolveRequest)
```

Handler `sync_conflict_dispatch` wymaga polityki `Admin`, wiec dostep idzie
przez normalny binary dispatcher i role z sesji TentaFlow, bez endpointow REST.
Dashboard ma zakladke `Settings -> Sync`, ktora uzywa tych requestow do
listowania konfliktow oraz akcji `keep_local`, `ignore`, `accept_remote`.

Strategie resolve:

```text
keep_local: konflikt zamkniety jako ignored, lokalny stan zostaje bez zmian
ignore: jawnie ignoruje operacje remote
accept_remote: ponawia remote write; dla INSERT usuwa lokalny wiersz po primary key w tej samej transakcji
```

## Snapshoty i Kompaktowanie

Snapshot:

```text
partition_id
snapshot_id
created_at
included_until_op
state_hash
ledger_root_hash
policy_epoch
signature
blob_refs
```

Nowy node moze dostac:

```text
snapshot + operacje po snapshotcie
```

Restore ma dwa tryby:

- z lokalnej historii ledgera: snapshot jest checkpointem integralnosci, a SQLite
  jest odtwarzane z operacji od `from_sequence` do aktualnego heada;
- z SQL snapshot package: snapshot podpisuje `blob_kind`, `blob_hash` i
  `blob_size_bytes`, a blob niesie zweryfikowany prefiks operacji SQL. Ten tryb
  dziala po usunieciu prefiksu z glownych rekordow ledgera i aplikuje blob +
  operacje po snapshocie.

`SnapshotPackageStore` utrwala te bloby content-addressed w
`<TENTAFLOW_HOME>/sync/snapshot-blobs/<kind>/<hash-prefix>/<hash>.bin`, dzieki
czemu sciezka nie zalezy od `snapshot_id` ani `partition_id`.

Mesh transport tych blobow uzywa `SyncSnapshotPull` i `SyncSnapshotResponse`.
Responder pobiera podpisany snapshot z ledgera, czyta blob z
`SnapshotPackageStore` i dolacza opcjonalny tail operacji. Receiver zapisuje
snapshot metadata, waliduje i utrwala blob content-addressed oraz wrzuca tail
operations do inboxa, gdzie przechodza ta sama weryfikacje, idempotencje i ACK
co zwykle operacje delta. Gdy zwykly `SyncPull` nie moze zostac obsluzony
ciaglym zakresem operacji, bo prefiks zostal skompaktowany, responder odsyla
snapshot response zamiast dziurawego pull response. Receiver aplikuje blob SQL
package do SQLite przed ustawieniem cursora snapshotu, zeby nie przeskoczyc
zakresu danych bez materializacji stanu.

Zasady kompaktowania:

```text
nie usuwac operacji bez snapshotu
nie usuwac operacji niepotwierdzonych przez wymagane nody
nie usuwac prefiksu partycji, jezeli ma niepotwierdzony outbox dla dowolnego targetu
tryb finality moze wymagac wszystkich targetow, jawnej listy targetow albo quorum M-of-N
nie kompaktowac globalnie po numerze sekwencji, tylko per partycja
snapshot musi miec utrwalony SQL snapshot package w SnapshotPackageStore
nie usuwac tombstone przed TTL
policy/security log ma osobna minimalna retencje
kompaktowanie musi zostawic checkpoint z root hash
```

## Retencja i Dysk

Domyslnie:

```text
ledger_retention = unlimited
```

Opcje GUI:

```text
bez limitu
30 dni
90 dni
6 miesiecy
12 miesiecy
limit GB
```

Alerty:

```text
20% wolnego miejsca: info
10% wolnego miejsca: warning
5% wolnego miejsca: critical
```

Przy stanie critical:

```text
blokada nowych duzych blobow
propozycja snapshot/compaction
lista partycji zajmujacych miejsce
lista nodow blokujacych kompaktowanie przez brak ACK
informacja o przewidywanym czasie do zapelnienia dysku
```

## GUI

Nowe sekcje platformy:

```text
Settings -> Devices
Settings -> Users & Org
Settings -> Permissions
Settings -> Sync
Settings -> Storage
Settings -> Audit
```

Dla addona:

```text
Addon -> Overview
Addon -> Permissions
Addon -> Sync
Addon -> Data
Addon -> Audit
```

Zakladka Sync pokazuje:

```text
zasob/tabela
tryb sync
authority node
offline allowed
retencja
rozmiar danych
ostatni sync
zalegle operacje
nody z opoznieniem
konflikty
```

## Uzycie Przez Addony

Addon uzywa Core Storage API:

```text
storage.sql_exec(...)
storage.sql_query(...)
storage.blob_put(...)
storage.blob_get(...)
```

Core przy zapisie:

```text
1. identyfikuje addon/resource/table
2. sprawdza permission
3. wykonuje zapis w SQLite
4. buduje patch operacji
5. zapisuje operacje w ledgerze
6. wrzuca operacje do outbox wedlug sync policy
```

Wymog:

```text
brak bezposredniego obejscia Core Storage API
kazdy zapis idzie przez kontrolowana sciezke
```

## Stary CRDT

Obecny `mesh/crdt.rs` i `mesh/crdt_store.rs` wymagaja audytu.

Dozwolone wyniki audytu:

```text
1. przenosimy przydatne typy i merge rules do nowego sync/ledger
2. rozbudowujemy istniejacy kod w miejscu, jesli da sie go uczynic docelowym ledgerem
3. usuwamy stary CRDT i piszemy nowy mechanizm czysto
```

Niedozwolone:

```text
utrzymywanie starego CRDT jako drugiego rownoleglego syncu
fallback na stary sync
kompatybilnosc wsteczna, ktora utrudnia nowa architekture
```

Dla istniejacego stanu danych:

```text
1. tworzymy genesis snapshot per partycja
2. podpisujemy snapshot
3. zapisujemy root hash
4. ledger startuje od pierwszej operacji po snapshotcie
```

## Wynik Spike RocksDB

Szczegoly sa zapisane w `docs/ROCKSDB_BUILD_MATRIX_SPIKE.md`.

Status:

```text
Linux x86_64: OK
Android aarch64: OK po jawnej konfiguracji NDK CC/CXX/AR/linkera
Android x86_64: OK po jawnej konfiguracji NDK CC/CXX/AR/linkera
iOS: zablokowane lokalnie przez brak Xcode xcrun na Linux
macOS: wymaga runnera macOS
Windows: wymaga runnera Windows z MSVC albo MinGW
```

Wniosek po spike: RocksDB da sie zbudowac lokalnie dla Linux i Android, ale jego
C++ toolchain zwieksza ryzyko utrzymania build matrix. Po pozniejszym benchmarku
i decyzji projektowej RocksDB nie jest domyslnym storage Sync Ledger.

## Benchmark Rustowych KV

Szczegoly sa zapisane w `docs/KV_STORE_BENCHMARK_REDB_FJALL.md`.

Wynik benchmarku `redb 4.1.0`, `fjall 3.1.4` i `rocksdb 0.24.0`:

```text
redb: mocny profil read-heavy, ale slabszy update-heavy path
fjall: wybrany storage Sync Ledger; pure Rust, dobry insert/update i prostszy mobile build
rocksdb: bardzo szybki insert/scan i najmniejszy dysk, ale C++ toolchain i wieksze ryzyko cross-build
```

Dla glownego Sync Ledger wybrany zostaje `fjall`. `redb` odpada jako pierwszy
wybor dla append/update-heavy ledgera. `rocksdb` pozostaje punktem odniesienia
wydajnosci i mozliwa przyszla opcja server-only, ale nie jest planowanym
storage dla implementacji docelowej.

## Zadania

| ID | Zadanie | Zlozonosc | Zaleznosci | Obszar |
|----|---------|-----------|------------|--------|
| 1 | Dodanie `fjall` do zaleznosci `tentaflow-core` | S | - | storage |
| 2 | Audyt `mesh/crdt.rs`, `mesh/crdt_store.rs`, obecnego CRDT sync i miejsc zapisu addonow | M | - | sync |
| 3 | Decyzja techniczna: rozbudowa CRDT w miejscu albo usuniecie i nowy `sync/ledger` | S | 2 | sync |
| 4 | Projekt `sync/ledger` i trait `SyncLedgerStore` | M | 1, 3 | sync |
| 5 | Implementacja `FjallSyncLedgerStore` | L | 4 | storage |
| 6 | Format `SyncOperation`, `PartitionId`, `OperationId`, `PeerCursor`, `SyncSnapshot` | L | 4 | sync |
| 7 | Podpisy operacji, hash-chain per partycja i Merkle summary | L | 6 | security |
| 8 | Specyfikacja crypto-ready: canonical encoding, stable hash, przyszly Block Builder contract | M | 6, 7 | architecture |
| 9 | Device Registry schema i repozytorium | L | - | identity |
| 10 | Users/Org/Roles schema brakujacych elementow pod effective permissions | L | - | identity |
| 11 | Permission Engine: owner, department, manager_subtree, explicit_share, admin, authority_node | XL | 9, 10 | permissions |
| 12 | Policy Ledger z epoch, threshold signatures i quorum model | XL | 7, 9, 10 | security |
| 13 | Sync Policy schema i walidacja trybow: local_only, replicated_by_permission, authority_readthrough, authority_write, sharded, ephemeral | L | 11, 12 | sync |
| 14 | Interceptor zapisow addon SQL w Core Storage API | XL | 11, 13 | addons |
| 15 | Budowanie patchy insert/update/delete z before_hash i after_hash | L | 14 | sync |
| 16 | Outbox, inbox, retry queue, deduplikacja i idempotent apply | XL | 5, 7, 15 | sync |
| 17 | Peer cursors, ACK, brakujace operacje i pull by partition | L | 16 | sync |
| 18 | Mesh Sync Protocol dla operacji, snapshotow, ACK i Merkle summaries | XL | 16, 17 | mesh |
| 19 | Snapshot Manager: genesis snapshot, periodic snapshot, restore from snapshot | L | 5, 7, 18 | sync |
| 20 | Compaction Manager z retencja unlimited domyslnie i politykami opcjonalnymi | L | 19 | storage |
| 21 | Storage Monitor: rozmiary SQLite/Fjall/blobow, alerty 20/10/5%, blokady critical | Zrobione backend + binary report; zostaja akcje operatora | 20 | storage |
| 22 | Conflict Manager i tabela `sync_conflicts` | L | 16 | sync |
| 23 | GUI Settings -> Devices | L | 9 | frontend |
| 24 | GUI Settings -> Users & Org | L | 10 | frontend |
| 25 | GUI Settings -> Permissions | L | 11 | frontend |
| 26 | GUI Settings -> Sync | XL | 13, 17, 22 | frontend |
| 27 | GUI Settings -> Storage | Zrobione bazowe read-only; zostaja akcje compaction/cleanup | 21 | frontend |
| 28 | GUI Settings -> Audit | M | 7, 12 | frontend |
| 29 | Addon Settings -> Permissions / Sync / Data / Audit | XL | 13, 25, 26 | frontend |
| 30 | Migracja starego stanu do genesis snapshotow | L | 19 | migration |
| 31 | Usuniecie albo przepisanie starego CRDT bez rownoleglego fallbacku | M | 3, 18, 30 | cleanup |
| 32 | Contacts jako pierwszy addon syncowany end-to-end | XL | 14-22, 26 | contacts |
| 33 | Testy jednostkowe ledgera, podpisow, hash-chain, retencji i kompaktowania | L | 5-22 | tests |
| 34 | Testy permissions: owner, department, manager_subtree, explicit_share, revocation | L | 11, 12 | tests |
| 35 | Testy mesh: offline, reconnect, multi-hop delivery, duplicate delivery, missing operations | XL | 18 | tests |
| 36 | Testy storage pressure: 20/10/5%, brak miejsca, blokada blobow, kompaktowanie | Bazowe testy monitora i blokady blobow zrobione; zostaje kompaktowanie/GUI | 21 | tests |
| 37 | Testy Contacts sync: create/update/delete, conflict, device revocation, permission filtering | XL | 32 | tests |
| 38 | Dokumentacja API Sync Ledger, operator guide i opis GUI | M | calosc | docs |
| 39 | Code review security/OWASP i review architektury po implementacji | M | calosc | review |
| 40 | Core Sync Registry dla Flow Buildera, userow, grup i rol | M | 6, 13 | core-sync |
| 41 | Core capture table i helper zapisu w transakcji core SQLite | L | 40 | core-sync |
| 42 | Capture zapisow Flow Buildera: `flows`, `flow_versions`, `flow_model_bindings` | L | 41 | core-sync |
| 43 | Capture zapisow identity/RBAC: `user_accounts`, `user_groups`, `group_members`, `roles`, `org_memberships` | XL | 41 | core-sync |
| 44 | Core Sync drainer do ledgera z `addon_id=core` i partycjami `core/org/...` | L | 41, 42, 43 | core-sync |
| 45 | Materializer incoming core operations z allowlista pol i konfliktami | XL | 44 | core-sync |
| 46 | Field-level security dla core sync: brak plaintext sekretow, tokenow i kluczy | L | 45 | security |
| 47 | E2E multi-node dla flowbuilder/user/role/group sync | XL | 42-46 | tests |
| 48 | Chunked/blob package sync dla bardzo duzych plikow RAG i mediow | Zrobione bazowe chunkowanie 1 MiB; zostaje adaptive chunk size/backpressure | 17, 19, 21 | storage |
| 49 | Central-only Storage Proxy SQL/KV/Blob bez lokalnej materializacji | Zrobione backend; GUI status zostaje osobno | 13, 18, 21 | sync |
| 50 | Core sync control-plane: identity, node assignments, org profile, policies, ACL, explicit shares | Zrobione backend i materializer allowlist | 40-45 | core-sync |

## Kolejnosc Wykonania

```text
1
2 -> 3
4 -> 5 -> 6 -> 7
6 + 7 -> 8
9 -> 10 -> 11
7 + 9 + 10 -> 12
11 + 12 -> 13
13 -> 14 -> 15 -> 16 -> 17 -> 18
18 -> 19 -> 20 -> 21
16 -> 22
19 -> 30 -> 31
14-22 + 26 -> 32
33-37 przez caly etap implementacji
38-39 na koncu
```

## Kryteria Akceptacji

- `tentaflow-core` ma `fjall` jako normalna zaleznosc pod Sync Ledger.
- SQLite pozostaje baza aktualnego stanu addonow.
- Addon nie decyduje o syncu.
- Kazdy zapis addonowy przechodzi przez Core Storage API.
- Kazda operacja jest podpisana po przydzieleniu sekwencji i `prev_partition_hash`, hashowana i zapisana w ledgerze.
- Format operacji ma canonical encoding, stabilny hash, domain-separated signing bytes i moze zostac zgrupowany w blok bez zmiany API addonow.
- Inbox zapisuje tylko operacje, ktore przeszly walidacje integralnosci i podpisu.
- Merkle summary obejmuje tylko spojny, ciagly zakres operacji z jednej partycji.
- Node offline po powrocie pobiera brakujace operacje.
- Operacja moze dojsc przez innego zaufanego noda.
- Node dostaje tylko dane, do ktorych ma prawo.
- Urzadzenie handlowca nie zawiera calej bazy CRM, jesli handlowiec nie ma do niej praw.
- Mozna ustawic zasob jako `local_only`.
- Mozna ustawic zasob jako `authority_readthrough`.
- Mozna ustawic zasob jako `authority_write`.
- Domyslna retencja jest bez limitu.
- System ostrzega przy 20%, 10% i 5% wolnego dysku.
- Snapshot pozwala dolaczyc nowemu nodowi bez pobierania calej historii.
- Stary CRDT nie zostaje jako drugi rownolegly mechanizm.
- Contacts dziala jako pierwszy realny addon synchronizowany.
- Testy obejmuja offline, reconnect, multi-hop, konflikt, revocation, permission filtering i brak miejsca.

## Otwarte Ryzyka

| Ryzyko | Wplyw | Mitygacja |
|--------|------|-----------|
| Fjall moze ujawnic problemy specyficzne dla filesystemu mobile | Sredni | Testy integracyjne ledgera na realnych sciezkach danych aplikacji |
| Sync patch dla arbitralnego SQL moze byc trudny | Wysoki | Ograniczyc zapisy addonow do Core Storage API z kontrolowanym execute/intercept |
| Permission filtering moze byc kosztowny | Sredni | Effective access cache i partycje per zakres dostepu |
| Niepotwierdzone nody moga blokowac kompaktowanie | Sredni | GUI pokazuje blokujace nody, admin moze zmienic wymagania ACK po polityce quorum |
| Konflikty CRM moga wymagac domenowych reguly | Sredni | Per-resource merge strategies i `sync_conflicts` |
| Policy quorum moze zablokowac organizacje po utracie adminow | Wysoki | Emergency recovery policy i jawnie skonfigurowane recovery keys |
