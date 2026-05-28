# Crypto-ready App Registry, Policy Ledger i Consensus

## Cel

TentaFlow ma byc przygotowany pod model dzialania podobny do systemow krypto:
operacje sa podpisane, hashowane, ulozone w hash-chain, moga zostac zebrane w
bloki i sfinalizowane przez quorum validatorow. Domyslnie system ma jednak
dzialac bez consensusu, bez tokenow i bez publicznego blockchaina.

Najwazniejsza zasada: addony, SQLite, KV, Blob i Flow Builder nie moga zalezec
od tego, czy consensus jest wlaczony. Wszystko nadal zapisuje przez TentaFlow
Core. Crypto mode jest warstwa finalizacji, governance i dowodow, a nie osobnym
API dla addonow.

## Tryby pracy

| Tryb | Domyslny | Znaczenie |
|------|----------|-----------|
| `local_ack` | Tak | Operacja jest podpisana, trafia do ledgera, idzie do outbox i zamyka sie po ACK targetow. |
| `quorum_ack` | Nie | Operacja wymaga ACK od skonfigurowanego quorum nodow, ale bez budowania blokow. |
| `block_finality` | Nie | Operacje trafiaja do mempoola, sa grupowane w bloki i finalizowane certyfikatem validatorow. |

Domyslny tryb musi byc `local_ack`, zeby TentaFlow dzialal offline-first i bez
centralnego serwera. `block_finality` wlacza sie per organizacja, per typ danych
albo tylko dla control-plane.

## Zero Trust

System zaklada brak zaufania do wszystkiego:

- node moze klamac,
- relay moze byc wrogi,
- paczka addona moze byc podstawiona,
- operator moze probowac ominac GUI,
- stare operacje moga byc odtworzone ponownie,
- peer moze wyslac poprawnie zakodowany, ale niedozwolony payload,
- validator moze podpisac zla propozycje,
- storage moze byc uszkodzony albo czesciowo skompromitowany.

Kazdy element musi byc weryfikowany lokalnie przed zapisem stanu:

```text
parse binary payload
-> canonical decode
-> schema version check
-> domain-separated hash check
-> signature check
-> replay check
-> policy epoch check
-> permission check
-> dependency/order check
-> apply only through allowlisted materializer
```

Nie wolno zakladac, ze operacja jest poprawna tylko dlatego, ze przyszla od
trusted peer. Trusted peer oznacza prawo do transportu, nie prawo do wykonania
dowolnej zmiany.

## Warstwy

```text
Core Storage API
  -> Sync Ledger
      -> local_ack finality
      -> optional Mempool
          -> Block Builder
              -> Consensus Engine
                  -> Finality Certificate
                      -> Finalized State
```

| Warstwa | Odpowiedzialnosc |
|---------|------------------|
| Sync Ledger | Operacje, hash-chain, outbox, inbox, ACK, snapshoty. |
| Policy Ledger | Krytyczne operacje governance i security. |
| App Registry | Wlascicielstwo addonow, wersje, hashe paczek i polityki instalacji. |
| Mempool | Tymczasowe operacje czekajace na blok. |
| Block Builder | Deterministyczne grupowanie operacji w bloki. |
| Consensus Engine | Glosowanie validatorow nad blokiem. |
| Finality Store | Certyfikaty finalizacji, validator set, wysokosc chaina. |
| Materializer | Stosuje tylko finalne albo dopuszczone optimistic operacje do SQLite/KV/Blob. |

## App Registry

W crypto mode aplikacje/addony musza byc rejestrowane w ledgerze. Sam kod addona
nie trafia do blockchaina. Do ledgera trafiaja metadane, wlasciciel, hashe,
podpisy i decyzje governance. Paczka WASM/UI/migracji/assets jest pobierana
przez content-addressed storage po hash.

```text
ledger:
  addon_ref
  owner_identity
  version
  package_hash
  manifest_hash
  wasm_hash
  migrations_hash
  permissions_hash
  signatures
  status

blob/mesh storage:
  package bytes
```

Node moze zainstalowac paczke tylko wtedy, gdy:

