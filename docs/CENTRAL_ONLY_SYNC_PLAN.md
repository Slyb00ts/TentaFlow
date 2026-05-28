# Plan: Central only dla danych addonów

## Podsumowanie

Tryb `Central only` oznacza, że dane danego zasobu addona są utrzymywane wyłącznie na wskazanym `authority_node_id`. Pozostałe nody znają politykę, uprawnienia i lokalizację authority, ale nie dostają trwałej kopii danych; odczyt i zapis przechodzą online przez mesh do authority node.

## Wymagania

- SQLite pozostaje lokalnym storage aktualnego stanu na nodzie authority.
- Addon nadal używa wyłącznie Core Storage API: SQL, KV i Blob.
- Addon nie definiuje trybu synchronizacji w manifest/TOML.
- Polityka jest ustawiana w TentaFlow GUI per organizacja, addon, typ zasobu albo konkretny zasób.
- Control-plane musi synchronizować minimalne metadane: nody, użytkowników, role, polityki sync, ACL i mapę authority.
- Dane addona w trybie central-only nie mogą materializować się na nodach bez `sync_receive`.
- Brak sieci dla central-only domyślnie daje błąd `authority_unreachable`; offline write nie jest włączany bez osobnej polityki.

## Semantyka trybów

| Tryb GUI | Tryb backendowy | Znaczenie |
| --- | --- | --- |
| Local only | `local_only` | Dane zostają tylko na lokalnym nodzie. |
| Sync offline | `replicated_by_permission` | Dane są replikowane do nodów, którym Permission Engine pozwala dostać kopię. |
| Central only | `authority_readthrough` | Odczyty i duże dane idą online do authority; lokalnie brak trwałej kopii. |
| Central write | `authority_write` | Zapisy idą przez authority; lokalna kopia może wynikać tylko z osobnej polityki replikacji. |
| Ephemeral | `ephemeral` | Brak trwałego syncu i brak replikacji. |

## Architektura

```text
Addon WASM
  -> Core Storage API
     -> Sync Policy Resolver
        -> local SQLite/KV/Blob, gdy node jest authority albo zasób jest replikowany
        -> Remote Storage Proxy, gdy zasób jest central-only na innym nodzie
           -> UFP/2 channel SyncStorage
              -> Authority Storage Executor
                 -> permission check
                 -> SQLite/KV/Blob write/read
                 -> ledger capture + audit
```

## Komponenty

| Komponent | Odpowiedzialność |
| --- | --- |
| Sync Policy Resolver | Zwraca efektywną politykę i authority node dla `org/addon/resource`. |
| Resource Directory | Lokalna mapa `org/addon/resource -> mode + authority_node_id + epoch`. |
| Remote Storage Proxy | Przekazuje SQL/KV/Blob requesty do authority node przez binarny protokół. |
| Authority Storage Executor | Wykonuje request po stronie authority z walidacją permissions i audit. |
| Control-plane Sync | Replikuje nody, userów, role, polityki, ACL i mapę authority. |
| Ledger Runtime | Na authority zapisuje operacje, ACK, snapshoty i repair; dla central-only nie fan-outuje danych do klientów. |

## Przepływy

### Odczyt SQL

```text
storage.sql_query(addon, sql)
  -> Core sprawdza policy
  -> jeśli local authority: wykonuje lokalnie
  -> jeśli remote authority: wysyła binary request do authority
  -> authority sprawdza read permission
  -> authority wykonuje parametryzowane query
  -> wynik wraca jako binarny response
```

### Zapis SQL

```text
storage.sql_exec(addon, sql)
  -> Core sprawdza policy
  -> jeśli remote authority: nie zapisuje lokalnie
  -> wysyła write-through do authority
  -> authority sprawdza write permission
  -> authority wykonuje zapis w transakcji
  -> authority zapisuje ledger operation i audit
  -> caller dostaje wynik write
```

### KV

```text
kv_get/kv_put
  -> local gdy authority albo replicated
  -> remote authority gdy central-only
```

### Blob

```text
blob_get
  -> manifest metadata może być widoczny lokalnie
  -> chunk bytes pobierane są z authority na żądanie

blob_put
  -> upload chunked do authority
  -> authority finalizuje hash, zapisuje manifest i ledger
```

## Zadania

