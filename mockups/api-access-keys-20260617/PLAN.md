# Plan: twarde zabezpieczenie + rozbudowa kluczy API i uprawnień (z sync mesh)

Status: **DO WERYFIKACJI PRZEZ CODEX** → po akceptacji implementacja → twarde testy.
Branch: `ml-studio`. Repo: `/home/critix/repos/rust/TentaFlow-ml`.

## 0. Cel i decyzje (zatwierdzone z użytkownikiem)

1. **/v1 = default-DENY.** Przez zewnętrzne API nic nie jest dostępne, dopóki nie zostanie jawnie nadane.
   Efektywny dostęp = `scope_klucza ∩ uprawnienia_podmiotu`. Dashboard (Tier 1, binarny) **bez zmian** (zostaje default-allow).
2. **Trzy typy klucza:** `user` (dziedziczy uprawnienia usera + jego grup), `group` (dziedziczy uprawnienia grupy),
   `general` (własna jawna allowlista modeli/flow/aliasów; **brak admin-bypass**).
3. **Zasoby ACL:** `model`, `flow`, `alias` — wszystkie trzy egzekwowane (dziś tylko `model`; flow/alias są default-allow → domknąć).
4. **Uprawnienia edytowalne z obu stron** (podmiot ↔ zasób) — ten sam zapis `resource_permissions`.
5. **Klucze API i uprawnienia synchronizują się przez mesh** — ten sam klucz i te same reguły działają identycznie na A/B/C.
6. **Bezwzględne odrzucanie** złych/niekompletnych kluczy (twarde 401), w tym domknięcie obecnej luki „klucz bez ownera → anonim bez ACL”.
7. UI: dedykowany ekran **„Dostęp i klucze API”** (admin-only) + uzupełnienie zakładek uprawnień usera/grupy. Macierz 1:1 ze stylem uprawnień funkcji addonów.

## 1. Stan obecny (z inwentaryzacji) — punkt wyjścia

- `api_keys` (migrations.rs:4031): `id INTEGER AUTOINCREMENT, key_hash UNIQUE, key_prefix, name, rate_limit_rps, is_active, created_at, last_used_at, owner_user_id`.
- `repository`: `create_api_key`(name,rps — **ignoruje scopes**), `list_api_keys` (bez hash), `verify_api_key`(hash→DbApiKey), `delete_api_key`.
- `unified_server.rs:371-442`: Bearer/x-api-key → `hash_api_key` (SHA-256) → `verify_api_key` → jeśli `owner_user_id` to `UserContext(uid, role)`; **brak owner = przechodzi jako anonim bez ACL** (luka).
- `resource_permissions` (migrations.rs:4744): `(resource_type, resource_id, subject_type∈{user,group}, access_level∈{allow,deny})`, UNIQUE(rt,rid,st,sid). `check()` = admin→user_deny→user_allow→group_deny→group_allow→**default ALLOW**.
- Handlery IAM: `ReqSet/ClearPermission`, `ReqListPermsFor{Resource,Subject}` (już są).
- ACL egzekwowane: `model_list_request` (Tier1), `/v1/chat`, `/v1/audio/speech`, tts-stream, flow-stream (resource_type="model"). **Flow/alias: brak (default-allow).**
- Sync: `CORE_SYNC_DESCRIPTORS` (core_registry.rs) — `user_accounts`, `user_groups`, `group_members`, `flows` **synchronizowane**; `api_keys`, `resource_permissions` **NIE**. Mechanizm: jawne `record_core_capture_tx` w transakcji zapisu; HLC last-write-wins; `ensure_default_core_sync_policies` tworzy `replicated_by_permission` per deskryptor.
- UI: `settings.js` zakładka API keys (tylko nazwa, `scopes:[]` hardcode). `users.js`: grupa ma działającą zakładkę perms (model/flow/addon, tri-state), user ma **placeholder TODO**. `catalog.js`: brak widoku „kto ma dostęp”.

## 2. Model danych (migracje — nowa migracja, bez backward-compat shimów)