1. wpis wersji addona jest finalny,
2. `addon_ref` jest wlasnoscia podpisujacej tozsamosci albo zatwierdzonego maintainer key,
3. hash pobranej paczki zgadza sie z `package_hash`,
4. hash manifestu, WASM, migracji i deklaracji permissions zgadza sie z wpisem,
5. org install policy pozwala uruchomic ten addon,
6. lokalna polityka noda nie blokuje wymaganych capabilities.

## Addon Ownership

`addon_id` albo `addon_ref` jest zasobem wlasnosciowym. Nikt poza ownerem albo
jawnie zatwierdzonym maintainerem nie moze dodac wersji, aktualizacji, revoke ani
transferu.

Preferowany identyfikator:

```text
addon_ref = owner_did + "/" + addon_name
```

Przyklad:

```text
did:tf:critix/contacts
```

Krotkie nazwy typu `contacts` powinny byc aliasem:

```text
alias = "contacts"
points_to = "did:tf:critix/contacts"
```

Alias tez ma ownera i wymaga finalizowanej operacji transferu albo revoke.

### Rekord ownera

```text
AddonOwnerRecord
  addon_ref
  owner_identity
  owner_policy_hash
  status
  created_block
  updated_block
```

### Polityka ownera

```text
AddonOwnerPolicy
  policy_id
  addon_ref
  mode: single_owner | multisig | org_governed | dao_governed
  threshold
  owner_keys[]
  maintainer_keys[]
  recovery_keys[]
  timelock_seconds
  valid_from_block
```

### Maintainer

```text
AddonMaintainerRecord
  addon_ref
  maintainer_identity
  permissions:
    propose_version
    approve_version
    revoke_version
    manage_permissions
    transfer_ownership
  valid_from
  valid_until
  revoked_at
```

Maintainer nie moze rozszerzyc swoich uprawnien sam. Zmiana maintainerow wymaga
podpisow zgodnych z `AddonOwnerPolicy`.

## Operacje App Registry

| Operacja | Wymagane podpisy | Efekt |
|----------|------------------|-------|
| `AddonNameClaimed` | owner albo governance alias registry | Rezerwuje `addon_ref` albo alias. |
| `AddonMaintainerAdded` | owner policy | Dodaje maintainer key. |
| `AddonMaintainerRevoked` | owner policy | Uniewaznia maintainer key. |
| `AddonVersionProposed` | owner albo maintainer z `propose_version` | Publikuje kandydacka wersje. |
| `AddonVersionApproved` | owner policy | Zatwierdza wersje jako instalowalna. |
| `AddonVersionRevoked` | owner policy albo security emergency policy | Blokuje wersje. |
| `AddonOwnershipTransferStarted` | aktualny owner policy | Rozpoczyna transfer. |
| `AddonOwnershipTransferAccepted` | nowy owner + warunki starej polityki | Konczy transfer. |
| `AddonAliasClaimed` | alias governance | Rezerwuje krotka nazwe. |
| `AddonAliasTransferred` | alias owner policy | Przenosi alias. |
| `AddonInstallPolicyChanged` | org governance | Pozwala, blokuje albo wymusza addon w organizacji. |

Kazda operacja musi miec stabilny `operation_id`, canonical binary encoding,
domain-separated signing bytes i jawny `policy_epoch`.

## Org Install Policy

Globalny registry mowi, ze addon istnieje i dana wersja jest prawdziwa.
Organizacja osobno decyduje, czy chce go uzywac.

```text
OrgAddonInstallPolicy
  org_id
  addon_ref
  version_selector: exact | latest_approved | range
  install_mode: disabled | allowed | required | pinned | auto_update
  approved_by
  required_capabilities_hash
  denied_capabilities[]
  finality_certificate
```

Node instaluje addon tylko, jesli globalny registry i org install policy sa
jednoczesnie poprawne.

## Policy Ledger

Policy Ledger jest osobna partycja albo osobny namespace dla rzeczy, ktore
decyduja o bezpieczenstwie calej organizacji.

Powinien obejmowac:

- `sync_policies`,
- `sync_resource_acl`,
- `sync_explicit_shares`,
- `sync_nodes`,
- `node_user_assignments`,
- `user_identity_keys`,
- role i czlonkostwa organizacji,
- app registry,
- org install policies,
- validator set,
- recovery policies.

Dane zwykle, np. notatka CRM, moga dzialac w `local_ack`. Zmiany security i
governance moga wymagac `quorum_ack` albo `block_finality`.

## Validator Set

