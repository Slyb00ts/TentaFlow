Mam teraz pełny obraz istniejącej architektury. Przygotowuję plan.

---

# Plan: System Addonow WASM dla TentaFlow.AI

## Podsumowanie

System addonow oparty o Wasmtime, rozszerzajacy TentaFlow.AI o pluginy trzecich stron (np. Teams bot, RAG connectors, integracje). Addony dzialaja w sandboxie WASM, komunikuja sie z Core przez host functions, maja deklaratywne UI i sa synchronizowane przez mesh CRDT. System obejmuje pelne zarzadzanie uzytkownikami/grupami z uprawnieniami per addon per zasob, SSO (OIDC), event bus, flow builder integration i audit logging.

## Analiza istniejacego kodu

### Co juz istnieje i mozna wykorzystac:

| Komponent | Plik | Status |
|-----------|------|--------|
| Migracje SQLite | `src/db/migrations.rs` | Wersjonowane, 13 migracji, dodajemy kolejne |
| CRDT (LamportClock, LWW-Register, OR-Set) | `src/mesh/crdt.rs` | Gotowe, rozszerzamy o nowe CrdtOperation |
| CRDT Store (persystencja) | `src/mesh/crdt_store.rs` | Gotowe, rozszerzamy apply_to_db |
| Mesh pipeline (mDNS + QUIC) | `src/mesh/pipeline.rs` | Gotowe, dodajemy sync addonow |
| Flow Engine (DAG) | `src/flow_engine/` | Gotowe, integrujemy bloczki addonow |
| Users (prosta tabela) | `migrations.rs` migracja 1 | Rozszerzamy o grupy, SSO, uprawnienia |
| Gossip protocol | `src/mesh/gossip.rs` | Gotowe, dodajemy nowe typy operacji |
| Router | `src/routing/router.rs` | Gotowe, addon tools rejestrowane przez Router |
| Crypto (argon2, AES-GCM) | `src/crypto/` + Cargo.toml | Gotowe, uzywamy do sekretow |

### Co trzeba dodac od zera:

- Wasmtime runtime + host functions
- Addon manager (lifecycle, instance pool)
- Permission system (per addon per user per resource)
- Event bus (pub/sub)
- Addon UI framework (deklaratywny)
- Addon SDK (Rust crate dla autorow addonow)
- SSO/OIDC
- Audit logging
- 20+ nowych tabel w SQLite

---

## 1. Pelny schemat bazy danych

### Migracja 14: `addon_system_users_groups`

```sql
-- =========================================================
-- Grupy uzytkownikow
-- =========================================================
CREATE TABLE IF NOT EXISTS groups (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL UNIQUE,
    description TEXT,
    is_system INTEGER NOT NULL DEFAULT 0,  -- 1 = nie mozna usunac (np. "admins")
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

-- Przynaleznosc uzytkownikow do grup (N:M)
CREATE TABLE IF NOT EXISTS user_groups (
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    group_id INTEGER NOT NULL REFERENCES groups(id) ON DELETE CASCADE,
    added_at TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (user_id, group_id)
);

-- Rozszerzenie tabeli users o pola SSO
-- (ALTER TABLE - kompatybilnosc wsteczna)
ALTER TABLE users ADD COLUMN display_name TEXT;
ALTER TABLE users ADD COLUMN email TEXT;
ALTER TABLE users ADD COLUMN avatar_url TEXT;
ALTER TABLE users ADD COLUMN sso_provider TEXT;         -- NULL = lokalne konto
ALTER TABLE users ADD COLUMN sso_subject TEXT;           -- subject claim z OIDC
ALTER TABLE users ADD COLUMN is_active INTEGER NOT NULL DEFAULT 1;
ALTER TABLE users ADD COLUMN updated_at TEXT NOT NULL DEFAULT (datetime('now'));

CREATE UNIQUE INDEX IF NOT EXISTS idx_users_sso ON users(sso_provider, sso_subject)
    WHERE sso_provider IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_users_email ON users(email);

-- Seed: grupa "admins" i przypisanie istniejacego admin
INSERT OR IGNORE INTO groups (name, description, is_system) VALUES ('admins', 'Administratorzy systemu', 1);
INSERT OR IGNORE INTO user_groups (user_id, group_id)
    SELECT u.id, g.id FROM users u, groups g WHERE u.username = 'admin' AND g.name = 'admins';
```

### Migracja 15: `addon_sso_providers`

```sql
-- =========================================================
-- Konfiguracja providerow SSO/OIDC
-- =========================================================
CREATE TABLE IF NOT EXISTS sso_providers (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL UNIQUE,                -- wyswietlana nazwa (np. "Azure AD Firma")
    provider_type TEXT NOT NULL CHECK(provider_type IN (
        'azure_ad', 'adfs', 'google', 'authentik', 'oidc_generic'
    )),
    client_id TEXT NOT NULL,
    client_secret_encrypted TEXT NOT NULL,     -- AES-256-GCM
    issuer_url TEXT NOT NULL,                  -- np. https://login.microsoftonline.com/{tenant}/v2.0
    authorization_url TEXT,                    -- nadpisanie auto-discovery
    token_url TEXT,                            -- nadpisanie auto-discovery
    userinfo_url TEXT,                         -- nadpisanie auto-discovery
    scopes TEXT NOT NULL DEFAULT 'openid profile email',
    -- Mapowanie claimow OIDC na pola uzytkownika
    claim_mapping_json TEXT NOT NULL DEFAULT '{"username":"preferred_username","email":"email","display_name":"name"}',
    -- Automatyczne przypisanie grup po zalogowaniu SSO
    auto_group_ids_json TEXT DEFAULT '[]',     -- np. [2, 5]
    is_active INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);
```

### Migracja 16: `addon_core_tables`

```sql
-- =========================================================
-- Addon: rejestr zainstalowanych addonow
-- =========================================================
CREATE TABLE IF NOT EXISTS addons (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    addon_id TEXT NOT NULL UNIQUE,             -- unikalna nazwa np. "tentaflow.teams"
    version TEXT NOT NULL,                      -- semver np. "1.2.3"
    display_name TEXT NOT NULL,
    description TEXT,
    author TEXT,
    -- Manifest
    manifest_toml TEXT NOT NULL,               -- pelny manifest.toml
    skill_md TEXT,                             -- SKILL.md (prompt dla LLM)
    blocks_json TEXT,                          -- blocks.json (bloczki flow builder)
    icon_blob BLOB,                            -- icon.png (max 256x256, <64KB)
    -- Platform targeting
    platforms_json TEXT NOT NULL DEFAULT '["all"]',  -- ["linux_x86_64","macos_aarch64","android","ios"]
    -- Status
    status TEXT NOT NULL DEFAULT 'installed' CHECK(status IN ('installed','active','disabled','error')),
    installed_by INTEGER REFERENCES users(id),
    wasm_size_bytes INTEGER NOT NULL DEFAULT 0,
    wasm_hash_sha256 TEXT NOT NULL,            -- hash pliku .wasm (weryfikacja integralnosci)
    -- Timestamps
    installed_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_addons_status ON addons(status);

-- =========================================================
-- Addon: plik WASM (osobna tabela — duzy BLOB)
-- =========================================================
CREATE TABLE IF NOT EXISTS addon_wasm (
    addon_id TEXT PRIMARY KEY REFERENCES addons(addon_id) ON DELETE CASCADE,
    wasm_bytes BLOB NOT NULL
);

-- =========================================================
-- Addon: instancje (multi-instance)
-- =========================================================
CREATE TABLE IF NOT EXISTS addon_instances (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    addon_id TEXT NOT NULL REFERENCES addons(addon_id) ON DELETE CASCADE,
    instance_id TEXT NOT NULL UNIQUE,          -- UUID
    instance_name TEXT NOT NULL,               -- np. "Bot Teams - Spotkanie Dev"
    config_json TEXT NOT NULL DEFAULT '{}',    -- konfiguracja per instancja
    status TEXT NOT NULL DEFAULT 'stopped' CHECK(status IN ('running','stopped','error','starting')),
    created_by INTEGER REFERENCES users(id),
    -- Resource limits (nadpisanie domyslnych z manifest)
    cpu_limit_millicores INTEGER,              -- NULL = domyslne z manifest
    ram_limit_mb INTEGER,
    storage_limit_mb INTEGER,
    -- Timestamps
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    started_at TEXT,
    stopped_at TEXT
);

CREATE INDEX IF NOT EXISTS idx_addon_instances_addon ON addon_instances(addon_id);
CREATE INDEX IF NOT EXISTS idx_addon_instances_status ON addon_instances(status);
```

### Migracja 17: `addon_permissions`