### 2.1 `api_keys` — typ + podmiot + stabilny UID (dla mesh)
- Dodaj kolumny:
  - `uid TEXT NOT NULL UNIQUE` — UUID stabilny między węzłami (AUTOINCREMENT `id` nie nadaje się na klucz sync; `uid` jest kluczem sync i `subject_id` w `resource_permissions` dla kluczy ogólnych).
  - `key_type TEXT NOT NULL DEFAULT 'user' CHECK(key_type IN ('user','group','general'))`.
  - `subject_id TEXT NULL` — `user_id` (user) / `group_id` (group) / NULL (general).
- **Migracja: USUŃ wszystkie istniejące wiersze `api_keys`** (decyzja użytkownika — nigdy nieużywane). Świeży schemat, brak legacy → znika problem deterministycznego uid (codex P1#6 zamknięty: nowe klucze dostają `uid = uuid_v4` raz na węźle-twórcy, wędruje z wierszem przy sync).
- `owner_user_id` **usunąć** (zastąpione przez `key_type`+`subject_id`) — zaktualizować wszystkie odczyty. (Zero shimów — reguła repo.)
- Kolumna trzymająca weryfikator: `key_verifier` = **HMAC-SHA256(org_pepper, token)** (NIE goły SHA-256). Replikowana (potrzebna do weryfikacji na każdym węźle); `org_pepper` trzymany **lokalnie/poza replikacją** (sam zsync. dump bez peppera nie wystarcza do podrobienia). Patrz 4.3.

### 2.2 `resource_permissions` — podmiot `api_key`
- Rozszerz `subject_type` o `'api_key'`: `CHECK(subject_type IN ('user','group','api_key'))`. Dla klucza ogólnego `subject_id = api_keys.uid`.
- `resource_type` ∈ `{'model','flow','alias'}` (+ istniejące `'addon'` dla addonów — bez zmian).
- Zachowaj UNIQUE(rt,rid,st,sid). Dodaj indeks po (subject_type='api_key').
- `set()` (repository.rs:16038): zaktualizować walidację `subject_type` o `api_key`.

### 2.3 Audyt
- `resource_permissions::set/clear` oraz mutacje `api_keys` → `repository::log_audit` (dziś brak dla perms) — wymóg „audyt per outcome”.

## 3. Protokół (tentaflow-protocol) — bez wymyślania nieobsługiwanych pól

- `ApiKeyCreateRequest`: zamień `scopes: Vec<String>` na:
  `{ name, key_type: String, subject_id: Option<String>, scope_resources: Vec<ResourceRef> }`
  gdzie `ResourceRef { resource_type, resource_id }` (używane TYLKO dla `general`).
- `ApiKeySummary`: dodaj `key_type`, `subject_id`, `subject_label`, `scope_count`, `is_active`, `last_used_at_epoch` (jest), `sync_nodes`/`sync_total` (opcjonalnie do badge).
- Nowe warianty (przez istniejący IAM/ApiKey body — append na końcu enuma, zachować indeksy CBOR):
  - `ApiKeyScopeListRequest{ key_uid } → ApiKeyScopeListResponse{ entries: Vec<PermissionEntry> }`
  - `ApiKeyScopeSetRequest{ key_uid, resource_type, resource_id, access_level }` (allow/deny dla klucza ogólnego)
  - `ApiKeyScopeClearRequest{ key_uid, resource_type, resource_id }`
  - `ApiKeyRotateRequest{ key_uid } → ApiKeyRotateResponse{ token }` (nowy token, ten sam uid+scope, stary hash unieważniony)
- `ReqSet/ClearPermission` + `PermissionEntry`: dopuść `subject_type='api_key'` i `resource_type∈{model,flow,alias}` (walidacja w handlerze).
- `/v1/models` per-key: bez zmian protokołu (to REST), ale handler filtruje (4.2).

## 4. Backend rdzenia

### 4.1 Resolver podmiotu klucza + twarde 401 (`unified_server.rs`)
- Po `verify_api_key`:
  - brak klucza / zły / `is_active=0` → **401** (jak dziś).
  - `key_type='user'` → wymaga istniejącego, aktywnego usera (`subject_id`); inaczej **401**. Buduj `Principal::User{uid, role}`.
  - `key_type='group'` → wymaga istniejącej grupy; `Principal::Group{group_id}`.
  - `key_type='general'` → `Principal::ApiKey{key_uid}` (rola = brak; **nigdy admin-bypass**).
  - Stempluj `last_used_at` (dziś nie aktualizowane) — async/cheap update.
- Wstrzyknij `Principal` (rozszerzenie obecnego `UserContext` lub nowy typ w `auth/acl.rs`) do `req.extensions()`. **Usuń** ścieżkę „brak owner → anonim”.

### 4.2 Egzekwowanie default-DENY na /v1 (`auth/acl.rs` + routing)
- Nowa funkcja `check_v1_access(db, resource_type, resource_id, principal) -> bool` z semantyką **default-DENY** i `scope ∩ subject`:
  - `Principal::User` → `resource_permissions::check_with_default(rt,rid,user,role, default=Deny)` (rozszerz `check()` o parametr `default_allow: bool`; Tier1 woła z `true`, /v1 z `false`).
  - `Principal::Group` → analogicznie, tylko reguły grupy + Domyślne=Deny.
  - `Principal::ApiKey` → wyłącznie wpisy `subject_type='api_key', subject_id=uid` (allow wymagany; brak = deny). Brak admin-bypass.
- Podłącz w **każdym** wejściu /v1 (chat blocking+stream, embeddings, tts, tts-stream, flow-stream, transcriptions). Mapowanie modelu→resource_type:
  - rozwiąż `model` w `catalog_snapshot`: `ServiceModel→'model'`, `Flow→'flow'`, `Alias→'alias'`; sprawdź `check_v1_access(rt, model, principal)`. Dla aliasu egzekwuj też dostęp do realnego targetu po rozwiązaniu (alias respektuje target + fallbacki — sprawdzamy alias, a przy fallbacku też docelowy model).
  - Odmowa → **404 `model_not_found`** (nie zdradzać istnienia), spójnie z obecnym chatem.
- `/v1/models` (`openai/server.rs:1227 handle_models_list`): przyjmij `Principal`, filtruj `advertised_entries()` przez `check_v1_access` per wpis (resource_type wg kind). Klucz widzi tylko swoje zasoby (wymóg listy per-klucz).

### 4.3 Sync (`sync/core_registry.rs`, `db/models.rs`, `repository.rs`)
- Dodaj `CoreSyncResourceKind::{ApiKey, ResourcePermission}` (models.rs enum).
- Dodaj 2 `CoreSyncDescriptor`:
  - `api_keys`: `resource_type="core.api_key"`, `primary_key_column="uid"`, scope `Organization`, retention `Durable`, partition `security`. Replikowane kolumny: `uid, key_hash, key_prefix, name, key_type, subject_id, rate_limit_rps, is_active`. **Wyklucz `last_used_at`** (local-only, jak `skills.last_used_at`).
  - `resource_permissions`: `resource_type="core.resource_permission"`, klucz kompozytowy `"resource_type,resource_id,subject_type,subject_id"`, scope `Organization`, retention `Durable`, partition `permissions`.
- Dodaj `record_core_capture_tx` w: `create_api_key`, `rotate` (update hash/prefix), `set_api_key_active`/`delete`, oraz `resource_permissions::set`/`clear` (wewnątrz transakcji).
- `ensure_default_core_sync_policies` automatycznie obejmie nowe deskryptory (`replicated_by_permission`).
- **Decyzja do oceny przez codex:** replikacja `key_hash`. Argument za: ten sam klucz musi działać na A/B/C (wymóg). `key_hash` to SHA-256 losowego tokenu (122-bit uuid) — nieodwracalny, brute-force niewykonalny. Alternatywa (re-encrypt per node jak sekrety) nie ma sensu dla hasha. **Rekomendacja: replikować hash.** Rozważyć podbicie KDF (np. HMAC-SHA256 z per-org salt) — ale to zmiana weryfikacji; trzymamy SHA-256 w v1 dla spójności z istniejącym `hash_api_key`, oznaczamy jako świadomą decyzję.
- Konflikty: HLC last-write-wins (istniejące). Rewokacja (`is_active=0`) i scope-deny propagują jak każda zmiana.

### 4.4 Repository — funkcje
- `create_api_key(name, key_type, subject_id, rate_limit_rps) -> (id, uid)` (zwraca uid).
- `rotate_api_key(uid, new_hash, new_prefix)`; `set_api_key_active(uid,bool)`; `get_api_key_by_uid`.
- `list_api_keys` → dołącz `key_type, subject_id, subject_label (join), scope_count (count z resource_permissions dla api_key)`.
- `resource_permissions::check_with_default(..., default_allow: bool)` (refaktor `check`; `check`/`check_access_safe` Tier1 wołają z `true`).
- Listy zasobów dla UI/scopingu: użyj `catalog_snapshot` (model/flow/alias) — jedno źródło prawdy.

## 5. Frontend (`tentaflow-core/www`)

### 5.1 Nowy moduł `modules/access-keys.js` + ekran „Dostęp i klucze API” (admin-only)
- Trzy zakładki (tf-tabs): **Klucze**, **Macierz dostępu**, **Wg zasobu** (mockupy 01/03/04).
- Klucze: tf-table (nazwa/prefiks, typ, podmiot/zakres, status, ostatnie użycie, badge sync, akcje: Zakres/Rotuj/Rewokuj). Kreator (mock 02) — **MODAL `tf-window` z wizardem** (NIE fragment strony): krok1 typ (karty), krok2 wybór usera/grupy (tf-select) LUB allowlista zasobów (tf-checkbox + tf-searchbox + filtry), krok3 token (jednorazowy, kopiuj). **Wskaźnik kroków IDENTYCZNY jak w deploy-service wizardzie** — te same klasy `.wizard-step-indicator/.wizard-step-dot{.active|.done}/.wizard-step-line` z `css/style.css` (numerki 1/2/3, aktywny indigo-glow, ukończony zielony-glow), wzorzec z `js/modules/catalog/engine-deploy-wizard.js::renderStepIndicator`.
- **Usunąć zakładkę „API keys" z `settings.js`** (potwierdzone) — funkcja przenosi się w całości na ekran „Dostęp i klucze API"; zero duplikacji (usunąć też martwe i18n/odwołania).
- Macierz (mock 03): **przenieś** wspólny CSS macierzy (`.perm-matrix/.perm-btn/.legend/.subtabs`) z `css/addons.css` do współdzielonego arkusza (lub reużyj klasy). Podzakładki Per grupa / Per user / Per klucz / **Domyślne** (Domyślne=DENY, read-only info). Kolumny = zasoby (Modele/Flow/Aliasy), tri-state przyciski → `iamSetPermissionRequest`/`iamClearPermissionRequest` (subject_type wg podzakładki) i `apiKeyScopeSet` dla kluczy.
- Wg zasobu (mock 04): wybór model/flow/alias → transponowana macierz (wiersze=podmioty: grupy/userzy/klucze). Ten sam zapis.
- Nawigacja: dodać pozycję menu (Zarządzanie → „Dostęp i klucze API”), admin-only. **Usunąć** zakładkę API keys ze `settings.js` (przenieść tu — zero duplikacji).

### 5.2 `users.js` — uzupełnienie
- User detail „Uprawnienia”: zastąpić placeholder (perms_user_todo) realną macierzą (model/flow/alias, tri-state) — analogicznie do grupy (mock 05a).
- Grupa „Uprawnienia”: dodać sekcję **Aliasy** (dziś model/flow/addon) — mock 05b.
- Reużyć ten sam komponent/render macierzy co ekran kluczy (DRY).

### 5.3 i18n
- Dodać sekcję `access_keys.*` (en/pl/de/es/fr), klucze `users.perm_aliases`, usunąć `users.perms_user_todo`. Komplet 5 języków (wymóg seedów min pl+en).

### 5.4 Komponenty
- Tylko `tf-*` (tf-table, tf-window, tf-tabs, tf-select, tf-checkbox, tf-searchbox, tf-chip, tf-button, tf-radio). Macierz tri-state: jeśli powtarza się w 3 miejscach → rozważyć `tf-perm-matrix` (zgodnie z regułą „wzorzec w 2+ miejscach → komponent”).

## 6. Testy (BARDZO mocne — nic nie może przejść bokiem)

### 6.1 Unit (Rust, `cargo test`)
- `check_with_default`: pełna tabela prawdy dla default-allow (Tier1) i default-DENY (/v1) × {admin, user_allow, user_deny, group_allow, group_deny, brak}.
- `Principal::ApiKey`: tylko własna allowlista; brak admin-bypass; deny wygrywa.
- Mapowanie catalog kind → resource_type (model/flow/alias); alias → target.
- Migracja: stare wiersze `api_keys` poprawnie dostają uid/key_type/subject_id.

### 6.2 Integracyjne /v1 (na żywej binarce, `--features dashboard-api,camera`)
- Brak nagłówka → 401; zły klucz → 401; klucz nieaktywny → 401; klucz `user` ze skasowanym userem → 401; **klucz bez subject (dawny anonim) → 401** (regресja luki).
- Klucz user: model dozwolony → 200; model z deny → 404; model bez reguły → 404 (default-DENY!).
- Klucz general: tylko zasoby z allowlisty → 200; poza → 404; pusta allowlista → wszystko 404.
- Flow gated: wywołanie flow bez uprawnień → 404; z uprawnieniem → 200. Alias: respektuje allow aliasu + target.
- `/v1/models`: zwraca **tylko** zasoby dostępne dla danego klucza (user/group/general), w tym poprawne `owned_by` model/flow/alias.
- Rotacja: stary token po `rotate` → 401; nowy → 200, ten sam scope.
- Header warianty: `Authorization: Bearer` i `x-api-key` równoważne; case-insensitive Bearer.

### 6.3 Mesh E2E (A lokal / B rig `ssh rig` / C Mac `ssh rig23`) — sedno wymagania
- Utwórz klucz na A → po sync **ten sam token** działa na /v1 węzła B i C (verify_api_key z replikowanego hash).
- Nadaj allow modelu na A → po sync żądanie na B przechodzi; ustaw deny na A → na B zaczyna zwracać 404.
- Rewokuj klucz na A → na B/C natychmiast (po propagacji) 401.
- Klucz general z allowlistą zawierającą model hostowany TYLKO na B: żądanie na A → routing przez mesh do B, ACL po stronie wejścia (A) na podstawie zsync. reguł → 200; model spoza scope → 404.
- Konflikt: równoległa zmiana reguły na A i B → HLC LWW, stan zbieżny na wszystkich węzłach.

### 6.4 Security / próby obejścia (must-fail)
- Próba użycia klucza general na zasobie spoza allowlisty mimo że user-admin istnieje (brak bypass).
- SQLi/encoding w nazwie/scope; nadmiarowe scope_resources; nieistniejący resource_id.
- Wyścig: rewokacja w trakcie streamu (kolejne chunk-i nie mają nowych uprawnień — przy następnym żądaniu 401).
- `/v1/models` nie wycieka zasobów spoza scope (porównanie zbiorów).

### 6.5 Frontend
- Playwright: kreator 3 typów, zapis scope, macierz tri-state (allow→deny→inherit) zapisuje i odświeża, widok „wg zasobu” == „wg podmiotu” (spójność), zakładka uprawnień usera/grupy.
- Weryfikacja wizualna 1:1 z mockupami @1440 i @390 (obowiązek z pamięci projektu).

## 7. Kolejność wdrożenia (plastry)
1. Migracja DB (api_keys uid/type/subject, resource_permissions api_key) + repo + testy migracji.
2. ACL default-DENY (`check_with_default`) + Principal + twarde 401 w unified_server + testy unit.
3. Egzekwowanie /v1 we wszystkich wejściach + `/v1/models` filtr + mapowanie kind + testy integ.
4. Sync deskryptory + captures + testy mesh E2E (A↔B↔C).
5. Protokół + handlery (create z typem, scope set/list, rotate) + audyt.
6. Frontend: ekran Dostęp i klucze API (Klucze/Macierz/Wg zasobu) + kreator; usuń zakładkę z settings.
7. Frontend: uprawnienia usera/grupy + aliasy; i18n 5 języków.
8. Pełny przebieg testów 6.1–6.5; weryfikacja wizualna; codex review końcowy.

## 9. Poprawki z weryfikacji codex (BLOKUJĄCE — wchodzą do implementacji)

Codex (high) ocenił plan i znalazł 6× P1 + 2× P2. Wszystkie przyjęte. Aktualizują sekcje wyżej:

- **[P1] /v1 fail-CLOSED (nie reużywać `check_access_safe`).** `auth/acl.rs:53` `check_access_safe` zwraca `true` przy błędzie DB (fail-open), a routery (`routing/chat.rs:38`, `streaming.rs:593`, `embeddings.rs:30`) sprawdzają ACL **tylko gdy istnieje `UserContext`** — brak kontekstu = brak ACL. Dla /v1 wprowadzić **osobną bramę `Principal` PRZED routingiem**, fail-CLOSED (błąd DB → 503/deny, nigdy allow). Tier1 zostaje na `check_access_safe`.
- **[P1] Kanoniczne resource_id + egzekwowanie w resolverze.** model = id z katalogu, flow = `flow_id` (NIE published name), alias = alias id. Sprawdzać allow **aliasu** ORAZ allow **rozwiązanego targetu/fallbacku** w resolverze/executorze (`flow_engine/dispatcher.rs:744`, `services/runtime/resolver`), nie tylko na wejściu handlera — inaczej alias obchodzi ACL targetu.
- **[P1] `handle_models_list` musi dostać `Principal`.** Zmiana sygnatury (`openai/server.rs:225,1256`) — filtr fail-closed per wpis.
- **[P1] Backend admin-only.** `api_key_list/create/revoke` + scope/permission set/list są dziś `#[policy(UserSession)]` (`dispatch/handlers.rs:354/376/423`) — zmienić na **`#[policy(Admin)]`** (UI admin-only to NIE kontrola).
- **[P1] Sync = deskryptory + materializer + tombstony.** Dodać ramiona apply/delete w `sync/core_materializer.rs` (dziś brak, ~:89) i LWW-tracking dla `api_keys`/`resource_permissions` (~:19); stabilne length-prefixed kompozytowe ID dla permissions; **tombstone** przy `clear`/deny, żeby nieaktualny `allow` nie wskrzesił się po propagacji.
- **[P1#6] `uid` — ZAMKNIĘTE decyzją.** Usuwamy wszystkie legacy `api_keys` w migracji (nieużywane), więc nie ma deterministycznego wyprowadzania uid ze starych wierszy. Nowe klucze: `uid = uuid_v4` generowany raz na węźle-twórcy, replikowany razem z wierszem (zbieżny z definicji, brak kolizji). `uid` to klucz sync i `subject_id` kluczy ogólnych.
- **[P1#7] Token + weryfikator — ZAMKNIĘTE decyzją: HMAC-SHA256 + per-org pepper.** Token **≥256 bit** CSPRNG (nie UUIDv4). W bazie `key_verifier = HMAC-SHA256(org_pepper, token)`. Zastąpić obecne `hash_api_key` (goły SHA-256) tą funkcją w całym kodzie spójnie (create/rotate/verify). Argon2id i podpisane tokeny odrzucone (koszt CPU na hot-path / złożoność rewokacji).
  - **KOREKTA dla mesh (plaster 4):** żeby ten sam token weryfikował się na A/B/C, `key_verifier` musi być odtwarzalny wszędzie → `org_pepper` MUSI być **wspólny w obrębie org** i synchronizowany tą samą ścieżką sekretów co `hf_token` (re-encrypt per węzeł, kanał baseline). W spoczynku pepper chroniony cipher'em ustawień każdego węzła; sam zsync. dump `api_keys` bez klucza cipher węzła nie pozwala podrobić kluczy. **Ryzyko rezydualne:** pełna kompromitacja zaufanego węzła (cipher + DB) = pepper całej org. Udokumentować. (W plastrze 1 pepper był lokalny — plaster 4 go ujednolica.)
- **[P2] Bez boolean-footgun.** Zamiast `check(default_allow: bool)` → dwie nazwane funkcje: `check_dashboard_access_default_allow(...)` i `check_v1_access_default_deny(...)`.
- **[P2] Brakujące kontrole (przed UI):** realny **per-key rate-limit** (`rate_limit_rps` dziś tylko zapisywane), aktualizacja `last_used_at` w `verify_api_key`, **audyt wyników uprawnień**, **inwalidacja cache** po zmianie zsynchronizowanej, dopisać `/v1/images/generations` (dziś 501) do audytu tras.

**Zaktualizowana kolejność plastrów:** 1) migracja (uid deterministyczny, token/hash story) → 2) brama Principal fail-CLOSED + dwie nazwane funkcje check → 3) egzekwowanie /v1 we wszystkich wejściach + resolver alias/target + `/v1/models(Principal)` → 4) sync (deskryptory + **materializer apply/delete + tombstony + LWW**) + testy mesh → 5) admin-only handlery + protokół + rate-limit + last_used + audyt → 6/7) frontend → 8) pełne testy + codex review końcowy.

