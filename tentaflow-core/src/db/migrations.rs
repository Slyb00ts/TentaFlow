// =============================================================================
// Plik: db/migrations.rs
// Opis: Schemat bazy danych SQLite i mechanizm migracji wersjonowanych.
//       Po squashu wszystkich historycznych migracji 1..71 do jednego
//       `initial_schema (v1)` — kazda nowa instalacja dostaje czysty
//       finalny schemat w jednym kroku.
// =============================================================================

use anyhow::Result;
use rusqlite::Connection;
use tracing::info;

/// Migracje moga byc:
/// - `Sql` — zwykly batch SQL wykonany przez `execute_batch`
/// - `Rust` — funkcja, ktora dostaje `&Connection` w transakcji. Uzywana
///   gdy logika nie da sie zapisac jako pure SQL (np. row-by-row JSON
///   serializacja po stronie Rust).
pub enum MigrationStep {
    Sql(&'static str),
    Rust(fn(&Connection) -> Result<()>),
}

/// Uruchamia migracje bazy danych.
pub fn run(conn: &Connection) -> Result<()> {
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

    for (version, name, step) in get_migrations() {
        if version > current_version {
            info!("Migracja {}: {}", version, name);
            let tx = conn.unchecked_transaction()?;
            match step {
                MigrationStep::Sql(sql) => tx.execute_batch(sql)?,
                MigrationStep::Rust(f) => f(&tx)?,
            }
            tx.execute(
                "INSERT INTO _migrations (version, name) VALUES (?1, ?2)",
                rusqlite::params![version, name],
            )?;
            tx.commit()?;
        }
    }

    Ok(())
}

fn get_migrations() -> Vec<(i64, &'static str, MigrationStep)> {
    vec![
        (1, "initial_schema", MigrationStep::Sql(INITIAL_SCHEMA)),
        (
            2,
            "flow_executions_allow_completed",
            MigrationStep::Sql(FLOW_EXECUTIONS_ALLOW_COMPLETED),
        ),
        (
            3,
            "deployments_full_columns",
            MigrationStep::Sql(DEPLOYMENTS_FULL_COLUMNS),
        ),
        (
            4,
            "flow_json_rename_edge_fields",
            MigrationStep::Sql(FLOW_JSON_RENAME_EDGE_FIELDS),
        ),
        (
            5,
            "services_progress_message",
            MigrationStep::Sql(SERVICES_PROGRESS_MESSAGE),
        ),
        (
            6,
            "flow_node_templates_params_schema",
            MigrationStep::Sql(FLOW_NODE_TEMPLATES_PARAMS_SCHEMA),
        ),
    ]
}

// params_schema: JSON-Schema-like opis pol konfiguracyjnych per node type.
// GUI flow builder rendere dynamic form z tej deklaracji (typ string z enum
// → select, number z range → slider, boolean → toggle, format=textarea →
// textarea, type=model_picker z `category` → dynamic dropdown z
// model_registry filtrowane po category). Bez tej kolumny config tab w
// builderze byl pusty bo wczytywal `template.params_schema` ktore byl
// undefined.
const FLOW_NODE_TEMPLATES_PARAMS_SCHEMA: &str = r#"
ALTER TABLE flow_node_templates ADD COLUMN params_schema TEXT;
"#;

// progress_message: krotki status text aktualizowany przez supervisor /
// detached deploy task podczas Starting (np. "warming up — alive 30s,
// waiting for /v1/models"). GUI snapshot pokazuje obok statusu, zeby
// user widzial PROGRES startu serwisu (vLLM cold start ~3 min, klient
// inaczej widzi tylko "Starting" przez kilka minut bez feedbacku).
//
// Health_last_err zostaje DEDYKOWANE dla bledow zdrowia (failed probe).
// Progress_message jest informacyjne, NULL gdy nic do powiedzenia.
const SERVICES_PROGRESS_MESSAGE: &str = r#"
ALTER TABLE services ADD COLUMN progress_message TEXT;
"#;

// Rename edge fieldow w flow_json: `from`/`to` -> `from_node`/`to_node`.
// GUI canvas (flows-builder/canvas.js) oczekuje `from_node`/`to_node`, seed
// historycznie pisal `from`/`to`. Bez tego edytor flow pokazuje nodes bez
// polaczen i flow zachowuje sie jakby byl pojedynczym blokiem.
// `replace()` w SQLite jest binarnie bezpieczny i podmienia substring;
// ograniczamy do flows.flow_json + flow_versions.flow_json zeby nie tknac
// settings/config z innymi `"from":` (np. mail, oauth).
const FLOW_JSON_RENAME_EDGE_FIELDS: &str = r#"
UPDATE flows
   SET flow_json = replace(replace(flow_json, '{"from":', '{"from_node":'), ',"to":', ',"to_node":')
 WHERE flow_json LIKE '%"edges"%';
UPDATE flow_versions
   SET flow_json = replace(replace(flow_json, '{"from":', '{"from_node":'), ',"to":', ',"to_node":')
 WHERE flow_json LIKE '%"edges"%';
"#;

// Squashed v1 mial uproszczona schema deployments (brak: deploy_id unique,
// node_id, phase, progress_pct, image_tag, container_name, user_id; pole
// `error_text` zamiast `error_message`). Repository i log_bus pisza do
// pelnego zestawu kolumn — bez tego startup czysci stale rows wybucha
// "no such column: error_message" i kazdy deploy progress update padl
// niewidocznie. deployments to log historii — drop+recreate akceptowalne.
const DEPLOYMENTS_FULL_COLUMNS: &str = r#"
DROP INDEX IF EXISTS idx_deployments_slug;
DROP TABLE IF EXISTS deployments;
CREATE TABLE deployments (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    deploy_id TEXT NOT NULL UNIQUE,
    engine_id TEXT NOT NULL,
    deploy_method TEXT NOT NULL,
    node_id TEXT NOT NULL DEFAULT '',
    status TEXT NOT NULL DEFAULT 'queued',
    phase TEXT NOT NULL DEFAULT '',
    progress_pct INTEGER NOT NULL DEFAULT 0,
    image_tag TEXT NOT NULL DEFAULT '',
    container_name TEXT NOT NULL DEFAULT '',
    config_json TEXT NOT NULL DEFAULT '{}',
    user_id INTEGER,
    started_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    finished_at TIMESTAMP,
    error_message TEXT,
    log_tail TEXT NOT NULL DEFAULT ''
);
CREATE INDEX idx_deployments_deploy_id ON deployments(deploy_id);
CREATE INDEX idx_deployments_engine ON deployments(engine_id);
"#;

// SQLite nie pozwala na ALTER TABLE dla CHECK constraintu — robimy klasyczne
// rebuild-via-temp-table. flow_executions to log historii, mozna stracic
// rzedy ktore i tak juz padly na CHECK (status='completed' nigdy do bazy
// nie trafil).
const FLOW_EXECUTIONS_ALLOW_COMPLETED: &str = r#"
CREATE TABLE flow_executions_new (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    flow_id INTEGER NOT NULL REFERENCES flows(id),
    request_id TEXT,
    model TEXT,
    started_at TEXT,
    finished_at TEXT,
    status TEXT CHECK(status IN ('running','success','completed','error','cancelled')),
    execution_log TEXT,
    total_latency_ms INTEGER,
    total_tokens INTEGER
);
INSERT INTO flow_executions_new (id, flow_id, request_id, model, started_at, finished_at, status, execution_log, total_latency_ms, total_tokens)
    SELECT id, flow_id, request_id, model, started_at, finished_at, status, execution_log, total_latency_ms, total_tokens FROM flow_executions;
DROP TABLE flow_executions;
ALTER TABLE flow_executions_new RENAME TO flow_executions;
CREATE INDEX idx_flow_executions_flow ON flow_executions(flow_id);
CREATE INDEX idx_flow_executions_status ON flow_executions(status);
"#;

const INITIAL_SCHEMA: &str = r#"
CREATE TABLE api_keys (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    key_hash TEXT NOT NULL UNIQUE,
    key_prefix TEXT NOT NULL,
    name TEXT NOT NULL,
    rate_limit_rps INTEGER NOT NULL DEFAULT 100,
    is_active INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    last_used_at TEXT,
    owner_user_id INTEGER
);
CREATE INDEX idx_api_keys_prefix ON api_keys(key_prefix);
CREATE INDEX idx_apikeys_owner ON api_keys(owner_user_id);

CREATE TABLE service_aliases (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    alias TEXT NOT NULL UNIQUE,
    target_service_id INTEGER NOT NULL REFERENCES services(id) ON DELETE CASCADE
);
CREATE INDEX idx_service_aliases_target ON service_aliases(target_service_id);

CREATE TABLE settings (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL,
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE users (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    username TEXT NOT NULL UNIQUE,
    password_hash TEXT NOT NULL,
    role TEXT NOT NULL DEFAULT 'admin' CHECK(role IN ('admin','viewer')),
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    last_login_at TEXT,
    must_change_password INTEGER NOT NULL DEFAULT 1,
    preferred_language TEXT
);

CREATE TABLE model_aliases (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    alias TEXT UNIQUE NOT NULL,
    target_model TEXT NOT NULL,
    is_active INTEGER NOT NULL DEFAULT 1,
    fallback_targets TEXT DEFAULT NULL,
    strategy TEXT DEFAULT 'first_available'
);
CREATE INDEX idx_model_aliases_alias ON model_aliases(alias);

CREATE TABLE flows (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL,
    description TEXT,
    version INTEGER DEFAULT 1,
    is_default INTEGER NOT NULL DEFAULT 0,
    service_type TEXT,
    flow_json TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'draft' CHECK(status IN ('draft','active','archived')),
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    published_model_name TEXT NULL
);
CREATE INDEX idx_flows_status ON flows(status);
CREATE INDEX idx_flows_service_type ON flows(service_type);
CREATE INDEX idx_flows_default_lookup ON flows(is_default, service_type, status);
CREATE UNIQUE INDEX idx_flows_published_model_name
    ON flows(published_model_name)
    WHERE published_model_name IS NOT NULL;

CREATE TABLE flow_model_bindings (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    flow_id INTEGER NOT NULL REFERENCES flows(id) ON DELETE CASCADE,
    model_pattern TEXT NOT NULL UNIQUE,
    priority INTEGER DEFAULT 0
);
CREATE INDEX idx_flow_model_bindings_flow ON flow_model_bindings(flow_id);
CREATE INDEX idx_flow_model_bindings_priority ON flow_model_bindings(flow_id, priority);

CREATE TABLE flow_node_templates (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    node_type TEXT NOT NULL,
    category TEXT NOT NULL CHECK(category IN ('trigger','service','transform','logic','output')),
    label TEXT NOT NULL,
    description TEXT,
    default_config TEXT NOT NULL DEFAULT '{}',
    icon TEXT
);
CREATE INDEX idx_flow_node_templates_category ON flow_node_templates(category);
CREATE UNIQUE INDEX idx_flow_node_templates_type_unique ON flow_node_templates(node_type);

CREATE TABLE pii_rules (
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
CREATE INDEX idx_pii_rules_active ON pii_rules(is_active, priority);
CREATE UNIQUE INDEX idx_pii_rules_name_unique ON pii_rules(name);

CREATE TABLE fast_path_patterns (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    module TEXT NOT NULL,
    pattern_type TEXT NOT NULL,
    pattern TEXT NOT NULL,
    match_type TEXT NOT NULL DEFAULT 'exact' CHECK(match_type IN ('exact','starts_with','contains','regex','length')),
    result_json TEXT NOT NULL,
    is_active INTEGER NOT NULL DEFAULT 1,
    priority INTEGER DEFAULT 0
);
CREATE INDEX idx_fast_path_module ON fast_path_patterns(module, pattern_type);
CREATE INDEX idx_fast_path_active_module ON fast_path_patterns(module, is_active, priority);
CREATE UNIQUE INDEX idx_fast_path_module_pattern_unique ON fast_path_patterns(module, pattern_type, pattern);

CREATE TABLE tts_cleaning_rules (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    rule_type TEXT NOT NULL CHECK(rule_type IN ('abbreviation','phonetic','emoji_range','regex_remove')),
    pattern TEXT NOT NULL,
    replacement TEXT,
    language TEXT NOT NULL DEFAULT 'pl',
    is_active INTEGER NOT NULL DEFAULT 1,
    priority INTEGER DEFAULT 0
);
CREATE INDEX idx_tts_rules_active ON tts_cleaning_rules(is_active, priority);
CREATE UNIQUE INDEX idx_tts_rules_type_pattern_unique ON tts_cleaning_rules(rule_type, pattern);

CREATE TABLE flow_executions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    flow_id INTEGER NOT NULL REFERENCES flows(id),
    request_id TEXT,
    model TEXT,
    started_at TEXT,
    finished_at TEXT,
    status TEXT CHECK(status IN ('running','success','completed','error','cancelled')),
    execution_log TEXT,
    total_latency_ms INTEGER,
    total_tokens INTEGER
);
CREATE INDEX idx_flow_executions_flow ON flow_executions(flow_id);
CREATE INDEX idx_flow_executions_status ON flow_executions(status);

CREATE TABLE registries (
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
CREATE INDEX idx_registries_name ON registries(name);

CREATE TABLE crdt_operations (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    clock_time INTEGER NOT NULL,
    clock_node_hash INTEGER NOT NULL,
    op_type TEXT NOT NULL,
    op_key TEXT NOT NULL,
    op_data TEXT NOT NULL,
    applied_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX idx_crdt_ops_time ON crdt_operations(clock_time);
CREATE INDEX idx_crdt_ops_key ON crdt_operations(op_key);

CREATE TABLE crdt_version_vector (
    node_hash INTEGER PRIMARY KEY,
    last_time INTEGER NOT NULL,
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE user_accounts (
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
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    must_change_password INTEGER NOT NULL DEFAULT 0,
    role TEXT NOT NULL DEFAULT 'user'
);

CREATE TABLE user_groups (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL UNIQUE,
    description TEXT DEFAULT '',
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE group_members (
    group_id INTEGER NOT NULL REFERENCES user_groups(id) ON DELETE CASCADE,
    user_id INTEGER NOT NULL REFERENCES user_accounts(id) ON DELETE CASCADE,
    PRIMARY KEY (group_id, user_id)
);

CREATE TABLE sso_providers (
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

CREATE TABLE addons (
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
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    skill_md TEXT,
    keywords_json TEXT NOT NULL DEFAULT '[]',
    category TEXT NOT NULL DEFAULT '',
    disambiguation_json TEXT NOT NULL DEFAULT '[]',
    admin_only INTEGER NOT NULL DEFAULT 0,
    icon TEXT,
    runtime TEXT NOT NULL DEFAULT 'wasmtime',
    wasm_size_bytes INTEGER NOT NULL DEFAULT 0,
    license TEXT NOT NULL DEFAULT '',
    show_in_catalog INTEGER NOT NULL DEFAULT 1
);

CREATE TABLE addon_secrets (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    addon_id TEXT NOT NULL,
    user_id INTEGER,
    key TEXT NOT NULL,
    value_encrypted TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE(addon_id, user_id, key)
);

CREATE TABLE audit_log (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    timestamp TEXT NOT NULL DEFAULT (datetime('now')),
    user_id INTEGER,
    addon_id TEXT,
    action TEXT NOT NULL,
    resource TEXT,
    details TEXT,
    ip_address TEXT,
    node_id TEXT,
    instance_id TEXT,
    resource_type TEXT,
    resource_id TEXT,
    result TEXT,
    error_message TEXT,
    action_hash INTEGER,
    severity TEXT NOT NULL DEFAULT 'info'
);
CREATE INDEX idx_audit_log_timestamp ON audit_log(timestamp);
CREATE INDEX idx_audit_log_user ON audit_log(user_id);
CREATE INDEX idx_audit_log_addon ON audit_log(addon_id);
CREATE INDEX idx_audit_log_severity ON audit_log(severity);

CREATE TABLE sync_exclusions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    group_id INTEGER REFERENCES user_groups(id) ON DELETE CASCADE,
    resource_type TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE(group_id, resource_type)
);

CREATE TABLE trusted_nodes (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    node_id TEXT NOT NULL UNIQUE,
    public_key TEXT NOT NULL,
    hostname TEXT DEFAULT '',
    approved_by TEXT DEFAULT '',
    approved_at TEXT NOT NULL DEFAULT (datetime('now')),
    is_active INTEGER NOT NULL DEFAULT 1,
    last_addresses TEXT NOT NULL DEFAULT ''
);
CREATE INDEX idx_trusted_nodes_node_id ON trusted_nodes(node_id);

CREATE TABLE pending_pairings (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    remote_node_id TEXT NOT NULL,
    pin_code TEXT NOT NULL,
    direction TEXT NOT NULL CHECK(direction IN ('outgoing','incoming')),
    expires_at TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX idx_pending_pairings_node ON pending_pairings(remote_node_id);

CREATE TABLE addon_resource_limits (
    addon_id TEXT NOT NULL UNIQUE,
    max_instances INTEGER NOT NULL DEFAULT 0,
    cpu_limit_ms_per_min INTEGER NOT NULL DEFAULT 0,
    ram_limit_mb INTEGER NOT NULL DEFAULT 0,
    gpu_enabled INTEGER NOT NULL DEFAULT 1,
    vram_limit_mb INTEGER NOT NULL DEFAULT 0,
    storage_limit_mb INTEGER NOT NULL DEFAULT 0,
    http_requests_per_min INTEGER NOT NULL DEFAULT 0,
    llm_tokens_per_min INTEGER NOT NULL DEFAULT 0,
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    fuel_limit INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX idx_addon_resource_limits_addon ON addon_resource_limits(addon_id);

CREATE TABLE addon_config (
    addon_id TEXT NOT NULL,
    key TEXT NOT NULL,
    value TEXT NOT NULL,
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    is_secret INTEGER NOT NULL DEFAULT 0,
    updated_by INTEGER,
    PRIMARY KEY (addon_id, key)
);

CREATE TABLE addon_permissions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    addon_id TEXT NOT NULL,
    subject_type TEXT NOT NULL CHECK(subject_type IN ('user','group')),
    subject_id INTEGER NOT NULL,
    permission_id TEXT NOT NULL,
    granted INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    grant_mode TEXT NOT NULL DEFAULT 'inherit'
        CHECK(grant_mode IN ('allow','deny','inherit')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_by INTEGER REFERENCES user_accounts(id) ON DELETE SET NULL,
    UNIQUE(addon_id, subject_type, subject_id, permission_id)
);

CREATE TABLE addon_storage (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    addon_id TEXT NOT NULL,
    instance_id TEXT NOT NULL,
    storage_key TEXT NOT NULL,
    storage_value BLOB,
    value_size_bytes INTEGER NOT NULL DEFAULT 0,
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE(addon_id, instance_id, storage_key)
);
CREATE INDEX idx_addon_storage_addon ON addon_storage(addon_id);

CREATE TABLE addon_instances (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    addon_id TEXT NOT NULL,
    instance_id TEXT NOT NULL UNIQUE,
    instance_name TEXT,
    status TEXT NOT NULL DEFAULT 'stopped',
    created_by INTEGER,
    started_at TEXT,
    stopped_at TEXT
);
CREATE INDEX idx_addon_instances_addon ON addon_instances(addon_id);

CREATE TABLE addon_wasm (
    addon_id TEXT NOT NULL UNIQUE,
    wasm_bytes BLOB NOT NULL
);

CREATE TABLE addon_tools (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    addon_id TEXT NOT NULL,
    tool_name TEXT NOT NULL,
    description TEXT DEFAULT '',
    parameters_schema_json TEXT DEFAULT '{}',
    return_schema_json TEXT,
    is_active INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    keywords_json TEXT NOT NULL DEFAULT '[]',
    UNIQUE(addon_id, tool_name)
);
CREATE INDEX idx_addon_tools_addon ON addon_tools(addon_id);

CREATE TABLE addon_declared_permissions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    addon_id TEXT NOT NULL,
    permission_type TEXT NOT NULL,
    UNIQUE(addon_id, permission_type)
);
CREATE INDEX idx_addon_declared_perms_addon ON addon_declared_permissions(addon_id);

CREATE TABLE addon_network_rules (
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
CREATE INDEX idx_addon_network_rules_addon ON addon_network_rules(addon_id);

CREATE TABLE clusters (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    cluster_id TEXT NOT NULL UNIQUE,
    name TEXT NOT NULL,
    description TEXT DEFAULT '',
    strategy TEXT NOT NULL DEFAULT 'distributed',
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    total_vram_mb INTEGER DEFAULT 0,
    total_ram_mb INTEGER DEFAULT 0,
    total_cpu_cores INTEGER DEFAULT 0,
    bottleneck_speed_mbps INTEGER DEFAULT 0,
    interconnect_type TEXT DEFAULT '',
    failover_enabled INTEGER NOT NULL DEFAULT 0,
    failover_target TEXT,
    health_check_interval_ms INTEGER NOT NULL DEFAULT 5000,
    timeout_ms INTEGER NOT NULL DEFAULT 10000
);
CREATE INDEX idx_clusters_cluster_id ON clusters(cluster_id);

CREATE TABLE cluster_members (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    cluster_id TEXT NOT NULL REFERENCES clusters(cluster_id) ON DELETE CASCADE,
    node_id TEXT NOT NULL,
    role TEXT NOT NULL DEFAULT 'worker',
    joined_at TEXT NOT NULL DEFAULT (datetime('now')),
    interface_name TEXT DEFAULT '',
    interface_ip TEXT DEFAULT '',
    interface_speed_mbps INTEGER DEFAULT 0,
    interface_type TEXT DEFAULT '',
    UNIQUE(cluster_id, node_id)
);
CREATE INDEX idx_cluster_members_cluster ON cluster_members(cluster_id);
CREATE INDEX idx_cluster_members_node ON cluster_members(node_id);

CREATE TABLE revoked_nodes (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    node_id TEXT NOT NULL UNIQUE,
    revoked_at TEXT NOT NULL DEFAULT (datetime('now')),
    revoked_by TEXT
);
CREATE INDEX idx_revoked_nodes_node_id ON revoked_nodes(node_id);

CREATE TABLE voice_profiles (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL UNIQUE,
    centroid BLOB NOT NULL,
    sample_count INTEGER NOT NULL DEFAULT 0,
    reliability_score REAL NOT NULL DEFAULT 0.0,
    source TEXT NOT NULL DEFAULT 'manual',
    metadata_json TEXT NOT NULL DEFAULT '{}',
    enrolled_at TEXT NOT NULL DEFAULT (datetime('now')),
    last_seen_at TEXT,
    total_utterances INTEGER NOT NULL DEFAULT 0,
    first_name TEXT NOT NULL DEFAULT '',
    last_name TEXT,
    nickname TEXT
);
CREATE INDEX idx_voice_profiles_name ON voice_profiles(name);
CREATE INDEX idx_voice_profiles_last_seen ON voice_profiles(last_seen_at);
CREATE INDEX idx_voice_profiles_first_last ON voice_profiles(first_name, last_name);
CREATE INDEX idx_voice_profiles_nickname ON voice_profiles(nickname);

CREATE TABLE voice_profile_samples (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    profile_id INTEGER NOT NULL REFERENCES voice_profiles(id) ON DELETE CASCADE,
    embedding BLOB NOT NULL,
    duration_ms INTEGER NOT NULL,
    snr_db REAL NOT NULL DEFAULT 0.0,
    intra_similarity REAL NOT NULL DEFAULT 0.0,
    meeting_id TEXT,
    source TEXT NOT NULL DEFAULT 'enrollment',
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX idx_voice_samples_profile ON voice_profile_samples(profile_id);
CREATE INDEX idx_voice_samples_created ON voice_profile_samples(created_at);

CREATE TABLE voice_temp_speakers (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    meeting_id TEXT NOT NULL,
    temp_label TEXT NOT NULL,
    embeddings_blob BLOB NOT NULL,
    sample_count INTEGER NOT NULL DEFAULT 0,
    total_duration_ms INTEGER NOT NULL DEFAULT 0,
    assigned_profile_id INTEGER REFERENCES voice_profiles(id) ON DELETE SET NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE(meeting_id, temp_label)
);
CREATE INDEX idx_voice_temp_meeting ON voice_temp_speakers(meeting_id);
CREATE INDEX idx_voice_temp_assigned ON voice_temp_speakers(assigned_profile_id);

CREATE TABLE meeting_sessions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    meeting_key TEXT NOT NULL UNIQUE,
    meeting_url TEXT,
    title TEXT,
    started_at TEXT NOT NULL DEFAULT (datetime('now')),
    last_activity_at TEXT NOT NULL DEFAULT (datetime('now')),
    status TEXT NOT NULL DEFAULT 'ended',
    ended_at TEXT,
    container_id TEXT,
    container_name TEXT,
    quic_port INTEGER,
    vnc_port INTEGER,
    novnc_port INTEGER,
    bot_endpoint_id TEXT,
    bot_secret_key_hex TEXT,
    platform TEXT,
    owner_user_id INTEGER,
    lifecycle_stage TEXT DEFAULT 'idle',
    lifecycle_details TEXT,
    lifecycle_updated_at TEXT,
    backend_stt_model TEXT,
    backend_tts_model TEXT,
    backend_summarization_model TEXT,
    backend_diarization_model TEXT,
    backend_streaming_latency_ms INTEGER,
    backend_enrolled_speakers INTEGER,
    backend_total_participants INTEGER
);
CREATE INDEX idx_meeting_sessions_started ON meeting_sessions(started_at DESC);
CREATE INDEX idx_meeting_sessions_last_activity ON meeting_sessions(last_activity_at DESC);
CREATE INDEX idx_meeting_sessions_status ON meeting_sessions(status);
CREATE INDEX idx_meeting_sessions_owner ON meeting_sessions(owner_user_id);

CREATE TABLE meeting_transcripts (
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

CREATE TABLE flow_versions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    flow_id INTEGER NOT NULL REFERENCES flows(id) ON DELETE CASCADE,
    version_num INTEGER NOT NULL,
    flow_json TEXT NOT NULL,
    name TEXT NOT NULL,
    description TEXT,
    status TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    created_by TEXT,
    UNIQUE(flow_id, version_num)
);
CREATE INDEX idx_flow_versions_flow_id ON flow_versions(flow_id, version_num DESC);

CREATE TABLE addon_permission_defaults (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    addon_id TEXT NOT NULL,
    permission_id TEXT NOT NULL,
    grant_mode TEXT NOT NULL CHECK(grant_mode IN ('allow','deny')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_by INTEGER REFERENCES user_accounts(id) ON DELETE SET NULL,
    UNIQUE(addon_id, permission_id)
);
CREATE INDEX idx_addon_perm_defaults_addon ON addon_permission_defaults(addon_id);

CREATE TABLE addon_visibility (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    addon_id TEXT NOT NULL,
    group_id INTEGER NOT NULL REFERENCES user_groups(id) ON DELETE CASCADE,
    visible INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_by INTEGER REFERENCES user_accounts(id) ON DELETE SET NULL,
    UNIQUE(addon_id, group_id)
);
CREATE INDEX idx_addon_visibility_addon ON addon_visibility(addon_id);
CREATE INDEX idx_addon_visibility_group ON addon_visibility(group_id);

CREATE TABLE addon_permission_catalog (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    addon_id TEXT NOT NULL,
    permission_id TEXT NOT NULL,
    display_name TEXT NOT NULL DEFAULT '',
    description TEXT NOT NULL DEFAULT '',
    risk TEXT NOT NULL DEFAULT 'low' CHECK(risk IN ('low','medium','high','critical')),
    sort_order INTEGER NOT NULL DEFAULT 0,
    UNIQUE(addon_id, permission_id)
);
CREATE INDEX idx_addon_perm_catalog_addon ON addon_permission_catalog(addon_id);

CREATE TABLE addon_oauth_providers (
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
CREATE INDEX idx_addon_oauth_providers_addon ON addon_oauth_providers(addon_id);

CREATE TABLE addon_oauth_config (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    addon_id TEXT NOT NULL,
    provider_id TEXT NOT NULL,
    client_id TEXT NOT NULL DEFAULT '',
    client_secret_encrypted BLOB,
    redirect_uri TEXT NOT NULL DEFAULT '',
    enabled INTEGER NOT NULL DEFAULT 0,
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_by INTEGER REFERENCES user_accounts(id) ON DELETE SET NULL,
    oauth_mode TEXT NOT NULL DEFAULT 'individual'
        CHECK(oauth_mode IN ('global','individual','none')),
    UNIQUE(addon_id, provider_id)
);
CREATE INDEX idx_addon_oauth_config_addon ON addon_oauth_config(addon_id);

CREATE TABLE oauth_pending_states (
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
CREATE INDEX idx_oauth_pending_states_expires ON oauth_pending_states(expires_at);

CREATE TABLE addon_network_config (
    addon_id TEXT NOT NULL PRIMARY KEY,
    allowed_hosts TEXT NOT NULL DEFAULT '[]',
    blocked_hosts TEXT NOT NULL DEFAULT '[]',
    mode TEXT NOT NULL DEFAULT 'strict' CHECK(mode IN ('strict','permissive')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_by INTEGER
);

CREATE TABLE user_oauth_accounts (
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
CREATE UNIQUE INDEX uq_user_oauth_individual
    ON user_oauth_accounts(user_id, addon_id, provider_id)
    WHERE user_id IS NOT NULL;
CREATE UNIQUE INDEX uq_user_oauth_global
    ON user_oauth_accounts(addon_id, provider_id)
    WHERE user_id IS NULL;
CREATE INDEX idx_user_oauth_accounts_user ON user_oauth_accounts(user_id);
CREATE INDEX idx_user_oauth_accounts_addon ON user_oauth_accounts(addon_id);
CREATE INDEX idx_user_oauth_accounts_addon_provider ON user_oauth_accounts(addon_id, provider_id);

CREATE TABLE notes (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id INTEGER NOT NULL REFERENCES user_accounts(id) ON DELETE CASCADE,
    title TEXT NOT NULL DEFAULT '',
    body TEXT NOT NULL DEFAULT '',
    pinned INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX idx_notes_user ON notes(user_id);
CREATE INDEX idx_notes_user_updated ON notes(user_id, updated_at DESC);

CREATE TABLE meeting_port_allocations (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    port INTEGER NOT NULL,
    kind TEXT NOT NULL,
    session_id INTEGER NOT NULL REFERENCES meeting_sessions(id) ON DELETE CASCADE,
    allocated_at TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE(port, kind)
);
CREATE INDEX idx_meeting_port_allocations_session ON meeting_port_allocations(session_id);

CREATE TABLE meeting_settings (
    user_id INTEGER NOT NULL REFERENCES user_accounts(id) ON DELETE CASCADE,
    key TEXT NOT NULL,
    value TEXT NOT NULL DEFAULT '',
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (user_id, key)
);

CREATE TABLE mesh_topology (
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
CREATE INDEX idx_mesh_topology_last_seen ON mesh_topology(last_seen_ms DESC);

CREATE TABLE resource_permissions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    resource_type TEXT NOT NULL,
    resource_id TEXT NOT NULL,
    subject_type TEXT NOT NULL,
    subject_id INTEGER NOT NULL,
    access_level TEXT NOT NULL CHECK(access_level IN ('allow','deny')),
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE(resource_type, resource_id, subject_type, subject_id)
);
CREATE INDEX idx_resperm_subject ON resource_permissions(subject_type, subject_id);
CREATE INDEX idx_resperm_resource ON resource_permissions(resource_type, resource_id);

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
CREATE INDEX idx_prompts_prompt_id ON prompts(prompt_id);
CREATE INDEX idx_prompts_language ON prompts(language);

CREATE TABLE meeting_summaries (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id INTEGER NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    decisions_text TEXT NOT NULL DEFAULT '',
    summary_text TEXT NOT NULL DEFAULT '',
    model TEXT NOT NULL DEFAULT '',
    FOREIGN KEY (session_id) REFERENCES meeting_sessions(id) ON DELETE CASCADE
);
CREATE INDEX idx_meeting_summaries_session ON meeting_summaries(session_id, created_at DESC);

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
CREATE INDEX idx_meeting_action_items_session ON meeting_action_items(session_id, status, created_at DESC);

CREATE TABLE teams_bot_wake_words (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    word TEXT NOT NULL UNIQUE COLLATE NOCASE,
    enabled INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE services (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    engine_id TEXT NOT NULL,
    category TEXT NOT NULL,
    display_name TEXT NOT NULL,
    deploy_method TEXT NOT NULL CHECK(deploy_method IN ('docker','native_embedded','native_binary','native_python_bundle','external')),
    transport TEXT NOT NULL CHECK(transport IN ('embedded','http_direct','sidecar_quic','external_http')),
    status TEXT NOT NULL CHECK(status IN ('starting','running','degraded','failed','stopped')) DEFAULT 'starting',
    pinned INTEGER NOT NULL DEFAULT 0,
    paused INTEGER NOT NULL DEFAULT 0,
    runtime_pid INTEGER,
    runtime_port INTEGER,
    sidecar_quic_port INTEGER,
    endpoint_url TEXT,
    config_json TEXT NOT NULL DEFAULT '{}',
    health_last_ok TIMESTAMP,
    health_last_err TEXT,
    -- progress_message dodawany przez migration 5 (services_progress_message).
    -- Nie dodajemy tu zeby ALTER TABLE w migracji nie zwalil "duplicate column"
    -- na fresh DB.
    restart_count INTEGER NOT NULL DEFAULT 0,
    created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
);
CREATE INDEX idx_services_status ON services(status);
CREATE INDEX idx_services_engine ON services(engine_id);
CREATE INDEX idx_services_category ON services(category);

CREATE TABLE model_registry (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    service_id INTEGER NOT NULL REFERENCES services(id) ON DELETE CASCADE,
    model_name TEXT NOT NULL,
    display_name TEXT,
    capabilities TEXT NOT NULL DEFAULT '[]',
    context_length INTEGER,
    quantization TEXT,
    is_default INTEGER NOT NULL DEFAULT 0,
    created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE(service_id, model_name)
);
CREATE INDEX idx_models_service ON model_registry(service_id);
CREATE INDEX idx_models_name ON model_registry(model_name);

CREATE TABLE deployments (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    deploy_id TEXT NOT NULL UNIQUE,
    engine_id TEXT NOT NULL,
    deploy_method TEXT NOT NULL,
    node_id TEXT NOT NULL DEFAULT '',
    status TEXT NOT NULL DEFAULT 'queued',
    phase TEXT NOT NULL DEFAULT '',
    progress_pct INTEGER NOT NULL DEFAULT 0,
    image_tag TEXT NOT NULL DEFAULT '',
    container_name TEXT NOT NULL DEFAULT '',
    config_json TEXT NOT NULL DEFAULT '{}',
    user_id INTEGER,
    started_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    finished_at TIMESTAMP,
    error_message TEXT,
    log_tail TEXT NOT NULL DEFAULT ''
);
CREATE INDEX idx_deployments_deploy_id ON deployments(deploy_id);
CREATE INDEX idx_deployments_engine ON deployments(engine_id);

CREATE TABLE peer_persisted (
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

CREATE TABLE peer_hints (
    node_id     BLOB NOT NULL,
    hint_kind   INTEGER NOT NULL,
    payload     TEXT NOT NULL,
    last_ok_ms  INTEGER,
    fail_count  INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (node_id, hint_kind, payload),
    FOREIGN KEY (node_id) REFERENCES peer_persisted(node_id) ON DELETE CASCADE
);
CREATE INDEX idx_peer_hints_node ON peer_hints(node_id);

INSERT INTO user_groups (id, name, description) VALUES (1, 'admins', 'Administratorzy systemu');

INSERT INTO settings(key, value) VALUES
    ('mesh.bind_mode', 'auto'),
    ('mesh.bind_ipv4', ''),
    ('mesh.advertise_hide_docker', '1'),
    ('mesh.advertise_hide_link_local', '1'),
    ('mesh.advertise_hide_loopback', '1'),
    ('mesh.advertise_hide_cgnat', '0'),
    ('mesh.advertise_prefer_same_subnet', '1'),
    ('mesh.iroh_relay_url', 'https://relay.nextapp.pl');
"#;