```sql
-- =========================================================
-- Uprawnienia addonu (deklarowane w manifest)
-- =========================================================
CREATE TABLE IF NOT EXISTS addon_declared_permissions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    addon_id TEXT NOT NULL REFERENCES addons(addon_id) ON DELETE CASCADE,
    permission_type TEXT NOT NULL CHECK(permission_type IN (
        'llm', 'llm_model', 'embeddings', 'rag',
        'storage', 'http', 'events', 'ui',
        'audio_capture', 'audio_play', 'tts', 'stt',
        'camera', 'notifications', 'background',
        'secrets', 'user_info', 'timer',
        'addon_communicate'
    )),
    -- Szczegoly uprawnienia
    resource_pattern TEXT,                    -- np. "bielik-*" dla llm_model, "*" dla http
    access_level TEXT NOT NULL DEFAULT 'ro' CHECK(access_level IN ('ro','rw','rwd')),
    reason TEXT,                              -- dlaczego addon potrzebuje (wyswietlane userowi)
    is_required INTEGER NOT NULL DEFAULT 1,   -- 1 = wymagane, 0 = opcjonalne
    UNIQUE(addon_id, permission_type, resource_pattern)
);

CREATE INDEX IF NOT EXISTS idx_addon_declared_perms ON addon_declared_permissions(addon_id);

-- =========================================================
-- Przyznane uprawnienia (per user lub per group)
-- =========================================================
CREATE TABLE IF NOT EXISTS addon_granted_permissions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    addon_id TEXT NOT NULL REFERENCES addons(addon_id) ON DELETE CASCADE,
    -- Dotyczy usera LUB grupy (jedno z dwoch NOT NULL)
    user_id INTEGER REFERENCES users(id) ON DELETE CASCADE,
    group_id INTEGER REFERENCES groups(id) ON DELETE CASCADE,
    -- Co przyznano
    permission_type TEXT NOT NULL,
    resource_pattern TEXT,
    access_level TEXT NOT NULL DEFAULT 'ro' CHECK(access_level IN ('ro','rw','rwd')),
    granted INTEGER NOT NULL DEFAULT 1,       -- 1 = przyznano, 0 = jawnie odmowiono
    granted_by INTEGER REFERENCES users(id),
    granted_at TEXT NOT NULL DEFAULT (datetime('now')),
    CHECK (user_id IS NOT NULL OR group_id IS NOT NULL),
    UNIQUE(addon_id, user_id, group_id, permission_type, resource_pattern)
);

CREATE INDEX IF NOT EXISTS idx_addon_granted_addon ON addon_granted_permissions(addon_id);
CREATE INDEX IF NOT EXISTS idx_addon_granted_user ON addon_granted_permissions(user_id);
CREATE INDEX IF NOT EXISTS idx_addon_granted_group ON addon_granted_permissions(group_id);
```

### Migracja 18: `addon_storage_secrets`

```sql
-- =========================================================
-- Addon: sandboxed key-value storage (per addon per instance)
-- =========================================================
CREATE TABLE IF NOT EXISTS addon_storage (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    addon_id TEXT NOT NULL,
    instance_id TEXT,                          -- NULL = wspolne dla wszystkich instancji
    storage_key TEXT NOT NULL,
    storage_value BLOB NOT NULL,               -- dane binarne (addon moze przechowywac cokolwiek)
    value_size_bytes INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE(addon_id, instance_id, storage_key)
);

CREATE INDEX IF NOT EXISTS idx_addon_storage_lookup ON addon_storage(addon_id, instance_id);

-- =========================================================
-- Addon: sekrety (szyfrowane per addon per user)
-- =========================================================
CREATE TABLE IF NOT EXISTS addon_secrets (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    addon_id TEXT NOT NULL REFERENCES addons(addon_id) ON DELETE CASCADE,
    user_id INTEGER REFERENCES users(id) ON DELETE CASCADE,  -- NULL = secret globalny addonu
    secret_key TEXT NOT NULL,
    encrypted_value BLOB NOT NULL,            -- AES-256-GCM encrypted
    nonce BLOB NOT NULL,                       -- 12 bajtow nonce
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE(addon_id, user_id, secret_key)
);

CREATE INDEX IF NOT EXISTS idx_addon_secrets_lookup ON addon_secrets(addon_id, user_id);

-- =========================================================
-- Addon: migracje bazy danych (sled per addon)
-- =========================================================
CREATE TABLE IF NOT EXISTS addon_migrations (
    addon_id TEXT NOT NULL,
    version INTEGER NOT NULL,
    name TEXT NOT NULL,
    applied_at TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (addon_id, version)
);
```

### Migracja 19: `addon_events_tools`

```sql
-- =========================================================
-- Event subscriptions (per addon instance)
-- =========================================================
CREATE TABLE IF NOT EXISTS addon_event_subscriptions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    addon_id TEXT NOT NULL REFERENCES addons(addon_id) ON DELETE CASCADE,
    instance_id TEXT REFERENCES addon_instances(instance_id) ON DELETE CASCADE,
    event_type TEXT NOT NULL,                  -- np. "message_received", "model_loaded", "*"
    event_filter_json TEXT,                    -- opcjonalny filtr (np. {"channel": "general"})
    is_active INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE(addon_id, instance_id, event_type)
);

CREATE INDEX IF NOT EXISTS idx_addon_events_type ON addon_event_subscriptions(event_type, is_active);

-- =========================================================
-- Addon: zarejestrowane tools (do LLM function calling)
-- =========================================================
CREATE TABLE IF NOT EXISTS addon_tools (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    addon_id TEXT NOT NULL REFERENCES addons(addon_id) ON DELETE CASCADE,
    tool_name TEXT NOT NULL,                   -- np. "teams_send_message"
    description TEXT NOT NULL,                 -- opis dla LLM
    parameters_schema_json TEXT NOT NULL,       -- JSON Schema parametrow
    return_schema_json TEXT,                   -- JSON Schema odpowiedzi
    is_active INTEGER NOT NULL DEFAULT 1,
    UNIQUE(addon_id, tool_name)
);

CREATE INDEX IF NOT EXISTS idx_addon_tools_active ON addon_tools(is_active);

-- =========================================================
-- Addon: zarejestrowane bloczki flow builder
-- =========================================================
CREATE TABLE IF NOT EXISTS addon_flow_blocks (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    addon_id TEXT NOT NULL REFERENCES addons(addon_id) ON DELETE CASCADE,
    block_type TEXT NOT NULL,                  -- np. "teams_send", "teams_read_channel"
    category TEXT NOT NULL CHECK(category IN ('trigger','service','transform','logic','output','addon')),
    label TEXT NOT NULL,
    description TEXT,
    config_schema_json TEXT NOT NULL DEFAULT '{}',
    input_ports_json TEXT NOT NULL DEFAULT '["input"]',
    output_ports_json TEXT NOT NULL DEFAULT '["output"]',
    icon TEXT,
    UNIQUE(addon_id, block_type)
);

CREATE INDEX IF NOT EXISTS idx_addon_flow_blocks_category ON addon_flow_blocks(category);
```

### Migracja 20: `addon_audit_log`

```sql
-- =========================================================
-- Audit log (per host, bez synchronizacji CRDT)
-- =========================================================
CREATE TABLE IF NOT EXISTS audit_log (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    -- Kto
    user_id INTEGER,
    username TEXT,
    -- Co
    addon_id TEXT,
    instance_id TEXT,
    action TEXT NOT NULL,                      -- np. "llm.generate", "storage.set", "http.request"
    -- Szczegoly
    resource_type TEXT,                        -- np. "llm", "storage", "http"
    resource_id TEXT,                          -- np. nazwa modelu, klucz storage
    details_json TEXT,                         -- dodatkowe info (request size, model, itp.)
    -- Wynik
    result TEXT NOT NULL DEFAULT 'ok' CHECK(result IN ('ok','denied','error')),
    error_message TEXT,
    -- Czas
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    -- Kompaktowy format: 8-bajtowy hash zamiast stringow
    action_hash INTEGER,                       -- FNV-1a hash action stringa
    duration_us INTEGER                        -- czas operacji w mikrosekundach
);

-- Indeksy: per addon, per user, po czasie (rotacja), po action_hash (szybkie filtry)
CREATE INDEX IF NOT EXISTS idx_audit_addon ON audit_log(addon_id, created_at);
CREATE INDEX IF NOT EXISTS idx_audit_user ON audit_log(user_id, created_at);
CREATE INDEX IF NOT EXISTS idx_audit_time ON audit_log(created_at);
CREATE INDEX IF NOT EXISTS idx_audit_action ON audit_log(action_hash);

-- =========================================================
-- Konfiguracja audit log
-- =========================================================
INSERT OR IGNORE INTO settings (key, value) VALUES ('audit_retention_days', '90');
INSERT OR IGNORE INTO settings (key, value) VALUES ('audit_compact_enabled', '1');
```

### Migracja 21: `addon_resource_limits`