## 8. Ryzyka / punkty dla codex
- Replikacja `key_hash` (sekcja 4.3) — czy akceptowalne vs. ewentualny per-org KDF.
- AUTOINCREMENT `id` w sync — dlatego wprowadzamy `uid` jako klucz; sprawdzić wszystkie miejsca używające `id`/`owner_user_id`.
- Zmiana semantyki `check()` (parametr default) — upewnić się, że Tier1 (dashboard) NIE zmienia zachowania (nadal default-allow).
- Spójność „alias respektuje target+fallbacki” z ACL (nie obejść modelu przez alias).
- Brak regресji: istniejące klucze (po migracji `user`) działają na Tier1 i /v1 zgodnie z uprawnieniami usera.

## GSTACK REVIEW REPORT

| Review | Trigger | Why | Runs | Status | Findings |
|--------|---------|-----|------|--------|----------|
| Codex Review | `/codex` consult | Niezależna 2. opinia (bezpieczeństwo/regresja) | 1 | issues_found | 6×P1 + 2×P2; wszystkie przyjęte, naniesione do sekcji 9 |

**CODEX:** 6 krytycznych: (1) /v1 fail-CLOSED bez check_access_safe, (2) kanoniczne resource_id + ACL aliasu i targetu w resolverze, (3) handle_models_list(Principal), (4) handlery kluczy `#[policy(Admin)]`, (5) sync = materializer apply/delete + tombstony + LWW, (6) uid deterministyczny + token ≥256-bit + HMAC(org_pepper)/Argon2id. 2 advisory: brak boolean-footgun (dwie nazwane funkcje check), per-key rate-limit + last_used + audyt + inwalidacja cache.

**VERDICT:** Plan po naniesieniu sekcji 9 jest gotowy do implementacji po akceptacji użytkownika. Mockupy (index + 6 ekranów) i plan kompletne; implementacja czeka na „go”.

**ROZSTRZYGNIĘTE DECYZJE (użytkownik, 2026-06-17):**
- Token/weryfikator: **HMAC-SHA256(org_pepper, token)**, token ≥256-bit, pepper per-org NIE replikowany, porównanie stałoczasowe.
- Legacy klucze: **usunąć wszystkie w migracji** (nieużywane) → świeży schemat, uid = uuid_v4 generowany raz i replikowany.

NO UNRESOLVED DECISIONS
