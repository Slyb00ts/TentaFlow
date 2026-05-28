# Addon SDK: ACL i synchronizacja danych

Ten dokument opisuje, jak addony powinny opisywac widocznosc swoich danych dla synchronizacji TentaFlow. Dotyczy to SQL, KV i blobow addona.

## Zasada glowna

Addon nie decyduje sam, do ktorych nodow synchronizowac dane. Tryb synchronizacji ustawia TentaFlow Core w konfiguracji addona i zasobow.

Addon odpowiada za przekazanie metadanych domenowych zasobu:

| Pole | Znaczenie |
|------|-----------|
| `resource_type` | Typ zasobu w addonie, np. `contact`, `company`, `calendar_event` |
| `resource_id` | Stabilne ID zasobu w addonie |
| `owner_user_id` | Uzytkownik, ktory jest wlascicielem danych |
| `assigned_user_id` | Uzytkownik przypisany do pracy na zasobie |
| `department_id` | Dzial, ktory ma widziec zasob |
| `manager_user_id` | Uzytkownik, od ktorego liczymy drzewo podwladnych |
| `visibility_scope` | Regula widocznosci, np. `private`, `assigned`, `department`, `manager_subtree`, `all` |

Core zapisuje te informacje w swoich tabelach ACL i na ich podstawie decyduje, czy konkretny node moze:

- dostac lokalna kopie danych (`sync_receive`),
- czytac dane przez central-only proxy (`read`),
- zapisywac dane przez central-only proxy (`write`),
- zarzadzac regulem udostepniania (`admin`).

## Czy addon ma dodawac `owner_user_id` i `assigned_user_id` do swoich tabel?

Nie jako mechanizm synchronizacji.

Addon moze miec takie kolumny w swojej tabeli, jezeli sa czescia modelu biznesowego, np. kontakt faktycznie ma handlowca prowadzacego albo osobe przypisana. Nie wolno jednak traktowac tych kolumn jako jedynego zrodla prawdy dla synca.

Zrodlem prawdy dla synchronizacji jest Core ACL:

- `sync_policies` okresla tryb sync dla addona, typu zasobu albo konkretnego zasobu,
- `sync_resource_acl` opisuje regule widocznosci zasobu,
- `sync_explicit_shares` trzyma wyjatki i reczne udostepnienia.

Addon po zapisie lub zmianie danych musi zaktualizowac ACL w Core. Core pozniej sam filtruje outbox, snapshoty, repair i central-only proxy.

## Widocznosc zasobow

`visibility_scope` powinien byc jednym z ponizszych trybow:

| Scope | Znaczenie |
|-------|-----------|
| `private` | Tylko wlasciciel |
| `own` | Dane zalozone przez danego uzytkownika |
| `assigned` | Wlasciciel oraz uzytkownik przypisany |
| `department` | Uzytkownicy z danego dzialu |
| `manager_subtree` | Przelozony oraz jego podwladni w glab struktury |
| `explicit_share` | Tylko podmioty wskazane w `sync_explicit_shares` |
| `all` | Wszyscy uzytkownicy organizacji |

Core laczy scope z przypisaniem noda do uzytkownika. Node nie jest automatycznie "urzadzeniem firmy"; node dostaje dane tylko wtedy, gdy jest przypisany do uzytkownika, grupy albo ma jawna role uprawniajaca do odbioru.

## Przyklad: kontakt w CRM

Kontakt utworzony przez uzytkownika `12`, przypisany do handlowca `31`:

```text
addon_id = "contacts"
resource_type = "contact"
resource_id = "contact_01J..."
owner_user_id = 12
assigned_user_id = 31
visibility_scope = "assigned"
```

Efekt:

- node wlasciciela moze dostac lokalna kopie,
- node przypisanego handlowca moze dostac lokalna kopie,
- inne nody nie materializuja kontaktu lokalnie,
- central-only node z prawem `read` moze pobrac kontakt online z authority node,
- central-only node z prawem `write` moze zapisac zmiane online przez authority node.

Kontakt widoczny dla calego dzialu sprzedazy:

```text
resource_type = "contact"
resource_id = "contact_01J..."
department_id = 7
visibility_scope = "department"
```

Kontakt widoczny dla przelozonego i calego drzewa podwladnych:

```text
resource_type = "contact"
resource_id = "contact_01J..."
manager_user_id = 4
visibility_scope = "manager_subtree"
```

## Cykl zycia zapisu

1. Addon zapisuje dane przez SDK TentaFlow, np. SQL/KV/blob.
2. Addon przekazuje lub aktualizuje metadane ACL dla zasobu.
3. Core zapisuje operacje w ledgerze i outboxie.
4. Core wybiera targety synchronizacji na podstawie polityki sync oraz ACL.
5. Node bez prawa `sync_receive` nie dostaje lokalnej kopii.
6. Node w trybie central-only moze korzystac z danych online tylko jezeli ma `read` albo `write`.

Zmiana wlasciciela, przypisania, dzialu albo scope musi aktualizowac ACL. Od tego momentu kolejne operacje, snapshoty i repair uzywaja nowej reguly.

## Interfejs SDK

Addon nie powinien pisac bezposrednio do tabel Core. SDK udostepnia host functions dla ACL przez binarny payload CBOR:

```text
sync_acl_upsert(resource_type, resource_id, owner_user_id, assigned_user_id, department_id, manager_user_id, visibility_scope)
sync_acl_delete(resource_type, resource_id)
sync_share_grant(resource_type, resource_id, subject_type, subject_id, action, granted_by)
sync_share_revoke(resource_type, resource_id, subject_type, subject_id, action)
```

`subject_type` okresla, komu nadajemy wyjatek:

| `subject_type` | `subject_id` |
|----------------|--------------|
| `user` | ID uzytkownika |
| `node` | ID konkretnego noda |

`action` powinno byc jednym z:

- `read`,
- `write`,
- `sync_receive`,
- `admin`.

Te funkcje aktualizuja tabele Core i podbijaja `sync.permission_epoch:{org_id}`. Zmiana ACL albo share jest rowniez zapisywana jako core sync capture, wiec control-plane dochodzi do innych nodow przez Sync Ledger.

## Central-only

Tryb central-only oznacza, ze dane nie sa materializowane lokalnie na nodzie klienta. Dane sa pobierane online z authority node przez binarny storage proxy.

Warunki dzialania:

- polityka sync wskazuje authority node,
- node klienta zna authority node i ma transport mesh,
- `sync_resource_acl` albo `sync_explicit_shares` daje nodowi akcje `read` lub `write`,
- authority node ma lokalne dane i obsluguje dany storage kind,
- SQL, KV i Blob ida przez binarny Storage Proxy; Blob uzywa chunked put/get z walidacja hashy po stronie authority.

Central-only nie jest cachem. Jezeli authority node jest offline, odczyt lub zapis powinien zwrocic blad zamiast uzywac starej lokalnej kopii.

## Zasady dla autorow addonow

- Nie dodawaj wlasnego systemu sync w addonie.
- Nie zakladaj, ze kazdy node ma cala baze addona.
- Nie uzywaj kolumn domenowych jako jedynego filtra synchronizacji.
- Zawsze nadaj stabilne `resource_id` dla danych, ktore maja podlegac syncowi.
- Aktualizuj ACL po kazdej zmianie wlasciciela, przypisania, dzialu albo widocznosci.
- W narzedziach LLM opisuj, czy operacja moze dotyczyc danych prywatnych, dzialowych czy globalnych.