| ID | Zadanie | Złożoność | Zależności |
| --- | --- | --- | --- |
| C1 | Doprecyzować enum trybów w kodzie zamiast gołych stringów dla `sync_policies.mode` | M | - |
| C2 | Dodać resolver `EffectiveStorageLocation` dla SQL/KV/Blob | M | C1 |
| C3 | Dodać Resource Directory dla authority map i policy epoch | M | C1 |
| C4 | Dodać binarne UFP/2 payloady `StorageRead`, `StorageWrite`, `KvGet`, `KvPut`, `BlobGetChunk`, `BlobPutChunk` | L | C2 |
| C5 | Dodać Remote Storage Proxy po stronie host functions SQL/KV/Blob | L | C2, C4 |
| C6 | Dodać Authority Storage Executor po stronie odbiorcy mesh | L | C4 |
| C7 | Wpiąć permission check i audit po stronie authority | M | C6 |
| C8 | Zablokować lokalną materializację danych central-only na nodach bez `sync_receive` | M | C2 |
| C9 | Dodać błędy `authority_unreachable`, `authority_denied`, `authority_timeout` | S | C5 |
| C10 | Dodać testy jednostkowe resolvera trybów i targetów | M | C1, C2 |
| C11 | Dodać testy procesowe 4 nodów: authority + replicated + 2 central-only | L | C8 |
| C12 | Dodać testy remote SQL/KV/Blob read-through/write-through | XL | C4-C7 |
| C13 | Dodać GUI status central-only: authority, connectivity, dry-run targetów | M | C3 |

Status backendu bez GUI i PostgreSQL:

- C1 jest zrobione: `sync_policies.mode` jest typowany przez `SyncPolicyMode`, nie przez luzny string.
- C2/C4/C5/C6/C7/C8/C9 sa zrobione dla SQL, KV i Blob przez binarny Storage Proxy.
- C10/C11 sa zrobione w testach jednostkowych i `process_four_node_sync`.
- C12 jest zrobione backendowo dla SQL/KV oraz dla chunked Blob proxy po stronie authority/client.
- C13 zostaje jako osobny etap GUI.

## Test 4 nodów

Docelowy scenariusz:

```text
Node A: authority dla addona
Node B: ma prawo sync_receive i dostaje lokalną kopię
Node C: central-only client, nie dostaje danych lokalnie, odpytuje A online
Node D: central-only client, nie dostaje danych lokalnie, odpytuje A online
```

Kryteria:

- zapis na A materializuje się na B,
- zapis na A nie materializuje się na C/D,
- C/D nie dostają payloadu push,
- C/D nie mogą pobrać danych przez repair/snapshot bez `sync_receive`,
- po implementacji proxy C/D mogą czytać przez remote read-through z A,
- po implementacji proxy zapis z C/D jest wykonywany na A, a nie lokalnie.

## Ryzyka

| Ryzyko | Wpływ | Mitygacja |
| --- | --- | --- |
| Central-only bez Resource Directory nie wie gdzie pytać | Wysoki | Control-plane musi być replikowany niezależnie od danych addona. |
| Remote query może omijać ACL | Wysoki | Permission check wyłącznie po stronie authority przed wykonaniem storage. |
| Blob cache zostawi trwałą kopię danych | Średni | Cache techniczny musi być TTL/temp albo wyłączony dla central-only. |
| Offline write stworzy niespójność | Wysoki | Domyślnie odrzucać offline write dla central-only. |
| Mieszanie replikacji i read-through dla tego samego zasobu | Średni | Polityka musi jawnie rozróżniać targety materializujące dane od klientów read-through. |

## Stan obecny

- Ledger, outbox, inbox, ACK, repair, snapshot i blob chunks są zaimplementowane.
- `sync_policies` obsługują tryby `authority_readthrough`, `authority_write`, `replicated_by_permission`, `sharded`, `local_only` i `ephemeral` jako enum backendowy.
- Remote Storage Proxy obsługuje SQL, KV i chunked Blob przez binarny protokół mesh.
- Host functions SQL/KV kierują central-only read-through/write-through do authority node.
- Authority wykonuje permission check przed storage proxy i odrzuca brak `read`, `write` albo `sync_receive`.
- Test `process_four_node_sync` pokrywa authority + replicated receiver + central-only clients bez lokalnej materializacji danych.