```sql
-- =========================================================
-- Globalne limity zasobow per addon (domyslne)
-- =========================================================
CREATE TABLE IF NOT EXISTS addon_resource_limits (
    addon_id TEXT PRIMARY KEY REFERENCES addons(addon_id) ON DELETE CASCADE,
    max_instances INTEGER NOT NULL DEFAULT 10,
    cpu_limit_millicores INTEGER NOT NULL DEFAULT 500,     -- 0.5 CPU
    ram_limit_mb INTEGER NOT NULL DEFAULT 256,
    storage_limit_mb INTEGER NOT NULL DEFAULT 100,
    rate_limit_rps INTEGER NOT NULL DEFAULT 100,           -- requestow/sek per addon
    max_concurrent_requests INTEGER NOT NULL DEFAULT 50,
    http_request_timeout_ms INTEGER NOT NULL DEFAULT 30000,
    max_http_requests_per_minute INTEGER NOT NULL DEFAULT 600,
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

-- =========================================================
-- Konfiguracja mesh sync addonow
-- =========================================================
CREATE TABLE IF NOT EXISTS addon_sync_config (
    addon_id TEXT NOT NULL REFERENCES addons(addon_id) ON DELETE CASCADE,
    -- Wykluczenia
    exclude_group_id INTEGER REFERENCES groups(id) ON DELETE CASCADE,
    exclude_platform TEXT,                    -- np. "android", "ios"
    PRIMARY KEY (addon_id, exclude_group_id, exclude_platform)
);
```

---

## 2. Struktura modulow Rust

Nowe pliki w `/Users/critix/repos/dotnet/nextapp/TentaFlow.Core/src/`:

```
src/
├── addon/                          # <-- NOWY MODUL GLOWNY
│   ├── mod.rs                      # Eksport publiczny
│   ├── manager.rs                  # AddonManager — lifecycle, install/uninstall/upgrade
│   ├── registry.rs                 # Rejestr addonow w DB, manifest parser
│   ├── manifest.rs                 # Parsowanie manifest.toml, walidacja
│   ├── instance.rs                 # AddonInstance — lifecycle per instancja
│   ├── instance_pool.rs            # Pre-warmed WASM instance pool
│   ├── wasm_runtime.rs             # Wasmtime Engine, Linker, Store setup
│   ├── host_functions/             # Host functions (Core API dla addonow)
│   │   ├── mod.rs
│   │   ├── llm.rs                  # generate, generate_stream, embeddings
│   │   ├── storage.rs              # get, set, delete, list
│   │   ├── http.rs                 # request (proxy z logowaniem)
│   │   ├── events.rs               # subscribe, publish
│   │   ├── ui.rs                   # render_panel, show_notification, show_modal, update_widget
│   │   ├── audio.rs                # capture_stream, play_stream
│   │   ├── user.rs                 # get_current_user, check_permission
│   │   ├── secrets.rs              # get_secret, set_secret
│   │   ├── log.rs                  # info, warn, error
│   │   └── timer.rs                # set_interval, set_timeout
│   ├── permissions.rs              # PermissionChecker — sprawdzanie uprawnien
│   ├── tools.rs                    # Rejestracja addon tools w LLM
│   ├── flow_integration.rs         # Integracja bloczków addonow z flow engine
│   └── ui_framework.rs             # Deklaratywny UI — model komponentow
│
├── auth/                           # <-- NOWY MODUL
│   ├── mod.rs
│   ├── sso.rs                      # OIDC/SSO flow (Azure AD, Google, itp.)
│   ├── session.rs                  # Sesje uzytkownikow (JWT extended)
│   └── password.rs                 # Zmiana hasla, polityka hasel
│
├── events/                         # <-- NOWY MODUL
│   ├── mod.rs
│   ├── bus.rs                      # EventBus — publish/subscribe in-process
│   ├── types.rs                    # Event enum, EventFilter, EventPayload
│   └── dispatcher.rs              # Dispatch eventow do addonow (WASM call)
│
├── audit/                          # <-- NOWY MODUL
│   ├── mod.rs
│   ├── logger.rs                   # AuditLogger — logowanie operacji
│   ├── retention.rs                # Rotacja logow (background task)
│   └── query.rs                    # Wyszukiwanie/filtrowanie logow
│
├── mesh/
│   ├── crdt.rs                     # ISTNIEJACY — rozszerzamy CrdtOperation
│   ├── crdt_store.rs               # ISTNIEJACY — rozszerzamy apply_to_db
│   └── addon_sync.rs               # <-- NOWY: sync plikow .wasm przez mesh
│
├── db/
│   ├── migrations.rs               # ISTNIEJACY — dodajemy migracje 14-21
│   └── models.rs                   # ISTNIEJACY — dodajemy nowe struktury
│
└── flow_engine/
    ├── adapters/
    │   └── addon_block.rs           # <-- NOWY: adapter do wywolywania bloczków addonow
    └── types.rs                     # ISTNIEJACY — rozszerzamy FlowNode
```

Nowy crate SDK:

```
TentaFlow.Addon.SDK/               # <-- NOWY CRATE
├── Cargo.toml
├── src/
│   ├── lib.rs                      # Eksport publiczny
│   ├── host.rs                     # FFI bindings do host functions
│   ├── types.rs                    # Typy danych (Event, UIComponent, Request/Response)
│   ├── ui.rs                       # Builder deklaratywnego UI
│   ├── macros.rs                   # Makra: #[addon_main], #[on_event], #[tool]
│   └── prelude.rs                  # use tentaflow_addon::prelude::*;
└── examples/
    └── hello_world/
        ├── Cargo.toml
        ├── manifest.toml
        └── src/lib.rs
```

### Feature flags (Cargo.toml Core rozszerzenie):

```toml
[features]
addon-runtime = ["dep:wasmtime", "dep:wasmtime-wasi"]

[dependencies]
wasmtime = { version = "29", optional = true }
wasmtime-wasi = { version = "29", optional = true }
```

---

## 3. WASM Host Functions (pelna lista z sygnaturami)

Wszystkie host functions operuja na **liniowej pamieci WASM** - stringi/dane przekazywane jako `(ptr, len)` do guest memory, wyniki pisane do guest-allocated bufora.

### Konwencje ABI

```
Kazda host function zwraca i32:
  0  = sukces
  -1 = brak uprawnien
  -2 = blad operacji
  -3 = timeout
  -4 = rate limit exceeded
  -5 = resource not found

Dane wyjsciowe: addon alokuje bufor, podaje (out_ptr, out_capacity).
Host wpisuje dane i zwraca faktyczny rozmiar w out_len_ptr.
Jesli bufor za maly: zwraca wymagany rozmiar (addon realokuje i ponawia).
```

### 3.1 LLM API

```rust
// Generowanie tekstu (synchroniczne — blokuje guest)
// prompt_ptr/len: wskaznik do UTF-8 stringa z promptem
// model_ptr/len: opcjonalna nazwa modelu (0,0 = domyslny)
// options_ptr/len: JSON z opcjami {temperature, max_tokens, ...}
// out_ptr/out_cap: bufor na odpowiedz
// out_len_ptr: ile bajtow zapisano
fn llm_generate(
    prompt_ptr: i32, prompt_len: i32,
    model_ptr: i32, model_len: i32,
    options_ptr: i32, options_len: i32,
    out_ptr: i32, out_cap: i32,
    out_len_ptr: i32,
) -> i32;

// Generowanie strumieniowe — callback-based
// Rejestruje callback_id; Core wywola guest export `on_stream_chunk(callback_id, chunk_ptr, chunk_len)`
// Zwraca callback_id (>0) lub blad (<0)
fn llm_generate_stream(
    prompt_ptr: i32, prompt_len: i32,
    model_ptr: i32, model_len: i32,
    options_ptr: i32, options_len: i32,
) -> i32;

// Embeddings
fn llm_embeddings(
    texts_json_ptr: i32, texts_json_len: i32,  // JSON array of strings
    model_ptr: i32, model_len: i32,
    out_ptr: i32, out_cap: i32,
    out_len_ptr: i32,                           // JSON array of float arrays
) -> i32;
```

### 3.2 Storage API

```rust
fn storage_get(
    key_ptr: i32, key_len: i32,
    out_ptr: i32, out_cap: i32,
    out_len_ptr: i32,
) -> i32;

fn storage_set(
    key_ptr: i32, key_len: i32,
    value_ptr: i32, value_len: i32,
) -> i32;

fn storage_delete(
    key_ptr: i32, key_len: i32,
) -> i32;

// Lista kluczy z opcjonalnym prefixem
fn storage_list(
    prefix_ptr: i32, prefix_len: i32,  // 0,0 = wszystkie
    out_ptr: i32, out_cap: i32,        // JSON array of strings
    out_len_ptr: i32,
) -> i32;
```

