// =============================================================================
// Plik: db/migrations.rs
// Opis: Schemat bazy danych SQLite i mechanizm migracji wersjonowanych.
// =============================================================================

use anyhow::Result;
use rusqlite::Connection;
use tracing::info;

/// Uruchamia migracje bazy danych.
pub fn run(conn: &Connection) -> Result<()> {
    // Utworz tabele migracji
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS _migrations (
            version INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            applied_at TEXT NOT NULL DEFAULT (datetime('now'))
        );
    ",
    )?;

    let current_version: i64 = conn.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM _migrations",
        [],
        |row| row.get(0),
    )?;

    let migrations = get_migrations();

    for (version, name, sql) in migrations {
        if *version > current_version {
            info!("Migracja {}: {}", version, name);
            let tx = conn.unchecked_transaction()?;
            tx.execute_batch(sql)?;
            tx.execute(
                "INSERT INTO _migrations (version, name) VALUES (?1, ?2)",
                rusqlite::params![version, name],
            )?;
            tx.commit()?;
        }
    }

    Ok(())
}

fn get_migrations() -> &'static [(i64, &'static str, &'static str)] {
    &[(
        1,
        "initial_schema",
        "
            -- Serwisy AI (LLM, RAG, Embeddings, STT, TTS, Memory)
            CREATE TABLE IF NOT EXISTS services (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL UNIQUE,
                service_type TEXT NOT NULL CHECK(service_type IN ('llm','embedding','rag','vision','stt','tts','memory')),
                strategy TEXT NOT NULL DEFAULT 'single' CHECK(strategy IN ('single','least_loaded','round_robin','weighted')),
                model_category TEXT DEFAULT 'main',
                status TEXT NOT NULL DEFAULT 'active' CHECK(status IN ('active','disabled','maintenance')),
                config_json TEXT NOT NULL DEFAULT '{}',
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now'))
            );

            -- Backendy serwisow (1 serwis moze miec N backendow)
            CREATE TABLE IF NOT EXISTS service_backends (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                service_id INTEGER NOT NULL REFERENCES services(id) ON DELETE CASCADE,
                connection_type TEXT NOT NULL CHECK(connection_type IN ('openai_api','quic')),
                config_json TEXT NOT NULL DEFAULT '{}',
                max_concurrent INTEGER NOT NULL DEFAULT 50,
                timeout_ms INTEGER NOT NULL DEFAULT 30000,
                weight INTEGER NOT NULL DEFAULT 1,
                model_name_override TEXT,
                health_check_path TEXT,
                is_active INTEGER NOT NULL DEFAULT 1
            );

            -- Klucze API
            CREATE TABLE IF NOT EXISTS api_keys (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                key_hash TEXT NOT NULL UNIQUE,
                key_prefix TEXT NOT NULL,
                name TEXT NOT NULL,
                rate_limit_rps INTEGER NOT NULL DEFAULT 100,
                is_active INTEGER NOT NULL DEFAULT 1,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                last_used_at TEXT
            );

            -- Aliasy serwisow
            CREATE TABLE IF NOT EXISTS service_aliases (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                alias TEXT NOT NULL UNIQUE,
                target_service_id INTEGER NOT NULL REFERENCES services(id) ON DELETE CASCADE
            );

            -- Ustawienia routera (key-value)
            CREATE TABLE IF NOT EXISTS settings (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL,
                updated_at TEXT NOT NULL DEFAULT (datetime('now'))
            );

            -- Uzytkownicy dashboardu
            CREATE TABLE IF NOT EXISTS users (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                username TEXT NOT NULL UNIQUE,
                password_hash TEXT NOT NULL,
                role TEXT NOT NULL DEFAULT 'admin' CHECK(role IN ('admin','viewer')),
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                last_login_at TEXT
            );

            -- Indeksy
            CREATE INDEX IF NOT EXISTS idx_services_type ON services(service_type);
            CREATE INDEX IF NOT EXISTS idx_service_backends_service ON service_backends(service_id);
            CREATE INDEX IF NOT EXISTS idx_api_keys_prefix ON api_keys(key_prefix);
            CREATE INDEX IF NOT EXISTS idx_service_aliases_target ON service_aliases(target_service_id);
        ",
    ),
    (
        2,
        "add_agents_table",
        "
            CREATE TABLE IF NOT EXISTS agents (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                agent_id TEXT NOT NULL UNIQUE,
                hostname TEXT NOT NULL,
                machine_id TEXT,
                os_info TEXT,
                gpu_info_json TEXT,
                docker_version TEXT,
                agent_version TEXT,
                labels_json TEXT,
                status TEXT NOT NULL DEFAULT 'disconnected',
                last_seen_at TEXT,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            CREATE INDEX IF NOT EXISTS idx_agents_agent_id ON agents(agent_id);
            CREATE INDEX IF NOT EXISTS idx_agents_status ON agents(status);
        ",
    ),
    (
        3,
        "flow_builder_tables",
        "
            -- Prompty systemowe i szablony
            CREATE TABLE IF NOT EXISTS prompts (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                prompt_id TEXT UNIQUE NOT NULL,
                name TEXT NOT NULL,
                description TEXT,
                content TEXT NOT NULL,
                prompt_type TEXT NOT NULL CHECK(prompt_type IN ('system','suffix','template','user')),
                default_model TEXT,
                variables TEXT,
                cache_priority INTEGER DEFAULT 50,
                is_active INTEGER NOT NULL DEFAULT 1,
                version INTEGER DEFAULT 1,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now'))
            );

            -- Rejestr modeli AI
            CREATE TABLE IF NOT EXISTS model_registry (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                model_name TEXT UNIQUE NOT NULL,
                display_name TEXT,
                service_type TEXT NOT NULL CHECK(service_type IN ('llm','embedding','stt','tts','rag','memory')),
                connection_type TEXT NOT NULL CHECK(connection_type IN ('quic','openai_api','internal')),
                service_id INTEGER REFERENCES services(id) ON DELETE SET NULL,
                flow_id INTEGER,
                is_public INTEGER NOT NULL DEFAULT 1,
                is_active INTEGER NOT NULL DEFAULT 1,
                config_json TEXT NOT NULL DEFAULT '{}',
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );

            -- Aliasy modeli (np. 'gpt4' -> 'bielik-11b')
            CREATE TABLE IF NOT EXISTS model_aliases (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                alias TEXT UNIQUE NOT NULL,
                target_model TEXT NOT NULL,
                is_active INTEGER NOT NULL DEFAULT 1
            );

            -- Definicje flow (przeplywy przetwarzania)
            CREATE TABLE IF NOT EXISTS flows (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL,
                description TEXT,
                version INTEGER DEFAULT 1,
                is_default INTEGER NOT NULL DEFAULT 0,
                service_type TEXT,
                flow_json TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'draft' CHECK(status IN ('draft','active','archived')),
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now'))
            );

            -- Powiazania flow z modelami (pattern matching)
            CREATE TABLE IF NOT EXISTS flow_model_bindings (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                flow_id INTEGER NOT NULL REFERENCES flows(id) ON DELETE CASCADE,
                model_pattern TEXT NOT NULL UNIQUE,
                priority INTEGER DEFAULT 0
            );

            -- Szablony wezlow flow (paleta komponentow)
            CREATE TABLE IF NOT EXISTS flow_node_templates (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                node_type TEXT NOT NULL,
                category TEXT NOT NULL CHECK(category IN ('trigger','service','transform','logic','output')),
                label TEXT NOT NULL,
                description TEXT,
                default_config TEXT NOT NULL DEFAULT '{}',
                icon TEXT
            );

            -- Reguly filtrowania danych osobowych (PII)
            CREATE TABLE IF NOT EXISTS pii_rules (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL,
                category TEXT NOT NULL,
                pattern TEXT NOT NULL,
                replacement TEXT NOT NULL DEFAULT '[UKRYTY]',
                is_active INTEGER NOT NULL DEFAULT 1,
                priority INTEGER DEFAULT 0,
                description TEXT,
                test_examples TEXT,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );

            -- Wzorce szybkiej sciezki (fast path - omija pelne przetwarzanie)
            CREATE TABLE IF NOT EXISTS fast_path_patterns (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                module TEXT NOT NULL,
                pattern_type TEXT NOT NULL,
                pattern TEXT NOT NULL,
                match_type TEXT NOT NULL DEFAULT 'exact' CHECK(match_type IN ('exact','starts_with','contains','regex','length')),
                result_json TEXT NOT NULL,
                is_active INTEGER NOT NULL DEFAULT 1,
                priority INTEGER DEFAULT 0
            );

            -- Reguly czyszczenia tekstu dla TTS
            CREATE TABLE IF NOT EXISTS tts_cleaning_rules (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                rule_type TEXT NOT NULL CHECK(rule_type IN ('abbreviation','phonetic','emoji_range','regex_remove')),
                pattern TEXT NOT NULL,
                replacement TEXT,
                language TEXT NOT NULL DEFAULT 'pl',
                is_active INTEGER NOT NULL DEFAULT 1,
                priority INTEGER DEFAULT 0
            );

            -- Historia wykonan flow
            CREATE TABLE IF NOT EXISTS flow_executions (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                flow_id INTEGER NOT NULL REFERENCES flows(id),
                request_id TEXT,
                model TEXT,
                started_at TEXT,
                finished_at TEXT,
                status TEXT CHECK(status IN ('running','success','error','cancelled')),
                execution_log TEXT,
                total_latency_ms INTEGER,
                total_tokens INTEGER
            );

            -- Indeksy
            CREATE INDEX IF NOT EXISTS idx_prompts_prompt_id ON prompts(prompt_id);
            CREATE INDEX IF NOT EXISTS idx_prompts_type ON prompts(prompt_type);
            CREATE INDEX IF NOT EXISTS idx_model_registry_name ON model_registry(model_name);
            CREATE INDEX IF NOT EXISTS idx_model_registry_service_type ON model_registry(service_type);
            CREATE INDEX IF NOT EXISTS idx_model_aliases_alias ON model_aliases(alias);
            CREATE INDEX IF NOT EXISTS idx_flows_status ON flows(status);
            CREATE INDEX IF NOT EXISTS idx_flows_service_type ON flows(service_type);
            CREATE INDEX IF NOT EXISTS idx_flow_model_bindings_flow ON flow_model_bindings(flow_id);
            CREATE INDEX IF NOT EXISTS idx_flow_node_templates_category ON flow_node_templates(category);
            CREATE INDEX IF NOT EXISTS idx_pii_rules_active ON pii_rules(is_active, priority);
            CREATE INDEX IF NOT EXISTS idx_fast_path_module ON fast_path_patterns(module, pattern_type);
            CREATE INDEX IF NOT EXISTS idx_tts_rules_active ON tts_cleaning_rules(is_active, priority);
            CREATE INDEX IF NOT EXISTS idx_flow_executions_flow ON flow_executions(flow_id);
            CREATE INDEX IF NOT EXISTS idx_flow_executions_status ON flow_executions(status);
        ",
    ),
    (
        4,
        "add_missing_indexes",
        "
            -- Indeks kompozytowy dla wyszukiwania domyslnego flow po typie serwisu
            CREATE INDEX IF NOT EXISTS idx_flows_default_lookup ON flows(is_default, service_type, status);
            -- Indeks dla flow_model_bindings uzywany w JOIN z priority
            CREATE INDEX IF NOT EXISTS idx_flow_model_bindings_priority ON flow_model_bindings(flow_id, priority);
            -- Indeks kompozytowy dla fast_path_patterns uzywany w list_by_module
            CREATE INDEX IF NOT EXISTS idx_fast_path_active_module ON fast_path_patterns(module, is_active, priority);
        ",
    ),
    (
        5,
        "add_must_change_password",
        "
            ALTER TABLE users ADD COLUMN must_change_password INTEGER NOT NULL DEFAULT 1;
        ",
    ),
    (
        6,
        "add_unique_constraints_seed_tables",
        "
            CREATE UNIQUE INDEX IF NOT EXISTS idx_pii_rules_name_unique ON pii_rules(name);
            CREATE UNIQUE INDEX IF NOT EXISTS idx_tts_rules_type_pattern_unique ON tts_cleaning_rules(rule_type, pattern);
            CREATE UNIQUE INDEX IF NOT EXISTS idx_fast_path_module_pattern_unique ON fast_path_patterns(module, pattern_type, pattern);
            CREATE UNIQUE INDEX IF NOT EXISTS idx_flow_node_templates_type_unique ON flow_node_templates(node_type);
        ",
    ),
    (
        7,
        "add_portainer_instances",
        "
            CREATE TABLE IF NOT EXISTS portainer_instances (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL,
                url TEXT NOT NULL,
                api_key TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
        ",
    ),
    (
        8,
        "add_portainer_username",
        "ALTER TABLE portainer_instances ADD COLUMN username TEXT NOT NULL DEFAULT '';",
    ),
    (
        9,
        "add_portainer_password",
        "ALTER TABLE portainer_instances ADD COLUMN password TEXT NOT NULL DEFAULT '';",
    ),
    (
        10,
        "add_deployments_table",
        "CREATE TABLE IF NOT EXISTS deployments (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            agent_id TEXT NOT NULL,
            stack_name TEXT NOT NULL,
            service_name TEXT NOT NULL,
            config_json TEXT NOT NULL,
            compose_yaml TEXT NOT NULL,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now')),
            UNIQUE(agent_id, stack_name)
        );",
    ),
    (
        11,
        "add_docker_registries",
        "
            CREATE TABLE IF NOT EXISTS registries (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL,
                registry_type TEXT NOT NULL DEFAULT 'custom',
                url TEXT NOT NULL,
                username TEXT NOT NULL DEFAULT '',
                password_encrypted TEXT NOT NULL DEFAULT '',
                is_active INTEGER NOT NULL DEFAULT 1,
                skip_tls_verify INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            CREATE INDEX IF NOT EXISTS idx_registries_name ON registries(name);
        ",
    ),
    (
        12,
        "add_reranker_service_type",
        "
            -- Dodaj typ serwisu 'reranker' do tabel services i model_registry.
            -- SQLite nie wspiera ALTER CHECK, wiec tworzymy nowe tabele z rozszerzonym CHECK.

            -- 1. services
            CREATE TABLE IF NOT EXISTS services_new (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL UNIQUE,
                service_type TEXT NOT NULL CHECK(service_type IN ('llm','embedding','rag','vision','stt','tts','memory','reranker')),
                strategy TEXT NOT NULL DEFAULT 'single' CHECK(strategy IN ('single','least_loaded','round_robin','weighted')),
                model_category TEXT DEFAULT 'main',
                status TEXT NOT NULL DEFAULT 'active' CHECK(status IN ('active','disabled','maintenance')),
                config_json TEXT DEFAULT '{}',
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            INSERT INTO services_new SELECT * FROM services;
            DROP TABLE services;
            ALTER TABLE services_new RENAME TO services;
            CREATE INDEX IF NOT EXISTS idx_services_type ON services(service_type);

            -- 2. model_registry
            CREATE TABLE IF NOT EXISTS model_registry_new (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                model_name TEXT UNIQUE NOT NULL,
                display_name TEXT,
                service_type TEXT NOT NULL CHECK(service_type IN ('llm','embedding','stt','tts','rag','memory','reranker')),
                connection_type TEXT NOT NULL CHECK(connection_type IN ('quic','openai_api','internal')),
                service_id INTEGER REFERENCES services(id) ON DELETE SET NULL,
                flow_id INTEGER REFERENCES flows(id) ON DELETE SET NULL,
                is_public INTEGER NOT NULL DEFAULT 1,
                is_active INTEGER NOT NULL DEFAULT 1,
                config_json TEXT DEFAULT '{}',
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            INSERT INTO model_registry_new SELECT * FROM model_registry;
            DROP TABLE model_registry;
            ALTER TABLE model_registry_new RENAME TO model_registry;
            CREATE INDEX IF NOT EXISTS idx_model_registry_name ON model_registry(model_name);
            CREATE INDEX IF NOT EXISTS idx_model_registry_service_type ON model_registry(service_type);
        ",
    ),
    (
        13,
        "add_crdt_tables",
        "
            -- Tabela operacji CRDT do persystencji logu replikacji
            CREATE TABLE IF NOT EXISTS crdt_operations (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                clock_time INTEGER NOT NULL,
                clock_node_hash INTEGER NOT NULL,
                op_type TEXT NOT NULL,
                op_key TEXT NOT NULL,
                op_data TEXT NOT NULL,
                applied_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            CREATE INDEX IF NOT EXISTS idx_crdt_ops_time ON crdt_operations(clock_time);
            CREATE INDEX IF NOT EXISTS idx_crdt_ops_key ON crdt_operations(op_key);

            -- Version vector do delta sync miedzy peerami
            CREATE TABLE IF NOT EXISTS crdt_version_vector (
                node_hash INTEGER PRIMARY KEY,
                last_time INTEGER NOT NULL,
                updated_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
        ",
    ),
    (
        14,
        "addon_system",
        "
            -- Rozszerzone konta uzytkownikow (zastepuje prosta tabele users)
            CREATE TABLE IF NOT EXISTS user_accounts (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                username TEXT NOT NULL UNIQUE,
                password_hash TEXT NOT NULL,
                display_name TEXT NOT NULL DEFAULT '',
                email TEXT DEFAULT '',
                is_active INTEGER NOT NULL DEFAULT 1,
                is_admin INTEGER NOT NULL DEFAULT 0,
                sso_provider TEXT DEFAULT NULL,
                sso_subject TEXT DEFAULT NULL,
                last_login_at TEXT,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now'))
            );

            -- Grupy uzytkownikow
            CREATE TABLE IF NOT EXISTS user_groups (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL UNIQUE,
                description TEXT DEFAULT '',
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );

            -- Czlonkostwo w grupach (M:N)
            CREATE TABLE IF NOT EXISTS group_members (
                group_id INTEGER NOT NULL REFERENCES user_groups(id) ON DELETE CASCADE,
                user_id INTEGER NOT NULL REFERENCES user_accounts(id) ON DELETE CASCADE,
                PRIMARY KEY (group_id, user_id)
            );

            -- SSO providers (OIDC)
            CREATE TABLE IF NOT EXISTS sso_providers (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL UNIQUE,
                provider_type TEXT NOT NULL CHECK(provider_type IN ('oidc','azure_ad','google','adfs','authentik')),
                client_id TEXT NOT NULL,
                client_secret_encrypted TEXT NOT NULL,
                discovery_url TEXT NOT NULL,
                enabled INTEGER NOT NULL DEFAULT 1,
                auto_create_users INTEGER NOT NULL DEFAULT 0,
                default_group_id INTEGER REFERENCES user_groups(id),
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );

            -- Rejestr addonow
            CREATE TABLE IF NOT EXISTS addons (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                addon_id TEXT NOT NULL UNIQUE,
                name TEXT NOT NULL,
                version TEXT NOT NULL,
                description TEXT DEFAULT '',
                author TEXT DEFAULT '',
                platforms TEXT NOT NULL DEFAULT 'all',
                manifest_json TEXT NOT NULL DEFAULT '{}',
                is_enabled INTEGER NOT NULL DEFAULT 1,
                is_system INTEGER NOT NULL DEFAULT 0,
                installed_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now'))
            );

            -- Uprawnienia addonow (per addon per user/group per zasob)
            CREATE TABLE IF NOT EXISTS addon_permissions (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                addon_id TEXT NOT NULL,
                subject_type TEXT NOT NULL CHECK(subject_type IN ('user','group')),
                subject_id INTEGER NOT NULL,
                resource TEXT NOT NULL,
                access_level TEXT NOT NULL DEFAULT 'none' CHECK(access_level IN ('none','ro','rw','rwd')),
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                UNIQUE(addon_id, subject_type, subject_id, resource)
            );

            -- Sekrety addonow (szyfrowane per addon per user)
            CREATE TABLE IF NOT EXISTS addon_secrets (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                addon_id TEXT NOT NULL,
                user_id INTEGER,
                key TEXT NOT NULL,
                value_encrypted TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now')),
                UNIQUE(addon_id, user_id, key)
            );

            -- Audit log
            CREATE TABLE IF NOT EXISTS audit_log (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                timestamp TEXT NOT NULL DEFAULT (datetime('now')),
                user_id INTEGER,
                addon_id TEXT,
                action TEXT NOT NULL,
                resource TEXT,
                details TEXT,
                ip_address TEXT,
                node_id TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_audit_log_timestamp ON audit_log(timestamp);
            CREATE INDEX IF NOT EXISTS idx_audit_log_user ON audit_log(user_id);
            CREATE INDEX IF NOT EXISTS idx_audit_log_addon ON audit_log(addon_id);

            -- Sync exclusions (co nie synchronizowac per grupa)
            CREATE TABLE IF NOT EXISTS sync_exclusions (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                group_id INTEGER REFERENCES user_groups(id) ON DELETE CASCADE,
                resource_type TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                UNIQUE(group_id, resource_type)
            );

            -- Domyslne dane: grupa admins
            INSERT OR IGNORE INTO user_groups (id, name, description) VALUES (1, 'admins', 'Administratorzy systemu');
        ",
    ),
    (
        15,
        "mesh_security_tables",
        "
            -- Zaufane nody w mesh (klucze publiczne)
            CREATE TABLE IF NOT EXISTS trusted_nodes (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                node_id TEXT NOT NULL UNIQUE,
                public_key TEXT NOT NULL,
                hostname TEXT DEFAULT '',
                approved_by TEXT DEFAULT '',
                approved_at TEXT NOT NULL DEFAULT (datetime('now')),
                is_active INTEGER NOT NULL DEFAULT 1
            );
            CREATE INDEX IF NOT EXISTS idx_trusted_nodes_node_id ON trusted_nodes(node_id);

            -- Oczekujace parowania
            CREATE TABLE IF NOT EXISTS pending_pairings (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                remote_node_id TEXT NOT NULL,
                pin_code TEXT NOT NULL,
                direction TEXT NOT NULL CHECK(direction IN ('outgoing','incoming')),
                expires_at TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            CREATE INDEX IF NOT EXISTS idx_pending_pairings_node ON pending_pairings(remote_node_id);
        ",
    ),
    (
        16,
        "addon_resource_limits",
        "
            -- Limity zasobow addonow (0 = bez limitu / unlimited)
            CREATE TABLE IF NOT EXISTS addon_resource_limits (
                addon_id TEXT NOT NULL UNIQUE,
                max_instances INTEGER NOT NULL DEFAULT 0,
                cpu_limit_ms_per_min INTEGER NOT NULL DEFAULT 0,
                ram_limit_mb INTEGER NOT NULL DEFAULT 0,
                gpu_enabled INTEGER NOT NULL DEFAULT 1,
                vram_limit_mb INTEGER NOT NULL DEFAULT 0,
                storage_limit_mb INTEGER NOT NULL DEFAULT 0,
                http_requests_per_min INTEGER NOT NULL DEFAULT 0,
                llm_tokens_per_min INTEGER NOT NULL DEFAULT 0,
                updated_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            CREATE INDEX IF NOT EXISTS idx_addon_resource_limits_addon ON addon_resource_limits(addon_id);
        ",
    ),
    (
        17,
        "addon_config",
        "
            -- Konfiguracja addonow (wartosci ustawione przez admina)
            CREATE TABLE IF NOT EXISTS addon_config (
                addon_id TEXT NOT NULL,
                key TEXT NOT NULL,
                value TEXT NOT NULL,
                updated_at TEXT NOT NULL DEFAULT (datetime('now')),
                PRIMARY KEY (addon_id, key)
            );
        ",
    ),
    (
        18,
        "simplify_permissions",
        "
            -- Zmiana access_level na prosty boolean granted (1/0)
            -- Zachowaj istniejace dane — kazdy kto mial jakikolwiek access_level != 'none' dostaje granted=1
            CREATE TABLE addon_permissions_new (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                addon_id TEXT NOT NULL,
                subject_type TEXT NOT NULL CHECK(subject_type IN ('user','group')),
                subject_id INTEGER NOT NULL,
                permission_id TEXT NOT NULL,
                granted INTEGER NOT NULL DEFAULT 1,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                UNIQUE(addon_id, subject_type, subject_id, permission_id)
            );
            INSERT INTO addon_permissions_new (addon_id, subject_type, subject_id, permission_id, granted, created_at)
                SELECT addon_id, subject_type, subject_id, resource, CASE WHEN access_level != 'none' THEN 1 ELSE 0 END, created_at
                FROM addon_permissions;
            DROP TABLE addon_permissions;
            ALTER TABLE addon_permissions_new RENAME TO addon_permissions;
        ",
    ),
    (
        19,
        "user_accounts_must_change_password",
        "
            -- VULN-003: Kolumna wymuszajaca zmiane domyslnego hasla
            ALTER TABLE user_accounts ADD COLUMN must_change_password INTEGER NOT NULL DEFAULT 0;
            -- Wymusz zmiane hasla dla konta admin (id=1)
            UPDATE user_accounts SET must_change_password = 1 WHERE id = 1;
        ",
    ),
    (
        20,
        "addon_missing_tables_and_audit_columns",
        "
            -- Sandboxowany key-value storage addonow (uzywany przez host_functions/storage.rs)
            CREATE TABLE IF NOT EXISTS addon_storage (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                addon_id TEXT NOT NULL,
                instance_id TEXT NOT NULL,
                storage_key TEXT NOT NULL,
                storage_value BLOB,
                value_size_bytes INTEGER NOT NULL DEFAULT 0,
                updated_at TEXT NOT NULL DEFAULT (datetime('now')),
                UNIQUE(addon_id, instance_id, storage_key)
            );
            CREATE INDEX IF NOT EXISTS idx_addon_storage_addon ON addon_storage(addon_id);

            -- Instancje addonow (uzywane przez addon/mod.rs)
            CREATE TABLE IF NOT EXISTS addon_instances (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                addon_id TEXT NOT NULL,
                instance_id TEXT NOT NULL UNIQUE,
                instance_name TEXT,
                status TEXT NOT NULL DEFAULT 'stopped',
                created_by INTEGER,
                started_at TEXT,
                stopped_at TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_addon_instances_addon ON addon_instances(addon_id);

            -- Bajty WASM addonow (uzywane przez addon/mod.rs i api_addon_system.rs)
            CREATE TABLE IF NOT EXISTS addon_wasm (
                addon_id TEXT NOT NULL UNIQUE,
                wasm_bytes BLOB NOT NULL
            );

            -- Narzedzia zarejestrowane przez addony (uzywane przez host_functions/mod.rs tool_register)
            CREATE TABLE IF NOT EXISTS addon_tools (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                addon_id TEXT NOT NULL,
                tool_name TEXT NOT NULL,
                description TEXT DEFAULT '',
                parameters_schema_json TEXT DEFAULT '{}',
                return_schema_json TEXT,
                is_active INTEGER NOT NULL DEFAULT 1,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                UNIQUE(addon_id, tool_name)
            );
            CREATE INDEX IF NOT EXISTS idx_addon_tools_addon ON addon_tools(addon_id);

            -- Deklarowane uprawnienia addonow (z manifestu — uzywane przez addon/mod.rs)
            CREATE TABLE IF NOT EXISTS addon_declared_permissions (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                addon_id TEXT NOT NULL,
                permission_type TEXT NOT NULL,
                UNIQUE(addon_id, permission_type)
            );
            CREATE INDEX IF NOT EXISTS idx_addon_declared_perms_addon ON addon_declared_permissions(addon_id);

            -- Brakujace kolumny w audit_log (uzywane przez host_functions/mod.rs audit_log())
            ALTER TABLE audit_log ADD COLUMN instance_id TEXT;
            ALTER TABLE audit_log ADD COLUMN resource_type TEXT;
            ALTER TABLE audit_log ADD COLUMN resource_id TEXT;
            ALTER TABLE audit_log ADD COLUMN result TEXT;
            ALTER TABLE audit_log ADD COLUMN error_message TEXT;
            ALTER TABLE audit_log ADD COLUMN action_hash INTEGER;
        ",
    ),
    (
        21,
        "addon_network_rules",
        "
            -- Reguly sieciowe TCP/UDP addonow (proxy z walidacja i auditem)
            CREATE TABLE IF NOT EXISTS addon_network_rules (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                addon_id TEXT NOT NULL,
                rule_id TEXT NOT NULL,
                protocol TEXT NOT NULL CHECK(protocol IN ('tcp','udp')),
                host TEXT NOT NULL,
                port INTEGER NOT NULL,
                description TEXT DEFAULT '',
                required INTEGER NOT NULL DEFAULT 0,
                approved INTEGER NOT NULL DEFAULT 0,
                approved_by INTEGER,
                approved_at TEXT,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                UNIQUE(addon_id, rule_id)
            );
            CREATE INDEX IF NOT EXISTS idx_addon_network_rules_addon ON addon_network_rules(addon_id);
        ",
    ),
    (
        22,
        "add_fuel_limit_column",
        "
            -- Zamiana cpu_limit_ms_per_min na fuel_limit (instrukcje WASM per wywolanie)
            -- 0 = domyslny (10M), wartosc > 0 = konkretny limit
            -- Nie usuwamy cpu_limit_ms_per_min (SQLite nie wspiera DROP COLUMN latwo)
            ALTER TABLE addon_resource_limits ADD COLUMN fuel_limit INTEGER NOT NULL DEFAULT 0;
        ",
    ),
    (
        23,
        "drop_dead_tables_agents_deployments",
        "
            DROP TABLE IF EXISTS agents;
            DROP TABLE IF EXISTS deployments;
        ",
    ),
    (
        24,
        "add_clusters_and_extend_models_services",
        "
            -- Tabela clusterow
            CREATE TABLE IF NOT EXISTS clusters (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                cluster_id TEXT NOT NULL UNIQUE,
                name TEXT NOT NULL,
                description TEXT DEFAULT '',
                strategy TEXT NOT NULL DEFAULT 'distributed',
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now'))
            );

            -- Czlonkostwo nodow w clusterach
            CREATE TABLE IF NOT EXISTS cluster_members (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                cluster_id TEXT NOT NULL REFERENCES clusters(cluster_id) ON DELETE CASCADE,
                node_id TEXT NOT NULL,
                role TEXT NOT NULL DEFAULT 'worker',
                joined_at TEXT NOT NULL DEFAULT (datetime('now')),
                UNIQUE(cluster_id, node_id)
            );

            -- Rozszerzenie model_aliases o fallback i strategie
            ALTER TABLE model_aliases ADD COLUMN fallback_targets TEXT DEFAULT NULL;
            ALTER TABLE model_aliases ADD COLUMN strategy TEXT DEFAULT 'first_available';

            -- UUID i node_id dla serwisow
            ALTER TABLE services ADD COLUMN service_uuid TEXT DEFAULT NULL;
            ALTER TABLE services ADD COLUMN node_id TEXT DEFAULT NULL;

            -- Indeksy
            CREATE INDEX IF NOT EXISTS idx_clusters_cluster_id ON clusters(cluster_id);
            CREATE INDEX IF NOT EXISTS idx_cluster_members_cluster ON cluster_members(cluster_id);
            CREATE INDEX IF NOT EXISTS idx_cluster_members_node ON cluster_members(node_id);
        ",
    ),
    (
        25,
        "add_keywords_skill_md_category",
        "
            ALTER TABLE addon_tools ADD COLUMN keywords_json TEXT NOT NULL DEFAULT '[]';
            ALTER TABLE addons ADD COLUMN skill_md TEXT;
            ALTER TABLE addons ADD COLUMN keywords_json TEXT NOT NULL DEFAULT '[]';
            ALTER TABLE addons ADD COLUMN category TEXT NOT NULL DEFAULT '';
        ",
    ),
    (
        26,
        "add_disambiguation_json",
        "
            ALTER TABLE addons ADD COLUMN disambiguation_json TEXT NOT NULL DEFAULT '[]';
        ",
    ),
    (
        27,
        "revoked_nodes_table",
        "
            CREATE TABLE IF NOT EXISTS revoked_nodes (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                node_id TEXT NOT NULL UNIQUE,
                revoked_at TEXT NOT NULL DEFAULT (datetime('now')),
                revoked_by TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_revoked_nodes_node_id ON revoked_nodes(node_id);
        ",
    ),
    (
        28,
        "add_trusted_nodes_addresses",
        "ALTER TABLE trusted_nodes ADD COLUMN last_addresses TEXT NOT NULL DEFAULT '';",
    ),
    (
        29,
        "cluster_network_configuration",
        "
            ALTER TABLE cluster_members ADD COLUMN interface_name TEXT DEFAULT '';
            ALTER TABLE cluster_members ADD COLUMN interface_ip TEXT DEFAULT '';
            ALTER TABLE cluster_members ADD COLUMN interface_speed_mbps INTEGER DEFAULT 0;
            ALTER TABLE cluster_members ADD COLUMN interface_type TEXT DEFAULT '';

            ALTER TABLE clusters ADD COLUMN total_vram_mb INTEGER DEFAULT 0;
            ALTER TABLE clusters ADD COLUMN total_ram_mb INTEGER DEFAULT 0;
            ALTER TABLE clusters ADD COLUMN total_cpu_cores INTEGER DEFAULT 0;
            ALTER TABLE clusters ADD COLUMN bottleneck_speed_mbps INTEGER DEFAULT 0;
            ALTER TABLE clusters ADD COLUMN interconnect_type TEXT DEFAULT '';
        ",
    ),
    (
        30,
        "add_meeting_bot_service_type",
        "
            PRAGMA foreign_keys=OFF;

            CREATE TABLE services_tmp (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL UNIQUE,
                service_type TEXT NOT NULL CHECK(service_type IN ('llm','embedding','rag','vision','stt','tts','memory','reranker','meeting-bot')),
                strategy TEXT NOT NULL DEFAULT 'single' CHECK(strategy IN ('single','least_loaded','round_robin','weighted')),
                model_category TEXT DEFAULT 'main',
                status TEXT NOT NULL DEFAULT 'active' CHECK(status IN ('active','disabled','maintenance')),
                config_json TEXT DEFAULT '{}',
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now')),
                service_uuid TEXT DEFAULT NULL,
                node_id TEXT DEFAULT NULL
            );
            INSERT INTO services_tmp SELECT * FROM services;
            DROP TABLE services;
            ALTER TABLE services_tmp RENAME TO services;
            CREATE INDEX idx_services_type ON services(service_type);

            CREATE TABLE model_registry_tmp (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                model_name TEXT UNIQUE NOT NULL,
                display_name TEXT,
                service_type TEXT NOT NULL CHECK(service_type IN ('llm','embedding','stt','tts','rag','memory','reranker','meeting-bot')),
                connection_type TEXT NOT NULL CHECK(connection_type IN ('quic','openai_api','internal')),
                service_id INTEGER REFERENCES services(id) ON DELETE SET NULL,
                flow_id INTEGER REFERENCES flows(id) ON DELETE SET NULL,
                is_public INTEGER NOT NULL DEFAULT 1,
                is_active INTEGER NOT NULL DEFAULT 1,
                config_json TEXT DEFAULT '{}',
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            INSERT INTO model_registry_tmp SELECT * FROM model_registry;
            DROP TABLE model_registry;
            ALTER TABLE model_registry_tmp RENAME TO model_registry;
            CREATE INDEX idx_model_registry_name ON model_registry(model_name);
            CREATE INDEX idx_model_registry_service_type ON model_registry(service_type);

            PRAGMA foreign_keys=ON;
        ",
    ),
    (
        31,
        "voice_profiles",
        "
            -- Voice profile: profil glosowy jednej osoby (Jan Kowalski).
            -- Kazdy profil ma centroid + wiele samples dla odpornosci na wariancje akustyczna.
            CREATE TABLE IF NOT EXISTS voice_profiles (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL UNIQUE,
                -- L2-znormalizowany centroid embeddingow [192 × f32] = 768 bajtow
                centroid BLOB NOT NULL,
                sample_count INTEGER NOT NULL DEFAULT 0,
                -- Srednia wewnetrznej cos similarity miedzy samples — wysoka = dobry profil
                reliability_score REAL NOT NULL DEFAULT 0.0,
                -- Zrodlo: 'explicit' (LLM po przedstawieniu sie), 'merged' (ze scalania temp speakers),
                -- 'manual' (przez API/endpoint)
                source TEXT NOT NULL DEFAULT 'manual',
                -- Dodatkowe metadane JSON (np. language_hint, meeting_count)
                metadata_json TEXT NOT NULL DEFAULT '{}',
                enrolled_at TEXT NOT NULL DEFAULT (datetime('now')),
                last_seen_at TEXT,
                total_utterances INTEGER NOT NULL DEFAULT 0
            );
            CREATE INDEX idx_voice_profiles_name ON voice_profiles(name);
            CREATE INDEX idx_voice_profiles_last_seen ON voice_profiles(last_seen_at);

            -- Pojedyncze samples glosu per profil. Trzymamy je osobno zeby multi-sample
            -- matching dzialal — porownujemy nowy embedding z wszystkimi samples i bierzemy
            -- top-K srednia (odporne na outliery).
            CREATE TABLE IF NOT EXISTS voice_profile_samples (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                profile_id INTEGER NOT NULL REFERENCES voice_profiles(id) ON DELETE CASCADE,
                -- Raw embedding [192 × f32] = 768 bajtow (NIE znormalizowany)
                embedding BLOB NOT NULL,
                duration_ms INTEGER NOT NULL,
                -- Signal-to-noise ratio estimate w dB (wiecej = czystsze audio)
                snr_db REAL NOT NULL DEFAULT 0.0,
                -- Srednia cos similarity z pozostalymi samples tego profilu (spojnosc)
                intra_similarity REAL NOT NULL DEFAULT 0.0,
                -- Z ktorego meetingu pochodzi sample (opcjonalne)
                meeting_id TEXT,
                -- Zrodlo: 'enrollment' (explicit enrollment przez LLM),
                -- 'incremental' (dodane podczas meetingu gdy confidence byla wysoka),
                -- 'merged' (ze scalonego temp speaker)
                source TEXT NOT NULL DEFAULT 'enrollment',
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            CREATE INDEX idx_voice_samples_profile ON voice_profile_samples(profile_id);
            CREATE INDEX idx_voice_samples_created ON voice_profile_samples(created_at);

            -- Tymczasowi mowcy per meeting — zanim LLM ich przypisze do profilu.
            -- Pozwala po meetingu zrobic 'assign SPEAKER_01 → Jan Kowalski' i przeniesc
            -- embeddingi do voice_profile_samples.
            CREATE TABLE IF NOT EXISTS voice_temp_speakers (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                meeting_id TEXT NOT NULL,
                temp_label TEXT NOT NULL,  -- np. 'SPEAKER_00'
                -- Wszystkie embeddingi z tego meetingu dla tego temp speakera (JSON array
                -- of base64-encoded f32 arrays — maly overhead, elastyczne)
                embeddings_blob BLOB NOT NULL,
                sample_count INTEGER NOT NULL DEFAULT 0,
                total_duration_ms INTEGER NOT NULL DEFAULT 0,
                -- Jesli LLM/user przypisal do profilu, tu jest ref
                assigned_profile_id INTEGER REFERENCES voice_profiles(id) ON DELETE SET NULL,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                UNIQUE(meeting_id, temp_label)
            );
            CREATE INDEX idx_voice_temp_meeting ON voice_temp_speakers(meeting_id);
            CREATE INDEX idx_voice_temp_assigned ON voice_temp_speakers(assigned_profile_id);
        ",
    ),
]
}