```text
ValidatorSet
  validator_set_id
  org_id
  validators:
    node_id
    user_identity
    public_key
    voting_power
    status
  threshold
  valid_from_block
  valid_until_block
  recovery_policy_id
```

Validator set jest sam w sobie operacja governance. Nie wolno zmienic validator
setu zwyklym lokalnym zapisem. Zmiana wymaga dotychczasowego quorum albo jawnej
emergency recovery policy.

## Block Builder

Blok grupuje operacje, ale nie zmienia ich tresci.

```text
BlockHeader
  chain_id
  org_id
  block_height
  prev_block_hash
  operations_root
  state_root_before
  state_root_after
  policy_epoch
  validator_set_id
  proposer_node_id
  timestamp_ms
  protocol_version
```

```text
BlockBody
  operations[]
  dependency_edges[]
  rejected_operation_ids[]
```

Zasady:

- operacje w bloku sa posortowane deterministycznie,
- blok nie moze zawierac operacji z niepoprawnym podpisem,
- blok nie moze przeskoczyc dependency albo policy epoch,
- proposer nie ma prawa zmienic payloadu operacji,
- kazdy validator sam liczy `operations_root` i `state_root_after`.

## Consensus Engine

Consensus musi byc wymienialna warstwa:

```text
trait ConsensusEngine
  propose_block(block)
  validate_proposal(proposal)
  vote(proposal_id, vote)
  collect_finality(proposal_id)
  finalized_blocks(from_height)
```

Implementacje:

| Implementacja | Domyslna | Znaczenie |
|---------------|----------|-----------|
| `NoopConsensusEngine` | Tak | Brak consensusu, system dziala jak obecnie. |
| `QuorumAckConsensus` | Nie | Finality przez threshold ACK/glosow bez pelnego BFT. |
| `BftConsensus` | Nie | Pelna finalizacja blokow przez validator set. |

`NoopConsensusEngine` nie moze udawac finality blokowej. Ma jasno zwracac
`FinalityMode::LocalAck`.

## Finality Certificate

```text
FinalityCertificate
  block_hash
  block_height
  validator_set_id
  threshold
  signatures[]
  finalized_at_ms
```

Node uznaje blok za finalny tylko, gdy:

1. `block_hash` zgadza sie z lokalnie wyliczonym hashem,
2. validator set jest znany i aktywny dla tej wysokosci,
3. podpisy sa unikalne i wazne,
4. suma voting power przekracza threshold,
5. blok kontynuuje znany `prev_block_hash`,
6. operacje w bloku przechodza lokalna walidacje.

## State Root

Na start wystarczy deterministyczny Merkle root zasobow per partycja:

```text
resource_hash = hash(resource_type, resource_id, canonical_materialized_state)
state_root = merkle_root(sorted(resource_hashes))
```

Nie trzeba od razu budowac pelnego Ethereum-style trie. Wazne, zeby:

- format byl binarny i kanoniczny,
- kolejnosc byla deterministyczna,
- hash nie zalezny od lokalnych ID SQLite, jesli nie sa czescia modelu domenowego,
- root mozna bylo odtworzyc po restore snapshotu.

## Instalacja Addona w Crypto Mode

```text
1. Node odbiera finalne `AddonVersionApproved`.
2. Node sprawdza owner policy i finality certificate.
3. Node sprawdza org install policy.
4. Node pobiera paczke po `package_hash`.
5. Node waliduje hash paczki i wszystkich skladowych.
6. Node sprawdza deklarowane permissions/capabilities.
7. Node instaluje albo aktualizuje addon.
8. Node zapisuje lokalny audit install event.
```

Jesli ktorykolwiek krok nie przejdzie, addon nie jest instalowany.

## Revocation i Supply Chain Security

Revocation musi dzialac nawet wtedy, gdy zlosliwy node nadal rozglasza stara
paczke.

Wymagane reguly:

- zrevokowana wersja nie moze byc nowo instalowana,
- auto-update musi omijac zrevokowane wersje,
- node z juz zainstalowana zrevokowana wersja musi przejsc w tryb disabled albo
  wymusic decyzje admina wedlug polityki organizacji,
- revocation musi miec powod i podpis zgodny z owner policy albo emergency
  security policy,
- paczka z poprawnym hashem, ale zrevokowana w registry, nadal jest niedozwolona.