### 3.3 HTTP API

```rust
// HTTP request przez Core proxy (z audit logowaniem)
// request_json: {method, url, headers: {}, body: "", timeout_ms: 30000}
fn http_request(
    request_json_ptr: i32, request_json_len: i32,
    out_ptr: i32, out_cap: i32,    // JSON: {status, headers: {}, body: ""}
    out_len_ptr: i32,
) -> i32;
```

### 3.4 Event API

```rust
// Subskrybuj event type. Core wywola guest export `on_event(event_json_ptr, event_json_len)`.
fn event_subscribe(
    event_type_ptr: i32, event_type_len: i32,
    filter_json_ptr: i32, filter_json_len: i32,  // 0,0 = brak filtra
) -> i32;  // subscription_id lub blad

fn event_unsubscribe(subscription_id: i32) -> i32;

// Publikuj event (jesli addon ma uprawnienia 'events' z access_level 'rw')
fn event_publish(
    event_type_ptr: i32, event_type_len: i32,
    payload_json_ptr: i32, payload_json_len: i32,
) -> i32;
```

### 3.5 UI API

```rust
// Renderuj panel (glowny UI addonu)
// ui_json: deklaratywny opis layoutu (Block Kit-like JSON)
fn ui_render_panel(
    panel_id_ptr: i32, panel_id_len: i32,
    ui_json_ptr: i32, ui_json_len: i32,
) -> i32;

fn ui_show_notification(
    title_ptr: i32, title_len: i32,
    body_ptr: i32, body_len: i32,
    level_ptr: i32, level_len: i32,   // "info", "warning", "error", "success"
) -> i32;

fn ui_show_modal(
    title_ptr: i32, title_len: i32,
    ui_json_ptr: i32, ui_json_len: i32,
) -> i32;  // modal_id

// Aktualizuj widget (partial update — tylko zmieniony fragment)
fn ui_update_widget(
    widget_id_ptr: i32, widget_id_len: i32,
    properties_json_ptr: i32, properties_json_len: i32,
) -> i32;
```

### 3.6 Audio API

```rust
// Rozpocznij przechwytywanie audio (mikrofon). Wymaga uprawnienia 'audio_capture'.
// Core wywola guest export `on_audio_chunk(stream_id, pcm_ptr, pcm_len, sample_rate)`
fn audio_capture_start(
    config_json_ptr: i32, config_json_len: i32,  // {sample_rate: 16000, channels: 1, format: "pcm_f32le"}
) -> i32;  // stream_id lub blad

fn audio_capture_stop(stream_id: i32) -> i32;

// Odtwarzaj audio
fn audio_play(
    pcm_ptr: i32, pcm_len: i32,
    config_json_ptr: i32, config_json_len: i32,  // {sample_rate: 22050, channels: 1}
) -> i32;
```

### 3.7 User API

```rust
// Pobierz info o aktualnym uzytkowniku
fn user_get_current(
    out_ptr: i32, out_cap: i32,  // JSON: {id, username, display_name, email, groups: [...]}
    out_len_ptr: i32,
) -> i32;

// Sprawdz uprawnienie
fn user_check_permission(
    permission_type_ptr: i32, permission_type_len: i32,
    resource_ptr: i32, resource_len: i32,
    access_level_ptr: i32, access_level_len: i32,  // "ro", "rw", "rwd"
) -> i32;  // 0 = granted, -1 = denied
```

### 3.8 Secrets API

```rust
fn secret_get(
    key_ptr: i32, key_len: i32,
    out_ptr: i32, out_cap: i32,
    out_len_ptr: i32,
) -> i32;

fn secret_set(
    key_ptr: i32, key_len: i32,
    value_ptr: i32, value_len: i32,
) -> i32;
```

### 3.9 Log API

```rust
fn log_info(msg_ptr: i32, msg_len: i32) -> i32;
fn log_warn(msg_ptr: i32, msg_len: i32) -> i32;
fn log_error(msg_ptr: i32, msg_len: i32) -> i32;
```

### 3.10 Timer API

```rust
// Ustaw interval (w ms). Core wywola guest export `on_timer(timer_id)`.
fn timer_set_interval(interval_ms: i64) -> i32;  // timer_id lub blad

fn timer_set_timeout(delay_ms: i64) -> i32;  // timer_id lub blad

fn timer_clear(timer_id: i32) -> i32;
```

### WASM Guest Exports (addon musi eksportowac)

```rust
// Wymagane:
fn on_install() -> i32;
fn on_uninstall() -> i32;
fn on_start() -> i32;
fn on_stop() -> i32;

// Opcjonalne:
fn on_upgrade(old_version_ptr: i32, old_version_len: i32) -> i32;
fn on_event(event_json_ptr: i32, event_json_len: i32) -> i32;
fn on_request(request_json_ptr: i32, request_json_len: i32, out_ptr: i32, out_cap: i32, out_len_ptr: i32) -> i32;
fn on_stream_chunk(callback_id: i32, chunk_ptr: i32, chunk_len: i32) -> i32;
fn on_audio_chunk(stream_id: i32, pcm_ptr: i32, pcm_len: i32, sample_rate: i32) -> i32;
fn on_timer(timer_id: i32) -> i32;
fn on_ui_action(action_json_ptr: i32, action_json_len: i32) -> i32;

// Allocator (addon musi eksportowac):
fn alloc(size: i32) -> i32;    // alokuje bufor w guest memory, zwraca ptr
fn dealloc(ptr: i32, size: i32);
```

---

## 4. Event System Design

### 4.1 Typy eventow

```rust
pub enum EventType {
    // System
    SystemStartup,
    SystemShutdown,
    NodeJoined { node_id: String },
    NodeLeft { node_id: String },

    // Model
    ModelLoaded { model_name: String, engine: String },
    ModelUnloaded { model_name: String },
    InferenceCompleted { model: String, tokens: u32, latency_ms: u64 },

    // Addon
    AddonInstalled { addon_id: String },
    AddonStarted { addon_id: String, instance_id: String },
    AddonStopped { addon_id: String, instance_id: String },
    AddonEvent { addon_id: String, event_name: String, payload: serde_json::Value },

    // User
    UserLoggedIn { user_id: i64, username: String },
    UserLoggedOut { user_id: i64 },
    UserAction { user_id: i64, action: String, data: serde_json::Value },

    // Flow
    FlowStarted { flow_id: i64, request_id: String },
    FlowCompleted { flow_id: i64, request_id: String, status: String },

    // Communication
    MessageReceived { channel: String, from: String, content: String },
    MeetingStarted { meeting_id: String, participants: Vec<String> },
    MeetingEnded { meeting_id: String },

    // Timer
    TimerTick { timer_id: u32 },

    // Custom (addon-defined)
    Custom { event_type: String, payload: serde_json::Value },
}
```

### 4.2 EventBus architektura

```
                    ┌─────────────────────────────────────┐
                    │            EventBus                   │
                    │                                       │
   publish() ──►   │  ┌──────────┐    ┌──────────────┐    │
                    │  │ Dispatcher│──►│ Subscription  │    │
                    │  │          │    │ Registry      │    │
                    │  └──────────┘    │              │    │
                    │       │          │ [addon_id,   │    │
                    │       │          │  event_type, │    │
                    │       │          │  filter,     │    │
                    │       ▼          │  instance_id]│    │
                    │  ┌──────────┐    └──────────────┘    │
                    │  │Permission│                         │
                    │  │  Check   │                         │
                    │  └──────────┘                         │
                    │       │                               │
                    │       ▼                               │
                    │  ┌──────────┐                         │
                    │  │ Delivery │──► WASM on_event()     │
                    │  │  Queue   │──► Flow trigger         │
                    │  │ (mpsc)   │──► Audit log            │
                    │  └──────────┘                         │
                    └─────────────────────────────────────┘
```

Implementacja:
- **tokio::sync::broadcast** dla ogolnych eventow (niski koszt, fire-and-forget)
- **tokio::sync::mpsc** per addon instance dla guaranteed delivery
- **Ring buffer** 4096 eventow w pamieci, starsze wypadaja
- Permission check PRZED delivery (addon nie dostaje eventow do ktorych nie ma uprawnien)
- Events z flow buildera (FlowStarted, FlowCompleted) automatycznie trafiaja na bus

---

## 5. Permission Model

### 5.1 Hierarchia sprawdzania

```
1. Admin bypass — user w grupie "admins" ma pelne uprawnienia
2. Explicit deny — jawne odmowienie (granted=0) zawsze wygrywa
3. User grant — uprawnienie per user (najwyzszy priorytet po deny)
4. Group grant — uprawnienie per group (user dziedziczy z grup)
5. Default deny — brak wpisu = brak uprawnienia
```

### 5.2 Algorytm sprawdzania

