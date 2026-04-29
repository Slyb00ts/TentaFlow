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
    (
        32,
        "voice_profiles_person_fields",
        "
            -- Rozbicie 'name' na czesci osobowe. 'name' zostaje jako unikalny
            -- display-name (computed z first_name/last_name/nickname) dla
            -- kompatybilnosci oraz szybkiego lookup'u; nowe kolumny pozwalaja
            -- wyszukiwanie po imieniu/nazwisku/nicku osobno.
            --
            -- first_name jest wymagane (NOT NULL) — kazdy profil musi mieć
            -- chociaz imie. last_name i nickname sa opcjonalne — profil moze
            -- byc zapisany tylko jako 'Jan' albo 'Jan Kowalski' albo
            -- 'Jan Kowalski (janek)'.
            ALTER TABLE voice_profiles ADD COLUMN first_name TEXT NOT NULL DEFAULT '';
            ALTER TABLE voice_profiles ADD COLUMN last_name TEXT;
            ALTER TABLE voice_profiles ADD COLUMN nickname TEXT;

            CREATE INDEX idx_voice_profiles_first_last ON voice_profiles(first_name, last_name);
            CREATE INDEX idx_voice_profiles_nickname ON voice_profiles(nickname);
        ",
    ),
    (
        33,
        "meeting_transcripts",
        "
            -- Sesje spotkan — jedna sesja na rozmowe (klucz np. meeting_id z bota
            -- Teams lub hash URL). Trzymane na stale, do wygenerowania pelnego
            -- transcriptu po fakcie nawet po restarcie tentaflow.
            CREATE TABLE IF NOT EXISTS meeting_sessions (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                meeting_key TEXT NOT NULL UNIQUE,
                meeting_url TEXT,
                title TEXT,
                started_at TEXT NOT NULL DEFAULT (datetime('now')),
                last_activity_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            CREATE INDEX idx_meeting_sessions_started ON meeting_sessions(started_at DESC);

            -- Wpisy transkrypcji per sesja. Nie ma limitu — wszystkie wpisy STT
            -- z bota lecą tutaj. Index po (session_id, timestamp_ms) dla szybkiego
            -- pobrania pelnej historii w kolejnosci chronologicznej.
            CREATE TABLE IF NOT EXISTS meeting_transcripts (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id INTEGER NOT NULL REFERENCES meeting_sessions(id) ON DELETE CASCADE,
                timestamp_ms INTEGER NOT NULL,
                speaker TEXT NOT NULL,
                profile_id INTEGER,
                confidence REAL,
                is_enrolled INTEGER NOT NULL DEFAULT 0,
                text TEXT NOT NULL,
                model TEXT NOT NULL
            );
            CREATE INDEX idx_meeting_transcripts_session ON meeting_transcripts(session_id, timestamp_ms);
        ",
    ),
    (
        34,
        "meeting_sessions_index_last_activity",
        "
            -- list_sessions sortuje po last_activity_at, wiec dodajemy pod to
            -- dedykowany index. Stary (started_at) zostaje — nieszkodliwy.
            CREATE INDEX IF NOT EXISTS idx_meeting_sessions_last_activity
                ON meeting_sessions(last_activity_at DESC);
        ",
    ),
    (
        35,
        "flow_versions",
        "
            -- Historia wersji flow — snapshot poprzedniego stanu przy kazdej
            -- aktualizacji. Umozliwia rollback do poprzedniej wersji. Per flow
            -- przechowujemy 5 ostatnich wersji (starsze prunowane w handlerze).
            CREATE TABLE IF NOT EXISTS flow_versions (
                id              INTEGER PRIMARY KEY AUTOINCREMENT,
                flow_id         INTEGER NOT NULL REFERENCES flows(id) ON DELETE CASCADE,
                version_num     INTEGER NOT NULL,
                flow_json       TEXT NOT NULL,
                name            TEXT NOT NULL,
                description     TEXT,
                status          TEXT,
                created_at      TEXT NOT NULL DEFAULT (datetime('now')),
                created_by      TEXT,
                UNIQUE(flow_id, version_num)
            );
            CREATE INDEX IF NOT EXISTS idx_flow_versions_flow_id
                ON flow_versions(flow_id, version_num DESC);
        ",
    ),
    (
        36,
        "cluster_failover_health_columns",
        "
            -- Pola failover + health-check + timeout dla klastrow.
            -- Dotychczas trzymane wylacznie po stronie GUI; teraz persistowane w DB
            -- by binary protocol mogl je serwowac w ClusterListResponse / ClusterDetailResponse.
            ALTER TABLE clusters ADD COLUMN failover_enabled INTEGER NOT NULL DEFAULT 0;
            ALTER TABLE clusters ADD COLUMN failover_target TEXT;
            ALTER TABLE clusters ADD COLUMN health_check_interval_ms INTEGER NOT NULL DEFAULT 5000;
            ALTER TABLE clusters ADD COLUMN timeout_ms INTEGER NOT NULL DEFAULT 10000;
        ",
    ),
    (
        37,
        "drop_portainer_instances",
        "
            -- FAZA 4: Portainer usuwamy calkowicie. Tabela portainer_instances
            -- byla uzywana przez ekran Ustawienia → Portainer i REST /api/portainer*.
            DROP TABLE IF EXISTS portainer_instances;
        ",
    ),
    (
        38,
        "addon_permissions_oauth",
        "
            -- Rozszerzenie addon_permissions o grant_mode (allow/deny/inherit).
            -- Istniejace 'granted' zostaje dla kompatybilnosci; grant_mode zastepuje
            -- docelowo (inherit = uzyj default/group).
            ALTER TABLE addon_permissions ADD COLUMN grant_mode TEXT NOT NULL DEFAULT 'inherit'
                CHECK(grant_mode IN ('allow','deny','inherit'));
            ALTER TABLE addon_permissions ADD COLUMN updated_at TEXT NOT NULL DEFAULT (datetime('now'));

            -- Domyslne uprawnienia na poziomie addonu (fallback gdy user/group nic nie ustawili).
            CREATE TABLE IF NOT EXISTS addon_permission_defaults (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                addon_id TEXT NOT NULL,
                permission_id TEXT NOT NULL,
                grant_mode TEXT NOT NULL CHECK(grant_mode IN ('allow','deny')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now')),
                UNIQUE(addon_id, permission_id)
            );
            CREATE INDEX IF NOT EXISTS idx_addon_perm_defaults_addon
                ON addon_permission_defaults(addon_id);

            -- Widocznosc addonu per grupa uzytkownikow.
            CREATE TABLE IF NOT EXISTS addon_visibility (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                addon_id TEXT NOT NULL,
                group_id INTEGER NOT NULL REFERENCES user_groups(id) ON DELETE CASCADE,
                visible INTEGER NOT NULL DEFAULT 1,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                UNIQUE(addon_id, group_id)
            );
            CREATE INDEX IF NOT EXISTS idx_addon_visibility_addon ON addon_visibility(addon_id);
            CREATE INDEX IF NOT EXISTS idx_addon_visibility_group ON addon_visibility(group_id);

            -- Flaga admin_only na addons.
            ALTER TABLE addons ADD COLUMN admin_only INTEGER NOT NULL DEFAULT 0;

            -- Katalog uprawnien deklarowanych przez addon (dla UI — display_name, risk, sort).
            CREATE TABLE IF NOT EXISTS addon_permission_catalog (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                addon_id TEXT NOT NULL,
                permission_id TEXT NOT NULL,
                display_name TEXT NOT NULL DEFAULT '',
                description TEXT NOT NULL DEFAULT '',
                risk TEXT NOT NULL DEFAULT 'low' CHECK(risk IN ('low','medium','high','critical')),
                sort_order INTEGER NOT NULL DEFAULT 0,
                UNIQUE(addon_id, permission_id)
            );
            CREATE INDEX IF NOT EXISTS idx_addon_perm_catalog_addon
                ON addon_permission_catalog(addon_id);

            -- Deklarowani providerzy OAuth przez addon (z manifestu).
            CREATE TABLE IF NOT EXISTS addon_oauth_providers (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                addon_id TEXT NOT NULL,
                provider_id TEXT NOT NULL,
                display_name TEXT NOT NULL DEFAULT '',
                authorize_url TEXT NOT NULL,
                token_url TEXT NOT NULL,
                revoke_url TEXT,
                scopes TEXT NOT NULL DEFAULT '',
                mode TEXT NOT NULL DEFAULT 'individual'
                    CHECK(mode IN ('global','individual','none')),
                pkce INTEGER NOT NULL DEFAULT 1,
                UNIQUE(addon_id, provider_id)
            );
            CREATE INDEX IF NOT EXISTS idx_addon_oauth_providers_addon
                ON addon_oauth_providers(addon_id);

            -- Konfiguracja OAuth (admin ustawia client_id/secret/redirect_uri).
            CREATE TABLE IF NOT EXISTS addon_oauth_config (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                addon_id TEXT NOT NULL,
                provider_id TEXT NOT NULL,
                client_id TEXT NOT NULL DEFAULT '',
                client_secret_encrypted BLOB,
                redirect_uri TEXT NOT NULL DEFAULT '',
                enabled INTEGER NOT NULL DEFAULT 0,
                updated_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_by INTEGER REFERENCES user_accounts(id) ON DELETE SET NULL,
                UNIQUE(addon_id, provider_id)
            );
            CREATE INDEX IF NOT EXISTS idx_addon_oauth_config_addon
                ON addon_oauth_config(addon_id);

            -- Konta OAuth powiazane per user (individual) lub globalne (user_id NULL).
            CREATE TABLE IF NOT EXISTS user_oauth_accounts (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                user_id INTEGER REFERENCES user_accounts(id) ON DELETE CASCADE,
                addon_id TEXT NOT NULL,
                provider_id TEXT NOT NULL,
                external_account_id TEXT NOT NULL DEFAULT '',
                display_name TEXT NOT NULL DEFAULT '',
                access_token_encrypted BLOB,
                refresh_token_encrypted BLOB,
                token_type TEXT NOT NULL DEFAULT 'Bearer',
                scopes TEXT NOT NULL DEFAULT '',
                expires_at TEXT,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now')),
                last_used_at TEXT,
                revoked INTEGER NOT NULL DEFAULT 0,
                UNIQUE(user_id, addon_id, provider_id)
            );
            CREATE INDEX IF NOT EXISTS idx_user_oauth_accounts_user
                ON user_oauth_accounts(user_id);
            CREATE INDEX IF NOT EXISTS idx_user_oauth_accounts_addon
                ON user_oauth_accounts(addon_id);
            CREATE INDEX IF NOT EXISTS idx_user_oauth_accounts_addon_provider
                ON user_oauth_accounts(addon_id, provider_id);

            -- Pending OAuth states (anti-CSRF, PKCE verifier).
            CREATE TABLE IF NOT EXISTS oauth_pending_states (
                state TEXT PRIMARY KEY,
                user_id INTEGER REFERENCES user_accounts(id) ON DELETE CASCADE,
                addon_id TEXT NOT NULL,
                provider_id TEXT NOT NULL,
                mode TEXT NOT NULL CHECK(mode IN ('global','individual')),
                code_verifier TEXT NOT NULL DEFAULT '',
                redirect_after TEXT NOT NULL DEFAULT '',
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                expires_at TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_oauth_pending_states_expires
                ON oauth_pending_states(expires_at);
        ",
    ),
    (
        39,
        "audit_updated_by_and_severity",
        "
            -- Kolumny sladu zmian dla uprawnien addonow — kto ostatnio zmodyfikowal.
            ALTER TABLE addon_permissions
                ADD COLUMN updated_by INTEGER REFERENCES user_accounts(id) ON DELETE SET NULL;
            ALTER TABLE addon_permission_defaults
                ADD COLUMN updated_by INTEGER REFERENCES user_accounts(id) ON DELETE SET NULL;
            ALTER TABLE addon_visibility
                ADD COLUMN updated_by INTEGER REFERENCES user_accounts(id) ON DELETE SET NULL;

            -- Poziom wagi wpisu audytowego (info/warning/critical) — do filtrowania i alertow.
            ALTER TABLE audit_log ADD COLUMN severity TEXT NOT NULL DEFAULT 'info';
            CREATE INDEX IF NOT EXISTS idx_audit_log_severity ON audit_log(severity);
        ",
    ),
    (
        40,
        "addon_lifecycle_tables",
        "
            -- Prosty model regul sieciowych addona (allowed/blocked hosts + tryb).
            -- Rozny od addon_network_rules (per-rule protocol/host/port approval) — tutaj
            -- trzymamy listy hostow w JSON dla wygodnego edytowania z GUI.
            CREATE TABLE IF NOT EXISTS addon_network_config (
                addon_id TEXT NOT NULL PRIMARY KEY,
                allowed_hosts TEXT NOT NULL DEFAULT '[]',
                blocked_hosts TEXT NOT NULL DEFAULT '[]',
                mode TEXT NOT NULL DEFAULT 'strict' CHECK(mode IN ('strict','permissive')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_by INTEGER
            );

            -- Kolumny audytu dla addon_config — kto i kiedy ostatnio zmienil, czy sekret.
            ALTER TABLE addon_config ADD COLUMN is_secret INTEGER NOT NULL DEFAULT 0;
            ALTER TABLE addon_config ADD COLUMN updated_by INTEGER;
        ",
    ),
    (
        41,
        "addon_oauth_mode_field",
        "
            -- Tryb OAuth (global/individual/none) przechowywany w addon_oauth_config,
            -- aby admin mogl go realnie zmieniac z GUI. Wczesniej tryb byl tylko
            -- w addon_oauth_providers (deklaracja z manifestu) i nie mogl byc nadpisany.
            ALTER TABLE addon_oauth_config ADD COLUMN oauth_mode TEXT NOT NULL DEFAULT 'individual'
                CHECK(oauth_mode IN ('global','individual','none'));

            -- Skopiuj tryb z deklaracji manifestu do config dla juz istniejacych wpisow,
            -- ktore nie byly jeszcze recznie ustawione (pozostaly przy 'individual' defaulcie).
            UPDATE addon_oauth_config
            SET oauth_mode = (
                SELECT mode FROM addon_oauth_providers
                WHERE addon_oauth_providers.addon_id = addon_oauth_config.addon_id
                  AND addon_oauth_providers.provider_id = addon_oauth_config.provider_id
            )
            WHERE oauth_mode = 'individual'
              AND EXISTS (
                SELECT 1 FROM addon_oauth_providers
                WHERE addon_oauth_providers.addon_id = addon_oauth_config.addon_id
                  AND addon_oauth_providers.provider_id = addon_oauth_config.provider_id
              );
        ",
    ),
    (
        42,
        "addon_oauth_accounts_unique_fix",
        "
            -- SQLite traktuje NULL jako rozne wartosci w UNIQUE, wiec tabelowy
            -- UNIQUE(user_id, addon_id, provider_id) NIE zapobiega duplikatom
            -- tokenow globalnych (user_id=NULL). Zastepujemy go dwoma partial
            -- unique indexes: jeden dla indywidualnych (user_id IS NOT NULL),
            -- drugi dla globalnych (user_id IS NULL).
            --
            -- Tabelowy UNIQUE jest zaimplementowany jako sqlite_autoindex_*,
            -- ktorego nie mozna DROP INDEX — dlatego rekonstruujemy tabele.

            CREATE TABLE IF NOT EXISTS user_oauth_accounts_new (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                user_id INTEGER REFERENCES user_accounts(id) ON DELETE CASCADE,
                addon_id TEXT NOT NULL,
                provider_id TEXT NOT NULL,
                external_account_id TEXT NOT NULL DEFAULT '',
                display_name TEXT NOT NULL DEFAULT '',
                access_token_encrypted BLOB,
                refresh_token_encrypted BLOB,
                token_type TEXT NOT NULL DEFAULT 'Bearer',
                scopes TEXT NOT NULL DEFAULT '',
                expires_at TEXT,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now')),
                last_used_at TEXT,
                revoked INTEGER NOT NULL DEFAULT 0
            );

            INSERT INTO user_oauth_accounts_new
                (id, user_id, addon_id, provider_id, external_account_id, display_name,
                 access_token_encrypted, refresh_token_encrypted, token_type, scopes,
                 expires_at, created_at, updated_at, last_used_at, revoked)
                SELECT id, user_id, addon_id, provider_id, external_account_id, display_name,
                       access_token_encrypted, refresh_token_encrypted, token_type, scopes,
                       expires_at, created_at, updated_at, last_used_at, revoked
                FROM user_oauth_accounts;

            DROP TABLE user_oauth_accounts;
            ALTER TABLE user_oauth_accounts_new RENAME TO user_oauth_accounts;

            CREATE UNIQUE INDEX IF NOT EXISTS uq_user_oauth_individual
                ON user_oauth_accounts(user_id, addon_id, provider_id)
                WHERE user_id IS NOT NULL;

            CREATE UNIQUE INDEX IF NOT EXISTS uq_user_oauth_global
                ON user_oauth_accounts(addon_id, provider_id)
                WHERE user_id IS NULL;

            CREATE INDEX IF NOT EXISTS idx_user_oauth_accounts_user
                ON user_oauth_accounts(user_id);
            CREATE INDEX IF NOT EXISTS idx_user_oauth_accounts_addon
                ON user_oauth_accounts(addon_id);
            CREATE INDEX IF NOT EXISTS idx_user_oauth_accounts_addon_provider
                ON user_oauth_accounts(addon_id, provider_id);
        ",
    ),
    (
        43,
        "addons_ui_metadata",
        "
            -- UI metadata surfaced on the admin addons list: sprite icon, category label,
            -- runtime tag (wasmtime/wasmi) and compiled WASM size. Columns are created
            -- defensively via separate ALTERs wrapped in a compatibility check below.
            -- The addons.category column was historically introduced in migration 26 as
            -- part of the disambiguation rollout; older databases without it get it here.

            -- SQLite lacks 'ADD COLUMN IF NOT EXISTS'. Each ALTER is a no-op when the
            -- column already exists (fails with 'duplicate column name'), which we
            -- tolerate by executing the statements independently in Rust. But inside
            -- this single SQL batch we only add columns that are guaranteed to be new
            -- in this migration: icon, runtime, wasm_size_bytes.
            ALTER TABLE addons ADD COLUMN icon TEXT;
            ALTER TABLE addons ADD COLUMN runtime TEXT NOT NULL DEFAULT 'wasmtime';
            ALTER TABLE addons ADD COLUMN wasm_size_bytes INTEGER NOT NULL DEFAULT 0;
        ",
    ),
    (
        44,
        "addons_detail_metadata",
        "
            -- Metadata used by the addon detail header card (mockup addons-permissions):
            -- license string (e.g. 'Apache-2.0') backfilled from manifest at install time,
            -- and show_in_catalog flag (default ON) gating the \"Available apps\" listing
            -- for non-privileged users. Idempotent ALTERs — dynamic version below handles
            -- repeat installs. This inline batch expects first-time application.
            ALTER TABLE addons ADD COLUMN license TEXT NOT NULL DEFAULT '';
            ALTER TABLE addons ADD COLUMN show_in_catalog INTEGER NOT NULL DEFAULT 1;
        ",
    ),
    (
        46,
        "notes_table",
        "
            -- Per-user notes backing the user-facing Notes app (left sidebar list,
            -- right editor). Strictly user-scoped: every read/write in repository
            -- includes user_id guard to prevent BOLA. Pinned notes sort first,
            -- then by updated_at DESC (list order).
            CREATE TABLE IF NOT EXISTS notes (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                user_id INTEGER NOT NULL REFERENCES user_accounts(id) ON DELETE CASCADE,
                title TEXT NOT NULL DEFAULT '',
                body TEXT NOT NULL DEFAULT '',
                pinned INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            CREATE INDEX IF NOT EXISTS idx_notes_user ON notes(user_id);
            CREATE INDEX IF NOT EXISTS idx_notes_user_updated ON notes(user_id, updated_at DESC);
        ",
    ),
    (
        47,
        "meeting_bot_lifecycle",
        "
            -- Extend meeting_sessions with lifecycle + container metadata. Each
            -- meeting now spawns an ephemeral docker container with dedicated
            -- ports, tracked here for lookup + cleanup on restart.
            ALTER TABLE meeting_sessions ADD COLUMN status TEXT NOT NULL DEFAULT 'ended';
            ALTER TABLE meeting_sessions ADD COLUMN ended_at TEXT;
            ALTER TABLE meeting_sessions ADD COLUMN container_id TEXT;
            ALTER TABLE meeting_sessions ADD COLUMN container_name TEXT;
            ALTER TABLE meeting_sessions ADD COLUMN quic_port INTEGER;
            ALTER TABLE meeting_sessions ADD COLUMN vnc_port INTEGER;
            ALTER TABLE meeting_sessions ADD COLUMN novnc_port INTEGER;
            ALTER TABLE meeting_sessions ADD COLUMN bot_endpoint_id TEXT;
            ALTER TABLE meeting_sessions ADD COLUMN bot_secret_key_hex TEXT;
            ALTER TABLE meeting_sessions ADD COLUMN platform TEXT;
            ALTER TABLE meeting_sessions ADD COLUMN owner_user_id INTEGER;

            CREATE INDEX IF NOT EXISTS idx_meeting_sessions_status ON meeting_sessions(status);
            CREATE INDEX IF NOT EXISTS idx_meeting_sessions_owner ON meeting_sessions(owner_user_id);

            -- Port allocations — atomic reservation per session. Row exists while
            -- port is taken; deleted when session ends. UNIQUE(port, kind) prevents
            -- double-allocation across concurrent session_start calls.
            CREATE TABLE IF NOT EXISTS meeting_port_allocations (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                port INTEGER NOT NULL,
                kind TEXT NOT NULL,
                session_id INTEGER NOT NULL REFERENCES meeting_sessions(id) ON DELETE CASCADE,
                allocated_at TEXT NOT NULL DEFAULT (datetime('now')),
                UNIQUE(port, kind)
            );
            CREATE INDEX IF NOT EXISTS idx_meeting_port_allocations_session
                ON meeting_port_allocations(session_id);

            -- AI summaries generated post-meeting by LLM (qwen). One row per
            -- session, re-generated on demand if user clicks 'refresh summary'.
            CREATE TABLE IF NOT EXISTS meeting_session_summaries (
                session_id INTEGER PRIMARY KEY REFERENCES meeting_sessions(id) ON DELETE CASCADE,
                tldr TEXT NOT NULL DEFAULT '',
                decisions TEXT NOT NULL DEFAULT '',
                action_items_json TEXT NOT NULL DEFAULT '[]',
                open_questions TEXT NOT NULL DEFAULT '',
                model TEXT NOT NULL DEFAULT '',
                generated_at TEXT NOT NULL DEFAULT (datetime('now'))
            );

            -- User-level meeting bot settings. Key-value per user. Missing rows
            -- fall back to defaults handled in application code.
            CREATE TABLE IF NOT EXISTS meeting_settings (
                user_id INTEGER NOT NULL REFERENCES user_accounts(id) ON DELETE CASCADE,
                key TEXT NOT NULL,
                value TEXT NOT NULL DEFAULT '',
                updated_at TEXT NOT NULL DEFAULT (datetime('now')),
                PRIMARY KEY (user_id, key)
            );

            -- Mark any sessions left 'active' by an unclean shutdown as ended —
            -- old rows before this migration have no status column value and
            -- DEFAULT 'ended' covers them; no-op if already ended.
            UPDATE meeting_sessions SET status = 'ended' WHERE status IS NULL OR status = '';
        ",
    ),
    (
        48,
        "deployments_tracking",
        "
            -- Każde wywołanie ServiceManifestDeployRequest tworzy wiersz. Status
            -- updatowany z background task przez cały lifecycle: queued → building
            -- → pulling → running → registering → success/failure. Log tail
            -- (ostatnie 200 linii) trzymany w kolumnie żeby frontend mógł odzyskać
            -- stan po reconnect/refresh bez zależności od subscription.
            CREATE TABLE IF NOT EXISTS deployments (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                deploy_id TEXT NOT NULL UNIQUE,
                engine_id TEXT NOT NULL,
                deploy_method TEXT NOT NULL,
                node_id TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'queued',
                phase TEXT NOT NULL DEFAULT '',
                progress_pct INTEGER NOT NULL DEFAULT 0,
                image_tag TEXT NOT NULL DEFAULT '',
                container_name TEXT NOT NULL DEFAULT '',
                config_json TEXT NOT NULL DEFAULT '{}',
                user_id INTEGER,
                started_at TEXT NOT NULL DEFAULT (datetime('now')),
                finished_at TEXT,
                error_message TEXT,
                log_tail TEXT NOT NULL DEFAULT ''
            );
            CREATE INDEX IF NOT EXISTS idx_deployments_user ON deployments(user_id);
            CREATE INDEX IF NOT EXISTS idx_deployments_status ON deployments(status);
            CREATE INDEX IF NOT EXISTS idx_deployments_engine ON deployments(engine_id);
            CREATE INDEX IF NOT EXISTS idx_deployments_started ON deployments(started_at DESC);
        ",
    ),
    (
        49,
        "mesh_topology_snapshot",
        "
            -- Persystencja topologii mesh odbieranej przez TopologyAnnounce. Sluzy
            -- do bootstrapu peer_store po restarcie noda zanim gossip przyniesie
            -- aktualne dane. Upsert po kazdym odbiorze; TTL 7 dni (cleanup przy starcie).
            CREATE TABLE IF NOT EXISTS mesh_topology (
                node_id TEXT PRIMARY KEY,
                hostname TEXT NOT NULL DEFAULT '',
                platform TEXT NOT NULL DEFAULT '',
                os_info TEXT NOT NULL DEFAULT '',
                connected_to TEXT NOT NULL DEFAULT '[]',
                direct_addrs TEXT NOT NULL DEFAULT '[]',
                port INTEGER NOT NULL DEFAULT 0,
                services_json TEXT NOT NULL DEFAULT '[]',
                models_json TEXT NOT NULL DEFAULT '[]',
                last_epoch INTEGER NOT NULL DEFAULT 0,
                last_seen_ms INTEGER NOT NULL DEFAULT 0
            );
            CREATE INDEX IF NOT EXISTS idx_mesh_topology_last_seen
                ON mesh_topology(last_seen_ms DESC);
        ",
    ),
    (
        50,
        "user_roles_and_resource_permissions",
        "
            -- 3 role: user / power_user / admin. Migrujemy z is_admin (bool) na
            -- role text column. User accounts z is_admin=1 dostaja 'admin',
            -- reszta 'user'. Power user mozna ustawic recznie przez UI.
            ALTER TABLE user_accounts ADD COLUMN role TEXT NOT NULL DEFAULT 'user';
            UPDATE user_accounts SET role = 'admin' WHERE is_admin = 1;

            -- Generyczna tabela ACL — dla modeli, flowow, addonow i przyszlych
            -- zasobow. (resource_type, resource_id) identyfikuje konkretny zasob,
            -- (subject_type, subject_id) konto albo grupe. access_level:
            --   allow — explicit zezwolono
            --   deny  — explicit odmowa (wygrywa nad default i grupa deny)
            -- Brak wpisu = default (zezwolono dla wszystkich).
            -- Priorytet rozstrzygania: user_deny > user_allow > group_deny >
            -- group_allow > default_allow.
            CREATE TABLE IF NOT EXISTS resource_permissions (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                resource_type TEXT NOT NULL,
                resource_id TEXT NOT NULL,
                subject_type TEXT NOT NULL,
                subject_id INTEGER NOT NULL,
                access_level TEXT NOT NULL CHECK(access_level IN ('allow','deny')),
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                UNIQUE(resource_type, resource_id, subject_type, subject_id)
            );
            CREATE INDEX IF NOT EXISTS idx_resperm_subject
                ON resource_permissions(subject_type, subject_id);
            CREATE INDEX IF NOT EXISTS idx_resperm_resource
                ON resource_permissions(resource_type, resource_id);
        ",
    ),
    (
        51,
        "api_keys_owner_user",
        "
            -- API keys dostaja owner_user_id zeby ACL na /v1/* moglo dzialac per-user.
            -- Existing keys get NULL — traktowane jako admin-equivalent (legacy).
            -- Nowe klucze tworzone z dashboard maja owner_user_id ustawione na
            -- creator-a. Admin moze rotowac keys z innym owner_id.
            ALTER TABLE api_keys ADD COLUMN owner_user_id INTEGER;
            CREATE INDEX IF NOT EXISTS idx_apikeys_owner ON api_keys(owner_user_id);
        ",
    ),
    (
        52,
        "prompts_language_and_is_system",
        "
            -- UNIQUE(prompt_id) -> UNIQUE(prompt_id, language) aby ten sam
            -- prompt_id mogl wystapic w wielu jezykach. SQLite nie wspiera
            -- DROP CONSTRAINT wiec rekreujemy tabele; wszystkie dotychczasowe
            -- wiersze sa kasowane (task T1.2 czysci stare prompty systemowe).
            DROP TABLE IF EXISTS prompts;
            CREATE TABLE prompts (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                prompt_id TEXT NOT NULL,
                name TEXT NOT NULL,
                description TEXT,
                content TEXT NOT NULL,
                prompt_type TEXT NOT NULL CHECK(prompt_type IN ('system','suffix','template','user')),
                default_model TEXT,
                variables TEXT,
                cache_priority INTEGER DEFAULT 50,
                is_active INTEGER NOT NULL DEFAULT 1,
                version INTEGER DEFAULT 1,
                language TEXT NOT NULL DEFAULT 'pl',
                is_system INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now')),
                UNIQUE(prompt_id, language)
            );
            CREATE INDEX IF NOT EXISTS idx_prompts_prompt_id ON prompts(prompt_id);
            CREATE INDEX IF NOT EXISTS idx_prompts_language ON prompts(language);
        ",
    ),
    (
        53,
        "meeting_summaries_and_action_items_rewrite",
        "
            -- Stara tabela meeting_session_summaries (migracja 47) byla dead stubem:
            -- handler zwracal NotImplemented, zaden produkcyjny path nic nie zapisywal.
            -- Caly schemat redesignowany pod Etap 2.2 (MeetingEvent z decisions_text,
            -- summary_text, lista action_items z deduplikacja przez content_hash).
            DROP TABLE IF EXISTS meeting_session_summaries;

            CREATE TABLE meeting_summaries (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id INTEGER NOT NULL,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                decisions_text TEXT NOT NULL DEFAULT '',
                summary_text TEXT NOT NULL DEFAULT '',
                model TEXT NOT NULL DEFAULT '',
                FOREIGN KEY (session_id) REFERENCES meeting_sessions(id) ON DELETE CASCADE
            );
            CREATE INDEX idx_meeting_summaries_session
                ON meeting_summaries(session_id, created_at DESC);

            CREATE TABLE meeting_action_items (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id INTEGER NOT NULL,
                owner TEXT NOT NULL,
                task TEXT NOT NULL,
                deadline TEXT,
                status TEXT NOT NULL DEFAULT 'pending'
                    CHECK(status IN ('pending','done','cancelled')),
                content_hash TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now')),
                FOREIGN KEY (session_id) REFERENCES meeting_sessions(id) ON DELETE CASCADE,
                UNIQUE(session_id, content_hash)
            );
            CREATE INDEX idx_meeting_action_items_session
                ON meeting_action_items(session_id, status, created_at DESC);
        ",
    ),
    (
        54,
        "services_allow_agent_and_on_demand",
        "
            -- Rozluznia CHECK constraints na services:
            -- * service_type akceptuje 'agent' i 'tool' (do tej pory tylko LLM/STT/TTS/...).
            --   Bez tego deploy teams-bota (category=agents) nie tworzyl wpisu
            --   i serwis nie pojawial sie w zakladce Services.
            -- * status akceptuje 'on_demand' — oznacza serwis ktory nie ma stale
            --   dzialajacego kontenera, tylko jest uruchamiany na zadanie
            --   (np. teams-bot spawnuje instancje per spotkanie przez MeetingManager).
            -- SQLite nie pozwala na ALTER CHECK w miejscu — robimy recreate.
            CREATE TABLE services_new (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL UNIQUE,
                service_type TEXT NOT NULL
                    CHECK(service_type IN ('llm','embedding','rag','vision','stt','tts','memory','agent','tool')),
                strategy TEXT NOT NULL DEFAULT 'single'
                    CHECK(strategy IN ('single','least_loaded','round_robin','weighted','first_available')),
                model_category TEXT DEFAULT 'main',
                status TEXT NOT NULL DEFAULT 'active'
                    CHECK(status IN ('active','disabled','maintenance','on_demand')),
                config_json TEXT NOT NULL DEFAULT '{}',
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now')),
                service_uuid TEXT,
                node_id TEXT
            );
            INSERT INTO services_new (id, name, service_type, strategy, model_category, status, config_json, created_at, updated_at, service_uuid, node_id)
                SELECT id, name, service_type, strategy, model_category, status, config_json, created_at, updated_at, service_uuid, node_id FROM services;
            DROP TABLE services;
            ALTER TABLE services_new RENAME TO services;
        ",
    ),
    (
        55,
        "services_pinned_paused_for_memory_guard",
        "
            -- MemoryGuard persistence — pinned (zawsze warm, nie evict) + paused
            -- (skip autostart). Domyslnie 0/0 dla istniejacych wpisow; auto-pin
            -- ustawiany przez deploy dla orchestratora (Qwen 0.8B, Whisper, sherpa).
            ALTER TABLE services ADD COLUMN pinned INTEGER NOT NULL DEFAULT 0;
            ALTER TABLE services ADD COLUMN paused INTEGER NOT NULL DEFAULT 0;
            -- Szacowana pamiec VRAM/RAM modelu w MB. NULL = brak oszacowania
            -- (guard uzyje heurystyki estimate_vram_for_model).
            ALTER TABLE services ADD COLUMN vram_estimate_mb INTEGER;
            CREATE INDEX IF NOT EXISTS idx_services_pinned ON services(pinned);
            CREATE INDEX IF NOT EXISTS idx_services_paused ON services(paused);
        ",
    ),
    (
        56,
        "meeting_sessions_lifecycle_stage",
        "
            -- Real lifecycle stage of the meeting bot, updated both by the host
            -- (initial 'container_spawned' after docker spawn) and by the bot
            -- itself (LifecycleUpdate events from browser.rs at browser_launched,
            -- navigating, prejoin_ready, joining, joined, failed). Independent
            -- from the existing coarse `status` column (idle/joining/active/ended)
            -- which tracks container availability.
            ALTER TABLE meeting_sessions ADD COLUMN lifecycle_stage TEXT DEFAULT 'idle';
            ALTER TABLE meeting_sessions ADD COLUMN lifecycle_details TEXT;
            ALTER TABLE meeting_sessions ADD COLUMN lifecycle_updated_at TEXT;
        ",
    ),
    (
        57,
        "services_source_hash",
        "
            -- Sha256 of the container source tree captured when the service
            -- was deployed. Compared against the compile-time hash in the
            -- manifest registry to flag 'update available' in the dashboard.
            -- NULL for rows created before this migration (treated as unknown).
            ALTER TABLE services ADD COLUMN deployed_source_hash TEXT;
        ",
    ),
    (
        58,
        "mesh_network_settings_defaults",
        "
            -- Seed domyslnych ustawien mesh & network (bind mode + advertise filters).
            -- Kolumna `is_secret` w settings nie istnieje — tabela ma (key, value, updated_at).
            INSERT OR IGNORE INTO settings(key, value) VALUES
              ('mesh.bind_mode', 'auto'),
              ('mesh.bind_ipv4', ''),
              ('mesh.advertise_hide_docker', '1'),
              ('mesh.advertise_hide_link_local', '1'),
              ('mesh.advertise_hide_loopback', '1'),
              ('mesh.advertise_hide_cgnat', '0'),
              ('mesh.advertise_prefer_same_subnet', '1'),
              ('mesh.iroh_relay_url', 'https://relay.nextapp.pl');
        ",
    ),
    (
        59,
        "meeting_sessions_backend_models",
        "
            -- Backend model identifiers reported by the bot via
            -- MeetingEventPayload::BackendUpdate. Persisted so that a live view
            -- mounted AFTER the broadcast still sees the BACKEND panel populated
            -- (STT/TTS/summarization/diarization + counters). Numeric columns
            -- stay NULL until the bot reports a concrete value.
            ALTER TABLE meeting_sessions ADD COLUMN backend_stt_model TEXT;
            ALTER TABLE meeting_sessions ADD COLUMN backend_tts_model TEXT;
            ALTER TABLE meeting_sessions ADD COLUMN backend_summarization_model TEXT;
            ALTER TABLE meeting_sessions ADD COLUMN backend_diarization_model TEXT;
            ALTER TABLE meeting_sessions ADD COLUMN backend_streaming_latency_ms INTEGER;
            ALTER TABLE meeting_sessions ADD COLUMN backend_enrolled_speakers INTEGER;
            ALTER TABLE meeting_sessions ADD COLUMN backend_total_participants INTEGER;
        ",
    ),
    (
        60,
        "teams_bot_wake_words",
        "
            -- Slowa aktywujace odpowiedz teams-bota (wake words).
            -- Kazdy wpis = jedno slowo (case-insensitive substring match w mowie).
            -- Domyslne wartosci sa wstawiane przy starcie tentaflow przez
            -- `ensure_teams_bot_defaults`, jezeli tabela jest pusta.
            CREATE TABLE IF NOT EXISTS teams_bot_wake_words (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                word TEXT NOT NULL UNIQUE COLLATE NOCASE,
                enabled INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
        ",
    ),
    (
        61,
        "add_user_preferred_language",
        "
            -- Preferowany jezyk uzytkownika (ISO-639-1: en/pl/fr/es/de).
            -- NULL = brak preferencji, handler TTS uzyje domyslnego \"en\".
            ALTER TABLE users ADD COLUMN preferred_language TEXT;
        ",
    ),
    (
        62,
        "peer_persisted_and_hints",
        "
            -- Single source of truth for peer state owned by PeerRegistry.
            -- One row per peer; state fields are written by PersistenceWriter
            -- in batched, debounced transactions guarded by persisted_ver
            -- (out-of-order writes are dropped via ON CONFLICT WHERE clause).
            CREATE TABLE IF NOT EXISTS peer_persisted (
                node_id        BLOB PRIMARY KEY,
                pubkey         BLOB NOT NULL,
                trust_state    INTEGER NOT NULL DEFAULT 0,
                hostname       TEXT,
                platform       TEXT,
                role           INTEGER NOT NULL DEFAULT 0,
                last_seen_ms   INTEGER NOT NULL DEFAULT 0,
                persisted_ver  INTEGER NOT NULL DEFAULT 0,
                updated_at_ms  INTEGER NOT NULL
            );

            -- Transport hints discovered for a peer (direct addrs, relay URLs,
            -- DNS hostnames). Many-to-one with peer_persisted; cascade-deleted.
            CREATE TABLE IF NOT EXISTS peer_hints (
                node_id     BLOB NOT NULL,
                hint_kind   INTEGER NOT NULL,
                payload     TEXT NOT NULL,
                last_ok_ms  INTEGER,
                fail_count  INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (node_id, hint_kind, payload),
                FOREIGN KEY (node_id) REFERENCES peer_persisted(node_id) ON DELETE CASCADE
            );

            CREATE INDEX IF NOT EXISTS idx_peer_hints_node ON peer_hints(node_id);
        ",
    ),
]
}