## Emergency Recovery

Zero trust wymaga procedury na utrate kluczy i przejecie maintainer key.

```text
EmergencyRecoveryPolicy
  recovery_keys[]
  threshold
  timelock_seconds
  allowed_actions:
    rotate_owner_keys
    revoke_version
    freeze_addon
    rotate_validator_set
  audit_required
```

Recovery nie moze byc cichym backdoorem. Musi byc finalizowane, audytowane i
widoczne dla wszystkich nodow organizacji.

## Czego Nie Robic

- Nie przechowywac paczek WASM/assets w blockchainie.
- Nie ufac nazwie addona bez sprawdzenia `addon_ref`, ownera i hashy.
- Nie instalowac addona tylko dlatego, ze peer go wyslal.
- Nie pozwalac organizacji przejac globalnego ownera addona; org policy moze
  decydowac o instalacji, nie o cudzej wlasnosci.
- Nie robic fallbacku do niepodpisanych update'ow.
- Nie akceptowac JSON jako formatu decyzyjnego dla ledgera, blokow ani
  finality. Wszystko musi miec canonical binary encoding.
- Nie robic jednego globalnego consensusu dla kazdego rekordu CRM jako default.

## Kolejnosc Implementacji

| ID | Zadanie | Zlozonosc | Zaleznosci |
|----|---------|-----------|------------|
| C1 | Dodac typy `FinalityMode`, `BlockHeader`, `BlockBody`, `FinalityCertificate` | M | - |
| C2 | Dodac storage blokow i certyfikatow w Fjall | L | C1 |
| C3 | Dodac `NoopConsensusEngine` jako domyslna implementacje | M | C1 |
| C4 | Dodac `PolicyLedger` namespace dla security/governance operations | XL | C1-C3 |
| C5 | Dodac App Registry: owner, maintainer, version, alias, install policy | XL | C4 |
| C6 | Dodac walidator owner policy i maintainer permissions | L | C5 |
| C7 | Dodac content-addressed addon package verification | L | C5 |
| C8 | Dodac `BlockBuilder` dla operacji policy/app registry | XL | C2, C4 |
| C9 | Dodac `QuorumAckConsensus` jako etap posredni | XL | C8 |
| C10 | Dodac `BftConsensus` za feature/config flag | XL | C8-C9 |
| C11 | Dodac finality gate dla wybranych resource types | L | C9-C10 |
| C12 | Dodac testy supply-chain: spoofing, unauthorized update, revoked package | XL | C5-C7 |
| C13 | Dodac testy consensus/finality na 4 nodach | XL | C8-C11 |

## Kryteria Akceptacji

- Domyslnie TentaFlow dziala bez consensusu tak jak obecny Sync Ledger.
- Wszystkie nowe typy maja canonical binary encoding i stabilne hashe.
- Ten sam blok ma ten sam hash na kazdym nodzie.
- Nie da sie opublikowac wersji addona bez podpisu ownera albo maintainer policy.
- Nie da sie przejac `addon_ref` po pierwszej finalnej rejestracji.
- Alias nie moze wskazywac na inny addon bez finalizowanego transferu.
- Node nie instaluje paczki, ktorej hash nie zgadza sie z registry.
- Node nie instaluje zrevokowanej wersji.
- Org install policy nie moze zmienic globalnego ownera addona.
- Policy/security operations moga wymagac `quorum_ack` albo `block_finality`.
- Brak consensus engine nie powoduje fallbacku do niepodpisanych albo
  nieweryfikowanych operacji.

## Otwarte Decyzje

| Decyzja | Opcje | Rekomendacja |
|---------|-------|--------------|
| Identyfikator addona | globalny `addon_id` albo `owner_did/name` | `owner_did/name` + alias registry |
| Domyslny finality mode | `local_ack`, `quorum_ack`, `block_finality` | `local_ack` |
| Pierwszy zakres block finality | wszystkie dane albo tylko control-plane | tylko control-plane |
| Consensus | custom BFT, Raft-like dla trusted org, HotStuff/Tendermint-like | zaczac od `QuorumAckConsensus`, potem BFT |
| Storage paczek | ledger, blob store, IPFS-like | content-addressed blob/mesh storage |
| State root | prosty Merkle zasobow, trie | prosty Merkle zasobow |