```rust
pub struct PermissionChecker {
    db: DbPool,
    cache: DashMap<(String, i64, String, String), PermissionResult>,  // (addon, user, type, resource)
}

impl PermissionChecker {
    pub fn check(
        &self,
        addon_id: &str,
        user_id: i64,
        permission_type: &str,
        resource: Option<&str>,
        access_level: AccessLevel,
    ) -> PermissionResult {
        // 1. Cache lookup
        // 2. Admin bypass
        // 3. Query addon_granted_permissions WHERE addon_id AND user_id AND permission_type
        // 4. Query addon_granted_permissions WHERE addon_id AND group_id IN (user groups) AND permission_type
        // 5. Pattern matching na resource (np. "bielik-*" matchuje "bielik-11b")
        // 6. Access level check (rwd > rw > ro)
        // 7. Cache result (TTL 60s)
    }
}

pub enum AccessLevel { Ro, Rw, Rwd }
pub enum PermissionResult { Granted, Denied, NotConfigured }
```

### 5.3 Uprawnienia na zasoby

| Permission Type | Resource Pattern | Znaczenie |
|----------------|-----------------|-----------|
| `llm` | `*` | Dostep do generowania tekstu (dowolny model) |
| `llm_model` | `bielik-*` | Dostep do konkretnych modeli (glob pattern) |
| `embeddings` | `*` | Dostep do embeddingow |
| `rag` | `*` | Dostep do RAG |
| `storage` | `*` | Sandboxed key-value store |
| `http` | `*.microsoft.com` | HTTP requesty (pattern na domeny) |
| `events` | `message_received` | Subskrypcja konkretnych eventow |
| `ui` | `*` | Renderowanie UI |
| `audio_capture` | `*` | Przechwytywanie mikrofonu |
| `audio_play` | `*` | Odtwarzanie audio |
| `tts` | `*` | Text-to-Speech |
| `stt` | `*` | Speech-to-Text |
| `camera` | `*` | Dostep do kamery |
| `notifications` | `*` | Pokazywanie notyfikacji |
| `background` | `*` | Dzialanie w tle |
| `secrets` | `*` | Szyfrowane sekrety |
| `user_info` | `*` | Informacje o uzytkowniku |
| `timer` | `*` | Timery i background tasks |
| `addon_communicate` | `tentaflow.rag-*` | Komunikacja z innymi addonami (pattern) |

---

## 6. Sync Protocol (CRDT Operations)

### 6.1 Nowe operacje CRDT

Rozszerzamy `CrdtOperation` w `src/mesh/crdt.rs`:

```rust
pub enum CrdtOperation {
    // === ISTNIEJACE (bez zmian) ===
    UpsertService { .. },
    DeleteService { .. },
    UpsertModel { .. },
    DeleteModel { .. },
    UpsertAlias { .. },
    DeleteAlias { .. },
    UpsertFlow { .. },
    UpsertPrompt { .. },
    UpsertApiKey { .. },
    UpsertSetting { .. },

    // === NOWE: Users/Groups (sync przez mesh) ===
    UpsertUser {
        id: i64,
        data_json: String,  // {username, password_hash, display_name, email, role, is_active, ...}
        clock: LamportClock,
    },
    DeleteUser {
        id: i64,
        clock: LamportClock,
    },
    UpsertGroup {
        id: i64,
        data_json: String,  // {name, description, is_system}
        clock: LamportClock,
    },
    DeleteGroup {
        id: i64,
        clock: LamportClock,
    },
    SetUserGroup {
        user_id: i64,
        group_id: i64,
        is_member: bool,    // true = dodaj, false = usun
        clock: LamportClock,
    },

    // === NOWE: Addon metadata (sync) ===
    UpsertAddon {
        addon_id: String,
        data_json: String,   // {version, display_name, manifest_toml, skill_md, blocks_json, platforms_json, status, wasm_hash_sha256}
        clock: LamportClock,
    },
    DeleteAddon {
        addon_id: String,
        clock: LamportClock,
    },

    // === NOWE: Permissions (sync) ===
    UpsertAddonPermission {
        id: i64,
        data_json: String,   // {addon_id, user_id, group_id, permission_type, resource_pattern, access_level, granted}
        clock: LamportClock,
    },
    DeleteAddonPermission {
        id: i64,
        clock: LamportClock,
    },

    // === NOWE: SSO providers (sync) ===
    UpsertSsoProvider {
        id: i64,
        data_json: String,
        clock: LamportClock,
    },
    DeleteSsoProvider {
        id: i64,
        clock: LamportClock,
    },

    // === NOWE: Addon secrets (sync — juz szyfrowane) ===
    UpsertAddonSecret {
        addon_id: String,
        user_id: Option<i64>,
        secret_key: String,
        encrypted_value_b64: String,
        nonce_b64: String,
        clock: LamportClock,
    },

    // === NOWE: Addon resource limits (sync) ===
    UpsertAddonResourceLimits {
        addon_id: String,
        data_json: String,
        clock: LamportClock,
    },
}
```

### 6.2 Co synchronizujemy, a co NIE

| Dane | Sync? | Powod |
|------|-------|-------|
| users | TAK | Uzytkownik loguje sie na dowolnym nodzie |
| groups | TAK | Grupy globalne |
| user_groups | TAK | Przypisania globalne |
| sso_providers | TAK | Konfiguracja SSO musi byc taka sama |
| addons (metadane) | TAK | Kazdy node musi znac liste addonow |
| addon_wasm (blobs) | WARUNKOWO | Sync jesli platforma targetu pasuje do noda |
| addon_granted_permissions | TAK | Uprawnienia musza byc spojne |
| addon_secrets | TAK | Juz szyfrowane — bezpiecznie sync |
| addon_resource_limits | TAK | Limity musza byc spojne |
| addon_instances | NIE | Runtime state — per host |
| addon_storage | NIE | Sandbox per node |
| addon_event_subscriptions | NIE | Runtime state — per host |
| audit_log | NIE | Per host (za duzo danych) |
| addon_migrations | NIE | Per host |

### 6.3 Sync plikow WASM

Osobny mechanizm (nie przez CRDT — pliki za duze):

```
1. Node A instaluje addon -> CrdtOperation::UpsertAddon z wasm_hash_sha256
2. Node B odbiera UpsertAddon, sprawdza czy ma plik .wasm o tym hashu
3. Jesli NIE — sprawdza platform target (czy addon jest dla tego noda)
4. Jesli TAK — wysyla QUIC request GET_ADDON_WASM {addon_id, hash} do dowolnego peera
5. Peer odpowiada strumieniem bajtow
6. Node B weryfikuje SHA-256, zapisuje do addon_wasm
7. Node B uruchamia migracje addonu i on_install()
```

---

## 7. Addon SDK API (Rust template)

### 7.1 Cargo.toml addonu

```toml
[package]
name = "addon-hello-world"
version = "0.1.0"
edition = "2021"

[lib]
crate-type = ["cdylib"]  # kompilacja do .wasm

[dependencies]
tentaflow-addon-sdk = "0.1"

[profile.release]
opt-level = "s"        # optymalizuj na rozmiar
lto = true
strip = true
```

### 7.2 src/lib.rs (szablon)

```rust
// =============================================================================
// Plik: lib.rs
// Opis: Szablon addonu TentaFlow.AI — Hello World
// =============================================================================

use tentaflow_addon_sdk::prelude::*;

#[addon_main]
struct HelloWorldAddon;

#[addon_lifecycle]
impl HelloWorldAddon {
    fn on_install(&self) -> AddonResult<()> {
        log::info("Hello World addon zainstalowany");
        Ok(())
    }

    fn on_start(&self) -> AddonResult<()> {
        // Subskrybuj eventy
        events::subscribe("message_received", None)?;

        // Zarejestruj timer (co 60 sekund)
        timer::set_interval(60_000)?;

        // Wyrenderuj poczatkowy UI
        ui::render_panel("main", &ui::column(vec![
            ui::text("Hello World Addon").bold(),
            ui::text("Status: aktywny").color("green"),
            ui::button("Wyslij pozdrowienia").on_click("greet"),
        ]))?;

        Ok(())
    }

    fn on_stop(&self) -> AddonResult<()> {
        log::info("Hello World addon zatrzymany");
        Ok(())
    }

    fn on_event(&self, event: Event) -> AddonResult<()> {
        match event.event_type.as_str() {
            "message_received" => {
                let msg: String = event.payload.get_string("content")?;
                log::info(&format!("Otrzymano wiadomosc: {}", msg));
            }
            _ => {}
        }
        Ok(())
    }

    fn on_ui_action(&self, action: UiAction) -> AddonResult<()> {
        if action.action_id == "greet" {
            ui::show_notification("Pozdrowienia!", "Witaj z Hello World Addon!", "info")?;
        }
        Ok(())
    }

    fn on_timer(&self, timer_id: u32) -> AddonResult<()> {
        log::info("Tick!");
        Ok(())
    }
}

// Rejestracja tools (opcjonalna)
#[tool(
    name = "hello_greet",
    description = "Pozdrow uzytkownika po imieniu",
    parameters = r#"{"type":"object","properties":{"name":{"type":"string","description":"Imie uzytkownika"}},"required":["name"]}"#
)]
fn greet(params: ToolParams) -> AddonResult<String> {
    let name = params.get_string("name")?;
    Ok(format!("Czesc, {}! Pozdrowienia z Hello World Addon!", name))
}
```

---

## 8. UI Component System

### 8.1 Deklaratywne komponenty (JSON)

```json
{
  "type": "column",
  "children": [
    {
      "type": "text",
      "id": "title",
      "props": {
        "content": "Teams Bot",
        "variant": "heading",
        "size": "lg"
      }
    },
    {
      "type": "card",
      "children": [
        {
          "type": "row",
          "children": [
            {
              "type": "image",
              "props": { "src": "addon://icon.png", "width": 48, "height": 48 }
            },
            {
              "type": "column",
              "children": [
                { "type": "text", "props": { "content": "Spotkanie Dev", "variant": "subtitle" } },
                { "type": "text", "id": "status", "props": { "content": "Aktywne", "color": "green" } }
              ]
            }
          ]
        }
      ]
    },
    {
      "type": "table",
      "id": "messages",
      "props": {
        "columns": ["Od", "Wiadomosc", "Czas"],
        "rows": [
          ["Jan", "Czesc!", "14:30"],
          ["Anna", "Hej!", "14:31"]
        ]
      }
    },
    {
      "type": "form",
      "children": [
        {
          "type": "input",
          "id": "msg_input",
          "props": { "placeholder": "Wpisz wiadomosc...", "variant": "text" }
        },
        {
          "type": "select",
          "id": "channel_select",
          "props": {
            "options": [
              {"value": "general", "label": "General"},
              {"value": "dev", "label": "Dev Team"}
            ],
            "selected": "general"
          }
        },
        {
          "type": "button",
          "props": { "label": "Wyslij", "variant": "primary", "on_click": "send_message" }
        }
      ]
    },
    {
      "type": "tabs",
      "props": {
        "tabs": [
          {"id": "chat", "label": "Czat"},
          {"id": "files", "label": "Pliki"},
          {"id": "calendar", "label": "Kalendarz"}
        ],
        "active": "chat"
      }
    }
  ]
}
```

### 8.2 Lista komponentow

| Komponent | Props | Opis |
|-----------|-------|------|
| `text` | content, variant (body/heading/subtitle/caption), size (sm/md/lg), color, bold, italic | Tekst |
| `input` | placeholder, variant (text/password/number/email/textarea), value, on_change, disabled | Pole tekstowe |
| `button` | label, variant (primary/secondary/danger/ghost), on_click, disabled, loading | Przycisk |
| `select` | options [{value, label}], selected, on_change, multiple, placeholder | Dropdown |
| `table` | columns [string], rows [[string]], on_row_click, sortable, paginated | Tabela |
| `list` | items [{id, primary, secondary, icon}], on_item_click | Lista |
| `image` | src (addon:// lub https://), width, height, alt | Obrazek |
| `card` | title, subtitle, children, elevated | Karta |
| `tabs` | tabs [{id, label}], active, on_tab_change | Zakladki |
| `form` | children, on_submit | Formularz |
| `row` | children, gap, align, justify | Wiersz (flex) |
| `column` | children, gap, align | Kolumna (flex) |
| `divider` | - | Linia separujaca |
| `progress` | value (0-100), variant (linear/circular), label | Pasek postepu |
| `badge` | content, color | Etykieta |
| `toggle` | checked, on_change, label | Przelacznik |
| `chip` | label, on_click, on_delete, selected | Chip |
| `alert` | message, variant (info/warning/error/success), dismissible | Alert |

### 8.3 Renderowanie

Core renderuje komponenty na aktualnym backendzie:

```
Addon JSON ──► UIComponentTree ──► Backend Renderer
                                   ├── HTML/CSS (Dashboard www)
                                   ├── WKWebView (iOS) — ten sam HTML
                                   └── [przyszlosc] WGPU (egui)
```

HTML renderer: mapowanie 1:1 na `<div>`, `<button>`, `<input>`, `<table>` z CSS classami (Tailwind-like).

---

## 9. manifest.toml Schema

```toml
[addon]
id = "tentaflow.teams"
name = "Microsoft Teams"
version = "1.0.0"
description = "Integracja z Microsoft Teams — wiadomosci, pliki, kalendarz, bot na spotkaniach"
author = "TentaFlow.AI"
license = "MIT"
min_core_version = "0.5.0"

[platforms]
targets = ["linux_x86_64", "linux_aarch64", "macos_x86_64", "macos_aarch64", "windows_x86_64"]
# Pominiete: "android", "ios" — Teams bot wymaga stabilnej sieci

[permissions]
# Kazde uprawnienie z powodem (wyswietlanym adminowi)

[[permissions.required]]
type = "http"
resource = "*.microsoft.com,*.office.com,*.microsoftonline.com"
access = "rw"
reason = "Komunikacja z Microsoft Graph API"

[[permissions.required]]
type = "llm"
resource = "*"
access = "ro"
reason = "Generowanie odpowiedzi na wiadomosci"

[[permissions.required]]
type = "llm_model"
resource = "bielik-11b"
access = "ro"
reason = "Model do odpowiadania po polsku"

[[permissions.required]]
type = "secrets"
access = "rw"
reason = "Przechowywanie tokenow OAuth"

[[permissions.required]]
type = "storage"
access = "rw"
reason = "Cache wiadomosci i plikow"

[[permissions.required]]
type = "events"
resource = "message_received,meeting_started,meeting_ended"
access = "rw"
reason = "Reagowanie na wiadomosci i spotkania"

[[permissions.required]]
type = "ui"
access = "rw"
reason = "Panel konfiguracji i podglad wiadomosci"

[[permissions.required]]
type = "notifications"
access = "rw"
reason = "Powiadomienia o nowych wiadomosciach"

[[permissions.required]]
type = "background"
access = "ro"
reason = "Nasluchiwanie na wiadomosci Teams w tle"

[[permissions.required]]
type = "audio_capture"
access = "ro"
reason = "Przechwytywanie audio ze spotkan (STT)"

[[permissions.required]]
type = "audio_play"
access = "rw"
reason = "Odtwarzanie odpowiedzi glosowych na spotkaniach"

[[permissions.required]]
type = "stt"
access = "ro"
reason = "Transkrypcja audio ze spotkan"

[[permissions.required]]
type = "tts"
access = "ro"
reason = "Generowanie mowy do odpowiedzi na spotkaniach"

[[permissions.required]]
type = "timer"
access = "ro"
reason = "Periodyczne sprawdzanie nowych wiadomosci"

[[permissions.optional]]
type = "camera"
access = "ro"
reason = "Opcjonalny podglad kamery uczestnikow"

[resources]
# Domyslne limity (admin moze nadpisac)
max_instances = 20
cpu_millicores = 1000
ram_mb = 512
storage_mb = 500
rate_limit_rps = 200

[config_schema]
# JSON Schema dla konfiguracji instancji (wyswietlane w UI przy tworzeniu instancji)
type = "object"
properties.tenant_id = { type = "string", title = "Azure Tenant ID", description = "ID tenanta Azure AD" }
properties.channel_filter = { type = "string", title = "Filtr kanalow", description = "Regex na nazwy kanalow do monitorowania", default = ".*" }
properties.auto_respond = { type = "boolean", title = "Auto-odpowiedz", description = "Automatyczne odpowiadanie na wzmianki", default = true }
properties.language = { type = "string", title = "Jezyk", enum = ["pl", "en", "de"], default = "pl" }
required = ["tenant_id"]

[tools]
# Narzedzia LLM zdefiniowane tutaj (alternatywnie w kodzie)
[[tools.list]]
name = "teams_send_message"
description = "Wyslij wiadomosc na kanal Microsoft Teams"
parameters = '{"type":"object","properties":{"channel":{"type":"string","description":"Nazwa kanalu"},"message":{"type":"string","description":"Tresc wiadomosci"}},"required":["channel","message"]}'

[[tools.list]]
name = "teams_read_messages"
description = "Przeczytaj ostatnie wiadomosci z kanalu Teams"
parameters = '{"type":"object","properties":{"channel":{"type":"string","description":"Nazwa kanalu"},"count":{"type":"integer","description":"Liczba wiadomosci","default":10}},"required":["channel"]}'

[[tools.list]]
name = "teams_list_channels"
description = "Wylistuj dostepne kanaly Teams"
parameters = '{"type":"object","properties":{}}'

[lifecycle]
# Timeout na kazdy hook lifecycle
install_timeout_ms = 30000
start_timeout_ms = 10000
stop_timeout_ms = 5000

[migrations]
# Kolejnosc migracji SQL (pliki w katalogu migrations/)
files = ["001_initial.sql", "002_add_cache.sql"]
```

---

## 10. SKILL.md Format

```markdown
# Microsoft Teams Bot

## Kiedy uzyc

Uzywaj tego narzedzia gdy uzytkownik:
- Chce wyslac wiadomosc na Teams
- Pyta o wiadomosci z Teams
- Chce sprawdzic kanaly lub pliki na Teams
- Prosi o dolaczenie do spotkania Teams
- Pyta o kalendarz Teams

## Kontekst

Ten addon daje dostep do Microsoft Teams organizacji uzytkownika.
Mozesz czytac i wysylac wiadomosci, przegladac pliki, sprawdzac kalendarz.
Na spotkaniach mozesz dolaczac jako bot, sluchac i odpowiadac.

## Narzedzia

### teams_send_message
Wysyla wiadomosc na kanal Teams.
- **channel** (wymagany): nazwa kanalu, np. "General", "Dev Team"
- **message** (wymagany): tresc wiadomosci (plain text lub Markdown)

Przyklad: `teams_send_message(channel="General", message="Notatki ze spotkania: ...")`

### teams_read_messages
Czyta ostatnie wiadomosci z kanalu.
- **channel** (wymagany): nazwa kanalu
- **count** (opcjonalny, domyslnie 10): ile wiadomosci

Przyklad: `teams_read_messages(channel="Dev Team", count=5)`

### teams_list_channels
Lista dostepnych kanalow.
Brak parametrow.

## Wazne

- Odpowiadaj w jezyku uzytkownika (domyslnie polski)
- Nie wysylaj wiadomosci bez wyraznej prosby uzytkownika
- Przy czytaniu wiadomosci, podsumuj je krotko zamiast cytowac calosc
- Jesli brak dostepu do kanalu, poinformuj uzytkownika
```

---

## 11. blocks.json Format

```json
{
  "blocks": [
    {
      "type": "teams_read_channel",
      "category": "addon",
      "label": "Teams: Czytaj kanal",
      "description": "Pobiera wiadomosci z kanalu Microsoft Teams",
      "icon": "message-square",
      "config_schema": {
        "type": "object",
        "properties": {
          "channel": {
            "type": "string",
            "title": "Kanal",
            "description": "Nazwa kanalu Teams"
          },
          "count": {
            "type": "integer",
            "title": "Liczba wiadomosci",
            "default": 10,
            "minimum": 1,
            "maximum": 100
          }
        },
        "required": ["channel"]
      },
      "input_ports": ["trigger"],
      "output_ports": ["messages", "error"]
    },
    {
      "type": "teams_send_message",
      "category": "addon",
      "label": "Teams: Wyslij wiadomosc",
      "description": "Wysyla wiadomosc na kanal Microsoft Teams",
      "icon": "send",
      "config_schema": {
        "type": "object",
        "properties": {
          "channel": {
            "type": "string",
            "title": "Kanal"
          },
          "message_template": {
            "type": "string",
            "title": "Szablon wiadomosci",
            "description": "Mozesz uzyc {{input}} jako placeholder"
          }
        },
        "required": ["channel"]
      },
      "input_ports": ["input"],
      "output_ports": ["success", "error"]
    },
    {
      "type": "teams_meeting_join",
      "category": "addon",
      "label": "Teams: Dolacz do spotkania",
      "description": "Dolacza bota do spotkania Teams",
      "icon": "video",
      "config_schema": {
        "type": "object",
        "properties": {
          "meeting_id": {
            "type": "string",
            "title": "ID spotkania"
          },
          "enable_stt": {
            "type": "boolean",
            "title": "Wlacz transkrypcje",
            "default": true
          },
          "enable_tts": {
            "type": "boolean",
            "title": "Wlacz odpowiedzi glosowe",
            "default": false
          }
        },
        "required": ["meeting_id"]
      },
      "input_ports": ["trigger"],
      "output_ports": ["transcription", "audio_stream", "error"]
    }
  ]
}
```

---

## 12. Audit Log Format

### 12.1 Struktura rekordu

```rust
pub struct AuditEntry {
    // Kto
    pub user_id: Option<i64>,
    pub username: Option<String>,

    // Co
    pub addon_id: Option<String>,
    pub instance_id: Option<String>,
    pub action: String,              // "llm.generate", "storage.set", "http.request"
    pub action_hash: u64,            // FNV-1a hash (do szybkiego filtrowania)

    // Szczegoly
    pub resource_type: Option<String>,
    pub resource_id: Option<String>,
    pub details: AuditDetails,

    // Wynik
    pub result: AuditResult,        // Ok, Denied, Error
    pub error_message: Option<String>,
    pub duration_us: Option<u64>,
}

pub enum AuditDetails {
    LlmGenerate { model: String, prompt_tokens: u32, completion_tokens: u32 },
    StorageOp { key: String, value_size: usize },
    HttpRequest { method: String, url: String, status: u16, response_size: usize },
    EventPublish { event_type: String },
    PermissionCheck { permission_type: String, resource: String },
    LifecycleHook { hook: String },
    UiRender { panel_id: String },
    ToolCall { tool_name: String },
    SecretAccess { key: String },
    TimerOp { timer_id: u32 },
    AudioOp { op: String, duration_ms: u64 },
}
```

### 12.2 Retencja

```
- Domyslnie: 90 dni
- Background task (co godzine): DELETE FROM audit_log WHERE created_at < datetime('now', '-X days')
- VACUUM co 24h (jesli usunieto > 10000 rekordow)
- Kompaktowy format: action_hash (8 bajtow) zamiast pelnych stringow w indeksie
```

---

## 13. Kolejnosc implementacji (fazy)

### Faza 1: Fundament — Users, Groups, Permissions, DB

| ID | Zadanie | Zlozonosc | Zaleznosci | Agent |
|----|---------|-----------|------------|-------|
| 1.1 | Migracje 14-21 (schemat DB) | L | - | programista-rust |
| 1.2 | Modele danych (structs w db/models.rs) | M | 1.1 | programista-rust |
| 1.3 | Repository functions (CRUD users/groups/permissions) | L | 1.2 | programista-rust |
| 1.4 | PermissionChecker z cache | M | 1.3 | programista-rust |
| 1.5 | Dashboard API: zarzadzanie users/groups | M | 1.3 | programista-rust |
| 1.6 | Rozszerzenie CrdtOperation o users/groups/permissions | M | 1.3 | programista-rust |
| 1.7 | Rozszerzenie crdt_store.rs apply_to_db | M | 1.6 | programista-rust |
| 1.8 | Testy jednostkowe (permissions, CRDT) | M | 1.4, 1.7 | tester-jednostkowy |

**Szacunek: 8-10 dni pracy**

### Faza 2: WASM Runtime + Host Functions

| ID | Zadanie | Zlozonosc | Zaleznosci | Agent |
|----|---------|-----------|------------|-------|
| 2.1 | Wasmtime integration (Engine, Store, Linker) | L | 1.1 | programista-rust |
| 2.2 | Instance pool (pre-warmed WASM instances) | L | 2.1 | programista-rust |
| 2.3 | Host functions: storage API | M | 2.1 | programista-rust |
| 2.4 | Host functions: LLM API | L | 2.1 | programista-rust |
| 2.5 | Host functions: log API | S | 2.1 | programista-rust |
| 2.6 | Host functions: user API + permission check | M | 2.1, 1.4 | programista-rust |
| 2.7 | Host functions: secrets API (AES-256-GCM) | M | 2.1 | programista-rust |
| 2.8 | Host functions: HTTP API (proxy z audit) | M | 2.1 | programista-rust |
| 2.9 | Host functions: timer API | M | 2.1 | programista-rust |
| 2.10 | AddonManager — install, uninstall, start, stop | L | 2.1, 2.2 | programista-rust |
| 2.11 | Manifest parser (TOML) | M | - | programista-rust |
| 2.12 | Resource limits enforcement (CPU, RAM per instance) | L | 2.2 | programista-rust |
| 2.13 | Crash isolation (catch panic in WASM) | M | 2.1 | programista-rust |
| 2.14 | Testy (mock addon, lifecycle, limits) | L | 2.1-2.13 | tester-jednostkowy |

**Szacunek: 14-18 dni pracy**

### Faza 3: Event Bus + Tools + Flow

| ID | Zadanie | Zlozonosc | Zaleznosci | Agent |
|----|---------|-----------|------------|-------|
| 3.1 | EventBus (broadcast + mpsc delivery) | L | - | programista-rust |
| 3.2 | Host functions: event API (subscribe, publish) | M | 3.1, 2.1 | programista-rust |
| 3.3 | Event dispatcher (WASM on_event callback) | M | 3.1, 3.2 | programista-rust |
| 3.4 | Addon tools registration | M | 2.10 | programista-rust |
| 3.5 | LLM tool calling integration | L | 3.4 | programista-rust |
| 3.6 | Flow builder: addon block adapter | L | 2.10 | programista-rust |
| 3.7 | blocks.json parser + registration | M | 3.6 | programista-rust |
| 3.8 | Testy (events, tool calling, flow) | L | 3.1-3.7 | tester-jednostkowy |

**Szacunek: 10-12 dni pracy**

### Faza 4: UI Framework

| ID | Zadanie | Zlozonosc | Zaleznosci | Agent |
|----|---------|-----------|------------|-------|
| 4.1 | UIComponentTree parser (JSON -> struct) | M | - | programista-rust |
| 4.2 | Host functions: UI API | M | 4.1, 2.1 | programista-rust |
| 4.3 | HTML renderer (UIComponentTree -> HTML/CSS) | L | 4.1 | programista-frontend |
| 4.4 | Dashboard: addon panel embedding | M | 4.3 | programista-frontend |
| 4.5 | UI action handler (button click -> WASM callback) | M | 4.2 | programista-rust |
| 4.6 | Partial update (ui_update_widget) | M | 4.2, 4.3 | programista-rust |
| 4.7 | Testy | M | 4.1-4.6 | tester-jednostkowy |

**Szacunek: 8-10 dni pracy**

### Faza 5: Audio + SSO

| ID | Zadanie | Zlozonosc | Zaleznosci | Agent |
|----|---------|-----------|------------|-------|
| 5.1 | Host functions: audio API (capture, play) | L | 2.1 | programista-rust |
| 5.2 | SSO/OIDC module (auth flow, token refresh) | L | 1.3 | programista-rust |
| 5.3 | Dashboard: SSO configuration UI | M | 5.2 | programista-frontend |
| 5.4 | Dashboard: addon management UI (install, perms, config) | L | 2.10 | programista-frontend |
| 5.5 | Testy SSO (mock OIDC provider) | M | 5.2 | tester-jednostkowy |

**Szacunek: 8-10 dni pracy**

### Faza 6: Audit + Sync + SDK

| ID | Zadanie | Zlozonosc | Zaleznosci | Agent |
|----|---------|-----------|------------|-------|
| 6.1 | AuditLogger (zapis + retencja) | M | 1.1 | programista-rust |
| 6.2 | Audit query API + Dashboard UI | M | 6.1 | programista-rust, programista-frontend |
| 6.3 | Addon WASM sync przez mesh (QUIC transfer) | L | 2.10 | programista-rust |
| 6.4 | Addon SDK crate | L | 2.1-2.9 | programista-rust |
| 6.5 | SDK: dokumentacja + przykady | M | 6.4 | dokumentator |
| 6.6 | Testy end-to-end (install addon, use, sync) | XL | all | tester-e2e |

**Szacunek: 10-12 dni pracy**

### Faza 7: Teams Addon (docelowy)

| ID | Zadanie | Zlozonosc | Zaleznosci | Agent |
|----|---------|-----------|------------|-------|
| 7.1 | Teams addon: Microsoft Graph API client | L | 6.4 | programista-rust |
| 7.2 | Teams addon: wiadomosci (czytaj/wysylaj) | L | 7.1 | programista-rust |
| 7.3 | Teams addon: pliki, kalendarz | M | 7.1 | programista-rust |
| 7.4 | Teams addon: bot na spotkaniach (audio) | XL | 7.1, 5.1 | programista-rust |
| 7.5 | Teams addon: multi-instance | M | 7.1-7.4 | programista-rust |
| 7.6 | Teams addon: UI panels | M | 7.1-7.4, 4.3 | programista-frontend |
| 7.7 | Teams addon: testy | L | 7.1-7.6 | tester-jednostkowy |

**Szacunek: 14-18 dni pracy**

---

## 14. Szacowany naklad pracy per faza

| Faza | Nazwa | Szacunek | Kumulatywnie |
|------|-------|----------|--------------|
| 1 | Fundament (Users/Groups/Perms/DB) | 8-10 dni | 8-10 dni |
| 2 | WASM Runtime + Host Functions | 14-18 dni | 22-28 dni |
| 3 | Event Bus + Tools + Flow | 10-12 dni | 32-40 dni |
| 4 | UI Framework | 8-10 dni | 40-50 dni |
| 5 | Audio + SSO | 8-10 dni | 48-60 dni |
| 6 | Audit + Sync + SDK | 10-12 dni | 58-72 dni |
| 7 | Teams Addon | 14-18 dni | 72-90 dni |
| **RAZEM** | | **72-90 dni roboczych** | ~3.5-4.5 miesiaca |

---

## Ryzyka

| Ryzyko | Prawdopodobienstwo | Wplyw | Mitygacja |
|--------|-------------------|-------|-----------|
| Wasmtime API changes (wersja 29) | Srednie | Sredni | Pin version, monitoruj changelog |
| WASM guest memory management bugs | Wysokie | Wysoki | Fuzzing alloc/dealloc, hardened bounds checking |
| Performance instance pool | Srednie | Wysoki | Benchmark early (Faza 2), COW pre-instantiation |
| Microsoft Graph API rate limits | Srednie | Sredni | Exponential backoff, queue per instance |
| CRDT conflict retentaflown edge cases | Niskie | Wysoki | Property-based tests (proptest), formal spec |
| Audio latency (STT/TTS na spotkaniach) | Wysokie | Sredni | Direct PCM streaming, skip WASM for audio path |
| WASM binary size (duze addony) | Niskie | Niski | wasm-opt, strip, split modules |
| SSO provider compatibility | Srednie | Sredni | Testuj z kazdym providerem, fallback na generic OIDC |

---

## Kolejnosc wykonania

```
Faza 1 ──────────────────────────────────────────────►
         Faza 2 (zalezy od 1.1) ─────────────────────────────────────────►
                                    Faza 3 (zalezy od 2.1, 2.10) ────────────────────►
                                    Faza 4 (rownolegla z 3) ─────────────────────────►
                                                  Faza 5 (czesciowo rownolegla z 3/4) ────────►
                                                                   Faza 6 (zalezy od all) ────────────►
                                                                                   Faza 7 (zalezy od 6.4) ──────────────────►
```

Fazy 3 i 4 moga byc realizowane rownolegle (rozni programisci).
Faza 5 (SSO) moze startowac jeszcze w trakcie Fazy 3.

---

## Kryteria akceptacji

- [ ] Addon hello-world instaluje sie, startuje, odpowiada na eventy, renderuje UI
- [ ] Permission check blokuje niedozwolone operacje (zero false positives)
- [ ] 100 instancji addonu dziala jednoczesnie bez degradacji
- [ ] Crash jednego addonu nie wplywa na Core ani inne addony
- [ ] CRDT sync users/groups/permissions dziala miedzy 3+ nodami
- [ ] SSO login przez Azure AD dziala end-to-end
- [ ] Audit log rejestruje kazda operacje addonu
- [ ] Flow builder widzi bloczki z addonow i wykonuje je
- [ ] LLM poprawnie wywoluje addon tools (function calling)
- [ ] Teams addon wysyla/czyta wiadomosci w multi-instance
- [ ] Testy jednostkowe pokrywaja >80% logiki addon runtime
- [ ] Testy E2E: install -> configure -> use -> uninstall

---

## Istotne pliki referencyjne

- `/Users/critix/repos/dotnet/nextapp/TentaFlow.Core/src/db/migrations.rs` — istniejace migracje (13), dodajemy 14-21
- `/Users/critix/repos/dotnet/nextapp/TentaFlow.Core/src/db/models.rs` — istniejace modele DB, rozszerzamy
- `/Users/critix/repos/dotnet/nextapp/TentaFlow.Core/src/mesh/crdt.rs` — CrdtOperation enum do rozszerzenia
- `/Users/critix/repos/dotnet/nextapp/TentaFlow.Core/src/mesh/crdt_store.rs` — apply_to_db do rozszerzenia
- `/Users/critix/repos/dotnet/nextapp/TentaFlow.Core/src/mesh/pipeline.rs` — mesh pipeline (dodajemy addon sync)
- `/Users/critix/repos/dotnet/nextapp/TentaFlow.Core/src/flow_engine/types.rs` — FlowNode do rozszerzenia
- `/Users/critix/repos/dotnet/nextapp/TentaFlow.Core/src/routing/router.rs` — Router (addon tools rejestrowane tu)
- `/Users/critix/repos/dotnet/nextapp/TentaFlow.Core/Cargo.toml` — dodajemy wasmtime dependency
