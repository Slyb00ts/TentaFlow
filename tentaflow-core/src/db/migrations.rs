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
/// - `RustSelfManaged` — funkcja, ktora sama zarzadza pragma + transakcja.
///   Uzywana dla rebuildow tabel wymagajacych `PRAGMA foreign_keys=OFF`
///   POZA transakcja (SQLite ignoruje zmiane tego pragma wewnatrz aktywnej
///   transakcji) oraz `PRAGMA foreign_key_check` przed commitem. Funkcja
///   musi sama zapisac wiersz `_migrations` w obrebie swojej transakcji,
///   bo runner nie otwiera dla niej wlasnej.
pub enum MigrationStep {
    Sql(&'static str),
    Rust(fn(&Connection) -> Result<()>),
    RustSelfManaged(fn(&Connection, i64, &str) -> Result<()>),
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

    // The v56 INTEGER→UUID identity flip rewrites every core PK, so every core
    // operation a peer already holds points at a dead integer id. Upgrading ACROSS
    // v56 this run arms a one-shot baseline reset that the sync runtime consumes
    // after it is up (it owns the Fjall ledger + signer this `Connection` lacks):
    // bump the epoch, drop the stale core ledger state, and re-seed the outbox
    // from the post-flip rows. The marker is set only on the crossing boot and
    // cleared by the consumer, so a routine restart never re-bumps the epoch.
    //
    // A FRESH install (`current_version == 0`) runs v56 against empty identity
    // tables and has no peers holding stale integer-keyed ops, so it must NOT
    // bump: it stays on the genesis epoch every other fresh node shares,
    // otherwise two fresh nodes could never exchange core ops (epoch is compared
    // for exact equality, origin node included).
    let crossing_identity_flip =
        current_version > 0 && current_version < CORE_IDENTITY_FLIP_VERSION;

    for (version, name, step) in get_migrations() {
        if version > current_version {
            info!("Migracja {}: {}", version, name);
            match step {
                MigrationStep::Sql(sql) => {
                    let tx = conn.unchecked_transaction()?;
                    tx.execute_batch(sql)?;
                    tx.execute(
                        "INSERT INTO _migrations (version, name) VALUES (?1, ?2)",
                        rusqlite::params![version, name],
                    )?;
                    tx.commit()?;
                }
                MigrationStep::Rust(f) => {
                    let tx = conn.unchecked_transaction()?;
                    f(&tx)?;
                    tx.execute(
                        "INSERT INTO _migrations (version, name) VALUES (?1, ?2)",
                        rusqlite::params![version, name],
                    )?;
                    tx.commit()?;
                }
                MigrationStep::RustSelfManaged(f) => {
                    // Runner nie otwiera transakcji ani nie zapisuje
                    // `_migrations` — robi to sama funkcja, bo musi
                    // sterowac `PRAGMA foreign_keys` poza transakcja.
                    f(conn, version, name)?;
                }
            }
        }
    }

    if crossing_identity_flip {
        conn.execute(
            "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, '1')",
            rusqlite::params![CORE_BASELINE_RESET_PENDING_KEY],
        )?;
    }

    Ok(())
}

/// Migration version of the INTEGER→UUID core identity flip. Crossing it arms
/// the one-shot Sync Ledger baseline reset.
pub const CORE_IDENTITY_FLIP_VERSION: i64 = 56;

/// `settings` key holding the one-shot "baseline reset pending after cutover"
/// flag. Written by `run` when v56 is crossed, consumed (and cleared) by the
/// sync runtime once it owns the ledger.
pub const CORE_BASELINE_RESET_PENDING_KEY: &str = "core_baseline_reset_pending";

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
        (
            7,
            "audit_log_risk_class",
            MigrationStep::Sql(AUDIT_LOG_RISK_CLASS),
        ),
        (
            8,
            "model_alias_owners",
            MigrationStep::Sql(MODEL_ALIAS_OWNERS),
        ),
        (9, "alias_calls", MigrationStep::Sql(ALIAS_CALLS)),
        (
            10,
            "model_alias_changes",
            MigrationStep::Sql(MODEL_ALIAS_CHANGES),
        ),
        (
            11,
            "addon_migrations_applied",
            MigrationStep::Sql(ADDON_MIGRATIONS_APPLIED),
        ),
        (12, "frame_pickup_log", MigrationStep::Sql(FRAME_PICKUP_LOG)),
        (
            13,
            "teams_bot_aliases_ownership_backfill",
            MigrationStep::Sql(TEAMS_BOT_ALIASES_OWNERSHIP_BACKFILL),
        ),
        (
            14,
            "rename_alias_manage_to_read",
            MigrationStep::Sql(RENAME_ALIAS_MANAGE_TO_READ),
        ),
        (
            15,
            "model_alias_visibility",
            MigrationStep::Sql(MODEL_ALIAS_VISIBILITY),
        ),
        (
            16,
            "model_alias_consumers",
            MigrationStep::Sql(MODEL_ALIAS_CONSUMERS),
        ),
        (17, "model_visibility", MigrationStep::Sql(MODEL_VISIBILITY)),
        (18, "model_consumers", MigrationStep::Sql(MODEL_CONSUMERS)),
        (19, "addon_uses_alias", MigrationStep::Sql(ADDON_USES_ALIAS)),
        (20, "addon_uses_model", MigrationStep::Sql(ADDON_USES_MODEL)),
        (21, "cameras_table", MigrationStep::Sql(CAMERAS_TABLE)),
        (22, "recordings_table", MigrationStep::Sql(RECORDINGS_TABLE)),
        (
            23,
            "cameras_vendor_check_rtsp_onvif",
            MigrationStep::Sql(CAMERAS_VENDOR_CHECK_RTSP_ONVIF),
        ),
        (
            24,
            "frame_pickup_log_source_node_id",
            MigrationStep::Rust(frame_pickup_log_add_source_node_id),
        ),
        (
            25,
            "audit_log_merkle_chain",
            MigrationStep::Rust(audit_log_add_merkle_chain_columns),
        ),
        (
            26,
            "trusted_publishers",
            MigrationStep::Sql(TRUSTED_PUBLISHERS),
        ),
        (
            27,
            "addon_vector_namespaces",
            MigrationStep::Sql(ADDON_VECTOR_NAMESPACES),
        ),
        (28, "policy_claims", MigrationStep::Sql(POLICY_CLAIMS)),
        (29, "flow_invocations", MigrationStep::Sql(FLOW_INVOCATIONS)),
        (
            30,
            "cameras_onvif_metadata",
            MigrationStep::Rust(cameras_add_onvif_metadata_columns),
        ),
        (
            31,
            "flow_invocations_actor_user_id",
            MigrationStep::Rust(flow_invocations_add_actor_user_id),
        ),
        (
            32,
            "multi_tenant_rbac_org_isolation",
            MigrationStep::Rust(setup_multi_tenant),
        ),
        (
            33,
            "model_aliases_strategy_round_robin",
            MigrationStep::Rust(model_aliases_strategy_round_robin),
        ),
        (34, "gate_check_cache", MigrationStep::Sql(GATE_CHECK_CACHE)),
        (
            35,
            "cameras_metadata_supported",
            MigrationStep::Rust(cameras_add_metadata_supported_column),
        ),
        (
            36,
            "roles_add_camera_metadata",
            MigrationStep::Rust(roles_add_camera_metadata_permission),
        ),
        (
            37,
            "legal_documents",
            MigrationStep::Rust(create_legal_documents_table),
        ),
        (
            38,
            "backfill_admin_org_memberships",
            MigrationStep::Rust(backfill_admin_org_memberships),
        ),
        (39, "scheduled_jobs", MigrationStep::Sql(SCHEDULED_JOBS)),
        (
            40,
            "platform_locales",
            MigrationStep::Sql(PLATFORM_LOCALES_SCHEMA),
        ),
        (41, "role_catalog", MigrationStep::Sql(ROLE_CATALOG_SCHEMA)),
        (
            42,
            "sync_identity_registry",
            MigrationStep::Sql(SYNC_IDENTITY_REGISTRY),
        ),
        (
            43,
            "sync_permission_engine",
            MigrationStep::Sql(SYNC_PERMISSION_ENGINE),
        ),
        (44, "sync_policy", MigrationStep::Sql(SYNC_POLICY)),
        (
            45,
            "core_sync_captures",
            MigrationStep::Sql(CORE_SYNC_CAPTURES),
        ),
        (46, "kv_sync_captures", MigrationStep::Sql(KV_SYNC_CAPTURES)),
        (
            47,
            "blob_sync_captures",
            MigrationStep::Sql(BLOB_SYNC_CAPTURES),
        ),
        (
            48,
            "cameras_vendor_check_local_sources",
            MigrationStep::Sql(CAMERAS_VENDOR_CHECK_LOCAL_SOURCES),
        ),
        (
            49,
            "deployment_jobs_as_services",
            MigrationStep::Sql(DEPLOYMENT_JOBS_AS_SERVICES),
        ),
        (
            50,
            "compliance_core_foundation",
            MigrationStep::Rust(create_compliance_core_foundation),
        ),
        (
            51,
            "cameras_restore_org_id",
            MigrationStep::Rust(cameras_restore_org_id_column),
        ),
        (
            52,
            "services_deployed_source_hash",
            MigrationStep::Sql(SERVICES_DEPLOYED_SOURCE_HASH),
        ),
        (
            53,
            "addon_vector_namespaces_fields",
            MigrationStep::Sql(ADDON_VECTOR_NAMESPACES_FIELDS),
        ),
        (
            54,
            "addon_vector_namespaces_sparse",
            MigrationStep::Sql(ADDON_VECTOR_NAMESPACES_SPARSE),
        ),
        (
            55,
            "core_resource_versions",
            MigrationStep::Sql(CORE_RESOURCE_VERSIONS),
        ),
        (
            56,
            "core_identity_int_to_uuid",
            MigrationStep::RustSelfManaged(core_identity_int_to_uuid),
        ),
        (
            57,
            "core_sync_captures_hlc",
            MigrationStep::Sql(CORE_SYNC_CAPTURES_HLC),
        ),
        (
            58,
            "repair_admin_non_uuid_id",
            MigrationStep::RustSelfManaged(repair_admin_non_uuid_id),
        ),
        (
            59,
            "drop_legacy_users_table",
            MigrationStep::Sql(DROP_LEGACY_USERS_TABLE),
        ),
        (
            60,
            "addon_packages_and_instance_versioning",
            MigrationStep::Sql(ADDON_PACKAGES_AND_INSTANCE_VERSIONING),
        ),
        (
            61,
            "repair_default_flow_random_id",
            MigrationStep::RustSelfManaged(repair_default_flow_random_id),
        ),
        (
            62,
            "compliance_ai_tool_calls_llm_call_id",
            MigrationStep::Sql(COMPLIANCE_AI_TOOL_CALLS_LLM_CALL_ID),
        ),
        (63, "skills_registry", MigrationStep::Sql(SKILLS_REGISTRY)),
        (64, "agents_registry", MigrationStep::Sql(AGENTS_REGISTRY)),
        (
            65,
            "compliance_retention_agent_runs_scope",
            MigrationStep::RustSelfManaged(widen_retention_scope_for_agent_runs),
        ),
        (
            66,
            "compliance_ai_events_agent_context",
            MigrationStep::RustSelfManaged(add_ai_events_agent_context),
        ),
        (
            67,
            "flow_executions_parent_execution_id",
            MigrationStep::Sql(FLOW_EXECUTIONS_PARENT_EXECUTION_ID),
        ),
        (
            68,
            "agent_mailbox_and_auto_continuation",
            MigrationStep::Sql(AGENT_MAILBOX_AND_AUTO_CONTINUATION),
        ),
        (
            69,
            "skills_curator_snapshots",
            MigrationStep::Sql(SKILLS_CURATOR_SNAPSHOTS),
        ),
        (
            70,
            "conversation_messages",
            MigrationStep::Sql(CONVERSATION_MESSAGES),
        ),
        (
            71,
            "agent_run_inline_loop_region",
            MigrationStep::Rust(rewrite_agent_run_to_inline_region),
        ),
        (
            72,
            "agent_run_region_streaming",
            MigrationStep::Rust(rewrite_agent_run_to_inline_region),
        ),
        (
            73,
            "drop_legacy_harness_flows",
            MigrationStep::Rust(drop_legacy_harness_flows),
        ),
        (
            74,
            "agent_run_filled_defaults",
            MigrationStep::Rust(rewrite_agent_run_to_inline_region),
        ),
        (
            75,
            "model_visibility_changes",
            MigrationStep::Sql(MODEL_VISIBILITY_CHANGES),
        ),
        (
            76,
            "model_aliases_methods_column",
            MigrationStep::Rust(model_aliases_add_methods_column),
        ),
        (
            77,
            "addons_installed_bundle_hash_column",
            MigrationStep::Rust(addons_add_installed_bundle_hash_column),
        ),
        (
            78,
            "cameras_analysis_fps_column",
            MigrationStep::Rust(cameras_add_analysis_fps_column),
        ),
        (
            79,
            "cameras_analysis_flow_id_column",
            MigrationStep::Rust(cameras_add_analysis_flow_id_column),
        ),
        (
            80,
            "cameras_vendor_check_webrtc",
            MigrationStep::Sql(CAMERAS_VENDOR_CHECK_WEBRTC),
        ),
        (
            81,
            "camera_grants_table",
            MigrationStep::Sql(CAMERA_GRANTS_TABLE),
        ),
        (
            82,
            "api_keys_access_v2",
            MigrationStep::RustSelfManaged(api_keys_access_v2),
        ),
        (
            83,
            "roles_add_robot_permissions",
            MigrationStep::Rust(roles_add_robot_permissions),
        ),
        (
            84,
            "addon_state_table",
            MigrationStep::Sql(ADDON_STATE_TABLE),
        ),
        (
            85,
            "pii_rules_uuid_org_v2",
            MigrationStep::RustSelfManaged(pii_rules_uuid_org_v2),
        ),
        (
            86,
            "token_metrics_tables",
            MigrationStep::Sql(TOKEN_METRICS_SCHEMA),
        ),
        (
            87,
            "token_metrics_permissions",
            MigrationStep::Rust(token_metrics_add_permissions),
        ),
        (
            88,
            "token_metrics_modalities",
            MigrationStep::Sql(TOKEN_METRICS_MODALITIES),
        ),
        (
            89,
            "cameras_depth_mapping_columns",
            MigrationStep::Rust(cameras_add_depth_mapping_columns),
        ),
        (
            90,
            "cameras_depth_pose_source_column",
            MigrationStep::Rust(cameras_add_depth_pose_source_column),
        ),
        (
            91,
            "addon_graph_collections",
            MigrationStep::Rust(create_addon_graph_collections),
        ),
        (
            92,
            "roles_add_graph_permissions",
            MigrationStep::Rust(roles_add_graph_permissions),
        ),
        (
            93,
            "addon_document_store_limits",
            MigrationStep::Rust(create_addon_document_store_limits),
        ),
        (
            94,
            "roles_add_document_permissions",
            MigrationStep::Rust(roles_add_document_permissions),
        ),
        (
            95,
            "scheduled_jobs_org_id",
            MigrationStep::Rust(scheduled_jobs_add_org_id_column),
        ),
        (
            96,
            "cameras_depth_extrinsics_columns",
            MigrationStep::Rust(cameras_add_depth_extrinsics_columns),
        ),
        (
            97,
            "cameras_depth_fov_v",
            MigrationStep::Rust(cameras_add_depth_fov_v_column),
        ),
        (
            98,
            "cluster_members_rdma_columns",
            MigrationStep::Rust(cluster_members_add_rdma_columns),
        ),
        (
            99,
            "cluster_deployments_tables",
            MigrationStep::Rust(create_cluster_deployments_tables),
        ),
        (
            100,
            "cluster_members_rdma_gid_index",
            MigrationStep::Rust(cluster_members_add_gid_index),
        ),
        (
            101,
            "cluster_deployments_dist_port",
            MigrationStep::Rust(cluster_deployments_add_dist_port),
        ),
        (
            102,
            "model_metrics_rollup_table",
            MigrationStep::Sql(MODEL_METRICS_ROLLUP_SCHEMA),
        ),
        (
            103,
            "model_pricing_table",
            MigrationStep::Sql(MODEL_PRICING_SCHEMA),
        ),
        (
            104,
            "model_metrics_permissions",
            MigrationStep::Rust(model_metrics_add_permissions),
        ),
        (
            105,
            "ensure_token_permissions",
            MigrationStep::Rust(ensure_token_permissions),
        ),
        (
            106,
            "cameras_vendor_check_mjpeg",
            MigrationStep::Rust(cameras_vendor_check_add_mjpeg),
        ),
        (
            107,
            "benchmark_studio_tables",
            MigrationStep::Sql(BENCHMARK_STUDIO_SCHEMA),
        ),
        (
            108,
            "benchmark_permissions",
            MigrationStep::Rust(benchmark_add_permissions),
        ),
        (
            109,
            "camera_cv_pipelines",
            MigrationStep::Rust(create_camera_cv_pipelines),
        ),
        (
            110,
            "camera_cv_pipelines_org_scope",
            MigrationStep::Rust(camera_cv_pipelines_add_org_scope),
        ),
        (
            111,
            "vision_models_registry",
            MigrationStep::Rust(create_vision_models),
        ),
        (
            112,
            "drop_notes_table",
            MigrationStep::Sql(DROP_NOTES_TABLE),
        ),
        (
            113,
            "strip_pii_from_default_chat_flow",
            MigrationStep::Rust(strip_pii_from_default_chat_flow),
        ),
        (
            114,
            "cameras_vision_worker_slot",
            MigrationStep::Rust(cameras_add_vision_worker_slot_column),
        ),
        (
            115,
            "recordings_event_meta",
            MigrationStep::Rust(recordings_add_event_meta_column),
        ),
        (
            116,
            "recordings_search_columns",
            MigrationStep::Rust(recordings_add_search_columns),
        ),
        (
            117,
            "cameras_detection_zones",
            MigrationStep::Rust(cameras_add_zones_column),
        ),
        (
            118,
            "flows_is_system",
            MigrationStep::Rust(flows_add_is_system_column),
        ),
        (
            119,
            "project_studio_permissions",
            MigrationStep::Rust(roles_add_project_studio_permissions),
        ),
        (
            120,
            "conversation_messages_citations",
            MigrationStep::Rust(conversation_messages_add_citations_column),
        ),
        (
            121,
            "managed_cli_service_tags",
            MigrationStep::RustSelfManaged(managed_cli_service_tags),
        ),
        (
            122,
            "service_usage_snapshot",
            MigrationStep::Sql(SERVICE_USAGE_SNAPSHOT),
        ),
        (
            123,
            "conversation_messages_reasoning_content",
            MigrationStep::Sql(CONVERSATION_MESSAGES_REASONING_CONTENT),
        ),
        (
            124,
            "benchmark_targets_local_api_type",
            MigrationStep::RustSelfManaged(benchmark_targets_allow_local_api_type),
        ),
        (
            125,
            "code_studio_registry",
            MigrationStep::Sql(CODE_STUDIO_REGISTRY),
        ),
        (
            126,
            "code_studio_permissions",
            MigrationStep::Rust(roles_add_code_studio_permissions),
        ),
        (
            127,
            "agent_runs_token_accounting",
            MigrationStep::Rust(agent_runs_add_token_accounting_columns),
        ),
        (
            128,
            "agents_delegation_roster",
            MigrationStep::Rust(agents_add_delegation_roster),
        ),
        (
            129,
            "model_pricing_embedding_rate",
            MigrationStep::Rust(model_pricing_add_embedding_rate),
        ),
        (
            130,
            "model_metrics_rollup_usage_missing",
            MigrationStep::Rust(model_metrics_rollup_add_usage_missing),
        ),
        (
            131,
            "meeting_sessions_pipeline",
            MigrationStep::Rust(meeting_sessions_add_pipeline_columns),
        ),
        (
            132,
            "services_config_runtime_truth",
            MigrationStep::Sql(SERVICES_CONFIG_RUNTIME_TRUTH),
        ),
        (
            133,
            "events_tracking_foundation",
            MigrationStep::RustSelfManaged(create_events_tracking_foundation),
        ),
        (
            134,
            "graph_collection_paths_repair",
            MigrationStep::Rust(repair_graph_collection_paths),
        ),
        (
            135,
            "run_provenance_columns",
            MigrationStep::Rust(add_run_provenance_columns),
        ),
    ]
}

/// Rows deployed before the deploy pipeline reconciled the persisted config with
/// the runtime carry two fictions: the wizard's suggested `container_name`
/// (nothing is ever named after it — the real name is derived from the engine id
/// and the host port) and the requested `port` instead of the granted one. Both
/// are rewritten from the runtime columns, which have always been the truth.
const SERVICES_CONFIG_RUNTIME_TRUTH: &str = r#"
UPDATE services
   SET config_json = json_remove(config_json, '$.container_name')
 WHERE json_valid(config_json)
   AND json_type(config_json, '$.container_name') IS NOT NULL;

UPDATE services
   SET config_json = json_set(config_json, '$.port', runtime_port)
 WHERE json_valid(config_json)
   AND runtime_port IS NOT NULL
   AND json_extract(config_json, '$.port') IS NOT runtime_port;

UPDATE services
   SET config_json = json_remove(config_json, '$.port')
 WHERE json_valid(config_json)
   AND runtime_port IS NULL
   AND json_type(config_json, '$.port') IS NOT NULL;
"#;

/// The bot no longer receives STT/LLM/TTS aliases or a flow through env —
/// every FlowInvoke turn resolves them from the session row, so the effective
/// pipeline has to be persisted at spawn. NULL on legacy rows: the reader
/// falls back to the teams-* defaults and the factory Meeting Bot flow.
fn meeting_sessions_add_pipeline_columns(conn: &Connection) -> Result<()> {
    for column in ["stt_alias", "llm_alias", "tts_alias", "flow_id"] {
        if !column_exists(conn, "meeting_sessions", column)? {
            conn.execute_batch(&format!(
                "ALTER TABLE meeting_sessions ADD COLUMN {column} TEXT;"
            ))?;
        }
    }
    Ok(())
}

/// Embedding requests were counted (`embedding_tokens`) but never priced, so
/// every embedding model looked free. Separate rate because embedding tokens
/// are not prompt tokens (different models, different price lists).
fn model_pricing_add_embedding_rate(conn: &Connection) -> Result<()> {
    if !column_exists(conn, "model_pricing", "embedding_per_1k")? {
        conn.execute_batch(
            "ALTER TABLE model_pricing ADD COLUMN embedding_per_1k REAL NOT NULL DEFAULT 0;",
        )?;
    }
    Ok(())
}

/// Successful requests whose backend returned no `usage` landed in the rollup
/// with zero tokens and a silently zero cost. The counter makes that gap
/// visible (cost is a lower bound when it is non-zero). Errors never count.
fn model_metrics_rollup_add_usage_missing(conn: &Connection) -> Result<()> {
    if !column_exists(conn, "model_metrics_rollup", "usage_missing_count")? {
        conn.execute_batch(
            "ALTER TABLE model_metrics_rollup ADD COLUMN usage_missing_count INTEGER NOT NULL DEFAULT 0;",
        )?;
    }
    Ok(())
}

/// Which agents an agent may delegate to.
///
/// `core.agent_spawn` resolved any name at all, so holding the tool meant
/// holding the whole roster: the only bounds were numeric (depth, width, the
/// session run budget). A "team" was therefore a convention in a prompt rather
/// than a contract anything enforced.
///
/// Three states, each meaningful — which is why the column is nullable rather
/// than defaulting to an empty array:
///   * `NULL`      — no roster configured; any agent, i.e. what every existing
///                   agent does today,
///   * `'[]'`      — explicitly may delegate to nobody,
///   * `'["a"]'`   — only the names listed.
///
/// Idempotent — guarded by a column probe.
fn agents_add_delegation_roster(conn: &Connection) -> Result<()> {
    if !column_exists(conn, "agents", "allowed_agents_json")? {
        conn.execute_batch("ALTER TABLE agents ADD COLUMN allowed_agents_json TEXT")?;
    }
    Ok(())
}

// =============================================================================
// v133 — main-database groundwork for the event-tracking subsystem
// =============================================================================
//
// The timeline itself lives in a side database (`<data>/events.db`, own version
// table, outside the Sync Ledger like `flow_executions` and `audit_log`). Three
// things cannot live there, and they arrive together because each is useless
// alone:
//
//   (a) `audit_log.correlation_id` — the deep link from an accountability entry
//       to the point on the timeline that produced it. `request_id` is NOT
//       reusable for this: `code_studio/audit_outbox.rs` already stores an event
//       id there, and `compliance_ai_events` forces a distinct `request_id` per
//       row via `UNIQUE(org_id, request_id)`. `correlation_id` is the orthogonal
//       key tying ONE logical turn across `audit_log`, `compliance_ai_events`
//       and `run_events` — hence the same column name and the same
//       `(correlation_id, <time> DESC)` index shape as
//       `idx_compliance_ai_events_correlation`.
//
//   (b) `events.read` / `events.read_all` — reading one's own runs is an
//       ordinary user capability, reading every run on the node is not. Two
//       permissions rather than one, so the split is expressible without a
//       role-name check at the dispatch layer.
//
//   (c) retention scope `events` — §2.9: retention ships WITH the log, never
//       later. `compliance_retention_policies.scope_kind` carries a hardcoded
//       CHECK list, so widening it means rebuilding the table; and
//       `compliance_data_subject_retention` holds an FK to it, so the rebuild
//       needs `PRAGMA foreign_keys = OFF` OUTSIDE a transaction (SQLite ignores
//       that pragma inside one) — hence `RustSelfManaged`, following v65.
//
// The 30-day default is deliberately far below the >=183 days AI audit is bound
// to: the timeline is a diagnostic tool, the audit trail is a commitment. They
// are separate rows, so seeding one cannot shorten the other.
//
// Fully idempotent: a column probe for (a), `roles_add_permissions` skips
// permissions already present for (b), and (c) short-circuits the rebuild once
// the CHECK carries the new scope while the seed is `INSERT OR IGNORE` on a
// stable id.
fn create_events_tracking_foundation(conn: &Connection, version: i64, name: &str) -> Result<()> {
    let scope_already_widened: bool = conn
        .query_row(
            "SELECT instr(sql, '''events''') > 0 FROM sqlite_master \
             WHERE type = 'table' AND name = 'compliance_retention_policies'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .map(|value| value != 0)
        .unwrap_or(false);

    conn.execute_batch("PRAGMA foreign_keys = OFF;")?;

    let result = (|| -> Result<()> {
        let tx = conn.unchecked_transaction()?;

        if !column_exists(&tx, "audit_log", "correlation_id")? {
            tx.execute_batch("ALTER TABLE audit_log ADD COLUMN correlation_id TEXT NULL;")?;
        }
        tx.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_audit_log_correlation \
                 ON audit_log(correlation_id, timestamp DESC);",
        )?;

        roles_add_permissions(
            &tx,
            &[
                "org_admin",
                "org_operator",
                "org_viewer",
                "dpo",
                "supervisor",
            ],
            &["events.read"],
        )?;
        roles_add_permissions(&tx, &["org_admin"], &["events.read_all"])?;

        // The rebuild preserves every other column definition, constraint,
        // default, index and trigger exactly as v65 left them.
        if !scope_already_widened {
            tx.execute_batch(
                "
                CREATE TABLE compliance_retention_policies_new (
                    retention_policy_id TEXT PRIMARY KEY,
                    org_id TEXT NOT NULL REFERENCES organizations(org_id) ON DELETE CASCADE,
                    slug TEXT NOT NULL,
                    name_translations TEXT NOT NULL DEFAULT '{}',
                    scope_kind TEXT NOT NULL CHECK(scope_kind IN \
                        ('audit','ai_audit','data_category','document','dsar','breach','general','agent_runs','events')),
                    category_id TEXT NULL REFERENCES compliance_data_categories(category_id) ON DELETE SET NULL,
                    retention_days INTEGER NOT NULL,
                    minimum_days INTEGER NOT NULL DEFAULT 0,
                    action_after_retention TEXT NOT NULL DEFAULT 'delete'
                        CHECK(action_after_retention IN ('delete','anonymize','archive')),
                    is_default INTEGER NOT NULL DEFAULT 0 CHECK(is_default IN (0,1)),
                    is_active INTEGER NOT NULL DEFAULT 1 CHECK(is_active IN (0,1)),
                    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
                    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
                    CHECK(retention_days >= minimum_days),
                    UNIQUE(org_id, slug),
                    CHECK(json_valid(name_translations))
                );
                INSERT INTO compliance_retention_policies_new
                    SELECT retention_policy_id, org_id, slug, name_translations, scope_kind, \
                           category_id, retention_days, minimum_days, action_after_retention, \
                           is_default, is_active, created_at, updated_at \
                    FROM compliance_retention_policies;
                DROP TABLE compliance_retention_policies;
                ALTER TABLE compliance_retention_policies_new RENAME TO compliance_retention_policies;
                CREATE INDEX IF NOT EXISTS idx_compliance_retention_policies_lookup
                    ON compliance_retention_policies(org_id, scope_kind, category_id, is_active);
                CREATE UNIQUE INDEX IF NOT EXISTS idx_compliance_retention_one_default
                    ON compliance_retention_policies(org_id, scope_kind)
                    WHERE is_default = 1 AND category_id IS NULL AND is_active = 1;
                CREATE TRIGGER IF NOT EXISTS compliance_retention_policies_updated_at
                AFTER UPDATE ON compliance_retention_policies
                FOR EACH ROW
                BEGIN
                    UPDATE compliance_retention_policies
                    SET updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
                    WHERE retention_policy_id = NEW.retention_policy_id;
                END;
                ",
            )?;
        }

        // Backfill every existing org: `ensure_org_defaults` runs at org creation
        // only, so an upgraded fleet would otherwise never see the new scope. The
        // id matches `default_record_id` (bare base id for the default org,
        // '{org}:{base}' otherwise), so a later org re-seed is a no-op.
        tx.execute(
            "INSERT OR IGNORE INTO compliance_retention_policies \
                (retention_policy_id, org_id, slug, name_translations, scope_kind, category_id, \
                 retention_days, minimum_days, action_after_retention, is_default, is_active) \
             SELECT CASE WHEN org_id = ?1 THEN ?2 ELSE printf('%s:%s', org_id, ?2) END, \
                    org_id, 'events_default', ?3, 'events', NULL, ?4, 0, 'delete', 1, 1 \
             FROM organizations WHERE status <> 'deleted'",
            rusqlite::params![
                crate::services::org::DEFAULT_ORG_ID,
                crate::compliance::EVENTS_RETENTION_POLICY_BASE_ID,
                crate::compliance::EVENTS_RETENTION_NAME_TRANSLATIONS,
                crate::compliance::EVENTS_RETENTION_DAYS,
            ],
        )?;

        let fk_violations = foreign_key_check(&tx)?;
        if !fk_violations.is_empty() {
            anyhow::bail!(
                "create_events_tracking_foundation: foreign_key_check found {} violation(s): {}",
                fk_violations.len(),
                fk_violations.join("; ")
            );
        }

        tx.execute(
            "INSERT OR IGNORE INTO _migrations (version, name) VALUES (?1, ?2)",
            rusqlite::params![version, name],
        )?;
        tx.commit()?;
        Ok(())
    })();

    conn.execute_batch("PRAGMA foreign_keys = ON;")?;
    result
}

// =============================================================================
// v134 — repair `addon_graph_collections.file_path` after a data-directory move
// =============================================================================
//
// `GraphManager` now treats the stored `file_path` as the SOURCE OF TRUTH for an
// existing collection; it used to re-derive the path from the collection key on
// every open. The live "Ustawienia -> Magazyn danych" migration
// (`services/storage_admin.rs::run_live_migration`) moved the files and rewrote
// `addon_vector_namespaces.file_path`, but left the graph registry pointing at
// the OLD prefix. That was harmless while the path was re-derived; with the row
// as the source of truth it is silent data loss — such an installation opens a
// BRAND-NEW empty graph at the stale location, and `delete_collection` would
// then delete those empty files and drop the row, orphaning the real data for
// good. `run_live_migration` rewrites both tables now; this repairs the
// installations that already moved under the old code.
//
// WHAT THE CURRENT ORGANIZATIONS DIRECTORY IS, AT MIGRATION TIME.
// `paths::orgs_dir()` cannot be asked here and would answer wrongly: `main.rs`
// calls `load_path_overrides(|_| None)` BEFORE `db::init` and only re-loads the
// live `*_dir` keys from the database AFTER it returns, so while migrations run
// the process-wide override table still reads "default" no matter what the
// operator configured — a repair driven by it would rewrite CORRECT rows into
// wrong ones. The value is nonetheless knowable, from the same persistent source
// `load_path_overrides` consults a moment later: the `addons_data_dir` row of
// THIS connection's `settings` table, which `run_live_migration` writes with
// `set_setting` in the step that moves the files. `current_orgs_dir` therefore
// reproduces `paths::category_dir(AddonData)` — override, else default — which
// is the accessor's entire definition, not a fallback chain.
//
// WHICH ROWS ARE REWRITTEN. Exactly those whose stored path ends with the tail
// `GraphManager::file_path_for` derives from the row's own primary key
// (`<org_id>/addons/<addon_id>/graph/<collection>.cozo`) and does not already
// sit under the current root. For such a row, prefix replacement (what
// `run_live_migration` does to the vector table) and re-deriving from the key
// produce the same string, because a category move preserves every path
// component below the category root — but re-deriving does not need the old
// root, which no longer exists anywhere by the time this runs. A row that does
// NOT end with that tail was created through `GraphManager::ensure_collection_at`
// with an explicit directory and legitimately lives outside the organizations
// tree; it is left untouched.
//
// `updated_at` is deliberately not bumped: it records graph activity, and this
// migration corrects bookkeeping without the collection having changed.
//
// Idempotent: a second run finds every repairable row already equal to its
// expected path and issues no UPDATE at all.
fn repair_graph_collection_paths(conn: &Connection) -> Result<()> {
    let orgs_root = current_orgs_dir(conn)?;

    let rows: Vec<(String, String, String, String)> = {
        let mut stmt = conn.prepare(
            "SELECT org_id, addon_id, collection, file_path FROM addon_graph_collections",
        )?;
        let mut collected = Vec::new();
        let mut cursor = stmt.query([])?;
        while let Some(row) = cursor.next()? {
            collected.push((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?));
        }
        collected
    };

    let mut repaired = 0usize;
    for (org_id, addon_id, collection, file_path) in rows {
        let tail = std::path::PathBuf::from(&org_id)
            .join("addons")
            .join(&addon_id)
            .join("graph")
            .join(format!("{collection}.cozo"));
        let tail = format!("{}{}", std::path::MAIN_SEPARATOR, tail.to_string_lossy());
        if !file_path.ends_with(&tail) {
            continue;
        }
        let expected = orgs_root
            .join(&org_id)
            .join("addons")
            .join(&addon_id)
            .join("graph")
            .join(format!("{collection}.cozo"))
            .to_string_lossy()
            .to_string();
        if expected == file_path {
            continue;
        }
        conn.execute(
            "UPDATE addon_graph_collections SET file_path = ?4 \
             WHERE org_id = ?1 AND addon_id = ?2 AND collection = ?3",
            rusqlite::params![org_id, addon_id, collection, expected],
        )?;
        repaired += 1;
    }

    if repaired > 0 {
        info!(
            "v134: rewrote {} stale graph collection path(s) under {}",
            repaired,
            orgs_root.display()
        );
    }
    Ok(())
}

/// The organizations directory as the runtime resolves it, read from the
/// database rather than from `paths` — the process-wide override table is not
/// populated yet while migrations run (see the v134 header). Mirrors
/// `paths::category_dir(StorageCategory::AddonData)`: the operator override when
/// one is set to a non-blank value, otherwise the category default.
fn current_orgs_dir(conn: &Connection) -> Result<std::path::PathBuf> {
    use rusqlite::OptionalExtension;

    let category = crate::paths::StorageCategory::AddonData;
    let configured: Option<String> = conn
        .query_row(
            "SELECT value FROM settings WHERE key = ?1",
            rusqlite::params![category.setting_key()],
            |row| row.get(0),
        )
        .optional()?;
    // `paths::override_path` reads a blank setting as "no override".
    match configured.filter(|value| !value.trim().is_empty()) {
        Some(dir) => Ok(std::path::PathBuf::from(dir)),
        None => Ok(category.default_dir()),
    }
}

// =============================================================================
// v135 — provenance columns on `flow_executions` and `agent_runs`
// =============================================================================
//
// Stage 1 of the event-tracking plan (`docs/DOKONCZENIE_RAG_I_ZDARZENIA.md`
// §2.5, acceptance row §2.11) requires `flow_executions` to answer "from where
// and who" without a new table, and the same question is asked of an agent run,
// so both tables get the same five columns and the same index shapes.
//
// All five are NULLABLE with no default, and existing rows are NOT backfilled.
// An `ALTER TABLE ... ADD COLUMN ... NOT NULL` needs a default, and any default
// here would stamp a provenance the node never observed — spec invariant 6
// ("origin and actor* NEVER come from anywhere but the entry point"). A NULL
// origin on a historical run is the honest statement "this predates the stamp";
// a guessed one is fabrication that no later query can distinguish from a real
// observation. New rows always carry non-NULL `origin` and `actor_kind`:
// `NewAgentRun` and `NewFlowExecution` type both as `&str`, not `Option`, and
// the writers (`AgentRunManager::spawn`, `create_execution_record`) fill them
// from the principal / `ExecutionContext`. So the NULLs mark exactly the
// pre-migration population. `actor_id`, `actor_user_id` and `correlation_id`
// stay legitimately NULLABLE on new rows too — a system run has no actor id and
// a service API key has no user behind it.
//
// Index shapes follow the log they will be joined against:
//   * `(origin, <time> DESC)` and `(actor_id, <time> DESC)` mirror
//     `ix_run_events_origin` / `ix_run_events_actor` in §2.3 — both browse
//     queries filter on one value and order by time, so the time column belongs
//     in the index. `actor_kind` is deliberately NOT the leading column: it has
//     a handful of distinct values and would only cost a level of B-tree.
//   * `(correlation_id, <time> DESC)` matches `idx_audit_log_correlation` from
//     v133 and `idx_compliance_ai_events_correlation`, so the audit deep link
//     reads the same shape from every table it merges.
//
// The columns are NOT folded into `INITIAL_SCHEMA` / `AGENTS_REGISTRY`. v133 set
// the precedent both ways and the distinguishing factor is cost: it inlined the
// widened `scope_kind` CHECK because reaching it otherwise means a full table
// rebuild, and left `audit_log.correlation_id` as a plain migration because
// `ADD COLUMN` is O(1) metadata. These five are the second kind. For
// `flow_executions` inlining would additionally be WRONG: migration v2
// (`FLOW_EXECUTIONS_ALLOW_COMPLETED`) rebuilds that table from a fixed column
// list and runs on a fresh install too, so anything added to `INITIAL_SCHEMA`
// would be dropped again three migrations later.
//
// Idempotent — every column is guarded by a probe and every index by
// `IF NOT EXISTS`.
const RUN_PROVENANCE_COLUMNS: [&str; 5] = [
    "origin",
    "actor_kind",
    "actor_id",
    "actor_user_id",
    "correlation_id",
];

fn add_run_provenance_columns(conn: &Connection) -> Result<()> {
    for table in ["flow_executions", "agent_runs"] {
        for column in RUN_PROVENANCE_COLUMNS {
            if !column_exists(conn, table, column)? {
                conn.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {column} TEXT;"))?;
            }
        }
    }

    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_flow_executions_origin
             ON flow_executions(origin, started_at DESC);
         CREATE INDEX IF NOT EXISTS idx_flow_executions_actor
             ON flow_executions(actor_id, started_at DESC);
         CREATE INDEX IF NOT EXISTS idx_flow_executions_correlation
             ON flow_executions(correlation_id, started_at DESC);
         CREATE INDEX IF NOT EXISTS idx_agent_runs_origin
             ON agent_runs(origin, created_at DESC);
         CREATE INDEX IF NOT EXISTS idx_agent_runs_actor
             ON agent_runs(actor_id, created_at DESC);
         CREATE INDEX IF NOT EXISTS idx_agent_runs_correlation
             ON agent_runs(correlation_id, created_at DESC);",
    )?;
    Ok(())
}

/// Completes the accounting of an agent run: the model it spent tokens on and
/// the prompt/completion split next to the `total_tokens` column that was there
/// from the start. A run settled with a total alone cannot be billed, compared
/// across models, or copied into a Code Studio turn — which is why those three
/// columns stayed empty downstream. Idempotent — guarded by column probes.
fn agent_runs_add_token_accounting_columns(conn: &Connection) -> Result<()> {
    if !column_exists(conn, "agent_runs", "prompt_tokens")? {
        conn.execute_batch(
            "ALTER TABLE agent_runs ADD COLUMN prompt_tokens INTEGER NOT NULL DEFAULT 0;",
        )?;
    }
    if !column_exists(conn, "agent_runs", "completion_tokens")? {
        conn.execute_batch(
            "ALTER TABLE agent_runs ADD COLUMN completion_tokens INTEGER NOT NULL DEFAULT 0;",
        )?;
    }
    if !column_exists(conn, "agent_runs", "model")? {
        conn.execute_batch("ALTER TABLE agent_runs ADD COLUMN model TEXT;")?;
    }
    Ok(())
}

// Code Studio — registry of workspaces (org scope, synchronized) plus the
// node-local secret vault. The split is deliberate: `code_workspaces` and its
// satellites describe WHAT exists and who may touch it, and travel through the
// Sync Ledger; `code_workspace_secrets` and `code_agent_credentials` hold key
// material encrypted with the per-node SettingsCipher key and must never be
// added to `sync/core_registry.rs` — the precedent is `addon_config`, which
// synchronizes only `is_secret=0`.
const CODE_STUDIO_REGISTRY: &str = r#"
CREATE TABLE IF NOT EXISTS code_workspaces (
    id TEXT PRIMARY KEY,
    org_id TEXT NOT NULL,
    owner_user_id TEXT NOT NULL,
    name TEXT NOT NULL,
    slug TEXT NOT NULL,
    node_id TEXT NOT NULL,
    exec_mode TEXT NOT NULL DEFAULT 'trusted_native'
        CHECK(exec_mode IN ('container','trusted_native')),
    container_image TEXT,
    egress_enforcement TEXT NOT NULL
        CHECK(egress_enforcement IN ('namespace','firewall','unrestricted')),
    repo_kind TEXT NOT NULL CHECK(repo_kind IN ('empty','git')),
    repo_url TEXT,
    repo_auth_kind TEXT CHECK(repo_auth_kind IN ('none','token','ssh_key')),
    secret_ref TEXT,
    ssh_host_fingerprint TEXT,
    default_branch TEXT,
    target_branch TEXT,
    autonomy_ceiling TEXT NOT NULL DEFAULT 'normal'
        CHECK(autonomy_ceiling IN ('plan','normal','auto_edit','autonomous')),
    egress_policy TEXT NOT NULL DEFAULT 'org_approved'
        CHECK(egress_policy IN ('local_only','org_approved','any')),
    index_enabled INTEGER NOT NULL DEFAULT 0,
    quota_disk_bytes INTEGER,
    quota_sessions INTEGER,
    status TEXT NOT NULL
        CHECK(status IN ('provisioning','active','error','archived','deleted')),
    status_detail TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE(org_id, owner_user_id, slug)
);
CREATE INDEX IF NOT EXISTS idx_code_workspaces_org ON code_workspaces(org_id, status);
CREATE INDEX IF NOT EXISTS idx_code_workspaces_node ON code_workspaces(node_id, status);

CREATE TABLE IF NOT EXISTS code_workspace_members (
    workspace_id TEXT NOT NULL REFERENCES code_workspaces(id) ON DELETE CASCADE,
    user_id TEXT NOT NULL,
    role TEXT NOT NULL CHECK(role IN ('owner','editor','viewer')),
    added_by TEXT NOT NULL,
    added_at TEXT NOT NULL,
    PRIMARY KEY (workspace_id, user_id)
);
CREATE INDEX IF NOT EXISTS idx_code_workspace_members_user ON code_workspace_members(user_id);

CREATE TABLE IF NOT EXISTS code_workspace_project_links (
    workspace_id TEXT NOT NULL REFERENCES code_workspaces(id) ON DELETE CASCADE,
    project_id TEXT NOT NULL,
    linked_by TEXT NOT NULL,
    created_at TEXT NOT NULL,
    PRIMARY KEY (workspace_id, project_id)
);

CREATE TABLE IF NOT EXISTS code_workspace_creator_grants (
    org_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    granted_by TEXT NOT NULL,
    created_at TEXT NOT NULL,
    PRIMARY KEY (org_id, user_id)
);

CREATE TABLE IF NOT EXISTS code_workspace_allowlist (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    workspace_id TEXT NOT NULL REFERENCES code_workspaces(id) ON DELETE CASCADE,
    capability TEXT NOT NULL,
    pattern TEXT NOT NULL,
    created_by TEXT NOT NULL,
    created_at TEXT NOT NULL,
    UNIQUE(workspace_id, capability, pattern)
);

CREATE TABLE IF NOT EXISTS code_workspace_saga_steps (
    workspace_id TEXT NOT NULL,
    step TEXT NOT NULL,
    status TEXT NOT NULL CHECK(status IN ('pending','done','failed','compensated')),
    detail TEXT,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (workspace_id, step)
);

CREATE TABLE IF NOT EXISTS code_workspace_secrets (
    secret_ref TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL,
    kind TEXT NOT NULL CHECK(kind IN ('git_token','ssh_key')),
    material_enc BLOB NOT NULL,
    fingerprint TEXT,
    created_by TEXT NOT NULL,
    created_at TEXT NOT NULL,
    rotated_at TEXT,
    last_used_at TEXT
);
CREATE INDEX IF NOT EXISTS idx_code_workspace_secrets_ws
    ON code_workspace_secrets(workspace_id);

CREATE TABLE IF NOT EXISTS code_agent_credentials (
    org_id TEXT NOT NULL,
    node_id TEXT NOT NULL,
    engine_id TEXT NOT NULL,
    material_enc BLOB NOT NULL,
    provider_base_url TEXT NOT NULL,
    fingerprint TEXT,
    created_by TEXT NOT NULL,
    created_at TEXT NOT NULL,
    rotated_at TEXT,
    last_used_at TEXT,
    PRIMARY KEY (org_id, node_id, engine_id)
);

CREATE TABLE IF NOT EXISTS session_assertion_jti (
    jti TEXT PRIMARY KEY,
    expires_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_assertion_jti_exp ON session_assertion_jti(expires_at);
"#;

/// Reading the workspace catalog is available to every role that can see the
/// dashboard; administering someone else's workspace is not. Creating one needs
/// a per-user grant on top (`code_workspace_creator_grants`), the same shape
/// Project Studio uses, because a workspace consumes disk and can execute code.
fn roles_add_code_studio_permissions(conn: &Connection) -> Result<()> {
    roles_add_permissions(
        conn,
        &[
            "org_admin",
            "org_operator",
            "org_viewer",
            "dpo",
            "supervisor",
        ],
        &["code_studio.read"],
    )?;
    roles_add_permissions(conn, &["org_admin"], &["code_studio.admin"])
}

const SERVICE_USAGE_SNAPSHOT: &str = r#"
ALTER TABLE services ADD COLUMN usage_json TEXT;
ALTER TABLE services ADD COLUMN usage_updated_at TEXT;
"#;

const CONVERSATION_MESSAGES_REASONING_CONTENT: &str = r#"
ALTER TABLE conversation_messages ADD COLUMN reasoning_content TEXT;
"#;

/// v124 — a benchmark target may now be measured IN-PROCESS (`api_type='local'`),
/// which is the only way to reach a backend without a dialable chat endpoint:
/// embedded llama.cpp / MLX, a QUIC sidecar, a coding-agent bridge. SQLite cannot
/// alter a CHECK constraint, so the table is rebuilt. `kind` keeps its two values
/// — it records WHERE the target came from (picked from the service list vs typed
/// by hand), while `api_type` says how we talk to it.
fn benchmark_targets_allow_local_api_type(
    conn: &Connection,
    version: i64,
    name: &str,
) -> Result<()> {
    conn.execute_batch("PRAGMA foreign_keys = OFF;")?;
    let result = (|| -> Result<()> {
        let tx = conn.unchecked_transaction()?;
        tx.execute_batch(
            r#"
CREATE TABLE benchmark_targets_local_new (
    id            TEXT PRIMARY KEY,
    benchmark_id  TEXT NOT NULL REFERENCES benchmarks(id) ON DELETE CASCADE,
    kind          TEXT NOT NULL CHECK (kind IN ('service','external')),
    service_ref   TEXT,
    api_type      TEXT NOT NULL DEFAULT 'openai'
                  CHECK (api_type IN ('openai','anthropic','local')),
    host          TEXT NOT NULL,
    port          INTEGER NOT NULL DEFAULT 0,
    api_key_enc   TEXT,
    model         TEXT NOT NULL,
    label         TEXT NOT NULL
);
INSERT INTO benchmark_targets_local_new SELECT
    id, benchmark_id, kind, service_ref, api_type, host, port, api_key_enc, model, label
FROM benchmark_targets;
DROP TABLE benchmark_targets;
ALTER TABLE benchmark_targets_local_new RENAME TO benchmark_targets;
CREATE INDEX idx_benchmark_targets_benchmark ON benchmark_targets(benchmark_id);
"#,
        )?;
        let violations = foreign_key_check(&tx)?;
        if !violations.is_empty() {
            anyhow::bail!(
                "benchmark_targets_local_api_type: {}",
                violations.join("; ")
            );
        }
        tx.execute(
            "INSERT INTO _migrations (version, name) VALUES (?1, ?2)",
            rusqlite::params![version, name],
        )?;
        tx.commit()?;
        Ok(())
    })();
    conn.execute_batch("PRAGMA foreign_keys = ON;")?;
    result
}

fn managed_cli_service_tags(conn: &Connection, version: i64, name: &str) -> Result<()> {
    conn.execute_batch("PRAGMA foreign_keys = OFF;")?;
    let result = (|| -> Result<()> {
        let tx = conn.unchecked_transaction()?;
        tx.execute_batch(
        r#"
CREATE TABLE services_coding_agent_new (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    engine_id TEXT NOT NULL,
    category TEXT NOT NULL,
    display_name TEXT NOT NULL,
    deploy_method TEXT NOT NULL CHECK(deploy_method IN ('docker','native_embedded','native_binary','native_python_bundle','native_managed_cli','external')),
    transport TEXT NOT NULL CHECK(transport IN ('embedded','http_direct','sidecar_quic','external_http','agent_rpc')),
    status TEXT NOT NULL CHECK(status IN ('deploying','starting','running','degraded','failed','stopped','interrupted')) DEFAULT 'starting',
    pinned INTEGER NOT NULL DEFAULT 0,
    paused INTEGER NOT NULL DEFAULT 0,
    runtime_pid INTEGER,
    runtime_port INTEGER,
    sidecar_quic_port INTEGER,
    endpoint_url TEXT,
    config_json TEXT NOT NULL DEFAULT '{}',
    active_deploy_id TEXT NOT NULL DEFAULT '',
    last_deploy_id TEXT NOT NULL DEFAULT '',
    deployment_progress_pct INTEGER NOT NULL DEFAULT 0,
    deployed_source_hash TEXT NOT NULL DEFAULT '',
    health_last_ok TIMESTAMP,
    health_last_err TEXT,
    progress_message TEXT,
    restart_count INTEGER NOT NULL DEFAULT 0,
    created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
);
INSERT INTO services_coding_agent_new SELECT
    id, engine_id, category, display_name, deploy_method, transport, status,
    pinned, paused, runtime_pid, runtime_port, sidecar_quic_port, endpoint_url,
    config_json, active_deploy_id, last_deploy_id, deployment_progress_pct, deployed_source_hash,
    health_last_ok, health_last_err, progress_message, restart_count, created_at, updated_at
FROM services;
DROP TABLE services;
ALTER TABLE services_coding_agent_new RENAME TO services;
CREATE INDEX idx_services_status ON services(status);
CREATE INDEX idx_services_engine ON services(engine_id);
CREATE INDEX idx_services_category ON services(category);
CREATE INDEX idx_services_active_deploy ON services(active_deploy_id);
"#,
        )?;
        let violations = foreign_key_check(&tx)?;
        if !violations.is_empty() {
            anyhow::bail!("managed_cli_service_tags: {}", violations.join("; "));
        }
        tx.execute(
            "INSERT INTO _migrations (version, name) VALUES (?1, ?2)",
            rusqlite::params![version, name],
        )?;
        tx.commit()?;
        Ok(())
    })();
    conn.execute_batch("PRAGMA foreign_keys = ON;")?;
    result
}

// v112 — the built-in Notes screen was removed in favor of the notes addon
// (which keeps its data in its own addon SQLite). The platform `notes` table
// and its indexes are dead storage, so drop them.
const DROP_NOTES_TABLE: &str = r#"
DROP INDEX IF EXISTS idx_notes_user;
DROP INDEX IF EXISTS idx_notes_user_updated;
DROP TABLE IF EXISTS notes;
"#;

/// v111 — vision model registry for the configurable camera-CV pipeline.
/// Rows describe dynamic ONNX models (trained in ML Studio or imported) that
/// the generic `onnx-cv` embedded engine serves without recompiling. Bundled
/// fixed engines (rfdetr-adr / nalepka-stan / plate-ocr / onnx-ocr / apple-ocr)
/// are NOT seeded here — they keep resolving through their fixed engine ids.
/// The CHECK admits op='embed' so a future op does not need a schema change,
/// but registration rejects it until `CameraCvOp` grows an Embed variant.
/// Idempotent (IF NOT EXISTS).
fn create_vision_models(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS vision_models (
            model_name TEXT PRIMARY KEY,
            op TEXT NOT NULL CHECK(op IN ('detect','classify','ocr','embed')),
            file_name TEXT NOT NULL,
            sha256 TEXT NOT NULL,
            classes_json TEXT NOT NULL DEFAULT '[]',
            preprocess_json TEXT NOT NULL DEFAULT '{}',
            output_contract TEXT NOT NULL CHECK(output_contract IN ('rfdetr','softmax')),
            source TEXT NOT NULL CHECK(source IN ('bundled','trained','imported')),
            default_threshold REAL,
            org_id TEXT NOT NULL DEFAULT 'org-default',
            project_id TEXT,
            source_model_id TEXT,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL
        );",
    )?;
    Ok(())
}

/// v110 — org scoping for camera CV pipelines. Adds `org_id` (defaulting
/// existing rows to the single default org, same convention as other
/// org-scoped tables) and replaces the global "one default pipeline" unique
/// index with a per-org one, so every org resolves its own `is_default=1`
/// row. Idempotent (column probe + IF NOT EXISTS).
fn camera_cv_pipelines_add_org_scope(conn: &Connection) -> Result<()> {
    if !column_exists(conn, "camera_cv_pipelines", "org_id")? {
        conn.execute_batch(
            "ALTER TABLE camera_cv_pipelines \
             ADD COLUMN org_id TEXT NOT NULL DEFAULT 'org-default';",
        )?;
    }
    conn.execute_batch(
        "DROP INDEX IF EXISTS idx_camera_cv_pipelines_default;
        CREATE UNIQUE INDEX IF NOT EXISTS idx_camera_cv_pipelines_default_org
            ON camera_cv_pipelines(org_id) WHERE is_default = 1;",
    )?;
    Ok(())
}

/// v109 — configurable camera CV pipelines. `camera_cv_pipelines` stores the
/// stage graph as JSON (validated by `cv_pipeline::validate` on save);
/// `is_default=1` marks the seed-owned fallback used by cameras without an
/// explicit assignment. `cameras.cv_pipeline_id` is the per-camera pointer
/// (NULL = default pipeline). Idempotent (IF NOT EXISTS + column probe).
fn create_camera_cv_pipelines(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS camera_cv_pipelines (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            pipeline_json TEXT NOT NULL,
            is_default INTEGER NOT NULL DEFAULT 0,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL
        );
        CREATE UNIQUE INDEX IF NOT EXISTS idx_camera_cv_pipelines_default
            ON camera_cv_pipelines(is_default) WHERE is_default = 1;",
    )?;
    if !column_exists(conn, "cameras", "cv_pipeline_id")? {
        conn.execute_batch("ALTER TABLE cameras ADD COLUMN cv_pipeline_id TEXT NULL;")?;
    }
    Ok(())
}

// v108 — RBAC dla Benchmark Studio. Admin/DPO dostają odczyt i zapis (edycja
// definicji, start i anulowanie runów), operator oraz viewer tylko odczyt.
// Idempotentne przez `roles_add_permissions` (wzorzec jak `model_metrics_add_permissions`).
fn benchmark_add_permissions(conn: &Connection) -> Result<()> {
    roles_add_permissions(
        conn,
        &["org_admin", "dpo"],
        &["benchmark.read", "benchmark.write"],
    )?;
    roles_add_permissions(conn, &["org_operator", "org_viewer"], &["benchmark.read"])?;
    Ok(())
}

// v105 — backfill `tokens.*` grants. v87 (`token_metrics_add_permissions`) was added
// after some databases had already migrated past version 87, so it never ran there and
// admins ended up without `tokens.read` (PolicyDenied on the token usage screen). Re-run
// the same grants at a fresh version; `roles_add_permissions` is idempotent, so fresh
// installs (which already got v87) are a no-op.
fn ensure_token_permissions(conn: &Connection) -> Result<()> {
    roles_add_permissions(
        conn,
        &["org_admin", "dpo"],
        &["tokens.read", "tokens.write"],
    )?;
    roles_add_permissions(conn, &["org_operator", "org_viewer"], &["tokens.read"])?;
    Ok(())
}

// v106 — dodaje vendor 'mjpeg' (kamery MJPEG po HTTP, multipart/x-mixed-replace)
// do CHECK-a `cameras.vendor`. SQLite nie umie zmienić CHECK-a w miejscu, więc
// tabela jest przebudowywana. W odróżnieniu od wcześniejszych rebuildów
// (v48/v80 z zaszytym schematem) schemat bierzemy DYNAMICZNIE z sqlite_master —
// po v80 kolejne migracje dokładały kolumny (analysis_*, depth_*) przez ALTER
// TABLE, więc zaszyta lista kolumn groziłaby ich utratą. Podmieniamy wyłącznie
// listę vendorów w CHECK-u, kopiujemy wiersze 1:1 (`INSERT ... SELECT *` —
// identyczna kolejność kolumn po rebuildzie z tego samego SQL) i odtwarzamy
// indeksy. Idempotentna: gdy CHECK zawiera już 'mjpeg', nic nie robi. Żadna
// tabela nie ma FK na `cameras`, więc rebuild w transakcji runnera jest
// bezpieczny bez żonglowania `PRAGMA foreign_keys`.
fn cameras_vendor_check_add_mjpeg(conn: &Connection) -> Result<()> {
    let sql: String = conn.query_row(
        "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'cameras'",
        [],
        |row| row.get(0),
    )?;
    if sql.contains("'mjpeg'") {
        return Ok(());
    }
    let old_list = "'fake_file', 'rtsp', 'onvif', 'local_camera', 'v4l2', 'webrtc'";
    let new_list = "'fake_file', 'rtsp', 'onvif', 'local_camera', 'v4l2', 'webrtc', 'mjpeg'";
    let patched = sql.replace(old_list, new_list);
    if patched == sql {
        anyhow::bail!(
            "cameras_vendor_check_add_mjpeg: nie znaleziono oczekiwanej listy vendorów w CHECK — \
             schemat tabeli cameras odbiega od v80"
        );
    }
    // Pierwsze wystąpienie 'cameras' w SQL to nazwa tabeli (goła lub w
    // cudzysłowie) — podmiana na 'cameras_new' daje poprawny CREATE w obu
    // wariantach zapisu.
    let create_new = patched.replacen("cameras", "cameras_new", 1);

    conn.execute_batch("DROP TABLE IF EXISTS cameras_new;")?;
    conn.execute_batch(&create_new)?;
    conn.execute_batch(
        r#"
INSERT INTO cameras_new SELECT * FROM cameras;
DROP TABLE cameras;
ALTER TABLE cameras_new RENAME TO cameras;

CREATE UNIQUE INDEX idx_cameras_camera_id_active ON cameras(camera_id) WHERE removed_at IS NULL;
CREATE INDEX idx_cameras_owner ON cameras(owner_addon_id, removed_at);
CREATE INDEX idx_cameras_status ON cameras(status, removed_at);
CREATE INDEX idx_cameras_org_id ON cameras(org_id);
"#,
    )?;
    Ok(())
}

// v104 — RBAC dla metryk modeli. Admin/DPO dostają odczyt i zapis cennika,
// operator oraz viewer tylko odczyt. Idempotentne przez `roles_add_permissions`
// (wzorzec jak `token_metrics_add_permissions`).
fn model_metrics_add_permissions(conn: &Connection) -> Result<()> {
    roles_add_permissions(
        conn,
        &["org_admin", "dpo"],
        &["metrics.read", "metrics.write"],
    )?;
    roles_add_permissions(conn, &["org_operator", "org_viewer"], &["metrics.read"])?;
    Ok(())
}

// v102 — mesh-wide hourly rollup of model performance and usage metrics. Like
// `token_usage_daily` it is single-writer-per-row (the owning node accumulates
// only its own `id` rows; a mesh-wide figure is the SUM across all node rows) yet
// replicates so any node can render fleet metrics. `id` is a deterministic hash of
// all dimensions + node_id so the same logical bucket minted on different nodes
// never collides. Latency/throughput distributions are captured as fixed-edge
// histograms with a per-histogram `sample_count` so a bucket sum of 0 (no samples)
// is distinguishable from a genuine measured 0. `histogram_version` lets a future
// edge change re-key rows instead of silently mixing incompatible bucketings.
const MODEL_METRICS_ROLLUP_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS model_metrics_rollup (
    id                     TEXT PRIMARY KEY,
    node_id                TEXT NOT NULL,
    org_id                 TEXT NOT NULL REFERENCES organizations(org_id) ON DELETE CASCADE,
    user_id                TEXT NOT NULL,
    model_id               TEXT NOT NULL,
    service_key            TEXT NOT NULL,
    backend                TEXT NOT NULL,
    modality               TEXT NOT NULL,
    hour_bucket            TEXT NOT NULL,
    histogram_version      INTEGER NOT NULL DEFAULT 1,
    request_count          INTEGER NOT NULL DEFAULT 0,
    success_count          INTEGER NOT NULL DEFAULT 0,
    error_count            INTEGER NOT NULL DEFAULT 0,
    prompt_tokens          INTEGER NOT NULL DEFAULT 0,
    completion_tokens      INTEGER NOT NULL DEFAULT 0,
    total_tokens           INTEGER NOT NULL DEFAULT 0,
    embedding_tokens       INTEGER NOT NULL DEFAULT 0,
    audio_ms               INTEGER NOT NULL DEFAULT 0,
    images                 INTEGER NOT NULL DEFAULT 0,
    prefill_secs_sum       REAL NOT NULL DEFAULT 0,
    decode_secs_sum        REAL NOT NULL DEFAULT 0,
    e2e_latency_ms_sum     INTEGER NOT NULL DEFAULT 0,
    queue_ms_sum           INTEGER NOT NULL DEFAULT 0,
    ttft_b0                INTEGER NOT NULL DEFAULT 0,
    ttft_b1                INTEGER NOT NULL DEFAULT 0,
    ttft_b2                INTEGER NOT NULL DEFAULT 0,
    ttft_b3                INTEGER NOT NULL DEFAULT 0,
    ttft_b4                INTEGER NOT NULL DEFAULT 0,
    ttft_b5                INTEGER NOT NULL DEFAULT 0,
    ttft_b6                INTEGER NOT NULL DEFAULT 0,
    ttft_b7                INTEGER NOT NULL DEFAULT 0,
    ttft_b8                INTEGER NOT NULL DEFAULT 0,
    ttft_b9                INTEGER NOT NULL DEFAULT 0,
    ttft_sample_count      INTEGER NOT NULL DEFAULT 0,
    decode_tps_b0          INTEGER NOT NULL DEFAULT 0,
    decode_tps_b1          INTEGER NOT NULL DEFAULT 0,
    decode_tps_b2          INTEGER NOT NULL DEFAULT 0,
    decode_tps_b3          INTEGER NOT NULL DEFAULT 0,
    decode_tps_b4          INTEGER NOT NULL DEFAULT 0,
    decode_tps_b5          INTEGER NOT NULL DEFAULT 0,
    decode_tps_b6          INTEGER NOT NULL DEFAULT 0,
    decode_tps_b7          INTEGER NOT NULL DEFAULT 0,
    decode_tps_sample_count INTEGER NOT NULL DEFAULT 0,
    e2e_b0                 INTEGER NOT NULL DEFAULT 0,
    e2e_b1                 INTEGER NOT NULL DEFAULT 0,
    e2e_b2                 INTEGER NOT NULL DEFAULT 0,
    e2e_b3                 INTEGER NOT NULL DEFAULT 0,
    e2e_b4                 INTEGER NOT NULL DEFAULT 0,
    e2e_b5                 INTEGER NOT NULL DEFAULT 0,
    e2e_b6                 INTEGER NOT NULL DEFAULT 0,
    e2e_b7                 INTEGER NOT NULL DEFAULT 0,
    e2e_b8                 INTEGER NOT NULL DEFAULT 0,
    e2e_b9                 INTEGER NOT NULL DEFAULT 0,
    e2e_sample_count       INTEGER NOT NULL DEFAULT 0,
    updated_at             TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_model_metrics_model ON model_metrics_rollup(org_id, model_id, hour_bucket);
CREATE INDEX IF NOT EXISTS idx_model_metrics_user ON model_metrics_rollup(org_id, user_id, hour_bucket);
CREATE INDEX IF NOT EXISTS idx_model_metrics_node ON model_metrics_rollup(node_id);
CREATE INDEX IF NOT EXISTS idx_model_metrics_updated ON model_metrics_rollup(updated_at);
"#;

// v103 — per-model pricing, admin edited and LWW replicated (like `token_quota`).
// The primary key is a deterministic synthetic `id` (hash of org_id+model_id) so
// the same logical row minted on different nodes never collides and, crucially,
// two organizations pricing the same `model_id` stay isolated. `updated_at` is the
// node-local watermark and is NOT synced; the HLC on the sync operation drives
// last-writer-wins, so a stale concurrent edit never clobbers a newer one.
const MODEL_PRICING_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS model_pricing (
    id                 TEXT PRIMARY KEY,
    org_id             TEXT NOT NULL REFERENCES organizations(org_id) ON DELETE CASCADE,
    model_id           TEXT NOT NULL,
    prompt_per_1k      REAL NOT NULL DEFAULT 0,
    completion_per_1k  REAL NOT NULL DEFAULT 0,
    audio_per_min      REAL NOT NULL DEFAULT 0,
    image_each         REAL NOT NULL DEFAULT 0,
    updated_at         TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE(org_id, model_id)
);
CREATE INDEX IF NOT EXISTS idx_model_pricing_org ON model_pricing(org_id);
"#;

// v107 — Benchmark Studio: user-defined API benchmarks (llama-bench-style) for
// mesh services and external LLM APIs. `benchmark_results.target_id` has NO FK
// on purpose: targets may be edited/replaced between runs while historical
// results must stay intact, hence the `target_label` snapshot on each row.
// External API keys land in `api_key_enc` encrypted with the settings cipher.
const BENCHMARK_STUDIO_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS benchmarks (
    id           TEXT PRIMARY KEY,
    org_id       TEXT NOT NULL REFERENCES organizations(org_id) ON DELETE CASCADE,
    name         TEXT NOT NULL,
    config_json  TEXT NOT NULL DEFAULT '{}',
    created_at   TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at   TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_benchmarks_org ON benchmarks(org_id);

CREATE TABLE IF NOT EXISTS benchmark_targets (
    id            TEXT PRIMARY KEY,
    benchmark_id  TEXT NOT NULL REFERENCES benchmarks(id) ON DELETE CASCADE,
    kind          TEXT NOT NULL CHECK (kind IN ('service','external')),
    service_ref   TEXT,
    api_type      TEXT NOT NULL DEFAULT 'openai' CHECK (api_type IN ('openai','anthropic')),
    host          TEXT NOT NULL,
    port          INTEGER NOT NULL DEFAULT 0,
    api_key_enc   TEXT,
    model         TEXT NOT NULL,
    label         TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_benchmark_targets_benchmark ON benchmark_targets(benchmark_id);

CREATE TABLE IF NOT EXISTS benchmark_runs (
    id                TEXT PRIMARY KEY,
    benchmark_id      TEXT NOT NULL REFERENCES benchmarks(id) ON DELETE CASCADE,
    started_at        TEXT NOT NULL DEFAULT (datetime('now')),
    finished_at       TEXT,
    status            TEXT NOT NULL DEFAULT 'running'
                      CHECK (status IN ('running','success','failed','cancelled')),
    error             TEXT,
    engine_meta_json  TEXT NOT NULL DEFAULT '{}'
);
CREATE INDEX IF NOT EXISTS idx_benchmark_runs_benchmark ON benchmark_runs(benchmark_id, started_at DESC);

CREATE TABLE IF NOT EXISTS benchmark_results (
    id                TEXT PRIMARY KEY,
    run_id            TEXT NOT NULL REFERENCES benchmark_runs(id) ON DELETE CASCADE,
    target_id         TEXT NOT NULL,
    target_label      TEXT NOT NULL,
    scenario          TEXT NOT NULL CHECK (scenario IN ('latency','throughput','context','sustained')),
    variant_json      TEXT NOT NULL DEFAULT '{}',
    ttft_ms_mean      REAL,
    ttft_ms_sigma     REAL,
    prefill_tps_mean  REAL,
    prefill_tps_sigma REAL,
    decode_tps_mean   REAL,
    decode_tps_sigma  REAL,
    total_ms_mean     REAL,
    total_ms_sigma    REAL,
    p50_ms            REAL,
    p90_ms            REAL,
    p99_ms            REAL,
    requests          INTEGER NOT NULL DEFAULT 0,
    errors            INTEGER NOT NULL DEFAULT 0,
    samples_json      TEXT NOT NULL DEFAULT '[]'
);
CREATE INDEX IF NOT EXISTS idx_benchmark_results_run ON benchmark_results(run_id);
"#;

/// Persist the torch.distributed TCPStore master port (`VLLM_PORT`) leased from
/// the coordinator's `PortAllocator` alongside the serve port, so a clean stop
/// can release BOTH leases (serve `port` was already persisted; `dist_port` was
/// not, leaking that lease on every successful teardown). Idempotent (column
/// probe). Default 0 = legacy row predating allocation; stop skips releasing 0.
fn cluster_deployments_add_dist_port(conn: &Connection) -> Result<()> {
    if !column_exists(conn, "cluster_deployments", "dist_port")? {
        conn.execute_batch(
            "ALTER TABLE cluster_deployments ADD COLUMN dist_port INTEGER NOT NULL DEFAULT 0;",
        )?;
    }
    Ok(())
}

/// Per-member RoCEv2 GID index for `NCCL_IB_GID_INDEX` (D3). Default 3 — the
/// verified RoCEv2-IPv4 GID on the ConnectX-7 twins of the DGX Spark; carried
/// per member so a future RoCE probe can persist a detected value without a
/// code change (no hardcode in the deploy path). Idempotent (column probe).
fn cluster_members_add_gid_index(conn: &Connection) -> Result<()> {
    if !column_exists(conn, "cluster_members", "rdma_gid_index")? {
        conn.execute_batch(
            "ALTER TABLE cluster_members ADD COLUMN rdma_gid_index INTEGER NOT NULL DEFAULT 3;",
        )?;
    }
    Ok(())
}

/// Distributed deploy persistence (D3) — one row per multi-node tensor-parallel
/// deployment (`cluster_deployments`) plus its head/worker membership
/// (`cluster_deployment_members`). Survives restart so a running cluster-service
/// can be listed, stopped (head + all workers) and redeployed. Idempotent
/// (`CREATE TABLE IF NOT EXISTS`).
fn create_cluster_deployments_tables(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS cluster_deployments (
            deployment_cluster_id TEXT PRIMARY KEY,
            cluster_id            TEXT NOT NULL,
            engine_id             TEXT NOT NULL,
            model                 TEXT NOT NULL,
            served_model_name     TEXT NOT NULL DEFAULT '',
            tp_size               INTEGER NOT NULL,
            head_node_id          TEXT NOT NULL,
            port                  INTEGER NOT NULL,
            endpoint_url          TEXT,
            status                TEXT NOT NULL DEFAULT 'deploying',
            created_at            TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at            TEXT NOT NULL DEFAULT (datetime('now'))
        );
        CREATE TABLE IF NOT EXISTS cluster_deployment_members (
            deployment_cluster_id TEXT NOT NULL,
            node_id               TEXT NOT NULL,
            role                  TEXT NOT NULL,
            container_name        TEXT NOT NULL DEFAULT '',
            PRIMARY KEY (deployment_cluster_id, node_id),
            FOREIGN KEY (deployment_cluster_id)
                REFERENCES cluster_deployments(deployment_cluster_id) ON DELETE CASCADE
        );
        CREATE INDEX IF NOT EXISTS idx_cluster_deployments_cluster
            ON cluster_deployments(cluster_id);",
    )?;
    Ok(())
}

/// Cluster RDMA auto-config (D1) — persist per-member the RoCE devices to use
/// for distributed deploy (`NCCL_IB_HCA`, BOTH QSFP twins), the QSFP socket
/// netdev and the primary RDMA IP. The downstream distributed-deploy step (D3)
/// reads these to build the correct NCCL environment without re-probing.
/// Idempotent (PRAGMA table_info probe per column).
fn cluster_members_add_rdma_columns(conn: &Connection) -> Result<()> {
    let mut existing = std::collections::HashSet::new();
    {
        let mut stmt = conn.prepare("PRAGMA table_info(cluster_members)")?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let name: String = row.get(1)?;
            existing.insert(name);
        }
    }
    if !existing.contains("rdma_devices") {
        conn.execute_batch(
            "ALTER TABLE cluster_members ADD COLUMN rdma_devices TEXT NOT NULL DEFAULT '';",
        )?;
    }
    if !existing.contains("rdma_ip") {
        conn.execute_batch(
            "ALTER TABLE cluster_members ADD COLUMN rdma_ip TEXT NOT NULL DEFAULT '';",
        )?;
    }
    if !existing.contains("rdma_socket_ifname") {
        conn.execute_batch(
            "ALTER TABLE cluster_members ADD COLUMN rdma_socket_ifname TEXT NOT NULL DEFAULT '';",
        )?;
    }
    Ok(())
}

/// Adds camera extrinsic-calibration columns for depth mapping: `depth_camera_pitch_deg`
/// (mount tilt of the camera vs the body forward axis — the Go2 front camera is angled
/// down, which a 0-pitch model places wrong vs the lidar) and `depth_scale` (metric
/// scale correction for the monocular depth — ZoeDepth's absolute scale is approximate).
/// Both live-tunable so the camera cloud can be aligned to the lidar without a rebuild.
/// Idempotent (column probes).
fn cameras_add_depth_extrinsics_columns(conn: &Connection) -> Result<()> {
    if !column_exists(conn, "cameras", "depth_camera_pitch_deg")? {
        conn.execute_batch(
            "ALTER TABLE cameras ADD COLUMN depth_camera_pitch_deg REAL NOT NULL DEFAULT 0.0;",
        )?;
    }
    if !column_exists(conn, "cameras", "depth_scale")? {
        conn.execute_batch(
            "ALTER TABLE cameras ADD COLUMN depth_scale REAL NOT NULL DEFAULT 1.0;",
        )?;
    }
    Ok(())
}

/// Adds `depth_camera_fov_v_deg` — the camera's VERTICAL field of view. The depth
/// model runs on a square 518² frame STRETCHED from the camera's wide 16:9 stream, so
/// the vertical FOV differs sharply from the horizontal (Go2: 100° H, 56° V). The old
/// back-projection assumed square pixels (`fy = fx`), over-spreading the cloud
/// vertically off the floor. `0.0` keeps that legacy square behaviour; a positive
/// value sets the true vertical FOV. Idempotent (column probe).
fn cameras_add_depth_fov_v_column(conn: &Connection) -> Result<()> {
    if !column_exists(conn, "cameras", "depth_camera_fov_v_deg")? {
        conn.execute_batch(
            "ALTER TABLE cameras ADD COLUMN depth_camera_fov_v_deg REAL NOT NULL DEFAULT 0.0;",
        )?;
    }
    Ok(())
}

/// v89 — `org_id` na scheduled_jobs (MemGraphRAG D3 / R4). Scheduled job musi niesc
/// org_id instancji addona, by `execute_job` przekazal go do `start_addon` i host-fns
/// (graf/SQL) trafialy w dane WLASCIWEGO najemcy zamiast default org (luka multi-tenant
/// — `start_addon(..., None)`). Kolumna NULLABLE: istniejace joby maja org_id=NULL =>
/// zachowuja dotychczasowe zachowanie (default org), wiec migracja nie lamie ich biegu.
fn scheduled_jobs_add_org_id_column(conn: &Connection) -> Result<()> {
    if !column_exists(conn, "scheduled_jobs", "org_id")? {
        conn.execute_batch("ALTER TABLE scheduled_jobs ADD COLUMN org_id TEXT;")?;
    }
    Ok(())
}

/// v87 — kolumna limitu document/blob store (RAG E1.3) w `addon_resource_limits`.
/// Per-instancja addon przechowuje wgrane pliki (PDF/obraz > limit KV 1MB) w
/// osobnym per-instance `FileBlobStore`; `document_storage_mb` ogranicza łączny
/// rozmiar tych plików. `0` = brak limitu (spójne z `storage_limit_mb`). Dodawana
/// idempotentnie, bo wcześniejsze slice'y (codex pkt 9) świadomie zostawiły tę
/// kolumnę na slice document.
fn create_addon_document_store_limits(conn: &Connection) -> Result<()> {
    if !column_exists(conn, "addon_resource_limits", "document_storage_mb")? {
        conn.execute_batch(
            "ALTER TABLE addon_resource_limits ADD COLUMN document_storage_mb INTEGER NOT NULL DEFAULT 0;",
        )?;
    }
    Ok(())
}

/// v88 — uprawnienia `document.read`/`document.write` dla ról, które mają już
/// odpowiedniki wektorowe/grafowe (RAG E1.3). Admin/operator dostają zapis i
/// odczyt, viewer tylko odczyt — lustro `vector.read`/`vector.write`.
fn roles_add_document_permissions(conn: &Connection) -> Result<()> {
    roles_add_permissions(
        conn,
        &["org_admin", "org_operator"],
        &["document.read", "document.write"],
    )?;
    roles_add_permissions(conn, &["org_viewer"], &["document.read"])?;
    Ok(())
}

/// v86 — udostępnia uprawnienia `graph.read`/`graph.write` rolom, które już mają
/// odpowiedniki wektorowe (RAG 0.2). Admin/operator dostają zapis i odczyt,
/// viewer tylko odczyt — lustro `vector.read`/`vector.write`.
fn roles_add_graph_permissions(conn: &Connection) -> Result<()> {
    roles_add_permissions(
        conn,
        &["org_admin", "org_operator"],
        &["graph.read", "graph.write"],
    )?;
    roles_add_permissions(conn, &["org_viewer"], &["graph.read"])
}

/// v85 — rejestr kolekcji grafowych CozoDB (services/graph) + kolumny limitów
/// grafu w `addon_resource_limits`. Tabela `addon_graph_collections` lustro
/// `addon_vector_namespaces`, ale PK MUSI zawierać `org_id` (poprawka codex
/// pkt 3): ten sam `addon_id` w dwóch organizacjach to fizycznie osobne grafy.
/// Kolumny limitów (`graph_nodes_max`/`graph_edges_max`, poprawka codex pkt 9)
/// dodawane idempotentnie — `document_storage_mb` zostaje na slice document.
/// CHECK na `engine` dopuszcza `('mem','sled','rocksdb')`: wasm32 wstawia
/// `engine='mem'` (sled nie kompiluje się w przeglądarce), więc 'mem' musi być
/// legalny (runda 2 codex bug #2).
fn create_addon_graph_collections(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
CREATE TABLE IF NOT EXISTS addon_graph_collections (
    org_id TEXT NOT NULL,
    addon_id TEXT NOT NULL,
    collection TEXT NOT NULL,
    file_path TEXT NOT NULL,
    engine TEXT NOT NULL CHECK(engine IN ('mem', 'sled', 'rocksdb')),
    node_count INTEGER NOT NULL DEFAULT 0,
    edge_count INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (org_id, addon_id, collection)
);
CREATE INDEX IF NOT EXISTS idx_addon_graph_collections_addon
    ON addon_graph_collections(org_id, addon_id);
"#,
    )?;

    if !column_exists(conn, "addon_resource_limits", "graph_nodes_max")? {
        conn.execute_batch(
            "ALTER TABLE addon_resource_limits ADD COLUMN graph_nodes_max INTEGER NOT NULL DEFAULT 0;",
        )?;
    }
    if !column_exists(conn, "addon_resource_limits", "graph_edges_max")? {
        conn.execute_batch(
            "ALTER TABLE addon_resource_limits ADD COLUMN graph_edges_max INTEGER NOT NULL DEFAULT 0;",
        )?;
    }
    Ok(())
}

// v88 — rozszerza per-dzienny licznik o pozostale modalnosci poza chatem LLM:
// STT (milisekundy audio), generacja obrazow (sztuki) i embeddingi (tokeny).
// Liczniki kumulatywne w zakresie i64 (audio_ms moze byc bardzo duze).
const TOKEN_METRICS_MODALITIES: &str = r#"
ALTER TABLE token_usage_daily ADD COLUMN audio_ms INTEGER NOT NULL DEFAULT 0;
ALTER TABLE token_usage_daily ADD COLUMN images INTEGER NOT NULL DEFAULT 0;
ALTER TABLE token_usage_daily ADD COLUMN embedding_tokens INTEGER NOT NULL DEFAULT 0;
"#;

// v87 — RBAC dla metryk tokenów. Admin/DPO dostają odczyt i zapis, operator
// oraz viewer tylko odczyt. Idempotentne przez `roles_add_permissions`.
fn token_metrics_add_permissions(conn: &Connection) -> Result<()> {
    roles_add_permissions(
        conn,
        &["org_admin", "dpo"],
        &["tokens.read", "tokens.write"],
    )?;
    roles_add_permissions(conn, &["org_operator", "org_viewer"], &["tokens.read"])?;
    Ok(())
}

// v86 — per-model/per-user/per-group token accounting that rides the Sync Ledger.
// All three tables use TEXT primary keys (deterministic synthetic or UUID) so the
// same logical row minted on different nodes never collides under an autoincrement
// INTEGER. `token_usage_daily` is single-writer-per-row (only the owning node
// mutates its own rows; the sum across all node rows is the global usage), while
// quotas and leases are admin/coordinator edited and replicate LWW.
const TOKEN_METRICS_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS token_usage_daily (
    id                TEXT PRIMARY KEY,
    node_id           TEXT NOT NULL,
    org_id            TEXT NOT NULL REFERENCES organizations(org_id) ON DELETE CASCADE,
    user_id           TEXT NOT NULL,
    model_id          TEXT NOT NULL,
    usage_day         TEXT NOT NULL,
    prompt_tokens     INTEGER NOT NULL DEFAULT 0,
    completion_tokens INTEGER NOT NULL DEFAULT 0,
    total_tokens      INTEGER NOT NULL DEFAULT 0,
    request_count     INTEGER NOT NULL DEFAULT 0,
    updated_at        TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_token_usage_user ON token_usage_daily(org_id, user_id, usage_day);
CREATE INDEX IF NOT EXISTS idx_token_usage_model ON token_usage_daily(org_id, model_id, usage_day);
CREATE INDEX IF NOT EXISTS idx_token_usage_updated ON token_usage_daily(updated_at);

CREATE TABLE IF NOT EXISTS token_quota (
    id               TEXT PRIMARY KEY,
    org_id           TEXT NOT NULL REFERENCES organizations(org_id) ON DELETE CASCADE,
    scope_type       TEXT NOT NULL,
    subject_id       TEXT,
    model_id         TEXT,
    period           TEXT NOT NULL,
    max_total_tokens INTEGER NOT NULL,
    is_active        INTEGER NOT NULL DEFAULT 1,
    created_at       TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE(org_id, scope_type, subject_id, model_id, period)
);

CREATE TABLE IF NOT EXISTS token_lease (
    id                  TEXT PRIMARY KEY,
    org_id              TEXT NOT NULL REFERENCES organizations(org_id) ON DELETE CASCADE,
    quota_id            TEXT NOT NULL,
    node_id             TEXT NOT NULL,
    period_key          TEXT NOT NULL,
    base_used           INTEGER NOT NULL,
    granted_tokens      INTEGER NOT NULL,
    coordinator_node_id TEXT NOT NULL,
    expires_at          TEXT NOT NULL,
    created_at          TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE(org_id, quota_id, node_id, period_key)
);
"#;

// v85 — rebuild pii_rules with a TEXT UUID primary key and an org_id column so
// it can replicate as a per-org core sync resource. Existing INTEGER rows get a
// fresh UUID and default to DEFAULT_ORG_ID; the name-unique index becomes a
// (org_id, name)-unique index. Modeled on `api_keys_access_v2`.
fn pii_rules_uuid_org_v2(conn: &Connection, version: i64, name: &str) -> Result<()> {
    let already_done: bool = conn
        .query_row(
            "SELECT instr(sql, 'org_id') > 0 FROM sqlite_master \
             WHERE type = 'table' AND name = 'pii_rules'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .map(|v| v != 0)
        .unwrap_or(false);
    if already_done {
        conn.execute(
            "INSERT OR IGNORE INTO _migrations (version, name) VALUES (?1, ?2)",
            rusqlite::params![version, name],
        )?;
        return Ok(());
    }

    conn.execute_batch("PRAGMA foreign_keys = OFF;")?;

    let result = (|| -> Result<()> {
        let tx = conn.unchecked_transaction()?;

        tx.execute_batch(
            "
            DROP INDEX IF EXISTS idx_pii_rules_active;
            DROP INDEX IF EXISTS idx_pii_rules_name_unique;
            CREATE TABLE pii_rules_new (
                id TEXT PRIMARY KEY,
                org_id TEXT NOT NULL REFERENCES organizations(org_id) ON DELETE CASCADE,
                name TEXT NOT NULL,
                category TEXT NOT NULL,
                pattern TEXT NOT NULL,
                replacement TEXT NOT NULL DEFAULT '[UKRYTY]',
                is_active INTEGER NOT NULL DEFAULT 1,
                priority INTEGER DEFAULT 0,
                description TEXT,
                test_examples TEXT,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                UNIQUE(org_id, name)
            );
            ",
        )?;

        // Carry over every existing row with a fresh UUID id and the default org.
        let legacy: Vec<(
            String,
            String,
            String,
            String,
            i64,
            i64,
            Option<String>,
            Option<String>,
            String,
        )> = {
            let mut stmt = tx.prepare(
                "SELECT name, category, pattern, replacement, is_active, priority, \
                        description, test_examples, created_at FROM pii_rules",
            )?;
            let rows = stmt
                .query_map([], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, i64>(4)?,
                        r.get::<_, i64>(5)?,
                        r.get::<_, Option<String>>(6)?,
                        r.get::<_, Option<String>>(7)?,
                        r.get::<_, String>(8)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        };
        for (
            rule_name,
            category,
            pattern,
            replacement,
            is_active,
            priority,
            description,
            test_examples,
            created_at,
        ) in legacy
        {
            tx.execute(
                "INSERT INTO pii_rules_new \
                 (id, org_id, name, category, pattern, replacement, is_active, priority, description, test_examples, created_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                rusqlite::params![
                    uuid::Uuid::new_v4().to_string(),
                    crate::services::org::DEFAULT_ORG_ID,
                    rule_name,
                    category,
                    pattern,
                    replacement,
                    is_active,
                    priority,
                    description,
                    test_examples,
                    created_at,
                ],
            )?;
        }

        tx.execute_batch(
            "
            DROP TABLE pii_rules;
            ALTER TABLE pii_rules_new RENAME TO pii_rules;
            CREATE INDEX idx_pii_rules_active ON pii_rules(is_active, priority);
            ",
        )?;

        let fk_violations = foreign_key_check(&tx)?;
        if !fk_violations.is_empty() {
            anyhow::bail!(
                "pii_rules_uuid_org_v2: foreign_key_check found {} violation(s): {}",
                fk_violations.len(),
                fk_violations.join("; ")
            );
        }

        tx.execute(
            "INSERT INTO _migrations (version, name) VALUES (?1, ?2)",
            rusqlite::params![version, name],
        )?;
        tx.commit()?;
        Ok(())
    })();

    conn.execute_batch("PRAGMA foreign_keys = ON;")?;
    result
}

// Write-behind backing store for the in-RAM `AddonStateStore` Durable tier
// (A2). The store serves Durable entries from RAM and the periodic flusher
// persists them here so a restart recovers them; Ephemeral entries are never
// written. `value` is the opaque addon-owned blob. `updated_at` is the
// host-side last-write millis used as the last-write-wins marker. Node-local
// (not sync-replicated): each node owns its addons' state. The PK already
// indexes `addon_id` as a prefix, so per-addon load/purge scans are covered
// without a separate index.
const ADDON_STATE_TABLE: &str = r#"
CREATE TABLE IF NOT EXISTS addon_state (
    addon_id   TEXT NOT NULL,
    state_key  TEXT NOT NULL,
    value      BLOB NOT NULL,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (addon_id, state_key)
);
"#;

// Cross-addon camera access grants. A camera is owned by one addon
// (`cameras.owner_addon_id`); a grant lets ANOTHER addon read/view it without
// relaxing the per-owner isolation everywhere else. `grantee_addon_id = '*'`
// means org-wide. `level` is an allowlist (only 'read' today). Node-local like
// `cameras` (not sync-replicated). Authorization to CREATE a grant (owner/admin)
// is enforced at the host-fn layer, not here.
const CAMERA_GRANTS_TABLE: &str = r#"
CREATE TABLE IF NOT EXISTS camera_grants (
    camera_id        TEXT NOT NULL,
    grantee_addon_id TEXT NOT NULL,
    level            TEXT NOT NULL DEFAULT 'read' CHECK(level IN ('read')),
    org_id           TEXT NOT NULL,
    created_at       INTEGER NOT NULL,
    created_by       TEXT NOT NULL,
    PRIMARY KEY (camera_id, grantee_addon_id, level)
);
CREATE INDEX IF NOT EXISTS idx_camera_grants_grantee ON camera_grants(grantee_addon_id, org_id);
CREATE INDEX IF NOT EXISTS idx_camera_grants_camera ON camera_grants(camera_id);
"#;

/// Adds `analysis_flow_id` to `cameras` — the per-camera analysis Flow run by the
/// cold path on a detection event. NULL/empty = no flow (cold path enriches with
/// the default hardcoded pipeline). Idempotent (column probe).
fn cameras_add_analysis_flow_id_column(conn: &Connection) -> Result<()> {
    if !column_exists(conn, "cameras", "analysis_flow_id")? {
        conn.execute_batch("ALTER TABLE cameras ADD COLUMN analysis_flow_id TEXT;")?;
    }
    Ok(())
}

/// Adds the depth-mapping columns to `cameras`. When `depth_mapping_enabled`,
/// the always-on depth loop pulls RGB frames, runs a metric depth model, and
/// back-projects to a world point cloud fed into the shared SLAM map under
/// `depth_robot_id`. `depth_camera_fov_deg` is the horizontal field of view used
/// to derive pinhole intrinsics (no per-camera calibration table yet);
/// `depth_fps` paces the loop (depth inference is far heavier than detection).
/// Idempotent — each column guarded by a probe.
fn cameras_add_depth_mapping_columns(conn: &Connection) -> Result<()> {
    if !column_exists(conn, "cameras", "depth_mapping_enabled")? {
        conn.execute_batch(
            "ALTER TABLE cameras ADD COLUMN depth_mapping_enabled INTEGER NOT NULL DEFAULT 0;",
        )?;
    }
    if !column_exists(conn, "cameras", "depth_robot_id")? {
        conn.execute_batch("ALTER TABLE cameras ADD COLUMN depth_robot_id TEXT NULL;")?;
    }
    if !column_exists(conn, "cameras", "depth_camera_fov_deg")? {
        conn.execute_batch(
            "ALTER TABLE cameras ADD COLUMN depth_camera_fov_deg REAL NOT NULL DEFAULT 60.0;",
        )?;
    }
    if !column_exists(conn, "cameras", "depth_fps")? {
        conn.execute_batch("ALTER TABLE cameras ADD COLUMN depth_fps INTEGER NOT NULL DEFAULT 2;")?;
    }
    Ok(())
}

/// Adds `depth_pose_robot_id` to `cameras` — the robot whose POSE places the depth
/// cloud, decoupled from `depth_robot_id` (where the cloud is STORED). They differ
/// for calibration: a Go2 camera reuses the lidar robot's pose (`go2`) but stores
/// its depth cloud under a separate id (`go2-depth`) so the two clouds can be
/// overlaid and measured against each other. NULL ⇒ falls back to `depth_robot_id`
/// (the merged case — a phone places and stores under its own id). Idempotent.
fn cameras_add_depth_pose_source_column(conn: &Connection) -> Result<()> {
    if !column_exists(conn, "cameras", "depth_pose_robot_id")? {
        conn.execute_batch("ALTER TABLE cameras ADD COLUMN depth_pose_robot_id TEXT NULL;")?;
    }
    Ok(())
}

/// Adds `analysis_fps` to `cameras` — the per-camera AI analysis frame rate
/// honored by the always-on vision analysis loop. `0` means unlimited (run at
/// the native frame cadence); the default of `10` matches
/// `CAMERA_DEFAULT_ANALYSIS_FPS`. Idempotent — guarded by a column probe so a
/// re-run on an already-migrated database is a no-op.
/// Adds `vision_worker_slot` to `cameras` — the vision-worker sharding slot
/// (docs/VISION_WORKER_SHARDING.md Stage B). NULL = the camera runs in-process
/// on the core; a non-NULL slot means the camera is owned by that vision
/// worker. The core is the ONLY writer of this column (assignment authority);
/// workers act solely on AssignCamera/RemoveCamera link commands and never
/// read it. Idempotent — guarded by a column probe.
fn cameras_add_vision_worker_slot_column(conn: &Connection) -> Result<()> {
    if !column_exists(conn, "cameras", "vision_worker_slot")? {
        conn.execute_batch("ALTER TABLE cameras ADD COLUMN vision_worker_slot INTEGER NULL;")?;
    }
    Ok(())
}

/// Adds `event_meta` to `recordings` — a nullable JSON blob written by the
/// per-vehicle event recorder (`services/event_recorder.rs`): trigger classes,
/// plate/ADR OCR votes, condition flags and the event's start/stop wall-clock
/// range. NULL for every addon-saved snapshot/segment. `json_valid` is enforced
/// at write time by the recorder (single writer), not by a CHECK, so the ALTER
/// stays a cheap in-place column add. Idempotent — guarded by a column probe.
fn recordings_add_event_meta_column(conn: &Connection) -> Result<()> {
    if !column_exists(conn, "recordings", "event_meta")? {
        conn.execute_batch("ALTER TABLE recordings ADD COLUMN event_meta TEXT NULL;")?;
    }
    Ok(())
}

/// Adds the four search/thumbnail columns to `recordings`, all nullable and
/// written only by the per-vehicle event recorder at finalize:
///   * `plate_text` / `adr_text` — the gated OCR winner strings for the event
///     (NULL when unreadable), used by the recordings-browser LIKE search.
///   * `plate_thumb_ref` / `adr_thumb_ref` — snapshot refs of the FULL downscaled
///     camera frame captured at the highest-confidence plate/ADR read of the
///     event (whole scene, not a crop), rendered as list thumbnails.
/// `created_at` already carries `idx_recordings_owner`/`idx_recordings_camera`
/// (both begin with a purged-scan column, not `created_at`), so an explicit
/// `created_at` index backs the date-range filter's `ORDER BY`/range scan.
/// A `%plate%` LIKE cannot use an index, but the per-addon row count is small
/// (event clips, not raw frames) so a scan of the owner-scoped subset is fine.
/// Idempotent — every add is guarded by a column probe.
fn recordings_add_search_columns(conn: &Connection) -> Result<()> {
    if !column_exists(conn, "recordings", "plate_text")? {
        conn.execute_batch("ALTER TABLE recordings ADD COLUMN plate_text TEXT NULL;")?;
    }
    if !column_exists(conn, "recordings", "adr_text")? {
        conn.execute_batch("ALTER TABLE recordings ADD COLUMN adr_text TEXT NULL;")?;
    }
    if !column_exists(conn, "recordings", "plate_thumb_ref")? {
        conn.execute_batch("ALTER TABLE recordings ADD COLUMN plate_thumb_ref TEXT NULL;")?;
    }
    if !column_exists(conn, "recordings", "adr_thumb_ref")? {
        conn.execute_batch("ALTER TABLE recordings ADD COLUMN adr_thumb_ref TEXT NULL;")?;
    }
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_recordings_created_at ON recordings(created_at);",
    )?;
    Ok(())
}

/// Detection zones drawn on a camera: JSON array of polygons, each polygon a
/// JSON array of `[x, y]` points in NORMALIZED frame coordinates (0.0-1.0), so
/// a zone survives any resolution change. Empty array = no zones = the whole
/// frame stays live (backward compatible for every existing camera).
///
/// The vision engine drops every detection whose box centre falls outside ALL
/// zones BEFORE enrichment — an operator watching a wide scene then spends no
/// OCR/classify budget on positions they do not care about.
fn cameras_add_zones_column(conn: &Connection) -> Result<()> {
    if !column_exists(conn, "cameras", "zones_json")? {
        conn.execute_batch(
            "ALTER TABLE cameras ADD COLUMN zones_json TEXT NOT NULL DEFAULT '[]';",
        )?;
    }
    Ok(())
}

fn cameras_add_analysis_fps_column(conn: &Connection) -> Result<()> {
    if !column_exists(conn, "cameras", "analysis_fps")? {
        conn.execute_batch(
            "ALTER TABLE cameras ADD COLUMN analysis_fps INTEGER NOT NULL DEFAULT 10;",
        )?;
    }
    Ok(())
}

/// Adds `is_system` to `flows`. Marks platform-seeded flows that user-facing
/// handlers must refuse to edit, delete or change status on — the flag is only
/// ever set by platform seeding, never through the user create/update paths.
fn flows_add_is_system_column(conn: &Connection) -> Result<()> {
    if !column_exists(conn, "flows", "is_system")? {
        conn.execute_batch("ALTER TABLE flows ADD COLUMN is_system INTEGER NOT NULL DEFAULT 0;")?;
    }
    Ok(())
}

/// Adds `citations_json` to `conversation_messages`. Project-chat (ps-chat)
/// assistant replies persist their RAG citations next to the content, so
/// `ChatHistoryResponse` can rebuild the sources panel after a reload. NULL /
/// empty = message without citations (every non-ps-chat writer leaves it NULL).
/// Idempotent — guarded by a column probe.
fn conversation_messages_add_citations_column(conn: &Connection) -> Result<()> {
    if !column_exists(conn, "conversation_messages", "citations_json")? {
        conn.execute_batch("ALTER TABLE conversation_messages ADD COLUMN citations_json TEXT;")?;
    }
    Ok(())
}

/// Adds `installed_bundle_hash` to `addons` (the instance table). It records the
/// `addon_packages.bundle_hash` the instance was materialized from, so update
/// detection can fire on a CONTENT change (manifest/wasm/migrations) even when
/// the version string is unchanged — bundled addons routinely ship edits under
/// the same version. Existing rows default to '' which reads as "older than any
/// catalogued hash", so an instance correctly surfaces an available update after
/// the next rebuild. Idempotent — guarded by a column probe.
fn addons_add_installed_bundle_hash_column(conn: &Connection) -> Result<()> {
    if !column_exists(conn, "addons", "installed_bundle_hash")? {
        conn.execute_batch(
            "ALTER TABLE addons ADD COLUMN installed_bundle_hash TEXT NOT NULL DEFAULT '';",
        )?;
    }
    Ok(())
}

/// Adds the `methods` column to `model_aliases`. Methods are declared in the
/// owner addon's `[[alias]].methods` manifest list and were previously parsed
/// but never persisted, so a consumer addon had no way to learn which capability
/// (detect/recognize/embed/...) an alias serves. The column stores the methods
/// as a JSON array string; an empty list is the default for manual aliases and
/// addons that declare no methods. Idempotent — guarded by a column probe so a
/// re-run on an already-migrated database is a no-op.
fn model_aliases_add_methods_column(conn: &Connection) -> Result<()> {
    if !column_exists(conn, "model_aliases", "methods")? {
        conn.execute_batch(
            "ALTER TABLE model_aliases ADD COLUMN methods TEXT NOT NULL DEFAULT '[]';",
        )?;
    }
    Ok(())
}

/// Removes the legacy sub-flow harness rows (`…011` TentaFlow Harness, `…013`
/// Agent Iteration) from already-provisioned databases. The harness is now the
/// single "Agent Run" graph (`…012`) with an inline loop region, so these rows
/// are dead weight that would otherwise keep showing up as separate flows in the
/// builder. The agent block resolves only `AGENT_RUN_FLOW_ID` (`…012`).
fn drop_legacy_harness_flows(conn: &Connection) -> Result<()> {
    conn.execute(
        "DELETE FROM flows WHERE id IN \
         ('00000000-0000-4000-8000-000000000011', \
          '00000000-0000-4000-8000-000000000013')",
        [],
    )?;
    Ok(())
}

/// Stable id of the seeded "Agent Run" harness flow (mirrors `seed::AGENT_RUN_FLOW_ID`
/// and `agent_block::AGENT_RUN_FLOW_ID`). The migration rewrites this row in place.
const AGENT_RUN_FLOW_ID: &str = "00000000-0000-4000-8000-000000000012";

/// Installs the current "Agent Run" flow (`…012`) JSON in place. Used by v71
/// (legacy three-graph → single-graph inline `agent_turn` region) and v72
/// (region exit becomes the stream producer for codex-style live token
/// streaming). Both target `agent_run_flow_json()`, so v72 rewrites a v71 row to
/// the streaming shape. Idempotent: the UPDATE only fires when the stored
/// `flow_json` differs from the target, so a re-run is a no-op. The legacy
/// …011/…013 flows are left untouched (read-only legacy). A database without the
/// row (the seed inserts it on fresh installs) is also a no-op.
fn rewrite_agent_run_to_inline_region(conn: &Connection) -> Result<()> {
    let target = crate::db::seed::agent_run_flow_json();
    conn.execute(
        "UPDATE flows SET flow_json = ?1 WHERE id = ?2 AND flow_json != ?1",
        rusqlite::params![target, AGENT_RUN_FLOW_ID],
    )?;
    Ok(())
}

/// Stable id of the seeded "Default Chat" flow (mirrors `seed::DEFAULT_CHAT_FLOW_ID`).
const DEFAULT_CHAT_FLOW_ID: &str = "00000000-0000-4000-8000-000000000010";

/// Exact JSON of the previously-seeded "Default Chat" flow that carried a hidden
/// `pii_filter` node (`trigger -> llm -> pii_filter -> output(stream)`). This is
/// the byte-for-byte shape shipped to every already-seeded DB. Matched verbatim
/// so the migration only rewrites the untouched seeded row — any admin edit
/// (reordering, added node, whitespace) changes the string and is left alone.
const DEFAULT_CHAT_FLOW_JSON_WITH_PII: &str = r#"{"nodes":[{"id":"t1","type":"trigger","position":{"x":0,"y":0},"config":{}},{"id":"l1","type":"llm","position":{"x":200,"y":0},"config":{}},{"id":"p1","type":"pii_filter","position":{"x":400,"y":0},"config":{}},{"id":"o1","type":"output","position":{"x":600,"y":0},"config":{"mode":"stream"}}],"edges":[{"from_node":"t1","to_node":"l1","from_port":"text","data_type":"text"},{"from_node":"l1","to_node":"p1","from_port":"stream"},{"from_node":"p1","to_node":"o1","from_port":"stream","to_port":"text","data_type":"text"}]}"#;

/// Removes the hidden `pii_filter` from the seeded default chat flow. The default
/// flow resolves for every model without its own flow (like the removed synthetic
/// flow), so silently redacting the query/context/response was a hidden default a
/// user never configured. Rewrites the seeded `…010` row (is_default=1) to the
/// clean `trigger -> llm -> output(stream)` pipeline IN PLACE, but ONLY when its
/// stored JSON is exactly the seeded pii shape. Admin-edited flows and flows that
/// deliberately include pii_filter differ from that literal and are untouched.
/// Fresh installs already seed the clean shape, so this is a no-op there.
/// `pii_filter` stays a first-class node users can add themselves in Flow Builder.
fn strip_pii_from_default_chat_flow(conn: &Connection) -> Result<()> {
    conn.execute(
        "UPDATE flows SET flow_json = ?1, \
         description = 'Streaming chat pipeline: trigger -> LLM -> output(stream).' \
         WHERE id = ?2 AND is_default = 1 AND flow_json = ?3",
        rusqlite::params![
            crate::db::seed::LEGACY_DEFAULT_CHAT_FLOW_JSON,
            DEFAULT_CHAT_FLOW_ID,
            DEFAULT_CHAT_FLOW_JSON_WITH_PII,
        ],
    )?;
    Ok(())
}

// Durable conversation history (source of truth; the in-memory cache is only a
// read-through buffer). One row per chat turn message keeps the full structure
// the cache used to drop — `tool_calls` (assistant), `tool_call_id`/`name`
// (tool results) round-trip via JSON, and multimodal payloads live in the blob
// store referenced by `payload_ref`/`payload_kind` instead of being flattened
// to text. `seq` is a per-session monotonic counter; UNIQUE(session_id, seq)
// makes the per-turn batch insert idempotent on retry. Runtime-only table:
// it is not replicated through the sync ledger.
const CONVERSATION_MESSAGES: &str = "
CREATE TABLE conversation_messages (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id TEXT NOT NULL,
    seq INTEGER NOT NULL,
    role TEXT NOT NULL CHECK(role IN ('system','user','assistant','tool')),
    content TEXT,
    tool_calls TEXT,
    tool_call_id TEXT,
    name TEXT,
    payload_ref TEXT,
    payload_kind TEXT,
    node_id TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE(session_id, seq)
);
CREATE INDEX idx_conv_msgs_session ON conversation_messages(session_id, seq);
";

// Sub Flow (Harness §3.5 block 8): a nested flow run gets its own
// `flow_executions` row whose `parent_execution_id` points at the parent run,
// so the execution tree (parent → subflow / loop body / map element) is
// reconstructable from the audit table. NULL = top-level run. No FK on self
// because synthetic/light runs use id 0 and never insert a row.
const FLOW_EXECUTIONS_PARENT_EXECUTION_ID: &str = "
ALTER TABLE flow_executions ADD COLUMN parent_execution_id INTEGER;
CREATE INDEX idx_flow_executions_parent ON flow_executions(parent_execution_id);
";

// Skills registry (Harness plan §3.2): instruction-only skills (markdown +
// text references, never scripts). `name` is deliberately NOT UNIQUE — the
// table replicates fleet-wide (like `flows`) and a UNIQUE constraint would
// break sync apply when two nodes mint same-named skills concurrently;
// soft uniqueness is enforced at the handler/UI layer. `use_count` /
// `last_used_at` are node-local usage stats and never travel through sync.
const SKILLS_REGISTRY: &str = "
CREATE TABLE skills (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    display_name TEXT,
    description TEXT NOT NULL,
    content TEXT NOT NULL,
    tags_json TEXT NOT NULL DEFAULT '[]',
    category TEXT,
    source TEXT NOT NULL CHECK(source IN ('user','addon','hub')),
    source_ref TEXT,
    status TEXT NOT NULL DEFAULT 'active'
        CHECK(status IN ('active','disabled','quarantine','archived')),
    use_count INTEGER NOT NULL DEFAULT 0,
    last_used_at TEXT,
    created_by TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE skill_files (
    skill_id TEXT NOT NULL REFERENCES skills(id) ON DELETE CASCADE,
    path TEXT NOT NULL,
    content TEXT NOT NULL,
    PRIMARY KEY (skill_id, path)
);

CREATE INDEX idx_skills_source ON skills(source);
CREATE INDEX idx_skills_status ON skills(status);
";

// Agents registry (Harness plan §3.3). `agents` replicates fleet-wide like
// `flows`/`skills` (replicated_by_permission), so `name` is deliberately NOT
// UNIQUE — a UNIQUE constraint would break sync apply when two nodes mint a
// same-named agent concurrently; soft uniqueness is enforced in the handler/UI.
// Seeded agents (phase 5) carry stable UUIDs so every fleet node mints an
// identical row and sync apply is idempotent.
//
// `agent_runs` is RUNTIME state (one row per harness execution) and is NOT a
// sync resource — exactly like `flow_executions`. It carries the run principal
// (user_id/org_id) inherited by spawned children and a `run_log` whose PII is
// governed by a dedicated retention policy (see migration 65). The status set
// matches the lifecycle in §3.3 (queued → running → waiting/waiting_user →
// terminal).
const AGENTS_REGISTRY: &str = "
CREATE TABLE agents (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    display_name TEXT,
    description TEXT NOT NULL,
    system_prompt TEXT,
    model TEXT,
    tools_json TEXT NOT NULL DEFAULT '[]',
    skills_json TEXT NOT NULL DEFAULT '{}',
    params_json TEXT NOT NULL DEFAULT '{}',
    max_iterations INTEGER NOT NULL DEFAULT 25,
    timeout_secs INTEGER NOT NULL DEFAULT 600,
    max_subagents INTEGER NOT NULL DEFAULT 0,
    max_spawn_depth INTEGER NOT NULL DEFAULT 1,
    flow_id TEXT,
    routable INTEGER NOT NULL DEFAULT 1,
    is_enabled INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE agent_runs (
    id TEXT PRIMARY KEY,
    agent_id TEXT NOT NULL,
    parent_run_id TEXT,
    flow_execution_id INTEGER,
    user_id TEXT,
    org_id TEXT,
    status TEXT NOT NULL CHECK(status IN \
        ('queued','running','waiting','waiting_user','completed','failed','cancelled','interrupted')),
    prompt TEXT NOT NULL,
    result TEXT,
    exit_reason TEXT,
    iterations INTEGER NOT NULL DEFAULT 0,
    total_tokens INTEGER NOT NULL DEFAULT 0,
    run_log TEXT,
    last_heartbeat_at TEXT,
    started_at TEXT,
    finished_at TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX idx_agents_enabled ON agents(is_enabled);
CREATE INDEX idx_agent_runs_agent ON agent_runs(agent_id);
CREATE INDEX idx_agent_runs_status ON agent_runs(status);
CREATE INDEX idx_agent_runs_parent ON agent_runs(parent_run_id);
";

// Mailbox + auto-continuation (Harness §3.6 levels 2 & 3, Codex V2 pattern).
//
// `agent_mailbox`: when a background CHILD run settles and it knows the context
// that spawned it (a chat session and/or a parent agent), the manager enqueues
// the child's final answer here. The next time `agent_context` primes a run for
// that session/agent it drains the undelivered rows into the model context
// ("a delegated task finished with result: ...") and stamps `delivered_at`.
// `run_id` is the finished child run. Undelivered rows survive a restart
// (SQLite) — that is the whole point of the mailbox over the transient event.
// Runtime state, never synced (like `agent_runs`); retention rides the existing
// `agent_runs` retention scope (the periodic purge redacts both past term).
//
// `agents.on_child_complete`: opt-in autonomous continuation. `notify` (default)
// = phase-6 behavior (enqueue mailbox + emit event). `continue` = the child's
// completion starts a NEW parent run with the child result as input (Ralph
// style); it counts toward concurrency + depth caps like any run, so a mutual
// continuation loop dies on the limits. Admin-only to set (the agents upsert
// handler is already #[policy(Admin)]); the CHECK keeps the column to the two
// known values fleet-wide.
const AGENT_MAILBOX_AND_AUTO_CONTINUATION: &str = "
ALTER TABLE agents ADD COLUMN on_child_complete TEXT NOT NULL DEFAULT 'notify'
    CHECK(on_child_complete IN ('notify','continue'));

CREATE TABLE agent_mailbox (
    id TEXT PRIMARY KEY,
    run_id TEXT NOT NULL,
    target_session_id TEXT,
    target_agent_id TEXT,
    payload TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    delivered_at TEXT
);
CREATE INDEX idx_agent_mailbox_session ON agent_mailbox(target_session_id, delivered_at);
CREATE INDEX idx_agent_mailbox_agent ON agent_mailbox(target_agent_id, delivered_at);
";

// Skills curator (Harness plan §3.2 — grouping/umbrella mechanism). The curator
// is a report-then-apply maintenance pass: an LLM proposes merge/umbrella/archive
// actions over the skill index, an admin approves a subset, and apply mutates the
// `skills` table. Apply is reversible: before any mutation we snapshot the exact
// pre-apply rows of every skill the proposal touches into `skill_curator_snapshots`.
// Rollback restores those rows verbatim. Both tables are node-local runtime state
// (a maintenance audit trail, not synced) — the resulting skill mutations replicate
// fleet-wide through the normal `skills` sync capture, the snapshot rows do not.
const SKILLS_CURATOR_SNAPSHOTS: &str = "
CREATE TABLE skill_curator_snapshots (
    id TEXT PRIMARY KEY,
    proposal_json TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'open'
        CHECK(status IN ('open','applied','rolled_back')),
    created_by TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    applied_at TEXT,
    rolled_back_at TEXT
);

CREATE TABLE skill_curator_snapshot_rows (
    snapshot_id TEXT NOT NULL REFERENCES skill_curator_snapshots(id) ON DELETE CASCADE,
    skill_id TEXT NOT NULL,
    existed INTEGER NOT NULL,
    name TEXT,
    display_name TEXT,
    description TEXT,
    content TEXT,
    tags_json TEXT,
    category TEXT,
    source TEXT,
    source_ref TEXT,
    status TEXT,
    files_json TEXT NOT NULL DEFAULT '[]',
    PRIMARY KEY (snapshot_id, skill_id)
);

CREATE INDEX idx_skill_curator_snapshots_status ON skill_curator_snapshots(status, created_at);
";

// Multi-instance addons: split the single `addons.addon_id` identity into a
// versioned PACKAGE (the template — wasm + manifest + migrations, catalogued in
// `addon_packages`) and an INSTANCE (a row in `addons`, the durable scoping key
// for storage/config/permissions/flow-blocks/sync). Each instance pins one
// package version so instances update independently (test before prod).
//
// Backfill keeps existing single installs working: every addon becomes an
// instance of a same-named package at its current version, display_name = name.
const ADDON_PACKAGES_AND_INSTANCE_VERSIONING: &str = "
CREATE TABLE addon_packages (
    package_id TEXT NOT NULL,
    version TEXT NOT NULL,
    name TEXT NOT NULL DEFAULT '',
    manifest_json TEXT NOT NULL DEFAULT '{}',
    bundle_hash TEXT NOT NULL DEFAULT '',
    source TEXT NOT NULL DEFAULT 'bundled' CHECK(source IN ('bundled','uploaded')),
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (package_id, version)
);

ALTER TABLE addons ADD COLUMN package_id TEXT NOT NULL DEFAULT '';
ALTER TABLE addons ADD COLUMN package_version TEXT NOT NULL DEFAULT '';
ALTER TABLE addons ADD COLUMN display_name TEXT NOT NULL DEFAULT '';

UPDATE addons SET package_id = addon_id WHERE package_id = '';
UPDATE addons SET package_version = version WHERE package_version = '';
UPDATE addons SET display_name = name WHERE display_name = '';
";

// The legacy `users` table (F1a auth) is dead weight: dashboard login,
// session identity and every FK go through `user_accounts` (F2). v38/v56
// still read `users` to backfill memberships / flip identity on upgrading
// installs, so it must survive those migrations — this final step drops it
// once they have run. Fresh installs create it in INITIAL_SCHEMA, exercise
// the historical migrations, then drop it here. No runtime code references
// it after this point.
const DROP_LEGACY_USERS_TABLE: &str = "DROP TABLE IF EXISTS users;";

// =============================================================================
// v56 — core identity INTEGER -> TEXT UUID migration
// =============================================================================
//
// The five core identity tables (`flows`, `flow_model_bindings`,
// `flow_versions`, `user_accounts`, `user_groups`) historically used INTEGER
// AUTOINCREMENT primary keys. Decentralized sync requires globally-unique,
// collision-free identifiers, so this migration rewrites each PK to a TEXT
// UUIDv4 and remaps EVERY dependent FK / identity column accordingly.
//
// The remap is driven by `child_remaps()` — the single source of truth shared
// with the schema guard test, so a forgotten child column cannot silently drift.
// Each child column is rewritten through the same old_int -> new_uuid map that
// was applied to its parent's PK, keeping referential integrity intact.

/// One child column that references a core identity table by its old INTEGER id
/// and must be rewritten to the parent's new UUID.
struct ChildRemap {
    /// Child table holding the FK / identity column.
    table: &'static str,
    /// FK / identity column inside `table`.
    column: &'static str,
    /// Identity table whose PK map drives the rewrite.
    parent: IdentityTable,
}

/// Parent identity tables that appear as a remap target for a child column.
/// `flow_model_bindings` and `flow_versions` also flip to UUID PKs but no other
/// table references them, so they are not selectable parents here — their PK
/// rebuild is invoked directly.
#[derive(Clone, Copy, PartialEq, Eq)]
enum IdentityTable {
    Flows,
    UserAccounts,
    UserGroups,
}

impl IdentityTable {
    fn table_name(self) -> &'static str {
        match self {
            Self::Flows => "flows",
            Self::UserAccounts => "user_accounts",
            Self::UserGroups => "user_groups",
        }
    }
}

/// Exhaustive FK closure: every child column that references one of the five
/// core identity tables. Source of truth for both the migration remap and the
/// `INITIAL_SCHEMA` allowlist guard. A column referencing `user_accounts` or
/// `user_groups` is included even when no SQL-level `REFERENCES` clause exists
/// (several user-attribution columns were declared without a constraint).
fn child_remaps() -> Vec<ChildRemap> {
    use IdentityTable::*;
    let f = |table, column, parent| ChildRemap {
        table,
        column,
        parent,
    };
    vec![
        // -- references to flows(id) --
        f("flow_versions", "flow_id", Flows),
        f("flow_model_bindings", "flow_id", Flows),
        f("flow_executions", "flow_id", Flows),
        f("flow_invocations", "flow_id", Flows),
        f("compliance_ai_events", "flow_id", Flows),
        // -- references to user_accounts(id) --
        f("api_keys", "owner_user_id", UserAccounts),
        f("addon_secrets", "user_id", UserAccounts),
        f("audit_log", "user_id", UserAccounts),
        f("addon_config", "updated_by", UserAccounts),
        f("addon_permissions", "updated_by", UserAccounts),
        f("addon_permission_defaults", "updated_by", UserAccounts),
        f("addon_visibility", "updated_by", UserAccounts),
        f("addon_oauth_config", "updated_by", UserAccounts),
        f("addon_network_config", "updated_by", UserAccounts),
        f("addon_instances", "created_by", UserAccounts),
        f("addon_network_rules", "approved_by", UserAccounts),
        f("oauth_pending_states", "user_id", UserAccounts),
        f("user_oauth_accounts", "user_id", UserAccounts),
        f("meeting_settings", "user_id", UserAccounts),
        f("meeting_sessions", "owner_user_id", UserAccounts),
        f("deployments", "user_id", UserAccounts),
        f("scheduled_jobs", "created_by_user_id", UserAccounts),
        f("sync_nodes", "owner_user_id", UserAccounts),
        f("user_identity_keys", "user_id", UserAccounts),
        f("node_user_assignments", "user_id", UserAccounts),
        f("node_user_assignments", "created_by", UserAccounts),
        f("sync_user_org_profiles", "user_id", UserAccounts),
        f("sync_user_org_profiles", "manager_user_id", UserAccounts),
        f("sync_resource_acl", "owner_user_id", UserAccounts),
        f("sync_resource_acl", "assigned_user_id", UserAccounts),
        f("sync_resource_acl", "manager_user_id", UserAccounts),
        f("sync_explicit_shares", "granted_by", UserAccounts),
        f(
            "__tentaflow_core_sync_captures",
            "actor_user_id",
            UserAccounts,
        ),
        f(
            "__tentaflow_kv_sync_captures",
            "actor_user_id",
            UserAccounts,
        ),
        f(
            "__tentaflow_blob_sync_captures",
            "actor_user_id",
            UserAccounts,
        ),
        f(
            "compliance_processing_activities",
            "owner_user_id",
            UserAccounts,
        ),
        f("compliance_legal_holds", "created_by_user_id", UserAccounts),
        f(
            "compliance_legal_holds",
            "released_by_user_id",
            UserAccounts,
        ),
        f("compliance_documents", "created_by_user_id", UserAccounts),
        f("compliance_ai_events", "user_id", UserAccounts),
        f(
            "compliance_dsar_requests",
            "handled_by_user_id",
            UserAccounts,
        ),
        f("compliance_dpia_records", "owner_user_id", UserAccounts),
        f(
            "compliance_breach_incidents",
            "created_by_user_id",
            UserAccounts,
        ),
        // -- references to user_groups(id) --
        f("sso_providers", "default_group_id", UserGroups),
        f("addon_visibility", "group_id", UserGroups),
        f("sync_exclusions", "group_id", UserGroups),
        // -- polymorphic subject columns (user OR group, by subject_type) --
        // One remap per subject kind: the user-typed rows resolve against the
        // user map, the group-typed rows against the group map.
        f("addon_permissions", "subject_id", UserAccounts),
        f("addon_permissions", "subject_id", UserGroups),
        f("resource_permissions", "subject_id", UserAccounts),
        f("resource_permissions", "subject_id", UserGroups),
        // -- composite member table: both columns flip --
        f("group_members", "group_id", UserGroups),
        f("group_members", "user_id", UserAccounts),
    ]
}

/// INTEGER columns that match the FK naming pattern but intentionally stay
/// INTEGER because they reference a table whose PK is NOT migrated in this step
/// (services, voice_profiles, meeting_sessions, model_aliases) or are a local
/// surrogate / payload value, not a core-identity FK.
#[cfg(test)]
struct IntentionalLocalInteger {
    table: &'static str,
    column: &'static str,
    /// WHY this column is exempt.
    reason: &'static str,
}

#[cfg(test)]
fn intentionally_local_integers() -> Vec<IntentionalLocalInteger> {
    let l = |table, column, reason| IntentionalLocalInteger {
        table,
        column,
        reason,
    };
    vec![
        l(
            "meeting_transcripts",
            "profile_id",
            "references voice_profiles(id), which stays INTEGER",
        ),
        l(
            "model_alias_visibility",
            "updated_by_user_id",
            "model_aliases subtree retains legacy INTEGER user attribution; no FK, audit-only",
        ),
        l(
            "model_alias_consumers",
            "granted_by_user_id",
            "model_aliases subtree audit-only user attribution, no FK constraint",
        ),
        l(
            "model_visibility",
            "updated_by_user_id",
            "model_aliases subtree audit-only user attribution, no FK constraint",
        ),
        l(
            "model_consumers",
            "granted_by_user_id",
            "model_aliases subtree audit-only user attribution, no FK constraint",
        ),
        l(
            "addon_uses_alias",
            "grant_decided_by_user_id",
            "model_aliases subtree audit-only user attribution, no FK constraint",
        ),
        l(
            "addon_uses_model",
            "grant_decided_by_user_id",
            "model_aliases subtree audit-only user attribution, no FK constraint",
        ),
        l(
            "alias_calls",
            "caller_user_id",
            "model_aliases call-log user attribution, no FK constraint",
        ),
        l(
            "model_alias_changes",
            "changed_by_user_id",
            "model_aliases change-log user attribution, no FK constraint",
        ),
        l(
            "model_visibility_changes",
            "changed_by_user_id",
            "model access change-log user attribution, no FK constraint",
        ),
        l(
            "deployments",
            "target_service_id",
            "references services(id), which stays INTEGER",
        ),
        l(
            "service_aliases",
            "target_service_id",
            "references services(id), which stays INTEGER",
        ),
        l(
            "model_registry",
            "service_id",
            "references services(id), which stays INTEGER",
        ),
    ]
}

/// Columns matching the FK naming pattern that are already TEXT but are NOT a
/// core-identity (user/group/flow) FK: free-text attribution, composite-FK user
/// references, or FKs to other TEXT-PK tables. Listed so the guard does not flag
/// them. Each entry documents WHY it is not a core-identity remap target.
#[cfg(test)]
struct IntentionalTextNonIdentity {
    table: &'static str,
    column: &'static str,
    reason: &'static str,
}

#[cfg(test)]
fn intentionally_text_non_identity() -> Vec<IntentionalTextNonIdentity> {
    let t = |table, column, reason| IntentionalTextNonIdentity {
        table,
        column,
        reason,
    };
    vec![
        t(
            "flow_invocations",
            "actor_user_id",
            "TEXT user_accounts(id) FK (enforced by foreign_key_check); legacy rows \
             held stringified-INTEGER session ids, value-remapped in place by v56 \
             (remap_text_int_column) — resolvable ids -> UUID, unknown -> NULL so \
             the enforced FK stays satisfied; not a rebuilt table",
        ),
        t(
            "org_memberships",
            "user_id",
            "already TEXT (CAST(id AS TEXT)); remapped in place by v56, not a rebuilt table",
        ),
        t(
            "org_memberships",
            "granted_by",
            "free-text grantor marker ('system' / admin id), not a user_accounts FK",
        ),
        t(
            "trusted_nodes",
            "approved_by",
            "free-text approver label, no user_accounts FK",
        ),
        t(
            "role_catalog",
            "created_by",
            "free-text creator marker, no user_accounts FK",
        ),
        t(
            "flow_versions",
            "created_by",
            "free-text author marker on the version snapshot, no user_accounts FK",
        ),
        t(
            "legal_documents",
            "generated_by_user_id",
            "composite FK to org_memberships(org_id,user_id); the user_id half is already \
             org-scoped TEXT, remapped via org_memberships, not user_accounts.id directly",
        ),
        t(
            "sync_explicit_shares",
            "subject_id",
            "polymorphic user|node id stored as free TEXT (subject_type discriminator)",
        ),
        t(
            "skills",
            "created_by",
            "creator provenance marker born TEXT in v63 (post-flip, never held an \
             INTEGER id); nullable, no declared user_accounts FK — synced rows may \
             reference an account the receiving node has not materialized yet",
        ),
        t(
            "skill_curator_snapshots",
            "created_by",
            "creator provenance marker born TEXT (post-flip, never held an INTEGER \
             id); nullable, no user_accounts FK. Node-local maintenance audit trail, \
             not synced, so it never needs the identity remap",
        ),
        t(
            "camera_grants",
            "created_by",
            "granting ADDON id (e.g. 'go2'), not a user_accounts FK; born TEXT in \
             v81 (post-flip), never held an INTEGER id",
        ),
        t(
            "api_keys",
            "subject_id",
            "polymorphic user|group id discriminated by key_type (NULL for 'general'), \
             not a single-table FK; born TEXT in v82 (rows wiped on the rebuild, \
             never held an INTEGER id)",
        ),
        t(
            "model_metrics_rollup",
            "user_id",
            "metrics-rollup key born TEXT in v102 (post-flip, never held an INTEGER id); \
             no declared user_accounts FK — synced rollup rows may reference an account \
             a receiving node has not materialized yet (same rationale as token_usage_daily)",
        ),
        t(
            "token_usage_daily",
            "user_id",
            "usage-counter key born TEXT in v86 (post-flip, never held an INTEGER id); \
             no declared user_accounts FK — synced metric rows may reference an account \
             a receiving node has not materialized yet",
        ),
        t(
            "token_quota",
            "subject_id",
            "polymorphic quota subject discriminated by scope_type (NULL for org-wide), \
             not a single-table FK; born TEXT in v86 (post-flip, never held an INTEGER id)",
        ),
        t(
            "agents",
            "flow_id",
            "agent harness flow id (flows.id is TEXT UUID); born TEXT in v64, no declared \
             FK so the agent row can sync ahead of (or independently of) the flow row",
        ),
        t(
            "meeting_sessions",
            "flow_id",
            "meeting pipeline flow id (flows.id is TEXT UUID); born TEXT in v131 \
             (post-flip, never held an INTEGER id), no declared FK so a session row \
             keeps its recorded pipeline even if the flow is later deleted",
        ),
        t(
            "agent_runs",
            "user_id",
            "run principal born TEXT in v64; runtime row, no declared user_accounts FK — \
             the run survives the account's deletion for audit, and a NULL principal is \
             the unattended-call case (§3.3)",
        ),
        t(
            "agent_runs",
            "actor_user_id",
            "provenance stamp born TEXT in v135 (post-flip, never held an INTEGER id); \
             runtime row, no declared user_accounts FK — like the run principal above, \
             the record of who caused a run outlives the account, and NULL is the \
             meaningful 'service API key with no user behind it' case (§2.5)",
        ),
        t(
            "flow_executions",
            "actor_user_id",
            "provenance stamp born TEXT in v135 (post-flip, never held an INTEGER id); \
             runtime log row, no declared user_accounts FK — the execution history must \
             not be cascaded away with an account, and NULL is the 'service API key with \
             no user behind it' case (§2.5)",
        ),
        t(
            "compliance_data_subjects",
            "subject_id",
            "PK of compliance_data_subjects (TEXT UUID), a data-subject, not a platform user",
        ),
        t(
            "compliance_data_subject_links",
            "subject_id",
            "FK to compliance_data_subjects(subject_id), not user_accounts",
        ),
        t(
            "compliance_consent_records",
            "subject_id",
            "FK to compliance_data_subjects(subject_id), not user_accounts",
        ),
        t(
            "compliance_dsar_requests",
            "subject_id",
            "FK to compliance_data_subjects(subject_id), not user_accounts",
        ),
        // Code Studio (v125). Every column below was born TEXT long after the
        // identity flip and carries no declared user_accounts FK ON PURPOSE:
        // the registry is synchronized across the org, so a workspace row can
        // reach a node that has not materialized the referenced account yet —
        // the same rationale as token_usage_daily.
        t(
            "code_workspaces",
            "owner_user_id",
            "workspace owner born TEXT in v125 (post-flip); no declared user_accounts FK \
             because the registry syncs across nodes that may not hold the account yet",
        ),
        t(
            "code_workspace_members",
            "user_id",
            "membership principal born TEXT in v125 (post-flip); no declared FK for the \
             same cross-node reason as the owner column",
        ),
        t(
            "code_workspace_creator_grants",
            "user_id",
            "grantee of the create-workspace right, born TEXT in v125 (post-flip); \
             org-scoped and synchronized, so no declared user_accounts FK",
        ),
        t(
            "code_workspace_creator_grants",
            "granted_by",
            "attribution of who granted the right; kept for audit even after the \
             granting account is deleted, so it is free text rather than an FK",
        ),
        t(
            "code_workspace_allowlist",
            "created_by",
            "attribution of the standing permission; kept for audit after the account \
             is gone, so it is deliberately not an FK",
        ),
        t(
            "code_workspace_secrets",
            "created_by",
            "attribution inside the NODE-LOCAL vault; the vault never syncs, and the \
             record must survive account deletion for audit",
        ),
        t(
            "code_agent_credentials",
            "created_by",
            "attribution inside the NODE-LOCAL vault; same rationale as \
             code_workspace_secrets.created_by",
        ),
    ]
}

/// Identity columns (the rewritten PKs) for the allowlist guard. Their parent
/// PK becomes TEXT; the guard treats them as migrated.
#[cfg(test)]
fn identity_pk_columns() -> &'static [(&'static str, &'static str)] {
    &[
        ("flows", "id"),
        ("flow_model_bindings", "id"),
        ("flow_versions", "id"),
        ("user_accounts", "id"),
        ("user_groups", "id"),
    ]
}

/// Rewrites the five core identity tables to TEXT UUID PKs and remaps every
/// dependent column. Self-managed: owns the `foreign_keys` pragma and the
/// transaction because the pragma flip is a no-op inside an open transaction.
fn core_identity_int_to_uuid(conn: &Connection, version: i64, name: &str) -> Result<()> {
    // A re-run after a successful prior attempt would find the PKs already TEXT;
    // the version guard in `run` prevents that, but stay defensive: if `flows.id`
    // is already TEXT, this DB was migrated — record the version and return.
    if column_is_text(conn, "flows", "id")? {
        conn.execute(
            "INSERT OR IGNORE INTO _migrations (version, name) VALUES (?1, ?2)",
            rusqlite::params![version, name],
        )?;
        return Ok(());
    }

    conn.execute_batch("PRAGMA foreign_keys = OFF;")?;

    let result = (|| -> Result<()> {
        let tx = conn.unchecked_transaction()?;

        // Phase 1: build the old_int -> new_uuid map for each identity table.
        let flows_map = build_id_map(&tx, "flows")?;
        let bindings_map = build_id_map(&tx, "flow_model_bindings")?;
        let versions_map = build_id_map(&tx, "flow_versions")?;
        let users_map = build_id_map(&tx, "user_accounts")?;
        let groups_map = build_id_map(&tx, "user_groups")?;

        let map_for = |parent: IdentityTable| -> &std::collections::HashMap<i64, String> {
            match parent {
                IdentityTable::Flows => &flows_map,
                IdentityTable::UserAccounts => &users_map,
                IdentityTable::UserGroups => &groups_map,
            }
        };

        // Phase 2: rebuild every child column FIRST while parents still hold
        // their old INTEGER ids (so a child value can still be matched against
        // the parent's pre-rewrite id space via the map). For polymorphic
        // subject_id columns the remap is conditioned on subject_type.
        for remap in child_remaps() {
            if !table_exists(&tx, remap.table)? {
                continue;
            }
            remap_child_column(&tx, &remap, map_for(remap.parent))?;
        }

        // Phase 3: rewrite each identity table's own PK to TEXT.
        rebuild_flows(&tx, &flows_map)?;
        rebuild_flow_model_bindings(&tx, &bindings_map)?;
        rebuild_flow_versions(&tx, &versions_map)?;
        rebuild_user_accounts(&tx, &users_map)?;
        rebuild_user_groups(&tx, &groups_map)?;

        // org_memberships.user_id is already TEXT (backfilled as CAST(id AS TEXT)
        // by v32/v38). Remap those textual integer values through the same map
        // so memberships keep pointing at the right user.
        if table_exists(&tx, "org_memberships")? {
            remap_text_int_column(
                &tx,
                "org_memberships",
                "user_id",
                &users_map,
                UnresolvedTextInt::Keep,
            )?;
        }

        // flow_invocations.actor_user_id is declared TEXT REFERENCES
        // user_accounts(id), and that FK is enforced by the Phase 4
        // `foreign_key_check` gate. Legacy rows stored the session user id as a
        // stringified INTEGER (the pre-UUID account id). It is a runtime log, so
        // an actor whose account was deleted before the migration cannot abort
        // it: resolvable ids are rewritten to the new UUID, and an id missing
        // from the user map is set to NULL (the column is nullable). Dropping the
        // attribution of an already-deleted actor in a log keeps the enforced FK
        // satisfied — the alternative (a dangling stringified-int) would make
        // foreign_key_check fail.
        if table_exists(&tx, "flow_invocations")? {
            remap_text_int_column(
                &tx,
                "flow_invocations",
                "actor_user_id",
                &users_map,
                UnresolvedTextInt::SetNull,
            )?;
        }

        // On an upgraded DB a value-remapped child column keeps its declared
        // INTEGER affinity even though it now stores UUID text. We deliberately
        // do NOT rebuild it to TEXT: SQLite has dynamic typing, and an INTEGER
        // affinity column stores a UUID string (hex with hyphens) verbatim — the
        // affinity rule only coerces strings that are valid integer literals, and
        // a UUID never is. Rebuilding the table to flip the declared type was
        // over-engineering that risked dropping CHECK / UNIQUE constraints and
        // indexes. The remapped values are correct and FKs resolve, so the
        // INTEGER-affinity-holding-UUID column is left in place. A fresh install
        // declares these columns TEXT (INITIAL_SCHEMA); the resulting asymmetry
        // (fresh=TEXT, upgraded=INTEGER-affinity-holding-UUID) is intentional and
        // safe because affinity does not change the stored bytes.

        // Drop AUTOINCREMENT bookkeeping for the rebuilt tables.
        if table_exists(&tx, "sqlite_sequence")? {
            tx.execute(
                "DELETE FROM sqlite_sequence WHERE name IN \
                 ('flows','flow_model_bindings','flow_versions','user_accounts','user_groups')",
                [],
            )?;
        }

        // Phase 4: referential integrity gate. `foreign_key_check` catches every
        // declared FK; untyped attribution columns (no REFERENCES clause) are
        // scanned manually for orphans.
        let fk_violations = foreign_key_check(&tx)?;
        if !fk_violations.is_empty() {
            anyhow::bail!(
                "core_identity_int_to_uuid: foreign_key_check found {} violation(s): {}",
                fk_violations.len(),
                fk_violations.join("; ")
            );
        }
        scan_untyped_orphans(&tx, &users_map, &groups_map, &flows_map)?;

        tx.execute(
            "INSERT INTO _migrations (version, name) VALUES (?1, ?2)",
            rusqlite::params![version, name],
        )?;
        tx.commit()?;
        Ok(())
    })();

    match result {
        Ok(()) => {
            conn.execute_batch("PRAGMA foreign_keys = ON;")?;
            Ok(())
        }
        Err(e) => {
            // Leave a recovery marker. The transaction already rolled back on
            // drop; a later recovery step can detect the half state.
            let _ = conn.execute(
                "INSERT OR REPLACE INTO settings (key, value) VALUES \
                 ('migration_phase', ?1)",
                rusqlite::params![format!("core_identity_int_to_uuid:failed:{e}")],
            );
            conn.execute_batch("PRAGMA foreign_keys = ON;")?;
            Err(e)
        }
    }
}

// =============================================================================
// v65 — poszerzenie CHECK scope_kind o 'agent_runs' (Harness §3.3)
// =============================================================================
//
// `compliance_retention_policies.scope_kind` ma zaszyty CHECK z listy 7 zakresow
// (v50). `agent_runs.run_log` bywa danymi osobowymi (wyniki narzedzi CRM/memory),
// wiec dostaje wlasny zakres retencji. Dodanie nowej wartosci do CHECK wymaga
// przebudowy tabeli — `compliance_data_subject_retention` ma do niej FK, wiec
// rebuild idzie z `foreign_keys=OFF` POZA transakcja (jak rebuildy v56), z
// `foreign_key_check` przed commitem. Domyslna polityka 30 dni dla zakresu
// 'agent_runs' jest seedowana przez `compliance::seed_org_compliance_defaults`
// (idempotentnie, na kazdym starcie, ze stalym `retention_policy_id`). Sam job
// czyszczacy (purge `run_log`/`prompt`/`result` po terminie) to faza 6/7.
fn widen_retention_scope_for_agent_runs(conn: &Connection, version: i64, name: &str) -> Result<()> {
    // Defensive idempotency: a re-run after success would find the widened CHECK
    // already in place. Detect it by attempting a no-op insert of the new value
    // into a probe and rolling back; simpler: read the table SQL and short-circuit.
    let already_widened: bool = conn
        .query_row(
            "SELECT instr(sql, 'agent_runs') > 0 FROM sqlite_master \
             WHERE type = 'table' AND name = 'compliance_retention_policies'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .map(|v| v != 0)
        .unwrap_or(false);
    if already_widened {
        conn.execute(
            "INSERT OR IGNORE INTO _migrations (version, name) VALUES (?1, ?2)",
            rusqlite::params![version, name],
        )?;
        return Ok(());
    }

    conn.execute_batch("PRAGMA foreign_keys = OFF;")?;

    let result = (|| -> Result<()> {
        let tx = conn.unchecked_transaction()?;

        // Rebuild the table with the widened CHECK, preserving every other column
        // definition, constraint, default, trigger and index exactly as v50.
        tx.execute_batch(
            "
            CREATE TABLE compliance_retention_policies_new (
                retention_policy_id TEXT PRIMARY KEY,
                org_id TEXT NOT NULL REFERENCES organizations(org_id) ON DELETE CASCADE,
                slug TEXT NOT NULL,
                name_translations TEXT NOT NULL DEFAULT '{}',
                scope_kind TEXT NOT NULL CHECK(scope_kind IN \
                    ('audit','ai_audit','data_category','document','dsar','breach','general','agent_runs')),
                category_id TEXT NULL REFERENCES compliance_data_categories(category_id) ON DELETE SET NULL,
                retention_days INTEGER NOT NULL,
                minimum_days INTEGER NOT NULL DEFAULT 0,
                action_after_retention TEXT NOT NULL DEFAULT 'delete'
                    CHECK(action_after_retention IN ('delete','anonymize','archive')),
                is_default INTEGER NOT NULL DEFAULT 0 CHECK(is_default IN (0,1)),
                is_active INTEGER NOT NULL DEFAULT 1 CHECK(is_active IN (0,1)),
                created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
                updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
                CHECK(retention_days >= minimum_days),
                UNIQUE(org_id, slug),
                CHECK(json_valid(name_translations))
            );
            INSERT INTO compliance_retention_policies_new
                SELECT retention_policy_id, org_id, slug, name_translations, scope_kind, \
                       category_id, retention_days, minimum_days, action_after_retention, \
                       is_default, is_active, created_at, updated_at \
                FROM compliance_retention_policies;
            DROP TABLE compliance_retention_policies;
            ALTER TABLE compliance_retention_policies_new RENAME TO compliance_retention_policies;
            CREATE INDEX IF NOT EXISTS idx_compliance_retention_policies_lookup
                ON compliance_retention_policies(org_id, scope_kind, category_id, is_active);
            CREATE UNIQUE INDEX IF NOT EXISTS idx_compliance_retention_one_default
                ON compliance_retention_policies(org_id, scope_kind)
                WHERE is_default = 1 AND category_id IS NULL AND is_active = 1;
            CREATE TRIGGER IF NOT EXISTS compliance_retention_policies_updated_at
            AFTER UPDATE ON compliance_retention_policies
            FOR EACH ROW
            BEGIN
                UPDATE compliance_retention_policies
                SET updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
                WHERE retention_policy_id = NEW.retention_policy_id;
            END;
            ",
        )?;

        // Seed the default 30-day agent_runs policy for every existing org. Fresh
        // installs get this via `ensure_org_defaults` at migration v50; upgrade
        // DBs already ran v50 (without this entry) and never re-run it, so this
        // migration is the one chance to backfill the policy on upgraded fleets.
        // The retention_policy_id matches `default_record_id` (bare base_id for the
        // default org, '{org}:{base}' otherwise) so the row stays stable and an
        // org-creation re-seed is an INSERT OR IGNORE no-op.
        tx.execute(
            "INSERT OR IGNORE INTO compliance_retention_policies \
                (retention_policy_id, org_id, slug, name_translations, scope_kind, category_id, \
                 retention_days, minimum_days, action_after_retention, is_default, is_active) \
             SELECT CASE WHEN org_id = ?1 THEN ?2 ELSE printf('%s:%s', org_id, ?2) END, \
                    org_id, 'agent_runs_default', \
                    json_object('pl','Przebiegi agentów', 'en','Agent runs'), \
                    'agent_runs', NULL, 30, 0, 'delete', 1, 1 \
             FROM organizations WHERE status <> 'deleted'",
            rusqlite::params![
                crate::services::org::DEFAULT_ORG_ID,
                "ret-core-agent-runs-default",
            ],
        )?;

        let fk_violations = foreign_key_check(&tx)?;
        if !fk_violations.is_empty() {
            anyhow::bail!(
                "widen_retention_scope_for_agent_runs: foreign_key_check found {} violation(s): {}",
                fk_violations.len(),
                fk_violations.join("; ")
            );
        }

        tx.execute(
            "INSERT INTO _migrations (version, name) VALUES (?1, ?2)",
            rusqlite::params![version, name],
        )?;
        tx.commit()?;
        Ok(())
    })();

    conn.execute_batch("PRAGMA foreign_keys = ON;")?;
    result
}

// =============================================================================
// v82 — rebuild api_keys (HMAC verifier + key_type/subject + stable uid) and
// widen resource_permissions.subject_type to allow 'api_key'. Self-managed
// because the table rebuild needs `PRAGMA foreign_keys=OFF` outside a
// transaction. All legacy api_keys rows are dropped (decision: never used).
// =============================================================================
fn api_keys_access_v2(conn: &Connection, version: i64, name: &str) -> Result<()> {
    // Defensive idempotency: detect the new schema by the presence of the
    // `key_verifier` column on api_keys; if it is already there this DB ran the
    // migration (version guard in `run` normally prevents a re-run anyway).
    let already_done: bool = conn
        .query_row(
            "SELECT instr(sql, 'key_verifier') > 0 FROM sqlite_master \
             WHERE type = 'table' AND name = 'api_keys'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .map(|v| v != 0)
        .unwrap_or(false);
    if already_done {
        conn.execute(
            "INSERT OR IGNORE INTO _migrations (version, name) VALUES (?1, ?2)",
            rusqlite::params![version, name],
        )?;
        return Ok(());
    }

    conn.execute_batch("PRAGMA foreign_keys = OFF;")?;

    let result = (|| -> Result<()> {
        let tx = conn.unchecked_transaction()?;

        // api_keys: drop every legacy row (never used in production) and rebuild
        // with the new column set. `key_hash` and `owner_user_id` are gone;
        // `key_verifier` (HMAC-SHA256), `uid`, `key_type` and `subject_id`
        // replace them.
        tx.execute_batch(
            "
            DELETE FROM api_keys;
            DROP INDEX IF EXISTS idx_api_keys_prefix;
            DROP INDEX IF EXISTS idx_apikeys_owner;
            CREATE TABLE api_keys_new (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                uid TEXT NOT NULL UNIQUE,
                key_verifier TEXT NOT NULL UNIQUE,
                key_prefix TEXT NOT NULL,
                name TEXT NOT NULL,
                key_type TEXT NOT NULL DEFAULT 'user'
                    CHECK(key_type IN ('user','group','general')),
                subject_id TEXT NULL,
                rate_limit_rps INTEGER NOT NULL DEFAULT 100,
                is_active INTEGER NOT NULL DEFAULT 1,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                last_used_at TEXT
            );
            DROP TABLE api_keys;
            ALTER TABLE api_keys_new RENAME TO api_keys;
            CREATE INDEX idx_api_keys_prefix ON api_keys(key_prefix);
            CREATE INDEX idx_api_keys_subject ON api_keys(key_type, subject_id);
            ",
        )?;

        // resource_permissions: widen subject_type CHECK to include 'api_key',
        // preserving existing rows and all other columns/constraints/indexes.
        // The base schema had no CHECK on subject_type, so a row with a value
        // outside the new allowlist would fail the INSERT...SELECT rebuild and
        // lose data silently. Preflight first and bail with a readable error.
        let bad_subject_types: Vec<String> = {
            let mut stmt = tx.prepare(
                "SELECT DISTINCT subject_type FROM resource_permissions \
                 WHERE subject_type NOT IN ('user','group','api_key')",
            )?;
            let rows = stmt
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        };
        if !bad_subject_types.is_empty() {
            anyhow::bail!(
                "api_keys_access_v2: resource_permissions has rows with subject_type \
                 outside ('user','group','api_key'): {}. Resolve these rows before \
                 migrating; refusing to drop them.",
                bad_subject_types.join(", ")
            );
        }

        tx.execute_batch(
            "
            DROP INDEX IF EXISTS idx_resperm_subject;
            DROP INDEX IF EXISTS idx_resperm_resource;
            CREATE TABLE resource_permissions_new (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                resource_type TEXT NOT NULL,
                resource_id TEXT NOT NULL,
                subject_type TEXT NOT NULL
                    CHECK(subject_type IN ('user','group','api_key')),
                subject_id TEXT NOT NULL,
                access_level TEXT NOT NULL CHECK(access_level IN ('allow','deny')),
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                UNIQUE(resource_type, resource_id, subject_type, subject_id)
            );
            INSERT INTO resource_permissions_new
                SELECT id, resource_type, resource_id, subject_type, subject_id, \
                       access_level, created_at
                FROM resource_permissions;
            DROP TABLE resource_permissions;
            ALTER TABLE resource_permissions_new RENAME TO resource_permissions;
            CREATE INDEX idx_resperm_subject ON resource_permissions(subject_type, subject_id);
            CREATE INDEX idx_resperm_resource ON resource_permissions(resource_type, resource_id);
            ",
        )?;

        let fk_violations = foreign_key_check(&tx)?;
        if !fk_violations.is_empty() {
            anyhow::bail!(
                "api_keys_access_v2: foreign_key_check found {} violation(s): {}",
                fk_violations.len(),
                fk_violations.join("; ")
            );
        }

        tx.execute(
            "INSERT INTO _migrations (version, name) VALUES (?1, ?2)",
            rusqlite::params![version, name],
        )?;
        tx.commit()?;
        Ok(())
    })();

    conn.execute_batch("PRAGMA foreign_keys = ON;")?;
    result
}

// =============================================================================
// v58 — naprawa admina zaseedowanego z nie-UUID id ('1')
// =============================================================================
//
// Seed domyslnego admina historycznie wstawial literal id '1' do user_accounts.id
// (TEXT, ma trzymac UUID). Login pakuje id do 16-bajtowej formy wire i odrzuca
// wszystko co nie jest UUID-em ("user id is not a valid UUID"). Remapujemy stray
// '1' na staly UUID admina, kaskadujac kazda kolumne-dziecko user_accounts.
// '1' nie jest poprawnym UUID, wiec WHERE col = '1' trafia wylacznie w zepsute
// referencje admina (grupy/node'y uzywaja UUID).
const REPAIRED_ADMIN_ID: &str = "00000000-0000-4000-8000-000000000002";

/// Canonical id of the seeded "Default Chat" flow. Must match
/// `crate::db::seed::DEFAULT_CHAT_FLOW_ID` (kept as a sibling literal, like
/// `REPAIRED_ADMIN_ID` mirrors `DEFAULT_ADMIN_ID`).
const REPAIRED_DEFAULT_FLOW_ID: &str = "00000000-0000-4000-8000-000000000010";

fn repair_admin_non_uuid_id(conn: &Connection, version: i64, name: &str) -> Result<()> {
    let needs_repair: bool = conn.query_row(
        "SELECT COUNT(*) > 0 FROM user_accounts WHERE id = '1'",
        [],
        |row| row.get(0),
    )?;
    if !needs_repair {
        conn.execute(
            "INSERT OR IGNORE INTO _migrations (version, name) VALUES (?1, ?2)",
            rusqlite::params![version, name],
        )?;
        return Ok(());
    }

    conn.execute_batch("PRAGMA foreign_keys = OFF;")?;

    let result = (|| -> Result<()> {
        let tx = conn.unchecked_transaction()?;

        // Najpierw kolumny-dzieci (dopoki rodzic dalej trzyma '1'), potem PK rodzica.
        for remap in child_remaps() {
            if remap.parent != IdentityTable::UserAccounts {
                continue;
            }
            if !table_exists(&tx, remap.table)? {
                continue;
            }
            // A later table rebuild may have dropped a still-listed child column
            // (e.g. v82 rebuilt `api_keys` without `owner_user_id`). Nothing to
            // remap then — skip rather than fail on a missing column.
            if !column_exists(&tx, remap.table, remap.column)? {
                continue;
            }
            tx.execute(
                &format!(
                    "UPDATE {} SET {} = ?1 WHERE {} = '1'",
                    remap.table, remap.column, remap.column
                ),
                rusqlite::params![REPAIRED_ADMIN_ID],
            )?;
        }

        tx.execute(
            "UPDATE user_accounts SET id = ?1 WHERE id = '1'",
            rusqlite::params![REPAIRED_ADMIN_ID],
        )?;

        let fk_violations = foreign_key_check(&tx)?;
        if !fk_violations.is_empty() {
            anyhow::bail!(
                "repair_admin_non_uuid_id: foreign_key_check found {} violation(s): {}",
                fk_violations.len(),
                fk_violations.join("; ")
            );
        }

        tx.execute(
            "INSERT INTO _migrations (version, name) VALUES (?1, ?2)",
            rusqlite::params![version, name],
        )?;
        tx.commit()?;
        Ok(())
    })();

    match result {
        Ok(()) => {
            conn.execute_batch("PRAGMA foreign_keys = ON;")?;
            Ok(())
        }
        Err(e) => {
            let _ = conn.execute(
                "INSERT OR REPLACE INTO settings (key, value) VALUES ('migration_phase', ?1)",
                rusqlite::params![format!("repair_admin_non_uuid_id:failed:{e}")],
            );
            conn.execute_batch("PRAGMA foreign_keys = ON;")?;
            Err(e)
        }
    }
}

/// Fresh DBs historically seeded "Default Chat" with a random `Uuid::new_v4()`,
/// so the same logical flow held a different id on every node. Core sync targets
/// rows by id (`UPDATE flows ... WHERE id = ?`), so editing the flow on one node
/// produced ops the others could not apply ("target row not found") and the flow
/// never converged. Re-key the existing default flow to the shared canonical id
/// (the value fresh seeds now use), remapping every child FK through the same
/// `child_remaps()` closure the v56 identity flip uses. Once every node has run
/// this, the default flow shares one id across the mesh and edits sync; LWW then
/// converges content. Mirrors `repair_admin_non_uuid_id` (FK pragma toggled
/// outside the transaction, so it must be `RustSelfManaged`).
fn repair_default_flow_random_id(conn: &Connection, version: i64, name: &str) -> Result<()> {
    let needs_repair: bool = conn.query_row(
        "SELECT COUNT(*) > 0 FROM flows WHERE name = 'Default Chat' AND id != ?1",
        rusqlite::params![REPAIRED_DEFAULT_FLOW_ID],
        |row| row.get(0),
    )?;
    if !needs_repair {
        conn.execute(
            "INSERT OR IGNORE INTO _migrations (version, name) VALUES (?1, ?2)",
            rusqlite::params![version, name],
        )?;
        return Ok(());
    }

    let old_id: String = conn.query_row(
        "SELECT id FROM flows WHERE name = 'Default Chat' AND id != ?1 LIMIT 1",
        rusqlite::params![REPAIRED_DEFAULT_FLOW_ID],
        |row| row.get(0),
    )?;

    // A row already on the canonical id alongside a legacy one would mean two
    // distinct "Default Chat" flows. Re-keying would collide on the PK; merging
    // them is out of scope. Fail loudly rather than silently fuse two flows.
    let canonical_exists: bool = conn.query_row(
        "SELECT COUNT(*) > 0 FROM flows WHERE id = ?1",
        rusqlite::params![REPAIRED_DEFAULT_FLOW_ID],
        |row| row.get(0),
    )?;
    if canonical_exists {
        anyhow::bail!(
            "repair_default_flow_random_id: legacy '{old_id}' and the canonical default \
             flow both exist; manual reconciliation needed"
        );
    }

    conn.execute_batch("PRAGMA foreign_keys = OFF;")?;

    let result = (|| -> Result<()> {
        let tx = conn.unchecked_transaction()?;

        // Children first (while the parent still holds the old id), then the PK.
        for remap in child_remaps() {
            if remap.parent != IdentityTable::Flows {
                continue;
            }
            if !table_exists(&tx, remap.table)? {
                continue;
            }
            // Skip a still-listed child column dropped by a later table rebuild.
            if !column_exists(&tx, remap.table, remap.column)? {
                continue;
            }
            tx.execute(
                &format!(
                    "UPDATE {} SET {} = ?1 WHERE {} = ?2",
                    remap.table, remap.column, remap.column
                ),
                rusqlite::params![REPAIRED_DEFAULT_FLOW_ID, old_id],
            )?;
        }

        tx.execute(
            "UPDATE flows SET id = ?1 WHERE id = ?2",
            rusqlite::params![REPAIRED_DEFAULT_FLOW_ID, old_id],
        )?;

        let fk_violations = foreign_key_check(&tx)?;
        if !fk_violations.is_empty() {
            anyhow::bail!(
                "repair_default_flow_random_id: foreign_key_check found {} violation(s): {}",
                fk_violations.len(),
                fk_violations.join("; ")
            );
        }

        tx.execute(
            "INSERT INTO _migrations (version, name) VALUES (?1, ?2)",
            rusqlite::params![version, name],
        )?;
        tx.commit()?;
        Ok(())
    })();

    match result {
        Ok(()) => {
            conn.execute_batch("PRAGMA foreign_keys = ON;")?;
            Ok(())
        }
        Err(e) => {
            let _ = conn.execute(
                "INSERT OR REPLACE INTO settings (key, value) VALUES ('migration_phase', ?1)",
                rusqlite::params![format!("repair_default_flow_random_id:failed:{e}")],
            );
            conn.execute_batch("PRAGMA foreign_keys = ON;")?;
            Err(e)
        }
    }
}

/// True when `table` has a column named `column` (per PRAGMA table_info).
fn column_exists(conn: &Connection, table: &str, column: &str) -> Result<bool> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let name: String = row.get(1)?;
        if name == column {
            return Ok(true);
        }
    }
    Ok(false)
}

/// True when `table.column` has declared type affinity TEXT (per PRAGMA).
fn column_is_text(conn: &Connection, table: &str, column: &str) -> Result<bool> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let name: String = row.get(1)?;
        if name == column {
            let ty: String = row.get(2)?;
            return Ok(ty.eq_ignore_ascii_case("TEXT"));
        }
    }
    Ok(false)
}

fn table_exists(conn: &Connection, table: &str) -> Result<bool> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
        rusqlite::params![table],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

/// Builds the old INTEGER id -> new UUIDv4 map for one identity table.
fn build_id_map(conn: &Connection, table: &str) -> Result<std::collections::HashMap<i64, String>> {
    let mut map = std::collections::HashMap::new();
    let mut stmt = conn.prepare(&format!("SELECT id FROM {table}"))?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let old_id: i64 = row.get(0)?;
        map.insert(old_id, uuid::Uuid::new_v4().to_string());
    }
    Ok(map)
}

/// Rewrites a child INTEGER FK column to the parent's new UUID. Rows whose
/// value is NULL stay NULL; rows whose value is missing from the map are
/// orphans and abort the migration (they would dangle after the PK flip).
/// `addon_permissions.subject_id` / `resource_permissions.subject_id` are
/// polymorphic: only rows with the matching `subject_type` are rewritten here,
/// the call site enqueues one remap per subject kind.
fn remap_child_column(
    conn: &Connection,
    remap: &ChildRemap,
    map: &std::collections::HashMap<i64, String>,
) -> Result<()> {
    let polymorphic = remap.column == "subject_id"
        && matches!(remap.table, "addon_permissions" | "resource_permissions");

    let select_sql = if polymorphic {
        let want = match remap.parent {
            IdentityTable::UserGroups => "group",
            _ => "user",
        };
        format!(
            "SELECT rowid, {col} FROM {tbl} \
             WHERE {col} IS NOT NULL AND subject_type = '{want}'",
            col = remap.column,
            tbl = remap.table,
        )
    } else {
        format!(
            "SELECT rowid, {col} FROM {tbl} WHERE {col} IS NOT NULL",
            col = remap.column,
            tbl = remap.table,
        )
    };

    let mut to_update: Vec<(i64, String)> = Vec::new();
    {
        let mut stmt = conn.prepare(&select_sql)?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let rowid: i64 = row.get(0)?;
            // Value may already be TEXT (e.g. polymorphic group rows pointing at
            // the legacy textual group id). Read as i64 first; fall back to TEXT
            // parse so a partially-textual column does not abort.
            let old_int: Option<i64> = match row.get::<_, i64>(1) {
                Ok(v) => Some(v),
                Err(_) => row
                    .get::<_, String>(1)
                    .ok()
                    .and_then(|s| s.parse::<i64>().ok()),
            };
            let Some(old_int) = old_int else {
                continue;
            };
            let Some(new_uuid) = map.get(&old_int) else {
                anyhow::bail!(
                    "core_identity_int_to_uuid: orphan {}.{} = {} has no parent in {}",
                    remap.table,
                    remap.column,
                    old_int,
                    remap.parent.table_name()
                );
            };
            to_update.push((rowid, new_uuid.clone()));
        }
    }

    let update_sql = format!(
        "UPDATE {tbl} SET {col} = ?1 WHERE rowid = ?2",
        tbl = remap.table,
        col = remap.column
    );
    for (rowid, new_uuid) in to_update {
        conn.execute(&update_sql, rusqlite::params![new_uuid, rowid])?;
    }
    Ok(())
}

/// What to do with a legacy stringified-INTEGER value that has no entry in the
/// id map (its parent row was deleted before the migration).
#[derive(Clone, Copy)]
enum UnresolvedTextInt {
    /// Leave the value as-is. Used for columns without an enforced FK, where a
    /// stale id is an audit artifact rather than corruption (`org_memberships`).
    Keep,
    /// Set the value to NULL. Required for columns with a `REFERENCES` clause:
    /// `foreign_key_check` would otherwise abort the migration on the dangling
    /// id. The column must be nullable (`flow_invocations.actor_user_id`).
    SetNull,
}

/// Remaps a column that already stores the old integer id as TEXT
/// (`CAST(id AS TEXT)`), e.g. `org_memberships.user_id`. Values that are already
/// UUIDs are left untouched (idempotent). Legacy integer values missing from the
/// map are handled per `unresolved`.
fn remap_text_int_column(
    conn: &Connection,
    table: &str,
    column: &str,
    map: &std::collections::HashMap<i64, String>,
    unresolved: UnresolvedTextInt,
) -> Result<()> {
    let mut to_set_uuid: Vec<(i64, String)> = Vec::new();
    let mut to_null: Vec<i64> = Vec::new();
    {
        let mut stmt = conn.prepare(&format!(
            "SELECT rowid, {column} FROM {table} WHERE {column} IS NOT NULL"
        ))?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let rowid: i64 = row.get(0)?;
            let cur: String = row.get(1)?;
            // Only legacy integers are candidates; UUIDs stay untouched.
            if let Ok(old_int) = cur.parse::<i64>() {
                match map.get(&old_int) {
                    Some(new_uuid) => to_set_uuid.push((rowid, new_uuid.clone())),
                    None => {
                        if matches!(unresolved, UnresolvedTextInt::SetNull) {
                            to_null.push(rowid);
                        }
                    }
                }
            }
        }
    }
    for (rowid, new_uuid) in to_set_uuid {
        conn.execute(
            &format!("UPDATE {table} SET {column} = ?1 WHERE rowid = ?2"),
            rusqlite::params![new_uuid, rowid],
        )?;
    }
    for rowid in to_null {
        conn.execute(
            &format!("UPDATE {table} SET {column} = NULL WHERE rowid = ?1"),
            rusqlite::params![rowid],
        )?;
    }
    Ok(())
}

/// Returns one human-readable line per `foreign_key_check` violation.
fn foreign_key_check(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn.prepare("PRAGMA foreign_key_check")?;
    let mut rows = stmt.query([])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        let table: String = row.get(0)?;
        let rowid: Option<i64> = row.get(1).ok();
        let parent: String = row.get(2)?;
        out.push(format!("{table} rowid={rowid:?} -> {parent}",));
    }
    Ok(out)
}

/// True when `table.column` is the child side of a declared foreign key, i.e.
/// `foreign_key_check` already validates it. Such columns are skipped by the
/// untyped-orphan scan to avoid duplicating the engine's own check.
fn column_has_declared_fk(conn: &Connection, table: &str, column: &str) -> Result<bool> {
    let mut stmt = conn.prepare(&format!("PRAGMA foreign_key_list({table})"))?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        // column index 3 of foreign_key_list is the child ("from") column.
        let from: String = row.get(3)?;
        if from.eq_ignore_ascii_case(column) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Scans every remapped column that carries NO declared foreign key for values
/// that no longer resolve to a parent UUID after the flip. `foreign_key_check`
/// already validates the FK-constrained children, so this covers exactly the
/// gap it leaves: untyped attribution columns (e.g. `audit_log.user_id`). A
/// dangling value here is still a data-integrity bug, so it aborts the
/// migration. Polymorphic `subject_id` rows are validated against the parent
/// selected by their `subject_type` discriminator.
fn scan_untyped_orphans(
    conn: &Connection,
    users_map: &std::collections::HashMap<i64, String>,
    groups_map: &std::collections::HashMap<i64, String>,
    flows_map: &std::collections::HashMap<i64, String>,
) -> Result<()> {
    let user_uuids: std::collections::HashSet<&String> = users_map.values().collect();
    let group_uuids: std::collections::HashSet<&String> = groups_map.values().collect();
    let flow_uuids: std::collections::HashSet<&String> = flows_map.values().collect();

    for remap in child_remaps() {
        if !table_exists(conn, remap.table)? {
            continue;
        }
        // Skip a still-listed child column dropped by a later table rebuild.
        if !column_exists(conn, remap.table, remap.column)? {
            continue;
        }
        // Skip children the engine already checks via their REFERENCES clause.
        if column_has_declared_fk(conn, remap.table, remap.column)? {
            continue;
        }

        let valid: &std::collections::HashSet<&String> = match remap.parent {
            IdentityTable::UserAccounts => &user_uuids,
            IdentityTable::UserGroups => &group_uuids,
            IdentityTable::Flows => &flow_uuids,
        };

        let polymorphic = remap.column == "subject_id"
            && matches!(remap.table, "addon_permissions" | "resource_permissions");
        let select_sql = if polymorphic {
            let want = match remap.parent {
                IdentityTable::UserGroups => "group",
                _ => "user",
            };
            format!(
                "SELECT {col} FROM {tbl} WHERE {col} IS NOT NULL AND subject_type = '{want}'",
                col = remap.column,
                tbl = remap.table,
            )
        } else {
            format!(
                "SELECT {col} FROM {tbl} WHERE {col} IS NOT NULL",
                col = remap.column,
                tbl = remap.table,
            )
        };

        let mut stmt = conn.prepare(&select_sql)?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let value: String = row.get(0)?;
            if !valid.contains(&value) {
                anyhow::bail!(
                    "core_identity_int_to_uuid: {}.{}={} is orphaned after remap",
                    remap.table,
                    remap.column,
                    value
                );
            }
        }
    }
    Ok(())
}

fn rebuild_flows(conn: &Connection, map: &std::collections::HashMap<i64, String>) -> Result<()> {
    conn.execute_batch(
        r#"
        DROP TABLE IF EXISTS flows_uuid_new;
        CREATE TABLE flows_uuid_new (
            id TEXT PRIMARY KEY NOT NULL,
            name TEXT NOT NULL,
            description TEXT,
            version INTEGER DEFAULT 1,
            is_default INTEGER NOT NULL DEFAULT 0,
            service_type TEXT,
            flow_json TEXT NOT NULL,
            status TEXT NOT NULL DEFAULT 'draft' CHECK(status IN ('draft','active','decoded')),
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now')),
            published_model_name TEXT NULL
        );
        "#,
    )?;
    {
        let mut sel = conn.prepare(
            "SELECT id, name, description, version, is_default, service_type, flow_json, \
             status, created_at, updated_at, published_model_name FROM flows",
        )?;
        let mut rows = sel.query([])?;
        while let Some(row) = rows.next()? {
            let old_id: i64 = row.get(0)?;
            let new_id = map.get(&old_id).expect("flows id in map");
            conn.execute(
                "INSERT INTO flows_uuid_new (id, name, description, version, is_default, \
                 service_type, flow_json, status, created_at, updated_at, published_model_name) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
                rusqlite::params![
                    new_id,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, String>(9)?,
                    row.get::<_, Option<String>>(10)?,
                ],
            )?;
        }
    }
    conn.execute_batch(
        r#"
        DROP TABLE flows;
        ALTER TABLE flows_uuid_new RENAME TO flows;
        CREATE INDEX idx_flows_status ON flows(status);
        CREATE INDEX idx_flows_service_type ON flows(service_type);
        CREATE INDEX idx_flows_default_lookup ON flows(is_default, service_type, status);
        CREATE UNIQUE INDEX idx_flows_published_model_name
            ON flows(published_model_name)
            WHERE published_model_name IS NOT NULL;
        "#,
    )?;
    Ok(())
}

fn rebuild_flow_model_bindings(
    conn: &Connection,
    map: &std::collections::HashMap<i64, String>,
) -> Result<()> {
    conn.execute_batch(
        r#"
        DROP TABLE IF EXISTS flow_model_bindings_uuid_new;
        CREATE TABLE flow_model_bindings_uuid_new (
            id TEXT PRIMARY KEY NOT NULL,
            flow_id TEXT NOT NULL REFERENCES flows(id) ON DELETE CASCADE,
            model_pattern TEXT NOT NULL UNIQUE,
            priority INTEGER DEFAULT 0
        );
        "#,
    )?;
    {
        let mut sel =
            conn.prepare("SELECT id, flow_id, model_pattern, priority FROM flow_model_bindings")?;
        let mut rows = sel.query([])?;
        while let Some(row) = rows.next()? {
            let old_id: i64 = row.get(0)?;
            let new_id = map.get(&old_id).expect("binding id in map");
            // flow_id was already rewritten to TEXT UUID in Phase 2.
            let flow_id: String = row.get(1)?;
            conn.execute(
                "INSERT INTO flow_model_bindings_uuid_new (id, flow_id, model_pattern, priority) \
                 VALUES (?1,?2,?3,?4)",
                rusqlite::params![
                    new_id,
                    flow_id,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                ],
            )?;
        }
    }
    conn.execute_batch(
        r#"
        DROP TABLE flow_model_bindings;
        ALTER TABLE flow_model_bindings_uuid_new RENAME TO flow_model_bindings;
        CREATE INDEX idx_flow_model_bindings_flow ON flow_model_bindings(flow_id);
        CREATE INDEX idx_flow_model_bindings_priority ON flow_model_bindings(flow_id, priority);
        "#,
    )?;
    Ok(())
}

fn rebuild_flow_versions(
    conn: &Connection,
    map: &std::collections::HashMap<i64, String>,
) -> Result<()> {
    conn.execute_batch(
        r#"
        DROP TABLE IF EXISTS flow_versions_uuid_new;
        CREATE TABLE flow_versions_uuid_new (
            id TEXT PRIMARY KEY NOT NULL,
            flow_id TEXT NOT NULL REFERENCES flows(id) ON DELETE CASCADE,
            version_num INTEGER NOT NULL,
            flow_json TEXT NOT NULL,
            name TEXT NOT NULL,
            description TEXT,
            status TEXT,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            created_by TEXT,
            UNIQUE(flow_id, version_num)
        );
        "#,
    )?;
    {
        let mut sel = conn.prepare(
            "SELECT id, flow_id, version_num, flow_json, name, description, status, \
             created_at, created_by FROM flow_versions",
        )?;
        let mut rows = sel.query([])?;
        while let Some(row) = rows.next()? {
            let old_id: i64 = row.get(0)?;
            let new_id = map.get(&old_id).expect("flow_version id in map");
            let flow_id: String = row.get(1)?;
            conn.execute(
                "INSERT INTO flow_versions_uuid_new (id, flow_id, version_num, flow_json, name, \
                 description, status, created_at, created_by) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
                rusqlite::params![
                    new_id,
                    flow_id,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, Option<String>>(8)?,
                ],
            )?;
        }
    }
    conn.execute_batch(
        r#"
        DROP TABLE flow_versions;
        ALTER TABLE flow_versions_uuid_new RENAME TO flow_versions;
        CREATE INDEX idx_flow_versions_flow_id ON flow_versions(flow_id, version_num DESC);
        "#,
    )?;
    Ok(())
}

fn rebuild_user_accounts(
    conn: &Connection,
    map: &std::collections::HashMap<i64, String>,
) -> Result<()> {
    conn.execute_batch(
        r#"
        DROP TABLE IF EXISTS user_accounts_uuid_new;
        CREATE TABLE user_accounts_uuid_new (
            id TEXT PRIMARY KEY NOT NULL,
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
            role TEXT NOT NULL DEFAULT 'user',
            preferred_language TEXT
        );
        "#,
    )?;
    {
        let mut sel = conn.prepare(
            "SELECT id, username, password_hash, display_name, email, is_active, is_admin, \
             sso_provider, sso_subject, last_login_at, created_at, updated_at, \
             must_change_password, role FROM user_accounts",
        )?;
        let mut rows = sel.query([])?;
        while let Some(row) = rows.next()? {
            let old_id: i64 = row.get(0)?;
            let new_id = map.get(&old_id).expect("user id in map");
            conn.execute(
                "INSERT INTO user_accounts_uuid_new (id, username, password_hash, display_name, \
                 email, is_active, is_admin, sso_provider, sso_subject, last_login_at, created_at, \
                 updated_at, must_change_password, role) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
                rusqlite::params![
                    new_id,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, Option<String>>(8)?,
                    row.get::<_, Option<String>>(9)?,
                    row.get::<_, String>(10)?,
                    row.get::<_, String>(11)?,
                    row.get::<_, i64>(12)?,
                    row.get::<_, String>(13)?,
                ],
            )?;
        }
    }
    // Carry over an existing `user_accounts.preferred_language` when the legacy
    // table already had the column (some upgraded DBs added it ad hoc). The
    // SELECT above omits it because the canonical legacy shape has no such
    // column, so copy it here keyed by the freshly-minted UUID's source row.
    if column_exists(conn, "user_accounts", "preferred_language")? {
        let mut sel = conn.prepare("SELECT id, preferred_language FROM user_accounts")?;
        let mut rows = sel.query([])?;
        while let Some(row) = rows.next()? {
            let old_id: i64 = row.get(0)?;
            let lang: Option<String> = row.get(1)?;
            if let (Some(new_id), Some(lang)) = (map.get(&old_id), lang) {
                conn.execute(
                    "UPDATE user_accounts_uuid_new SET preferred_language = ?1 WHERE id = ?2",
                    rusqlite::params![lang, new_id],
                )?;
            }
        }
    }
    conn.execute_batch(
        r#"
        DROP TABLE user_accounts;
        ALTER TABLE user_accounts_uuid_new RENAME TO user_accounts;
        "#,
    )?;

    // Backfill the per-user language preference from the legacy `users` table.
    // `users` (F1a auth) and `user_accounts` (F2 user mgmt) are distinct
    // populations joined by their unique `username`; `users.preferred_language`
    // is the only place the setting lived before user mgmt existed, so without
    // this copy every legacy operator silently loses their UI language on the
    // UUID flip. Only fills rows still missing a preference.
    if table_exists(conn, "users")? && column_exists(conn, "users", "preferred_language")? {
        conn.execute(
            "UPDATE user_accounts \
             SET preferred_language = ( \
                 SELECT u.preferred_language FROM users u \
                 WHERE u.username = user_accounts.username \
                   AND u.preferred_language IS NOT NULL \
             ) \
             WHERE preferred_language IS NULL \
               AND EXISTS ( \
                 SELECT 1 FROM users u \
                 WHERE u.username = user_accounts.username \
                   AND u.preferred_language IS NOT NULL \
               )",
            [],
        )?;
    }
    Ok(())
}

fn rebuild_user_groups(
    conn: &Connection,
    map: &std::collections::HashMap<i64, String>,
) -> Result<()> {
    conn.execute_batch(
        r#"
        DROP TABLE IF EXISTS user_groups_uuid_new;
        CREATE TABLE user_groups_uuid_new (
            id TEXT PRIMARY KEY NOT NULL,
            name TEXT NOT NULL UNIQUE,
            description TEXT DEFAULT '',
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );
        "#,
    )?;
    {
        let mut sel = conn.prepare("SELECT id, name, description, created_at FROM user_groups")?;
        let mut rows = sel.query([])?;
        while let Some(row) = rows.next()? {
            let old_id: i64 = row.get(0)?;
            let new_id = map.get(&old_id).expect("group id in map");
            conn.execute(
                "INSERT INTO user_groups_uuid_new (id, name, description, created_at) \
                 VALUES (?1,?2,?3,?4)",
                rusqlite::params![
                    new_id,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, String>(3)?,
                ],
            )?;
        }
    }
    conn.execute_batch(
        r#"
        DROP TABLE user_groups;
        ALTER TABLE user_groups_uuid_new RENAME TO user_groups;
        "#,
    )?;
    Ok(())
}

// The v48 `cameras` rebuild (CAMERAS_VENDOR_CHECK_LOCAL_SOURCES) recreated the
// table without the `org_id` column that v32 (setup_multi_tenant) had added,
// so every database that ran v48 lost tenant scoping on `cameras` and every
// camera_list/insert_camera query failed with "no such column: org_id". The
// v48 rebuild is fixed in place for fresh installs; this migration repairs
// databases already past v48. Idempotent: a fresh install (org_id present from
// the fixed v48) finds the column and skips the ALTER, only enforcing the
// backfill and index.
fn cameras_restore_org_id_column(conn: &Connection) -> Result<()> {
    add_org_id_column_if_missing(conn, "cameras")?;
    conn.execute(
        "UPDATE cameras SET org_id = 'org-default' WHERE org_id IS NULL",
        [],
    )?;
    conn.execute_batch("CREATE INDEX IF NOT EXISTS idx_cameras_org_id ON cameras(org_id)")?;
    Ok(())
}

fn create_compliance_core_foundation(conn: &Connection) -> Result<()> {
    conn.execute_batch(COMPLIANCE_CORE_FOUNDATION)?;
    roles_add_permissions(
        conn,
        &["org_admin", "dpo"],
        &["compliance.read", "compliance.write"],
    )?;

    let mut stmt = conn.prepare("SELECT org_id FROM organizations WHERE status <> 'deleted'")?;
    let org_ids = stmt
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    drop(stmt);

    for org_id in org_ids {
        crate::compliance::repository::ensure_org_defaults(conn, &org_id)?;
    }

    Ok(())
}

fn roles_add_permissions(
    conn: &Connection,
    role_names: &[&str],
    permissions: &[&str],
) -> Result<()> {
    for role_name in role_names {
        let row: Option<(String, String)> = conn
            .query_row(
                "SELECT role_id, permissions_json FROM roles WHERE name = ?1",
                rusqlite::params![role_name],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )
            .ok();
        let Some((role_id, perms_json)) = row else {
            continue;
        };
        let mut existing: Vec<String> = match serde_json::from_str(&perms_json) {
            Ok(value) => value,
            Err(_) => {
                tracing::warn!("rola '{role_name}' ma niepoprawne permissions_json");
                continue;
            }
        };
        let mut changed = false;
        for permission in permissions {
            if !existing.iter().any(|value| value == permission) {
                existing.push((*permission).to_string());
                changed = true;
            }
        }
        if changed {
            let updated = serde_json::to_string(&existing).unwrap_or(perms_json);
            conn.execute(
                "UPDATE roles SET permissions_json = ?1 WHERE role_id = ?2",
                rusqlite::params![updated, role_id],
            )?;
        }
    }
    Ok(())
}

// F2 P1.a follow-up — v32 (setup_multi_tenant) created the org_memberships
// table but did not seed entries for pre-existing admin users. As a result
// every legacy admin login resolves to `org_context=None` and every
// dispatch path requiring OrgContext (cameras, recordings, frame_url, ...)
// rejects the call. This step backfills every admin user from both legacy
// `users` (F1a auth) and `user_accounts` (F2 user mgmt) into `org-default`
// with role `role-org-admin`. Idempotent via `NOT EXISTS` guards.
fn backfill_admin_org_memberships(conn: &Connection) -> Result<()> {
    // Source 1: legacy `users` table (F1a). `role='admin'` is the only path
    // by which someone can mint a JWT pre-F2.
    conn.execute(
        "INSERT INTO org_memberships (org_id, user_id, role_id, granted_at, granted_by) \
         SELECT 'org-default', CAST(u.id AS TEXT), 'role-org-admin', \
                strftime('%Y-%m-%dT%H:%M:%SZ', 'now'), 'system' \
         FROM users u \
         WHERE u.role = 'admin' \
           AND NOT EXISTS ( \
               SELECT 1 FROM org_memberships m WHERE m.user_id = CAST(u.id AS TEXT) \
           )",
        [],
    )?;

    // Source 2: `user_accounts` table (F2 user mgmt). is_admin OR role='admin'.
    // Guarded by table-existence probe — F1a installs that never ran user
    // mgmt migrations skip this step cleanly.
    let user_accounts_exists: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='user_accounts'",
        [],
        |r| r.get(0),
    )?;
    if user_accounts_exists > 0 {
        conn.execute(
            "INSERT INTO org_memberships (org_id, user_id, role_id, granted_at, granted_by) \
             SELECT 'org-default', CAST(u.id AS TEXT), 'role-org-admin', \
                    strftime('%Y-%m-%dT%H:%M:%SZ', 'now'), 'system' \
             FROM user_accounts u \
             WHERE (u.is_admin = 1 OR u.role = 'admin') \
               AND NOT EXISTS ( \
                   SELECT 1 FROM org_memberships m WHERE m.user_id = CAST(u.id AS TEXT) \
               )",
            [],
        )?;
    }

    Ok(())
}

// F2 P8.a — RODO/GDPR document registry. Stores PDF artifacts generated per
// organization (short / standard / full variant). The `legal.read` and
// `legal.write` permission keys are already part of the v32 roles preseed
// (`org_admin`, `dpo`: read+write; `org_operator`, `org_viewer`: read only),
// so this migration adds storage only and does not touch `roles.permissions_json`.
//
// Row identity:
//   * `id`                       UUIDv4 minted by the repo (CHECK on shape)
//   * `org_id`                   tenant scope; ON DELETE RESTRICT — compliance
//                                retention forbids cascading on org delete
//                                (orgs are soft-deleted at the application layer)
//   * `variant`                  one of short|standard|full (CHECK)
//   * `generated_at`             unix epoch milliseconds
//   * `generated_by_user_id`     composite FK to org_memberships(org_id,user_id),
//                                ON DELETE RESTRICT — losing a membership must
//                                not erase the audit trail of generated documents
//   * `content_hash`             blake3 hex of the PDF bytes (64 lowercase hex, CHECK)
//   * `pdf_path`                 absolute path on disk
//   * `signed_url_ref`           HMAC ref published to the recording_url tier
//                                (NULL until the URL is minted)
//   * `revoked_at`               soft-delete timestamp; NULL = active
//
// The composite index supports the dominant query: list-by-org with newest
// rows first.
fn create_legal_documents_table(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS legal_documents (
            id TEXT NOT NULL PRIMARY KEY
                CHECK (length(id) = 36 AND id LIKE '________-____-____-____-____________'),
            org_id TEXT NOT NULL REFERENCES organizations(org_id) ON DELETE RESTRICT,
            variant TEXT NOT NULL CHECK (variant IN ('short','standard','full')),
            generated_at INTEGER NOT NULL,
            generated_by_user_id TEXT NOT NULL,
            content_hash TEXT NOT NULL
                CHECK (length(content_hash) = 64 AND content_hash GLOB '[0-9a-f]*'),
            pdf_path TEXT NOT NULL,
            signed_url_ref TEXT NULL,
            revoked_at INTEGER NULL,
            FOREIGN KEY (org_id, generated_by_user_id)
                REFERENCES org_memberships(org_id, user_id) ON DELETE RESTRICT
        );
        CREATE INDEX IF NOT EXISTS idx_legal_documents_org_id_generated_at
            ON legal_documents(org_id, generated_at DESC);
        "#,
    )?;
    Ok(())
}

// F2 P6.b — grant `camera.metadata` to operators so they can subscribe to
// ONVIF analytics streams. Admin already received this permission as part of
// the v32 seed; operator did not. Viewer is intentionally left alone — the
// subscription host fn mutates supervisor state (refcount + spawn pull task)
// and is therefore not a pure read operation.
//
// Idempotent: re-reads the permissions JSON, only appends when the entry is
// missing. Safe to run on a fresh DB (`org_operator` already exists from v32)
// and on an old DB where an admin manually edited the row.
fn roles_add_camera_metadata_permission(conn: &Connection) -> Result<()> {
    roles_add_permissions(conn, &["org_admin", "org_operator"], &["camera.metadata"])
}

// Grant robot control permissions to the operational roles. The robot
// dispatch path (`PermissionMatrix::has_permission`) enforces
// `robot.command` / `robot.estop` / `robot.telemetry` on the acting user;
// without this, even an org admin gets `permission_denied` when driving a
// robot. Viewer stays read-only (no robot control). Idempotent via
// `roles_add_permissions` — safe on fresh DBs (the v32 seed already carries
// these) and on existing DBs created before robots existed.
fn roles_add_robot_permissions(conn: &Connection) -> Result<()> {
    roles_add_permissions(
        conn,
        &["org_admin", "org_operator"],
        &["robot.command", "robot.estop", "robot.telemetry"],
    )
}

// v119 — Project Studio ("Projekty"). Every org role may enter the module
// (`project_studio.read` — per-project access is then gated by project
// membership roles), while `project_studio.admin` (creator grants, viewing
// projects outside membership, orphaned-project takeover) stays with
// org_admin. Idempotent via `roles_add_permissions`.
fn roles_add_project_studio_permissions(conn: &Connection) -> Result<()> {
    roles_add_permissions(
        conn,
        &[
            "org_admin",
            "org_operator",
            "org_viewer",
            "dpo",
            "supervisor",
        ],
        &["project_studio.read"],
    )?;
    roles_add_permissions(conn, &["org_admin"], &["project_studio.admin"])
}

// F2 P6.a — ONVIF metadata (Media2 + PullPoint events). The `cameras` table
// gains a boolean flag indicating whether the device exposes a metadata
// configuration that produces analytics events. Filled by the wizard when
// `GetMetadataConfigurations` returns a non-empty list; consumed by the
// upcoming event-pull supervisor to decide whether to subscribe.
fn cameras_add_metadata_supported_column(conn: &Connection) -> Result<()> {
    let mut stmt = conn.prepare("PRAGMA table_info(cameras)")?;
    let mut rows = stmt.query([])?;
    let mut has_col = false;
    while let Some(row) = rows.next()? {
        let name: String = row.get(1)?;
        if name == "metadata_supported" {
            has_col = true;
            break;
        }
    }
    drop(rows);
    drop(stmt);

    if !has_col {
        conn.execute_batch(
            "ALTER TABLE cameras ADD COLUMN metadata_supported INTEGER NOT NULL DEFAULT 0;",
        )?;
    }
    Ok(())
}

// F2 P3 — reserved persistence table for gate_check decisions. The F2
// runtime keeps the cache purely in-memory (`services::policy::cache`); F3
// will gate-flip a feature flag that wires reads/writes through this table
// so cache state survives restarts. Shipping the schema now means the F3
// switch is a code change, not a migration.
const GATE_CHECK_CACHE: &str = r#"
CREATE TABLE IF NOT EXISTS gate_check_cache (
    claim_id TEXT NOT NULL,
    ctx_hash TEXT NOT NULL,
    result TEXT NOT NULL,
    cached_at TEXT NOT NULL,
    expires_at TEXT NOT NULL,
    PRIMARY KEY (claim_id, ctx_hash)
);
CREATE INDEX IF NOT EXISTS idx_gate_cache_expires ON gate_check_cache(expires_at);
"#;

// v42 — Identity Registry dla Sync Ledger. `sync_nodes` opisuje techniczna
// tozsamosc node/device, `user_identity_keys` przechowuje kryptograficzne
// klucze uzytkownika, a `node_user_assignments` mapuje kto moze uzywac danego
// noda. `trusted_nodes` zostaje aktywnym store mesh trust; nowa tabela dostaje
// backfill jako warstwa administracyjna pod permissions/sync policy.
const SYNC_IDENTITY_REGISTRY: &str = r#"
CREATE TABLE IF NOT EXISTS sync_nodes (
    node_id TEXT PRIMARY KEY,
    public_key TEXT NOT NULL,
    public_key_type TEXT NOT NULL DEFAULT 'ed25519'
        CHECK(public_key_type IN ('ed25519','secp256k1')),
    display_name TEXT NOT NULL DEFAULT '',
    node_kind TEXT NOT NULL DEFAULT 'unknown'
        CHECK(node_kind IN ('unknown','phone','tablet','laptop','desktop','server','shared','authority')),
    trust_status TEXT NOT NULL DEFAULT 'untrusted'
        CHECK(trust_status IN ('untrusted','pending','trusted','revoked')),
    owner_user_id TEXT NULL REFERENCES user_accounts(id) ON DELETE SET NULL,
    sync_profile TEXT NOT NULL DEFAULT 'standard'
        CHECK(sync_profile IN ('standard','limited','authority','storage_only','ephemeral')),
    last_seen_at TEXT NULL,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
);

CREATE INDEX IF NOT EXISTS idx_sync_nodes_owner ON sync_nodes(owner_user_id);
CREATE INDEX IF NOT EXISTS idx_sync_nodes_trust ON sync_nodes(trust_status);
CREATE INDEX IF NOT EXISTS idx_sync_nodes_kind ON sync_nodes(node_kind);

CREATE TRIGGER IF NOT EXISTS sync_nodes_updated_at
AFTER UPDATE ON sync_nodes
FOR EACH ROW
BEGIN
    UPDATE sync_nodes
    SET updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
    WHERE node_id = NEW.node_id;
END;

CREATE TABLE IF NOT EXISTS user_identity_keys (
    key_id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES user_accounts(id) ON DELETE CASCADE,
    key_type TEXT NOT NULL CHECK(key_type IN ('ed25519','secp256k1')),
    public_key TEXT NOT NULL,
    purpose TEXT NOT NULL DEFAULT 'sync'
        CHECK(purpose IN ('auth','sync','admin','recovery')),
    status TEXT NOT NULL DEFAULT 'active'
        CHECK(status IN ('active','revoked')),
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    revoked_at TEXT NULL,
    UNIQUE(user_id, key_type, public_key)
);

CREATE INDEX IF NOT EXISTS idx_user_identity_keys_user ON user_identity_keys(user_id, status);
CREATE INDEX IF NOT EXISTS idx_user_identity_keys_public ON user_identity_keys(key_type, public_key);

CREATE TABLE IF NOT EXISTS node_user_assignments (
    node_id TEXT NOT NULL REFERENCES sync_nodes(node_id) ON DELETE CASCADE,
    user_id TEXT NOT NULL REFERENCES user_accounts(id) ON DELETE CASCADE,
    assignment_mode TEXT NOT NULL
        CHECK(assignment_mode IN ('primary','allowed','shared_session','authority_operator')),
    valid_from TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    valid_until TEXT NULL,
    created_by TEXT NULL REFERENCES user_accounts(id) ON DELETE SET NULL,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    PRIMARY KEY(node_id, user_id, assignment_mode)
);

CREATE INDEX IF NOT EXISTS idx_node_user_assignments_user ON node_user_assignments(user_id, valid_until);
CREATE INDEX IF NOT EXISTS idx_node_user_assignments_node ON node_user_assignments(node_id, valid_until);

INSERT OR IGNORE INTO sync_nodes (node_id, public_key, display_name, trust_status, created_at, updated_at)
SELECT node_id,
       public_key,
       COALESCE(NULLIF(hostname, ''), node_id),
       CASE WHEN is_active = 1 THEN 'trusted' ELSE 'untrusted' END,
       approved_at,
       approved_at
FROM trusted_nodes;
"#;

// v43 — fundament Permission Engine dla Sync Ledger. Tabele trzymaja
// minimalny, domenowo-neutralny opis zasobu i relacje dostepu, na ktorych
// pozniej opieraja sie polityki per addon oraz filtr outbox.
const SYNC_PERMISSION_ENGINE: &str = r#"
CREATE TABLE IF NOT EXISTS sync_user_org_profiles (
    org_id TEXT NOT NULL REFERENCES organizations(org_id) ON DELETE CASCADE,
    user_id TEXT NOT NULL REFERENCES user_accounts(id) ON DELETE CASCADE,
    department_id TEXT NULL,
    manager_user_id TEXT NULL REFERENCES user_accounts(id) ON DELETE SET NULL,
    is_department_manager INTEGER NOT NULL DEFAULT 0 CHECK(is_department_manager IN (0,1)),
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    PRIMARY KEY(org_id, user_id)
);

CREATE INDEX IF NOT EXISTS idx_sync_user_org_profiles_department
    ON sync_user_org_profiles(org_id, department_id);
CREATE INDEX IF NOT EXISTS idx_sync_user_org_profiles_manager
    ON sync_user_org_profiles(org_id, manager_user_id);

CREATE TRIGGER IF NOT EXISTS sync_user_org_profiles_updated_at
AFTER UPDATE ON sync_user_org_profiles
FOR EACH ROW
BEGIN
    UPDATE sync_user_org_profiles
    SET updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
    WHERE org_id = NEW.org_id AND user_id = NEW.user_id;
END;

CREATE TABLE IF NOT EXISTS sync_resource_acl (
    org_id TEXT NOT NULL REFERENCES organizations(org_id) ON DELETE CASCADE,
    addon_id TEXT NOT NULL,
    resource_type TEXT NOT NULL,
    resource_id TEXT NOT NULL,
    owner_user_id TEXT NULL REFERENCES user_accounts(id) ON DELETE SET NULL,
    assigned_user_id TEXT NULL REFERENCES user_accounts(id) ON DELETE SET NULL,
    department_id TEXT NULL,
    manager_user_id TEXT NULL REFERENCES user_accounts(id) ON DELETE SET NULL,
    visibility_scope TEXT NOT NULL DEFAULT 'assigned'
        CHECK(visibility_scope IN ('private','own','assigned','department','manager_subtree','explicit_share','all')),
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    PRIMARY KEY(org_id, addon_id, resource_type, resource_id)
);

CREATE INDEX IF NOT EXISTS idx_sync_resource_acl_owner
    ON sync_resource_acl(org_id, owner_user_id);
CREATE INDEX IF NOT EXISTS idx_sync_resource_acl_assigned
    ON sync_resource_acl(org_id, assigned_user_id);
CREATE INDEX IF NOT EXISTS idx_sync_resource_acl_department
    ON sync_resource_acl(org_id, department_id);
CREATE INDEX IF NOT EXISTS idx_sync_resource_acl_manager
    ON sync_resource_acl(org_id, manager_user_id);

CREATE TRIGGER IF NOT EXISTS sync_resource_acl_updated_at
AFTER UPDATE ON sync_resource_acl
FOR EACH ROW
BEGIN
    UPDATE sync_resource_acl
    SET updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
    WHERE org_id = NEW.org_id
      AND addon_id = NEW.addon_id
      AND resource_type = NEW.resource_type
      AND resource_id = NEW.resource_id;
END;

CREATE TABLE IF NOT EXISTS sync_explicit_shares (
    org_id TEXT NOT NULL REFERENCES organizations(org_id) ON DELETE CASCADE,
    addon_id TEXT NOT NULL,
    resource_type TEXT NOT NULL,
    resource_id TEXT NOT NULL,
    subject_type TEXT NOT NULL CHECK(subject_type IN ('user','node')),
    subject_id TEXT NOT NULL,
    action TEXT NOT NULL CHECK(action IN ('read','write','sync_receive','admin')),
    granted_by TEXT NULL REFERENCES user_accounts(id) ON DELETE SET NULL,
    granted_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    revoked_at TEXT NULL,
    PRIMARY KEY(org_id, addon_id, resource_type, resource_id, subject_type, subject_id, action)
);

CREATE INDEX IF NOT EXISTS idx_sync_explicit_shares_subject
    ON sync_explicit_shares(org_id, subject_type, subject_id, action, revoked_at);
CREATE INDEX IF NOT EXISTS idx_sync_explicit_shares_resource
    ON sync_explicit_shares(org_id, addon_id, resource_type, resource_id, revoked_at);
"#;

// v44 — Sync Policy. Polityka jest konfigurowana przez TentaFlow, nie przez
// addon. Najbardziej szczegolny wpis wygrywa: resource > resource_type > addon.
const SYNC_POLICY: &str = r#"
CREATE TABLE IF NOT EXISTS sync_policies (
    policy_id TEXT PRIMARY KEY,
    org_id TEXT NOT NULL REFERENCES organizations(org_id) ON DELETE CASCADE,
    addon_id TEXT NOT NULL,
    resource_type TEXT NOT NULL DEFAULT '',
    resource_id TEXT NOT NULL DEFAULT '',
    mode TEXT NOT NULL
        CHECK(mode IN ('local_only','replicated_by_permission','authority_readthrough','authority_write','sharded','ephemeral')),
    authority_node_id TEXT NULL REFERENCES sync_nodes(node_id) ON DELETE SET NULL,
    retention_days INTEGER NULL CHECK(retention_days IS NULL OR retention_days >= 0),
    is_enabled INTEGER NOT NULL DEFAULT 1 CHECK(is_enabled IN (0,1)),
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    CHECK (
        (mode IN ('authority_readthrough','authority_write') AND authority_node_id IS NOT NULL)
        OR (mode NOT IN ('authority_readthrough','authority_write'))
    ),
    CHECK (
        (resource_id = '')
        OR (resource_type <> '')
    ),
    UNIQUE(org_id, addon_id, resource_type, resource_id)
);

CREATE INDEX IF NOT EXISTS idx_sync_policies_lookup
    ON sync_policies(org_id, addon_id, resource_type, resource_id, is_enabled);
CREATE INDEX IF NOT EXISTS idx_sync_policies_authority
    ON sync_policies(authority_node_id, is_enabled);

CREATE TRIGGER IF NOT EXISTS sync_policies_updated_at
AFTER UPDATE ON sync_policies
FOR EACH ROW
BEGIN
    UPDATE sync_policies
    SET updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
    WHERE policy_id = NEW.policy_id;
END;
"#;

const CORE_SYNC_CAPTURES: &str = r#"
CREATE TABLE IF NOT EXISTS __tentaflow_core_sync_captures (
    capture_id TEXT PRIMARY KEY,
    org_id TEXT NOT NULL REFERENCES organizations(org_id) ON DELETE CASCADE,
    table_name TEXT NOT NULL,
    resource_type TEXT NOT NULL,
    resource_id TEXT NOT NULL,
    primary_key TEXT NOT NULL,
    action TEXT NOT NULL CHECK(action IN ('insert','update','delete')),
    changed_fields_blob BLOB NOT NULL,
    actor_user_id TEXT NULL REFERENCES user_accounts(id) ON DELETE SET NULL,
    status TEXT NOT NULL DEFAULT 'pending' CHECK(status IN ('pending','ledgered','error')),
    operation_id TEXT NULL,
    error_message TEXT NULL,
    created_at_ms INTEGER NOT NULL,
    ledgered_at_ms INTEGER NULL
);

CREATE INDEX IF NOT EXISTS idx_core_sync_captures_status
    ON __tentaflow_core_sync_captures(status, created_at_ms);
CREATE INDEX IF NOT EXISTS idx_core_sync_captures_resource
    ON __tentaflow_core_sync_captures(org_id, resource_type, resource_id);
CREATE INDEX IF NOT EXISTS idx_core_sync_captures_operation
    ON __tentaflow_core_sync_captures(operation_id);
"#;

// v57 — carry the pre-commit HLC stamp on each core capture row so the ledger
// operation drained later reuses the exact timestamp recorded inside the write
// transaction (not a fresh clock read at drain time), and so the materializer's
// HLC-LWW comparison sees the originating order.
const CORE_SYNC_CAPTURES_HLC: &str = r#"
ALTER TABLE __tentaflow_core_sync_captures ADD COLUMN hlc_wall INTEGER NOT NULL DEFAULT 0;
ALTER TABLE __tentaflow_core_sync_captures ADD COLUMN hlc_logical INTEGER NOT NULL DEFAULT 0;
ALTER TABLE __tentaflow_core_sync_captures ADD COLUMN hlc_node TEXT NOT NULL DEFAULT '';
"#;

const KV_SYNC_CAPTURES: &str = r#"
CREATE TABLE IF NOT EXISTS __tentaflow_kv_sync_captures (
    capture_id TEXT PRIMARY KEY,
    org_id TEXT NOT NULL REFERENCES organizations(org_id) ON DELETE CASCADE,
    addon_id TEXT NOT NULL,
    instance_id TEXT NOT NULL,
    storage_key TEXT NOT NULL,
    action TEXT NOT NULL CHECK(action IN ('set','delete')),
    storage_value BLOB NULL,
    actor_user_id TEXT NULL REFERENCES user_accounts(id) ON DELETE SET NULL,
    status TEXT NOT NULL DEFAULT 'pending' CHECK(status IN ('pending','ledgered','error')),
    operation_id TEXT NULL,
    error_message TEXT NULL,
    created_at_ms INTEGER NOT NULL,
    ledgered_at_ms INTEGER NULL
);

CREATE INDEX IF NOT EXISTS idx_kv_sync_captures_status
    ON __tentaflow_kv_sync_captures(status, created_at_ms);
CREATE INDEX IF NOT EXISTS idx_kv_sync_captures_resource
    ON __tentaflow_kv_sync_captures(org_id, addon_id, instance_id, storage_key);
CREATE INDEX IF NOT EXISTS idx_kv_sync_captures_operation
    ON __tentaflow_kv_sync_captures(operation_id);
"#;

const BLOB_SYNC_CAPTURES: &str = r#"
CREATE TABLE IF NOT EXISTS __tentaflow_blob_sync_captures (
    capture_id TEXT PRIMARY KEY,
    org_id TEXT NOT NULL REFERENCES organizations(org_id) ON DELETE CASCADE,
    blob_id TEXT NOT NULL,
    sha256 TEXT NOT NULL,
    mime TEXT NOT NULL,
    size_bytes INTEGER NOT NULL CHECK(size_bytes >= 0),
    file_path TEXT NOT NULL,
    actor_user_id TEXT NULL REFERENCES user_accounts(id) ON DELETE SET NULL,
    status TEXT NOT NULL DEFAULT 'pending' CHECK(status IN ('pending','ledgered','error')),
    operation_id TEXT NULL,
    error_message TEXT NULL,
    created_at_ms INTEGER NOT NULL,
    ledgered_at_ms INTEGER NULL
);

CREATE INDEX IF NOT EXISTS idx_blob_sync_captures_status
    ON __tentaflow_blob_sync_captures(status, created_at_ms);
CREATE INDEX IF NOT EXISTS idx_blob_sync_captures_sha
    ON __tentaflow_blob_sync_captures(org_id, sha256);
CREATE INDEX IF NOT EXISTS idx_blob_sync_captures_operation
    ON __tentaflow_blob_sync_captures(operation_id);
"#;

// v55 — last-writer HLC bookmark per synced resource. Phase B will write the
// HLC of the most recently applied operation here so conflict resolution can
// compare an incoming operation against the resource's current version without
// replaying the ledger. Additive in phase A: no write path touches it yet.
const CORE_RESOURCE_VERSIONS: &str = r#"
CREATE TABLE IF NOT EXISTS core_resource_versions (
    resource_type TEXT NOT NULL,
    resource_id TEXT NOT NULL,
    hlc_wall INTEGER NOT NULL,
    hlc_logical INTEGER NOT NULL,
    hlc_node TEXT NOT NULL,
    PRIMARY KEY(resource_type, resource_id)
);
"#;

// F2 P2.a — formalise the legal value set for `model_aliases.strategy`.
// The initial schema declared the column without a CHECK constraint, so
// `round_robin` was already accepted by the storage layer (and the runtime
// `Strategy::from_db` parser maps the literal to `Strategy::RoundRobin`).
// This migration pins the contract: only `first_available` and `round_robin`
// are valid going forward. Any future strategy must edit both this CHECK
// and `services::catalog::Strategy::from_db`.
//
// SQLite cannot ALTER a CHECK constraint in place; we rebuild the table
// using the canonical pattern (CREATE new + INSERT SELECT + DROP old +
// RENAME + recreate index). Idempotent at three levels:
//   * If the live `model_aliases` already has a CHECK that accepts both
//     values (re-run after success), the rebuild repeats but produces an
//     equivalent schema — wasted work, never incorrect.
//   * `model_aliases_new` is dropped at the start so a half-run from a
//     previous attempt cannot collide on the staging table name.
//   * `DROP TABLE IF EXISTS` on the post-rename leftover keeps a partial
//     rerun (process killed between RENAME and index creation) recoverable.
fn model_aliases_strategy_round_robin(conn: &Connection) -> Result<()> {
    // Drop any leftover staging table from a previous interrupted run.
    conn.execute_batch("DROP TABLE IF EXISTS model_aliases_new;")?;

    // Create the rebuilt table with an explicit CHECK constraint. Column
    // order, types and defaults mirror the v1 schema (line 1174) so the
    // INSERT SELECT below is a straight column-for-column copy.
    conn.execute_batch(
        r#"
        CREATE TABLE model_aliases_new (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            alias TEXT UNIQUE NOT NULL,
            target_model TEXT NOT NULL,
            is_active INTEGER NOT NULL DEFAULT 1,
            fallback_targets TEXT DEFAULT NULL,
            strategy TEXT DEFAULT 'first_available'
                CHECK(strategy IN ('first_available','round_robin'))
        );
        INSERT INTO model_aliases_new (id, alias, target_model, is_active, fallback_targets, strategy)
            SELECT id, alias, target_model, is_active, fallback_targets,
                   COALESCE(strategy, 'first_available')
              FROM model_aliases;
        DROP TABLE model_aliases;
        ALTER TABLE model_aliases_new RENAME TO model_aliases;
        CREATE INDEX IF NOT EXISTS idx_model_aliases_alias ON model_aliases(alias);
        "#,
    )?;
    Ok(())
}

// F2 P1.a — multi-tenant foundation. Creates the three control tables
// (organizations, roles, org_memberships), seeds the `org-default` row + the
// five standard roles, then PRAGMA-guards eight existing tables to grow a
// nullable `org_id` column and backfills every pre-existing row to
// `org-default`. Idempotent at every step:
//   * CREATE TABLE / CREATE INDEX use IF NOT EXISTS.
//   * Seeds use INSERT OR IGNORE so a second run leaves the rows untouched.
//   * ADD COLUMN is gated by a PRAGMA table_info probe.
//   * Backfill UPDATE only touches rows where org_id IS NULL — a second run
//     finds zero such rows and is a no-op.
// Backfill is small enough (single SQLite UPDATE per table) to run inline;
// no batching is required because the migration runs inside the per-version
// transaction opened by `db::migrations::run`.
fn setup_multi_tenant(conn: &Connection) -> Result<()> {
    // Step 1: control tables.
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS organizations (
            org_id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            slug TEXT NOT NULL UNIQUE,
            contact_email TEXT NULL,
            dpo_contact TEXT NULL,
            retention_policy_json TEXT NULL,
            status TEXT NOT NULL DEFAULT 'active'
                CHECK(status IN ('active','suspended','deleted')),
            created_at TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_organizations_status ON organizations(status);
        CREATE INDEX IF NOT EXISTS idx_organizations_slug ON organizations(slug);

        CREATE TABLE IF NOT EXISTS roles (
            role_id TEXT PRIMARY KEY,
            name TEXT NOT NULL UNIQUE,
            permissions_json TEXT NOT NULL,
            created_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS org_memberships (
            org_id TEXT NOT NULL REFERENCES organizations(org_id) ON DELETE CASCADE,
            user_id TEXT NOT NULL,
            role_id TEXT NOT NULL REFERENCES roles(role_id),
            granted_at TEXT NOT NULL,
            granted_by TEXT NOT NULL,
            PRIMARY KEY (org_id, user_id)
        );
        CREATE INDEX IF NOT EXISTS idx_org_memberships_user ON org_memberships(user_id);
        CREATE INDEX IF NOT EXISTS idx_org_memberships_role ON org_memberships(role_id);
        "#,
    )?;

    // Step 2: seed `org-default`. INSERT OR IGNORE keeps the migration
    // idempotent across re-runs and across operators who created the row by
    // hand before re-applying the migration.
    conn.execute(
        "INSERT OR IGNORE INTO organizations \
            (org_id, name, slug, contact_email, dpo_contact, retention_policy_json, status, created_at) \
         VALUES ('org-default', 'Default Organization', 'default', NULL, NULL, NULL, 'active', \
                 strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))",
        [],
    )?;

    // Step 3: seed five preseed roles. Permission keys chosen to align with
    // the host-fn permission checks already in flight across the codebase
    // (camera.*, service.*, sql.*, vector.*, policy.*, gate.check, flow.invoke,
    // legal.*) plus the new RBAC-specific keys (org.*, user.*, addon.*,
    // rbac.elevate).
    let role_seeds: &[(&str, &str, &[&str])] = &[
        (
            "role-org-admin",
            "org_admin",
            &[
                "org.read",
                "org.write",
                "org.admin",
                "user.read",
                "user.write",
                "user.assign_role",
                "addon.install",
                "addon.upgrade",
                "addon.uninstall",
                "camera.read",
                "camera.write",
                "camera.discover",
                "camera.metadata",
                "service.read",
                "service.call",
                "sql.read",
                "sql.write",
                "vector.read",
                "vector.write",
                "policy.read",
                "policy.write",
                "gate.check",
                "flow.invoke",
                "legal.read",
                "legal.write",
                "rbac.elevate",
                "robot.command",
                "robot.estop",
                "robot.telemetry",
            ],
        ),
        (
            "role-org-operator",
            "org_operator",
            &[
                "org.read",
                "camera.read",
                "camera.write",
                "camera.discover",
                "service.read",
                "service.call",
                "sql.read",
                "vector.read",
                "policy.read",
                "gate.check",
                "flow.invoke",
                "legal.read",
                "robot.command",
                "robot.estop",
                "robot.telemetry",
            ],
        ),
        (
            "role-org-viewer",
            "org_viewer",
            &[
                "org.read",
                "camera.read",
                "service.read",
                "sql.read",
                "vector.read",
                "policy.read",
                "legal.read",
            ],
        ),
        (
            "role-dpo",
            "dpo",
            &[
                "org.read",
                "policy.read",
                "policy.write",
                "gate.check",
                "legal.read",
                "legal.write",
                "rbac.elevate",
            ],
        ),
        (
            "role-supervisor",
            "supervisor",
            &["org.read", "policy.read", "gate.check", "rbac.elevate"],
        ),
    ];
    for (role_id, name, perms) in role_seeds {
        let perms_json = serialize_perms_json(perms);
        conn.execute(
            "INSERT OR IGNORE INTO roles (role_id, name, permissions_json, created_at) \
             VALUES (?1, ?2, ?3, strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))",
            rusqlite::params![role_id, name, perms_json],
        )?;
    }

    // Step 4 + 5 + 6: per-table ADD COLUMN (PRAGMA-guarded) + backfill +
    // index. The eight target tables hold every row that must scope to a
    // tenant from F2 onward. `addon_installations` lives under the name
    // `addons` in this schema (sole canonical addon registry — there is no
    // separate `addon_installations` table); we use the real name to avoid
    // creating a phantom column on a non-existent table.
    let tables: &[&str] = &[
        "users",
        "addons",
        "policy_claims",
        "cameras",
        "recordings",
        "addon_vector_namespaces",
        "audit_log",
        "frame_pickup_log",
    ];
    for table in tables {
        add_org_id_column_if_missing(conn, table)?;
        let sql_backfill = format!(
            "UPDATE {} SET org_id = 'org-default' WHERE org_id IS NULL",
            table
        );
        conn.execute(&sql_backfill, [])?;
        let sql_index = format!(
            "CREATE INDEX IF NOT EXISTS idx_{tbl}_org_id ON {tbl}(org_id)",
            tbl = table
        );
        conn.execute_batch(&sql_index)?;
    }

    Ok(())
}

// PRAGMA-guarded `ALTER TABLE <tbl> ADD COLUMN org_id TEXT NULL`. Idempotent:
// a second run finds the column already present and skips. The column is
// intentionally NULLABLE so the backfill UPDATE that follows is the single
// source of truth for the default-org assignment (an `NOT NULL DEFAULT
// 'org-default'` would silently mask a partial-fail backfill).
fn add_org_id_column_if_missing(conn: &Connection, table: &str) -> Result<()> {
    let pragma_sql = format!("PRAGMA table_info({})", table);
    let mut stmt = conn.prepare(&pragma_sql)?;
    let mut rows = stmt.query([])?;
    let mut has_col = false;
    while let Some(row) = rows.next()? {
        let name: String = row.get(1)?;
        if name == "org_id" {
            has_col = true;
            break;
        }
    }
    drop(rows);
    drop(stmt);

    if !has_col {
        let alter_sql = format!("ALTER TABLE {} ADD COLUMN org_id TEXT NULL", table);
        conn.execute_batch(&alter_sql)?;
    }
    Ok(())
}

// Serialize a permission list to canonical JSON without pulling in
// serde_json's value tree. Permission keys never contain `"` or `\` (they
// are dot-delimited ASCII identifiers, validated implicitly by the host-fn
// permission checks elsewhere in the codebase), so a hand-rolled writer is
// safe and avoids the serde_json dependency at the migrations layer.
fn serialize_perms_json(perms: &[&str]) -> String {
    let mut out = String::from("[");
    for (i, p) in perms.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push('"');
        out.push_str(p);
        out.push('"');
    }
    out.push(']');
    out
}

// F1c P7 — per-user audit attribution. Existing rows pre-dating this migration
// stay with NULL (system / unknown actor); WASM-driven flow invokes from this
// point forward carry `state.user_id` so DoD-9 / DoD-10 reports can join the
// row back to a concrete operator account. NULL remains valid for boot
// recovery, scheduled tasks, and mesh-originated invocations.
//
// SQLite has no `ADD COLUMN IF NOT EXISTS`. We probe PRAGMA table_info first
// so a partial reopen does not redo the ALTER.
fn flow_invocations_add_actor_user_id(conn: &Connection) -> Result<()> {
    let mut stmt = conn.prepare("PRAGMA table_info(flow_invocations)")?;
    let mut rows = stmt.query([])?;
    let mut has_col = false;
    while let Some(row) = rows.next()? {
        let name: String = row.get(1)?;
        if name == "actor_user_id" {
            has_col = true;
            break;
        }
    }
    drop(rows);
    drop(stmt);

    if !has_col {
        conn.execute_batch(
            "ALTER TABLE flow_invocations ADD COLUMN actor_user_id TEXT NULL \
                 REFERENCES user_accounts(id);",
        )?;
    }
    Ok(())
}

// F1c P6 — persist the ONVIF device-service URL and the profile token chosen
// at `camera_add_v1` time so the credentials-rotation path can re-derive the
// RTSP URI without forcing the operator to re-run discovery. Both columns
// are nullable: rows added under `vendor='rtsp'` (or pre-P6 ONVIF rows added
// by hand) keep NULL.
//
// SQLite has no `ADD COLUMN IF NOT EXISTS`. We read PRAGMA table_info first
// and skip the ALTER when the column already exists so the migration is
// idempotent across partial-run reopens.
fn cameras_add_onvif_metadata_columns(conn: &Connection) -> Result<()> {
    let mut stmt = conn.prepare("PRAGMA table_info(cameras)")?;
    let mut rows = stmt.query([])?;
    let mut has_url = false;
    let mut has_token = false;
    while let Some(row) = rows.next()? {
        let name: String = row.get(1)?;
        if name == "onvif_url" {
            has_url = true;
        } else if name == "onvif_profile_token" {
            has_token = true;
        }
    }
    drop(rows);
    drop(stmt);

    if !has_url {
        conn.execute_batch("ALTER TABLE cameras ADD COLUMN onvif_url TEXT NULL;")?;
    }
    if !has_token {
        conn.execute_batch("ALTER TABLE cameras ADD COLUMN onvif_profile_token TEXT NULL;")?;
    }
    Ok(())
}

// F1c P5 — runtime tracking of in-flight and historical flow invocations
// issued via flow_invoke_v1.
const FLOW_INVOCATIONS: &str = r#"
CREATE TABLE IF NOT EXISTS flow_invocations (
    id TEXT PRIMARY KEY,
    addon_id TEXT NOT NULL,
    flow_id TEXT NOT NULL,
    started_at TEXT NOT NULL,
    finished_at TEXT,
    status TEXT NOT NULL,
    error TEXT,
    result_toml TEXT,
    operators_completed INTEGER NOT NULL DEFAULT 0,
    operators_total INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_flow_invocations_addon ON flow_invocations(addon_id, started_at DESC);
CREATE INDEX IF NOT EXISTS idx_flow_invocations_running ON flow_invocations(status) WHERE status='running';
"#;

// F1c P4 — policy/claims engine tables. `policy_claims` records DPIA / FRIA
// approvals or legal grants issued by an administrator (via CLI). Each claim
// can be scoped globally (addon NULL) or narrowed to a specific addon and
// optionally a single namespace / alias id. Claims expire automatically
// (`valid_until`) and can be revoked at any time (`revoked_at` set non-NULL).
// `policy_claim_signatures` carries the multi-signer requirement — at least
// one signer per required role (typically DPO + supervisor) is enforced by
// the engine when verifying. `signature_b64` is optional: NULL means manual
// admin acknowledgment recorded via CLI; populated means a cryptographic
// Ed25519 signature exists alongside the manual approval (verified
// opportunistically — manual ack is the contract today, the signature is a
// future-proofed audit anchor).
const POLICY_CLAIMS: &str = r#"
CREATE TABLE IF NOT EXISTS policy_claims (
    claim_id TEXT PRIMARY KEY,
    claim_type TEXT NOT NULL,
    label TEXT NOT NULL,
    subject TEXT NULL,
    scope TEXT NULL,
    document_uri TEXT NULL,
    scope_addon_id TEXT NULL,
    scope_namespace TEXT NULL,
    valid_from TEXT NOT NULL,
    valid_until TEXT NOT NULL,
    revoked_at TEXT NULL,
    revoked_reason TEXT NULL,
    issued_by_user TEXT NOT NULL,
    created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_policy_claims_type ON policy_claims(claim_type);
CREATE INDEX IF NOT EXISTS idx_policy_claims_scope ON policy_claims(scope_addon_id, scope_namespace);

CREATE TABLE IF NOT EXISTS policy_claim_signatures (
    claim_id TEXT NOT NULL REFERENCES policy_claims(claim_id) ON DELETE CASCADE,
    signer_role TEXT NOT NULL,
    signer_user TEXT NOT NULL,
    signed_at TEXT NOT NULL,
    signature_b64 TEXT NULL,
    PRIMARY KEY (claim_id, signer_role, signer_user)
);
CREATE INDEX IF NOT EXISTS idx_policy_claim_sig_claim ON policy_claim_signatures(claim_id);
"#;

// F1c P3 — per-addon per-namespace HNSW vector index registry. The on-disk
// HNSW file lives at `file_path` (`<HOME>/.tentaflow/addons/<addon_id>/vectors/
// <namespace>.usearch`); this table mirrors the (addon_id, namespace) pair to
// the on-disk file plus the index geometry (`dim`, `metric`) so the namespace
// can be reopened after process restart without consulting the manifest.
// `count` is a best-effort cache updated on each upsert/delete — the
// authoritative size lives inside usearch.
const ADDON_VECTOR_NAMESPACES: &str = r#"
CREATE TABLE IF NOT EXISTS addon_vector_namespaces (
    addon_id TEXT NOT NULL,
    namespace TEXT NOT NULL,
    dim INTEGER NOT NULL CHECK(dim >= 1 AND dim <= 4096),
    metric TEXT NOT NULL CHECK(metric IN ('cosine', 'euclidean', 'dot')),
    count INTEGER NOT NULL DEFAULT 0,
    file_path TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (addon_id, namespace)
);
CREATE INDEX IF NOT EXISTS idx_addon_vector_ns_addon ON addon_vector_namespaces(addon_id);
"#;

// v52 — declared metadata field schema for a vector namespace, stored as a JSON
// array of {name, type, indexed} (the universal FieldSpec). Persisting it lets
// any access path (get / get_or_create) reconstruct the backend with the right
// column types, and lets reconciliation diff the manifest against the live
// collection on addon update. Default '[]' = no metadata fields (back-compat).
const ADDON_VECTOR_NAMESPACES_FIELDS: &str = r#"
ALTER TABLE addon_vector_namespaces ADD COLUMN fields_json TEXT NOT NULL DEFAULT '[]';
"#;

// v53 — whether the namespace carries a sparse vector field (hybrid search).
// Fixed at namespace creation: the backend collection gets a sparse column only
// when this is 1. 0 = dense-only (default, back-compat).
const ADDON_VECTOR_NAMESPACES_SPARSE: &str = r#"
ALTER TABLE addon_vector_namespaces ADD COLUMN sparse INTEGER NOT NULL DEFAULT 0;
"#;

// F1c P2 — admin-managed allowlist of Ed25519 public keys that may sign
// addon UI bundles. `key_b64` is the canonical 44-char base64 form (32 raw
// bytes). The table is intentionally NOT seeded: an empty trust store means
// no externally-published UI addon installs until an operator explicitly
// runs `tentaflow-cli addon trust-key`. `added_by_user` is reserved for the
// future RBAC subject (F1c P7); NULL for keys added pre-RBAC or via CLI
// without an authenticated session.
const TRUSTED_PUBLISHERS: &str = r#"
CREATE TABLE IF NOT EXISTS trusted_publishers (
    key_b64 TEXT PRIMARY KEY,
    label TEXT NOT NULL,
    added_at TEXT NOT NULL,
    added_by_user TEXT NULL,
    contact TEXT NULL
);
CREATE INDEX IF NOT EXISTS idx_trusted_publishers_label ON trusted_publishers(label);
"#;

// F1b P4 — Merkle hash chain for `audit_log` (DoD-15). Adds two BLOB columns:
//   - `prev_hash` (32 B) — copy of the previous row's `hash`, or NULL when
//     the chain has not started yet (pre-P4 legacy rows).
//   - `hash` (32 B) — `SHA256(canonical(row) || prev_hash)`.
//
// Existing F1a / pre-P4 rows keep NULL in both columns — `audit/verify.rs`
// counts them as `legacy_unchained` so a verify-after-upgrade run does not
// flag the entire pre-upgrade history as tampered. Every new row written
// through `audit_log_with_risk` after this migration MUST populate both
// columns.
//
// Idempotent via PRAGMA table_info — re-running the migration on a DB that
// already has the columns (e.g. partial earlier run that committed the
// _migrations row separately) is a no-op.
fn audit_log_add_merkle_chain_columns(conn: &Connection) -> Result<()> {
    let mut stmt = conn.prepare("PRAGMA table_info(audit_log)")?;
    let mut rows = stmt.query([])?;
    let mut has_prev_hash = false;
    let mut has_hash = false;
    while let Some(row) = rows.next()? {
        let name: String = row.get(1)?;
        if name == "prev_hash" {
            has_prev_hash = true;
        } else if name == "hash" {
            has_hash = true;
        }
    }
    drop(rows);
    drop(stmt);

    if !has_prev_hash {
        conn.execute_batch("ALTER TABLE audit_log ADD COLUMN prev_hash BLOB NULL;")?;
    }
    if !has_hash {
        conn.execute_batch("ALTER TABLE audit_log ADD COLUMN hash BLOB NULL;")?;
    }
    Ok(())
}

// F1b P3.C-2 — add a nullable `source_node_id` column to `frame_pickup_log`
// so the pickup handler can record which peer's HMAC key validated the
// token (NULL when the token verified locally). The audit query
// "from which node was this frame fetched?" needs the column even though
// SQLite has no easy `ADD COLUMN IF NOT EXISTS` — we read PRAGMA
// table_info first and skip the ALTER when the column already exists so
// the migration is idempotent if a partial earlier run committed the
// _migrations row separately from the ALTER (or if an operator added
// the column out of band).
fn frame_pickup_log_add_source_node_id(conn: &Connection) -> Result<()> {
    let mut stmt = conn.prepare("PRAGMA table_info(frame_pickup_log)")?;
    let mut rows = stmt.query([])?;
    let mut has_col = false;
    while let Some(row) = rows.next()? {
        let name: String = row.get(1)?;
        if name == "source_node_id" {
            has_col = true;
            break;
        }
    }
    drop(rows);
    drop(stmt);
    if !has_col {
        conn.execute_batch("ALTER TABLE frame_pickup_log ADD COLUMN source_node_id TEXT NULL;")?;
    }
    Ok(())
}

// F1a M1.W8 — TentaVision recording manager registry. One row per artifact
// (snapshot PNG or segment MP4) saved by an addon via `recording_save_*_v1`
// host functions. `ref` is the public addon-facing identifier
// (`snap_<uuid>` / `clip_<uuid>`). `file_path` is the absolute on-disk
// location under `~/.tentaflow/recordings/<camera_id>/{snapshots,segments}/`.
// `hash_sha256` is content hash for integrity / dedup, `retention_class` is
// copied from `cameras.retention_class` at save time (audit chain). F1a does
// no automatic purge — `purged_at` is set by `recording_purge_v1`.
const RECORDINGS_TABLE: &str = r#"
CREATE TABLE recordings (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    ref TEXT NOT NULL,
    kind TEXT NOT NULL CHECK(kind IN ('snapshot','segment')),
    owner_addon_id TEXT NOT NULL,
    camera_id TEXT NOT NULL,
    file_path TEXT NOT NULL,
    file_size_bytes INTEGER NOT NULL,
    duration_ms INTEGER NULL,
    width INTEGER NULL,
    height INTEGER NULL,
    pixel_format TEXT NULL,
    hash_sha256 TEXT NOT NULL,
    retention_class TEXT NOT NULL CHECK(retention_class IN ('A','B','C','Unclassified')),
    created_at INTEGER NOT NULL,
    purged_at INTEGER NULL
);
CREATE UNIQUE INDEX idx_recordings_ref_active ON recordings(ref) WHERE purged_at IS NULL;
CREATE INDEX idx_recordings_owner ON recordings(owner_addon_id, purged_at);
CREATE INDEX idx_recordings_camera ON recordings(camera_id, purged_at);
"#;

// F1a M1.W6 — TentaVision camera ingest registry. One row per camera owned
// by an addon. F1a only supports `fake_file` vendor (mp4 loop via GStreamer
// filesrc). `credentials_encrypted` carries opaque AES-GCM blob for vendors
// that need auth (unused for fake_file). `fps_actual` + `last_frame_at`
// expose health snapshot without a separate timeseries table.
const CAMERAS_TABLE: &str = r#"
CREATE TABLE cameras (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    camera_id TEXT NOT NULL,
    owner_addon_id TEXT NOT NULL,
    display_name TEXT NOT NULL,
    vendor TEXT NOT NULL CHECK(vendor IN ('fake_file')),
    url TEXT NOT NULL,
    credentials_encrypted BLOB NULL,
    profile TEXT NOT NULL DEFAULT 'default',
    target_fps INTEGER NOT NULL DEFAULT 30 CHECK(target_fps > 0 AND target_fps <= 60),
    resolution_width INTEGER NULL,
    resolution_height INTEGER NULL,
    retention_class TEXT NOT NULL DEFAULT 'C' CHECK(retention_class IN ('A','B','C','Unclassified')),
    status TEXT NOT NULL DEFAULT 'offline' CHECK(status IN ('offline','online','error','starting','stopping')),
    status_message TEXT NULL,
    fps_actual REAL NULL,
    last_frame_at INTEGER NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    removed_at INTEGER NULL
);
CREATE UNIQUE INDEX idx_cameras_camera_id_active ON cameras(camera_id) WHERE removed_at IS NULL;
CREATE INDEX idx_cameras_owner ON cameras(owner_addon_id, removed_at);
CREATE INDEX idx_cameras_status ON cameras(status, removed_at);
"#;

// F1b P1.A — extend `cameras.vendor` CHECK to allow `rtsp` and `onvif` next to
// the existing `fake_file`. SQLite cannot ALTER a CHECK constraint in-place,
// so we rebuild the table: create `cameras_new` with the new CHECK, copy rows
// 1:1, drop the old table, rename, recreate indexes. Foreign keys are turned
// off during the rebuild (SQLite requirement for safe table swap) and
// re-enabled at the end. `DROP TABLE IF EXISTS cameras_new` guards against a
// partial earlier run leaving the scratch table behind.
const CAMERAS_VENDOR_CHECK_RTSP_ONVIF: &str = r#"
PRAGMA foreign_keys = OFF;

DROP TABLE IF EXISTS cameras_new;

CREATE TABLE cameras_new (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    camera_id TEXT NOT NULL,
    owner_addon_id TEXT NOT NULL,
    display_name TEXT NOT NULL,
    vendor TEXT NOT NULL CHECK(vendor IN ('fake_file', 'rtsp', 'onvif')),
    url TEXT NOT NULL,
    credentials_encrypted BLOB NULL,
    profile TEXT NOT NULL DEFAULT 'default',
    target_fps INTEGER NOT NULL DEFAULT 30 CHECK(target_fps > 0 AND target_fps <= 60),
    resolution_width INTEGER NULL,
    resolution_height INTEGER NULL,
    retention_class TEXT NOT NULL DEFAULT 'C' CHECK(retention_class IN ('A','B','C','Unclassified')),
    status TEXT NOT NULL DEFAULT 'offline' CHECK(status IN ('offline','online','error','starting','stopping')),
    status_message TEXT NULL,
    fps_actual REAL NULL,
    last_frame_at INTEGER NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    removed_at INTEGER NULL
);

INSERT INTO cameras_new (
    id, camera_id, owner_addon_id, display_name, vendor, url,
    credentials_encrypted, profile, target_fps, resolution_width,
    resolution_height, retention_class, status, status_message,
    fps_actual, last_frame_at, created_at, updated_at, removed_at
)
SELECT
    id, camera_id, owner_addon_id, display_name, vendor, url,
    credentials_encrypted, profile, target_fps, resolution_width,
    resolution_height, retention_class, status, status_message,
    fps_actual, last_frame_at, created_at, updated_at, removed_at
FROM cameras;

DROP TABLE cameras;
ALTER TABLE cameras_new RENAME TO cameras;

CREATE UNIQUE INDEX idx_cameras_camera_id_active ON cameras(camera_id) WHERE removed_at IS NULL;
CREATE INDEX idx_cameras_owner ON cameras(owner_addon_id, removed_at);
CREATE INDEX idx_cameras_status ON cameras(status, removed_at);

PRAGMA foreign_keys = ON;
"#;

const CAMERAS_VENDOR_CHECK_LOCAL_SOURCES: &str = r#"
PRAGMA foreign_keys = OFF;

DROP TABLE IF EXISTS cameras_new;

CREATE TABLE cameras_new (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    camera_id TEXT NOT NULL,
    owner_addon_id TEXT NOT NULL,
    display_name TEXT NOT NULL,
    vendor TEXT NOT NULL CHECK(vendor IN ('fake_file', 'rtsp', 'onvif', 'local_camera', 'v4l2')),
    url TEXT NOT NULL,
    credentials_encrypted BLOB NULL,
    profile TEXT NOT NULL DEFAULT 'default',
    target_fps INTEGER NOT NULL DEFAULT 30 CHECK(target_fps > 0 AND target_fps <= 60),
    resolution_width INTEGER NULL,
    resolution_height INTEGER NULL,
    retention_class TEXT NOT NULL DEFAULT 'C' CHECK(retention_class IN ('A','B','C','Unclassified')),
    status TEXT NOT NULL DEFAULT 'offline' CHECK(status IN ('offline','online','error','starting','stopping')),
    status_message TEXT NULL,
    fps_actual REAL NULL,
    last_frame_at INTEGER NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    removed_at INTEGER NULL,
    onvif_url TEXT NULL,
    onvif_profile_token TEXT NULL,
    metadata_supported INTEGER NOT NULL DEFAULT 0 CHECK(metadata_supported IN (0,1)),
    org_id TEXT NOT NULL DEFAULT 'org-default'
);

INSERT INTO cameras_new (
    id, camera_id, owner_addon_id, display_name, vendor, url,
    credentials_encrypted, profile, target_fps, resolution_width,
    resolution_height, retention_class, status, status_message,
    fps_actual, last_frame_at, created_at, updated_at, removed_at,
    onvif_url, onvif_profile_token, metadata_supported, org_id
)
SELECT
    id, camera_id, owner_addon_id, display_name, vendor, url,
    credentials_encrypted, profile, target_fps, resolution_width,
    resolution_height, retention_class, status, status_message,
    fps_actual, last_frame_at, created_at, updated_at, removed_at,
    onvif_url, onvif_profile_token, metadata_supported,
    COALESCE(org_id, 'org-default')
FROM cameras;

DROP TABLE cameras;
ALTER TABLE cameras_new RENAME TO cameras;

CREATE UNIQUE INDEX idx_cameras_camera_id_active ON cameras(camera_id) WHERE removed_at IS NULL;
CREATE INDEX idx_cameras_owner ON cameras(owner_addon_id, removed_at);
CREATE INDEX idx_cameras_status ON cameras(status, removed_at);
CREATE INDEX idx_cameras_org_id ON cameras(org_id);

PRAGMA foreign_keys = ON;
"#;

// Adds the 'webrtc' vendor (robot/device backed cameras) to the CHECK. Rebuilds
// the table because SQLite cannot alter a CHECK in place. Schema mirrors the
// CURRENT cameras table: CAMERAS_VENDOR_CHECK_LOCAL_SOURCES + the later
// analysis_fps (v78) and analysis_flow_id (v79) columns. Ungated by feature —
// the schema must be identical regardless of build features.
const CAMERAS_VENDOR_CHECK_WEBRTC: &str = r#"
PRAGMA foreign_keys = OFF;

DROP TABLE IF EXISTS cameras_new;

CREATE TABLE cameras_new (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    camera_id TEXT NOT NULL,
    owner_addon_id TEXT NOT NULL,
    display_name TEXT NOT NULL,
    vendor TEXT NOT NULL CHECK(vendor IN ('fake_file', 'rtsp', 'onvif', 'local_camera', 'v4l2', 'webrtc')),
    url TEXT NOT NULL,
    credentials_encrypted BLOB NULL,
    profile TEXT NOT NULL DEFAULT 'default',
    target_fps INTEGER NOT NULL DEFAULT 30 CHECK(target_fps > 0 AND target_fps <= 60),
    resolution_width INTEGER NULL,
    resolution_height INTEGER NULL,
    retention_class TEXT NOT NULL DEFAULT 'C' CHECK(retention_class IN ('A','B','C','Unclassified')),
    status TEXT NOT NULL DEFAULT 'offline' CHECK(status IN ('offline','online','error','starting','stopping')),
    status_message TEXT NULL,
    fps_actual REAL NULL,
    last_frame_at INTEGER NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    removed_at INTEGER NULL,
    onvif_url TEXT NULL,
    onvif_profile_token TEXT NULL,
    metadata_supported INTEGER NOT NULL DEFAULT 0 CHECK(metadata_supported IN (0,1)),
    org_id TEXT NOT NULL DEFAULT 'org-default',
    analysis_fps INTEGER NOT NULL DEFAULT 10,
    analysis_flow_id TEXT NULL
);

INSERT INTO cameras_new (
    id, camera_id, owner_addon_id, display_name, vendor, url,
    credentials_encrypted, profile, target_fps, resolution_width,
    resolution_height, retention_class, status, status_message,
    fps_actual, last_frame_at, created_at, updated_at, removed_at,
    onvif_url, onvif_profile_token, metadata_supported, org_id,
    analysis_fps, analysis_flow_id
)
SELECT
    id, camera_id, owner_addon_id, display_name, vendor, url,
    credentials_encrypted, profile, target_fps, resolution_width,
    resolution_height, retention_class, status, status_message,
    fps_actual, last_frame_at, created_at, updated_at, removed_at,
    onvif_url, onvif_profile_token, metadata_supported,
    COALESCE(org_id, 'org-default'), analysis_fps, analysis_flow_id
FROM cameras;

DROP TABLE cameras;
ALTER TABLE cameras_new RENAME TO cameras;

CREATE UNIQUE INDEX idx_cameras_camera_id_active ON cameras(camera_id) WHERE removed_at IS NULL;
CREATE INDEX idx_cameras_owner ON cameras(owner_addon_id, removed_at);
CREATE INDEX idx_cameras_status ON cameras(status, removed_at);
CREATE INDEX idx_cameras_org_id ON cameras(org_id);

PRAGMA foreign_keys = ON;
"#;

// F1a §6.6 v0.6.0 — readonly aliases per Chunk C decision. Permission was
// renamed from `alias.manage` (rollback removed CRUD ABI) to `alias.read`.
// Idempotent UPDATEs touch only rows whose string column literally stores
// `alias.manage`; `addon_declared_permissions.permission_type` uses the
// same string semantics as the other catalogs (manifest [[permission]].id).
const RENAME_ALIAS_MANAGE_TO_READ: &str = r#"
UPDATE addon_permissions
   SET permission_id = 'alias.read'
 WHERE permission_id = 'alias.manage';
UPDATE addon_permission_defaults
   SET permission_id = 'alias.read'
 WHERE permission_id = 'alias.manage';
UPDATE addon_permission_catalog
   SET permission_id = 'alias.read'
 WHERE permission_id = 'alias.manage';
UPDATE addon_declared_permissions
   SET permission_type = 'alias.read'
 WHERE permission_type = 'alias.manage';
"#;

// F1a §6.6 v0.6.0 Chunk C — per-alias visibility scope.
// Three levels: `private` (only owner addon may resolve), `restricted`
// (whitelist in `model_alias_consumers`), `public` (any addon may resolve).
// PK = alias_id (1:1 with model_aliases). Default `private` from manifest
// is set explicitly at install time; this CHECK has no DEFAULT so writes
// must declare visibility.
const MODEL_ALIAS_VISIBILITY: &str = r#"
CREATE TABLE model_alias_visibility (
    alias_id INTEGER PRIMARY KEY REFERENCES model_aliases(id) ON DELETE CASCADE,
    visibility TEXT NOT NULL CHECK(visibility IN ('private','restricted','public')),
    updated_at INTEGER NOT NULL,
    updated_by_user_id INTEGER NULL
);
"#;

// F1a §6.6 v0.6.0 Chunk C — explicit consumer whitelist for `restricted`
// aliases. Owner declares `allowed_consumers = [...]` in manifest; install
// writes one row per consumer with `granted_by_user_id = NULL` (auto from
// manifest). Admin can later add/remove rows via M16b. PK guarantees one
// row per (alias, consumer) pair.
const MODEL_ALIAS_CONSUMERS: &str = r#"
CREATE TABLE model_alias_consumers (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    alias_id INTEGER NOT NULL REFERENCES model_aliases(id) ON DELETE CASCADE,
    consumer_addon_id TEXT NOT NULL,
    granted_by_user_id INTEGER NULL,
    granted_at INTEGER NOT NULL,
    revoked_at INTEGER NULL,
    UNIQUE(alias_id, consumer_addon_id)
);
CREATE INDEX idx_alias_consumers_lookup ON model_alias_consumers(consumer_addon_id, alias_id);
"#;

// F1a §6.6 v0.6.0 Chunk C — per-model visibility. Two levels only
// (`restricted` default, `public`). `model_id` is a free-form TEXT key —
// no FK because there is no `models` table in v0.6.0; the registry of
// "known model ids" lives in services + manual config. `restricted` is
// the default at the SQL layer so unknown models cannot be reached by
// addons without explicit grant.
const MODEL_VISIBILITY: &str = r#"
CREATE TABLE model_visibility (
    model_id TEXT PRIMARY KEY,
    visibility TEXT NOT NULL CHECK(visibility IN ('restricted','public')) DEFAULT 'restricted',
    updated_at INTEGER NOT NULL,
    updated_by_user_id INTEGER NULL
);
"#;

// F1a §6.6 v0.6.0 Chunk C — model consumer whitelist (symmetric to
// `model_alias_consumers`). `model_id` TEXT free-form (no FK).
const MODEL_CONSUMERS: &str = r#"
CREATE TABLE model_consumers (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    model_id TEXT NOT NULL,
    consumer_addon_id TEXT NOT NULL,
    granted_by_user_id INTEGER NULL,
    granted_at INTEGER NOT NULL,
    revoked_at INTEGER NULL,
    UNIQUE(model_id, consumer_addon_id)
);
CREATE INDEX idx_model_consumers_lookup ON model_consumers(consumer_addon_id, model_id);
"#;

// F1a §6.6 v0.6.0 Chunk C — consumer-side declaration `[[uses_alias]]`.
// `alias_target_name` stores the alias name (not id) because a consumer
// can declare its intent to use an alias BEFORE that alias' owner addon
// is installed; the row then stays `pending` until reconciliation runs
// at owner install time. Index `(alias_target_name, grant_status)` is
// hit by reconcile lookups; `(addon_id, grant_status)` by the resolver
// permission gate.
const ADDON_USES_ALIAS: &str = r#"
CREATE TABLE addon_uses_alias (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    addon_id TEXT NOT NULL,
    alias_target_name TEXT NOT NULL,
    required INTEGER NOT NULL CHECK(required IN (0,1)),
    reason TEXT NOT NULL,
    grant_status TEXT NOT NULL CHECK(grant_status IN ('pending','granted','denied','auto_granted')),
    grant_decided_at INTEGER NULL,
    grant_decided_by_user_id INTEGER NULL,
    created_at INTEGER NOT NULL,
    UNIQUE(addon_id, alias_target_name)
);
CREATE INDEX idx_addon_uses_alias_target ON addon_uses_alias(alias_target_name, grant_status);
CREATE INDEX idx_addon_uses_alias_addon ON addon_uses_alias(addon_id, grant_status);
"#;

// F1a §6.6 v0.6.0 Chunk C — consumer-side declaration `[[uses_model]]`.
// Same pending/reconcile pattern as `addon_uses_alias` but keyed on the
// free-form `model_id` string. `model_visibility` defaults to
// `restricted`, so unknown-model declarations stay `pending` until an
// admin explicitly grants (no auto-grant by absence of policy).
const ADDON_USES_MODEL: &str = r#"
CREATE TABLE addon_uses_model (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    addon_id TEXT NOT NULL,
    model_target_name TEXT NOT NULL,
    required INTEGER NOT NULL CHECK(required IN (0,1)),
    reason TEXT NOT NULL,
    grant_status TEXT NOT NULL CHECK(grant_status IN ('pending','granted','denied','auto_granted')),
    grant_decided_at INTEGER NULL,
    grant_decided_by_user_id INTEGER NULL,
    created_at INTEGER NOT NULL,
    UNIQUE(addon_id, model_target_name)
);
CREATE INDEX idx_addon_uses_model_target ON addon_uses_model(model_target_name, grant_status);
CREATE INDEX idx_addon_uses_model_addon ON addon_uses_model(addon_id, grant_status);
"#;

// After M1.W5: teams-bot declares aliases via [[alias]] manifest section.
// Hard-coded TEAMS_BOT_ALIASES const and activate/deactivate helpers were
// removed from addon/mod.rs. This migration backfills owner records for
// existing teams-bot aliases on already-deployed databases so the new
// owner-aware code path treats them correctly (start/stop activate/
// deactivate, uninstall preserves owner row for audit trail).
const TEAMS_BOT_ALIASES_OWNERSHIP_BACKFILL: &str = r#"
INSERT OR IGNORE INTO model_alias_owners (alias_id, owner_type, owner_id, created_at)
SELECT id, 'addon', 'teams-bot', datetime('now')
FROM model_aliases
WHERE alias IN ('teams-stt', 'teams-tts', 'teams-summary', 'teams-vision-face', 'teams-vision-emotion');
"#;

// F1a §6.5 — tabela powiazania aliasu z wlascicielem (addon lub manual).
// Pozwala odroznic aliasy stworzone automatycznie przez install addonu od
// tych wpisanych recznie przez admina (M1.W5 zacznie ja zasilac).
const MODEL_ALIAS_OWNERS: &str = r#"
CREATE TABLE model_alias_owners (
    alias_id INTEGER PRIMARY KEY REFERENCES model_aliases(id) ON DELETE CASCADE,
    owner_type TEXT NOT NULL CHECK(owner_type IN ('addon', 'manual')),
    owner_id TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX idx_alias_owners_addon ON model_alias_owners(owner_type, owner_id);
"#;

// F1a §6.5 — log wywolan aliasow AI. Kazdy alias_call (M1.W6) zapisuje
// rekord z target_used, request_id, fallback_chain_position; pozwala na
// debug fallback chain w UI M16 i metryki per alias.
const ALIAS_CALLS: &str = r#"
CREATE TABLE alias_calls (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    alias_id INTEGER NOT NULL REFERENCES model_aliases(id) ON DELETE CASCADE,
    alias_name TEXT NOT NULL,
    method TEXT,
    target_used TEXT NOT NULL,
    target_node_id TEXT,
    service_id TEXT,
    caller_addon_id TEXT,
    caller_user_id INTEGER,
    request_id TEXT,
    duration_ms INTEGER,
    payload_bytes INTEGER,
    response_bytes INTEGER,
    fallback_used INTEGER DEFAULT 0,
    fallback_chain_position INTEGER,
    result TEXT NOT NULL CHECK(result IN ('ok','error','no_target','timeout','permission_denied','gate_denied')),
    error_code TEXT,
    ts INTEGER NOT NULL
);
CREATE INDEX idx_alias_calls_alias_ts ON alias_calls(alias_id, ts);
CREATE INDEX idx_alias_calls_addon_ts ON alias_calls(caller_addon_id, ts);
CREATE INDEX idx_alias_calls_request_id ON alias_calls(request_id);
CREATE INDEX idx_alias_calls_fallback ON alias_calls(alias_id, fallback_used) WHERE fallback_used=1;
"#;

// F1a §6.5 — historia zmian aliasu (before/after snapshot, change_type,
// reason). UI M16 (alias detail panel) pokazuje audit trail; admin moze
// rollback przez wstawienie nowego rekordu z before_snapshot.
// Brak FK na model_aliases — alias mogl byc juz usuniety, ale historia
// musi pozostac (compliance F1a §6.2.Y).
const MODEL_ALIAS_CHANGES: &str = r#"
CREATE TABLE model_alias_changes (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    alias_id INTEGER NOT NULL,
    alias_name TEXT NOT NULL,
    changed_by_user_id INTEGER,
    changed_by_addon_id TEXT,
    before_snapshot TEXT,
    after_snapshot TEXT,
    change_type TEXT NOT NULL CHECK(change_type IN
        ('create','target_change','fallback_change','strategy_change',
         'activate','deactivate','delete','suggested_default_change')),
    reason TEXT,
    ts INTEGER NOT NULL
);
CREATE INDEX idx_alias_changes_alias ON model_alias_changes(alias_id);
CREATE INDEX idx_alias_changes_user_ts ON model_alias_changes(changed_by_user_id, ts);
"#;

// F1a §6.6 — change history for direct model access control, mirroring
// `model_alias_changes` for the model subtree. Records visibility flips and
// consumer grant/revoke events as before/after JSON snapshots so the admin
// Access panel can render an audit trail (compliance F1a §6.2.Y).
// `model_id` is free-form TEXT (no `models` table to FK against); no FK on
// the changer either (audit row must survive addon/user deletion).
const MODEL_VISIBILITY_CHANGES: &str = r#"
CREATE TABLE model_visibility_changes (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    model_id TEXT NOT NULL,
    changed_by_user_id INTEGER,
    changed_by_addon_id TEXT,
    before_snapshot TEXT,
    after_snapshot TEXT,
    change_type TEXT NOT NULL CHECK(change_type IN
        ('visibility_change','consumer_grant','consumer_revoke')),
    reason TEXT,
    ts INTEGER NOT NULL
);
CREATE INDEX idx_model_visibility_changes_model ON model_visibility_changes(model_id);
CREATE INDEX idx_model_visibility_changes_user_ts ON model_visibility_changes(changed_by_user_id, ts);
"#;

// F1a §6.5 — wykonanie migracji per-addon SQL storage. PRIMARY KEY
// (addon_id, migration_name) zapewnia idempotencje. Hash chroni przed
// "podmiana" tresci migracji po jej aplikacji.
const ADDON_MIGRATIONS_APPLIED: &str = r#"
CREATE TABLE addon_migrations_applied (
    addon_id TEXT NOT NULL,
    migration_name TEXT NOT NULL,
    migration_hash TEXT NOT NULL,
    applied_at TEXT NOT NULL DEFAULT (datetime('now')),
    applied_in_addon_version TEXT NOT NULL,
    status TEXT NOT NULL CHECK(status IN ('success', 'failed', 'partial')),
    error_message TEXT,
    duration_ms INTEGER,
    PRIMARY KEY (addon_id, migration_name)
);
CREATE INDEX idx_addon_migrations_status ON addon_migrations_applied(addon_id, status);
"#;

// F1a §6.5 — log pickupow surowych ramek (frame_ref) przez serwisy AI.
// Token zawarty w frame_ref ma TTL; rdzen weryfikuje go przy pickupie
// i loguje wynik (ok / token_invalid / token_expired / frame_purged /
// unauthorized). UI compliance M22 pokazuje time-to-pickup.
const FRAME_PICKUP_LOG: &str = r#"
CREATE TABLE frame_pickup_log (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    raw_frame_ref TEXT NOT NULL,
    service_id TEXT NOT NULL,
    caller_addon_id TEXT,
    request_id TEXT NOT NULL,
    picked_up_at INTEGER NOT NULL,
    result TEXT NOT NULL CHECK(result IN ('ok','token_invalid','token_expired','frame_purged','unauthorized'))
);
CREATE INDEX idx_frame_pickup_ref ON frame_pickup_log(raw_frame_ref);
CREATE INDEX idx_frame_pickup_request ON frame_pickup_log(request_id);
CREATE INDEX idx_frame_pickup_service_ts ON frame_pickup_log(service_id, picked_up_at);
"#;

// Rozszerzenie audit_log o pola wymagane przez F1a §6.2.Y:
// - risk_class — klasyfikacja RODO (A/B/C/unclassified); wpisy klasy B/C maja
//   indeks partial dla szybkich kwerend zgodnosciowych.
// - related_claim_id — powiazanie wpisu z claim (gate evaluation, F2).
// - request_id — korelacja wielu wpisow w obrebie jednego wywolania service_call
//   lub spans flow execution.
// SQLite nie wspiera CHECK przy ALTER TABLE — walidacja po stronie Rust w
// audit/mod.rs (RiskClass enum).
const AUDIT_LOG_RISK_CLASS: &str = r#"
ALTER TABLE audit_log ADD COLUMN risk_class TEXT NOT NULL DEFAULT 'unclassified';
ALTER TABLE audit_log ADD COLUMN related_claim_id TEXT;
ALTER TABLE audit_log ADD COLUMN request_id TEXT;
CREATE INDEX idx_audit_risk_class ON audit_log(risk_class) WHERE risk_class IN ('B','C');
CREATE INDEX idx_audit_claim ON audit_log(related_claim_id) WHERE related_claim_id IS NOT NULL;
CREATE INDEX idx_audit_request_id ON audit_log(request_id);
"#;

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

// deployed_source_hash: sha256 drzewa zrodel bundla (docker/native) z momentu
// deployu. build.rs liczy aktualny hash do manifestu; snapshot porownuje oba i
// wystawia flage update_available. Pusty = serwis embedded/external lub deploy
// sprzed tej kolumny (brak danych, brak falszywego alarmu o aktualizacji).
const SERVICES_DEPLOYED_SOURCE_HASH: &str = r#"
ALTER TABLE services ADD COLUMN deployed_source_hash TEXT NOT NULL DEFAULT '';
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
    status TEXT NOT NULL DEFAULT 'deploying',
    phase TEXT NOT NULL DEFAULT '',
    progress_pct INTEGER NOT NULL DEFAULT 0,
    image_tag TEXT NOT NULL DEFAULT '',
    container_name TEXT NOT NULL DEFAULT '',
    config_json TEXT NOT NULL DEFAULT '{}',
    user_id TEXT,
    started_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    finished_at TIMESTAMP,
    error_message TEXT,
    log_tail TEXT NOT NULL DEFAULT ''
);
CREATE INDEX idx_deployments_deploy_id ON deployments(deploy_id);
CREATE INDEX idx_deployments_engine ON deployments(engine_id);
"#;

const DEPLOYMENT_JOBS_AS_SERVICES: &str = r#"
CREATE TABLE services_new (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    engine_id TEXT NOT NULL,
    category TEXT NOT NULL,
    display_name TEXT NOT NULL,
    deploy_method TEXT NOT NULL CHECK(deploy_method IN ('docker','native_embedded','native_binary','native_python_bundle','external')),
    transport TEXT NOT NULL CHECK(transport IN ('embedded','http_direct','sidecar_quic','external_http')),
    status TEXT NOT NULL CHECK(status IN ('deploying','starting','running','degraded','failed','stopped','interrupted')) DEFAULT 'starting',
    pinned INTEGER NOT NULL DEFAULT 0,
    paused INTEGER NOT NULL DEFAULT 0,
    runtime_pid INTEGER,
    runtime_port INTEGER,
    sidecar_quic_port INTEGER,
    endpoint_url TEXT,
    config_json TEXT NOT NULL DEFAULT '{}',
    active_deploy_id TEXT NOT NULL DEFAULT '',
    last_deploy_id TEXT NOT NULL DEFAULT '',
    deployment_progress_pct INTEGER NOT NULL DEFAULT 0,
    health_last_ok TIMESTAMP,
    health_last_err TEXT,
    progress_message TEXT,
    restart_count INTEGER NOT NULL DEFAULT 0,
    created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
);

INSERT INTO services_new (
    id, engine_id, category, display_name, deploy_method, transport, status,
    pinned, paused, runtime_pid, runtime_port, sidecar_quic_port, endpoint_url,
    config_json, active_deploy_id, last_deploy_id, deployment_progress_pct,
    health_last_ok, health_last_err, progress_message,
    restart_count, created_at, updated_at
)
SELECT
    id, engine_id, category, display_name, deploy_method, transport, status,
    pinned, paused, runtime_pid, runtime_port, sidecar_quic_port, endpoint_url,
    config_json, '', '', 0, health_last_ok, health_last_err, progress_message,
    restart_count, created_at, updated_at
FROM services;

DROP TABLE services;
ALTER TABLE services_new RENAME TO services;
CREATE INDEX idx_services_status ON services(status);
CREATE INDEX idx_services_engine ON services(engine_id);
CREATE INDEX idx_services_category ON services(category);
CREATE INDEX idx_services_active_deploy ON services(active_deploy_id);

ALTER TABLE deployments ADD COLUMN target_service_id INTEGER;
ALTER TABLE deployments ADD COLUMN resume_policy TEXT NOT NULL DEFAULT 'manual';
ALTER TABLE deployments ADD COLUMN resume_attempts INTEGER NOT NULL DEFAULT 0;
ALTER TABLE deployments ADD COLUMN updated_at TIMESTAMP NOT NULL DEFAULT '';

UPDATE deployments
   SET status = CASE status
       WHEN 'queued' THEN 'deploying'
       WHEN 'building' THEN 'deploying'
       WHEN 'running' THEN 'deploying'
       WHEN 'failure' THEN 'failed'
       ELSE status
   END,
       updated_at = COALESCE(finished_at, started_at, datetime('now'));

CREATE INDEX IF NOT EXISTS idx_deployments_status ON deployments(status);
CREATE INDEX IF NOT EXISTS idx_deployments_node ON deployments(node_id);
CREATE INDEX IF NOT EXISTS idx_deployments_target_service ON deployments(target_service_id);
"#;

// SQLite nie pozwala na ALTER TABLE dla CHECK constraintu — robimy klasyczne
// rebuild-via-temp-table. flow_executions to log historii, mozna stracic
// rzedy ktore i tak juz padly na CHECK (status='completed' nigdy do bazy
// nie trafil).
const FLOW_EXECUTIONS_ALLOW_COMPLETED: &str = r#"
CREATE TABLE flow_executions_new (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    flow_id TEXT NOT NULL REFERENCES flows(id),
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
    owner_user_id TEXT
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
    id TEXT PRIMARY KEY NOT NULL,
    name TEXT NOT NULL,
    description TEXT,
    version INTEGER DEFAULT 1,
    is_default INTEGER NOT NULL DEFAULT 0,
    service_type TEXT,
    flow_json TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'draft' CHECK(status IN ('draft','active','decoded')),
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
    id TEXT PRIMARY KEY NOT NULL,
    flow_id TEXT NOT NULL REFERENCES flows(id) ON DELETE CASCADE,
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
    flow_id TEXT NOT NULL REFERENCES flows(id),
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

CREATE TABLE user_accounts (
    id TEXT PRIMARY KEY NOT NULL,
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
    role TEXT NOT NULL DEFAULT 'user',
    preferred_language TEXT
);

CREATE TABLE user_groups (
    id TEXT PRIMARY KEY NOT NULL,
    name TEXT NOT NULL UNIQUE,
    description TEXT DEFAULT '',
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE group_members (
    group_id TEXT NOT NULL REFERENCES user_groups(id) ON DELETE CASCADE,
    user_id TEXT NOT NULL REFERENCES user_accounts(id) ON DELETE CASCADE,
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
    default_group_id TEXT REFERENCES user_groups(id),
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
    user_id TEXT,
    key TEXT NOT NULL,
    value_encrypted TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE(addon_id, user_id, key)
);

CREATE TABLE audit_log (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    timestamp TEXT NOT NULL DEFAULT (datetime('now')),
    user_id TEXT,
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
    group_id TEXT REFERENCES user_groups(id) ON DELETE CASCADE,
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
    updated_by TEXT,
    PRIMARY KEY (addon_id, key)
);

CREATE TABLE addon_permissions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    addon_id TEXT NOT NULL,
    subject_type TEXT NOT NULL CHECK(subject_type IN ('user','group')),
    subject_id TEXT NOT NULL,
    permission_id TEXT NOT NULL,
    granted INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    grant_mode TEXT NOT NULL DEFAULT 'inherit'
        CHECK(grant_mode IN ('allow','deny','inherit')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_by TEXT REFERENCES user_accounts(id) ON DELETE SET NULL,
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
    created_by TEXT,
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
    approved_by TEXT,
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
    owner_user_id TEXT,
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
    id TEXT PRIMARY KEY NOT NULL,
    flow_id TEXT NOT NULL REFERENCES flows(id) ON DELETE CASCADE,
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
    updated_by TEXT REFERENCES user_accounts(id) ON DELETE SET NULL,
    UNIQUE(addon_id, permission_id)
);
CREATE INDEX idx_addon_perm_defaults_addon ON addon_permission_defaults(addon_id);

CREATE TABLE addon_visibility (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    addon_id TEXT NOT NULL,
    group_id TEXT NOT NULL REFERENCES user_groups(id) ON DELETE CASCADE,
    visible INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_by TEXT REFERENCES user_accounts(id) ON DELETE SET NULL,
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
    updated_by TEXT REFERENCES user_accounts(id) ON DELETE SET NULL,
    oauth_mode TEXT NOT NULL DEFAULT 'individual'
        CHECK(oauth_mode IN ('global','individual','none')),
    UNIQUE(addon_id, provider_id)
);
CREATE INDEX idx_addon_oauth_config_addon ON addon_oauth_config(addon_id);

CREATE TABLE oauth_pending_states (
    state TEXT PRIMARY KEY,
    user_id TEXT REFERENCES user_accounts(id) ON DELETE CASCADE,
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
    updated_by TEXT
);

CREATE TABLE user_oauth_accounts (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id TEXT REFERENCES user_accounts(id) ON DELETE CASCADE,
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
    user_id TEXT NOT NULL REFERENCES user_accounts(id) ON DELETE CASCADE,
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
    user_id TEXT NOT NULL REFERENCES user_accounts(id) ON DELETE CASCADE,
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
    subject_id TEXT NOT NULL,
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
    status TEXT NOT NULL CHECK(status IN ('deploying','starting','running','degraded','failed','stopped','interrupted')) DEFAULT 'starting',
    pinned INTEGER NOT NULL DEFAULT 0,
    paused INTEGER NOT NULL DEFAULT 0,
    runtime_pid INTEGER,
    runtime_port INTEGER,
    sidecar_quic_port INTEGER,
    endpoint_url TEXT,
    config_json TEXT NOT NULL DEFAULT '{}',
    active_deploy_id TEXT NOT NULL DEFAULT '',
    last_deploy_id TEXT NOT NULL DEFAULT '',
    deployment_progress_pct INTEGER NOT NULL DEFAULT 0,
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
    status TEXT NOT NULL DEFAULT 'deploying',
    phase TEXT NOT NULL DEFAULT '',
    progress_pct INTEGER NOT NULL DEFAULT 0,
    image_tag TEXT NOT NULL DEFAULT '',
    container_name TEXT NOT NULL DEFAULT '',
    config_json TEXT NOT NULL DEFAULT '{}',
    user_id TEXT,
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

INSERT INTO user_groups (id, name, description) VALUES
    ('00000000-0000-4000-8000-000000000001', 'admins', 'Administratorzy systemu');

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

const SCHEDULED_JOBS: &str = r#"
CREATE TABLE IF NOT EXISTS scheduled_jobs (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    enabled INTEGER NOT NULL DEFAULT 1,
    target_type TEXT NOT NULL,
    target_addon_id TEXT NOT NULL DEFAULT '',
    target_action_id TEXT NOT NULL DEFAULT '',
    payload_json TEXT NOT NULL DEFAULT '{}',
    schedule_kind TEXT NOT NULL,
    schedule_expr TEXT NOT NULL,
    timezone TEXT NOT NULL DEFAULT 'UTC',
    next_run_at TEXT,
    max_runtime_seconds INTEGER NOT NULL DEFAULT 1800,
    retry_policy_json TEXT NOT NULL DEFAULT '{"max_attempts":1,"backoff_seconds":60}',
    concurrency_policy TEXT NOT NULL DEFAULT 'skip',
    created_by_user_id TEXT,
    created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
);
CREATE INDEX IF NOT EXISTS idx_scheduled_jobs_due
    ON scheduled_jobs(enabled, next_run_at);
CREATE INDEX IF NOT EXISTS idx_scheduled_jobs_target
    ON scheduled_jobs(target_type, target_addon_id, target_action_id);

CREATE TABLE IF NOT EXISTS scheduled_runs (
    id TEXT PRIMARY KEY,
    job_id TEXT NOT NULL REFERENCES scheduled_jobs(id) ON DELETE CASCADE,
    status TEXT NOT NULL,
    scheduled_for TEXT NOT NULL,
    started_at TEXT,
    finished_at TEXT,
    result_json TEXT,
    error TEXT
);
CREATE INDEX IF NOT EXISTS idx_scheduled_runs_job
    ON scheduled_runs(job_id, scheduled_for DESC);
CREATE INDEX IF NOT EXISTS idx_scheduled_runs_status
    ON scheduled_runs(status);
"#;

/// Bazowy schemat scheduled_jobs (sprzed v89, bez kolumny org_id) — udostepniony testom
/// schedulera, by mogly zbudowac tabele bez calego init-chaina migracji.
#[cfg(test)]
pub fn scheduled_jobs_schema_for_test() -> &'static str {
    SCHEDULED_JOBS
}

const COMPLIANCE_CORE_FOUNDATION: &str = r#"
CREATE TABLE IF NOT EXISTS compliance_data_categories (
    category_id TEXT PRIMARY KEY,
    org_id TEXT NOT NULL REFERENCES organizations(org_id) ON DELETE CASCADE,
    slug TEXT NOT NULL,
    name_translations TEXT NOT NULL DEFAULT '{}',
    description_translations TEXT NOT NULL DEFAULT '{}',
    personal_data INTEGER NOT NULL DEFAULT 1 CHECK(personal_data IN (0,1)),
    sensitive_data INTEGER NOT NULL DEFAULT 0 CHECK(sensitive_data IN (0,1)),
    risk_class TEXT NOT NULL DEFAULT 'standard' CHECK(risk_class IN ('low','standard','high','critical')),
    source_scope TEXT NOT NULL DEFAULT 'core' CHECK(source_scope IN ('core','addon','external')),
    addon_id TEXT NULL,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    UNIQUE(org_id, slug),
    CHECK(json_valid(name_translations)),
    CHECK(json_valid(description_translations))
);
CREATE INDEX IF NOT EXISTS idx_compliance_data_categories_org
    ON compliance_data_categories(org_id, risk_class, personal_data);

CREATE TRIGGER IF NOT EXISTS compliance_data_categories_updated_at
AFTER UPDATE ON compliance_data_categories
FOR EACH ROW
BEGIN
    UPDATE compliance_data_categories
    SET updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
    WHERE category_id = NEW.category_id;
END;

CREATE TABLE IF NOT EXISTS compliance_processing_activities (
    activity_id TEXT PRIMARY KEY,
    org_id TEXT NOT NULL REFERENCES organizations(org_id) ON DELETE CASCADE,
    slug TEXT NOT NULL,
    name_translations TEXT NOT NULL DEFAULT '{}',
    purpose_translations TEXT NOT NULL DEFAULT '{}',
    controller_role TEXT NOT NULL DEFAULT 'controller'
        CHECK(controller_role IN ('controller','processor','joint_controller')),
    owner_user_id TEXT NULL REFERENCES user_accounts(id) ON DELETE SET NULL,
    system_scope TEXT NOT NULL DEFAULT 'core' CHECK(system_scope IN ('core','addon','external')),
    addon_id TEXT NULL,
    status TEXT NOT NULL DEFAULT 'active' CHECK(status IN ('draft','active','retired')),
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    UNIQUE(org_id, slug),
    CHECK(json_valid(name_translations)),
    CHECK(json_valid(purpose_translations))
);
CREATE INDEX IF NOT EXISTS idx_compliance_processing_activities_org
    ON compliance_processing_activities(org_id, status, system_scope);

CREATE TRIGGER IF NOT EXISTS compliance_processing_activities_updated_at
AFTER UPDATE ON compliance_processing_activities
FOR EACH ROW
BEGIN
    UPDATE compliance_processing_activities
    SET updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
    WHERE activity_id = NEW.activity_id;
END;

CREATE TABLE IF NOT EXISTS compliance_activity_categories (
    activity_id TEXT NOT NULL REFERENCES compliance_processing_activities(activity_id) ON DELETE CASCADE,
    category_id TEXT NOT NULL REFERENCES compliance_data_categories(category_id) ON DELETE CASCADE,
    PRIMARY KEY(activity_id, category_id)
);
CREATE INDEX IF NOT EXISTS idx_compliance_activity_categories_category
    ON compliance_activity_categories(category_id);

CREATE TABLE IF NOT EXISTS compliance_legal_basis (
    legal_basis_id TEXT PRIMARY KEY,
    org_id TEXT NOT NULL REFERENCES organizations(org_id) ON DELETE CASCADE,
    activity_id TEXT NULL REFERENCES compliance_processing_activities(activity_id) ON DELETE CASCADE,
    category_id TEXT NULL REFERENCES compliance_data_categories(category_id) ON DELETE CASCADE,
    basis_kind TEXT NOT NULL
        CHECK(basis_kind IN ('consent','contract','legal_obligation','vital_interests','public_task','legitimate_interest')),
    basis_reference TEXT NOT NULL DEFAULT '',
    description_translations TEXT NOT NULL DEFAULT '{}',
    is_active INTEGER NOT NULL DEFAULT 1 CHECK(is_active IN (0,1)),
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    CHECK(activity_id IS NOT NULL OR category_id IS NOT NULL),
    CHECK(json_valid(description_translations))
);
CREATE INDEX IF NOT EXISTS idx_compliance_legal_basis_lookup
    ON compliance_legal_basis(org_id, activity_id, category_id, is_active);

CREATE TRIGGER IF NOT EXISTS compliance_legal_basis_updated_at
AFTER UPDATE ON compliance_legal_basis
FOR EACH ROW
BEGIN
    UPDATE compliance_legal_basis
    SET updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
    WHERE legal_basis_id = NEW.legal_basis_id;
END;

CREATE TABLE IF NOT EXISTS compliance_retention_policies (
    retention_policy_id TEXT PRIMARY KEY,
    org_id TEXT NOT NULL REFERENCES organizations(org_id) ON DELETE CASCADE,
    slug TEXT NOT NULL,
    name_translations TEXT NOT NULL DEFAULT '{}',
    scope_kind TEXT NOT NULL CHECK(scope_kind IN ('audit','ai_audit','data_category','document','dsar','breach','general','agent_runs','events')),
    category_id TEXT NULL REFERENCES compliance_data_categories(category_id) ON DELETE SET NULL,
    retention_days INTEGER NOT NULL,
    minimum_days INTEGER NOT NULL DEFAULT 0,
    action_after_retention TEXT NOT NULL DEFAULT 'delete'
        CHECK(action_after_retention IN ('delete','anonymize','archive')),
    is_default INTEGER NOT NULL DEFAULT 0 CHECK(is_default IN (0,1)),
    is_active INTEGER NOT NULL DEFAULT 1 CHECK(is_active IN (0,1)),
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    CHECK(retention_days >= minimum_days),
    UNIQUE(org_id, slug),
    CHECK(json_valid(name_translations))
);
CREATE INDEX IF NOT EXISTS idx_compliance_retention_policies_lookup
    ON compliance_retention_policies(org_id, scope_kind, category_id, is_active);
CREATE UNIQUE INDEX IF NOT EXISTS idx_compliance_retention_one_default
    ON compliance_retention_policies(org_id, scope_kind)
    WHERE is_default = 1 AND category_id IS NULL AND is_active = 1;

CREATE TRIGGER IF NOT EXISTS compliance_retention_policies_updated_at
AFTER UPDATE ON compliance_retention_policies
FOR EACH ROW
BEGIN
    UPDATE compliance_retention_policies
    SET updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
    WHERE retention_policy_id = NEW.retention_policy_id;
END;

CREATE TABLE IF NOT EXISTS compliance_legal_holds (
    legal_hold_id TEXT PRIMARY KEY,
    org_id TEXT NOT NULL REFERENCES organizations(org_id) ON DELETE CASCADE,
    scope_kind TEXT NOT NULL CHECK(scope_kind IN ('audit','ai_audit','data_category','document','dsar','breach','resource','general')),
    scope_id TEXT NOT NULL DEFAULT '',
    reason TEXT NOT NULL,
    created_by_user_id TEXT NULL REFERENCES user_accounts(id) ON DELETE SET NULL,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    released_at TEXT NULL,
    released_by_user_id TEXT NULL REFERENCES user_accounts(id) ON DELETE SET NULL,
    release_reason TEXT NULL
);
CREATE INDEX IF NOT EXISTS idx_compliance_legal_holds_active
    ON compliance_legal_holds(org_id, scope_kind, scope_id, released_at);

CREATE TABLE IF NOT EXISTS compliance_documents (
    compliance_document_id TEXT PRIMARY KEY,
    org_id TEXT NOT NULL REFERENCES organizations(org_id) ON DELETE CASCADE,
    source_legal_document_id TEXT NULL REFERENCES legal_documents(id) ON DELETE SET NULL,
    document_type TEXT NOT NULL,
    title_translations TEXT NOT NULL DEFAULT '{}',
    status TEXT NOT NULL DEFAULT 'draft' CHECK(status IN ('draft','active','archived')),
    version INTEGER NOT NULL DEFAULT 1,
    artifact_path TEXT NOT NULL DEFAULT '',
    artifact_hash TEXT NOT NULL DEFAULT '',
    created_by_user_id TEXT NULL REFERENCES user_accounts(id) ON DELETE SET NULL,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    UNIQUE(org_id, document_type, version),
    CHECK(json_valid(title_translations))
);
CREATE INDEX IF NOT EXISTS idx_compliance_documents_org
    ON compliance_documents(org_id, document_type, status);

CREATE TRIGGER IF NOT EXISTS compliance_documents_updated_at
AFTER UPDATE ON compliance_documents
FOR EACH ROW
BEGIN
    UPDATE compliance_documents
    SET updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
    WHERE compliance_document_id = NEW.compliance_document_id;
END;

CREATE TABLE IF NOT EXISTS compliance_ai_events (
    event_id TEXT PRIMARY KEY,
    org_id TEXT NOT NULL REFERENCES organizations(org_id) ON DELETE CASCADE,
    user_id TEXT NULL REFERENCES user_accounts(id) ON DELETE SET NULL,
    node_id TEXT NOT NULL DEFAULT '',
    addon_id TEXT NULL,
    instance_id TEXT NULL,
    flow_id TEXT NULL REFERENCES flows(id) ON DELETE SET NULL,
    flow_node_id TEXT NULL,
    agent_id TEXT NULL,
    agent_run_id TEXT NULL,
    request_id TEXT NOT NULL,
    correlation_id TEXT NULL,
    model_id TEXT NOT NULL DEFAULT '',
    backend TEXT NOT NULL DEFAULT '',
    started_at TEXT NOT NULL,
    finished_at TEXT NULL,
    status TEXT NOT NULL CHECK(status IN ('running','success','failed','cancelled')),
    risk_class TEXT NOT NULL DEFAULT 'standard' CHECK(risk_class IN ('low','standard','high','critical')),
    legal_basis_id TEXT NULL REFERENCES compliance_legal_basis(legal_basis_id) ON DELETE SET NULL,
    retention_policy_id TEXT NOT NULL REFERENCES compliance_retention_policies(retention_policy_id) ON DELETE RESTRICT,
    prompt_hash TEXT NOT NULL DEFAULT '',
    response_hash TEXT NOT NULL DEFAULT '',
    audit_log_id INTEGER NULL REFERENCES audit_log(id) ON DELETE SET NULL,
    error_message TEXT NULL,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    UNIQUE(org_id, request_id)
);
CREATE INDEX IF NOT EXISTS idx_compliance_ai_events_org_started
    ON compliance_ai_events(org_id, started_at DESC);
CREATE INDEX IF NOT EXISTS idx_compliance_ai_events_user
    ON compliance_ai_events(org_id, user_id, started_at DESC);
CREATE INDEX IF NOT EXISTS idx_compliance_ai_events_addon
    ON compliance_ai_events(org_id, addon_id, started_at DESC);
CREATE INDEX IF NOT EXISTS idx_compliance_ai_events_status
    ON compliance_ai_events(org_id, status, started_at DESC);
CREATE INDEX IF NOT EXISTS idx_compliance_ai_events_agent_run
    ON compliance_ai_events(agent_run_id, started_at DESC);
CREATE INDEX IF NOT EXISTS idx_compliance_ai_events_correlation
    ON compliance_ai_events(correlation_id, started_at DESC);

CREATE TRIGGER IF NOT EXISTS compliance_ai_events_updated_at
AFTER UPDATE ON compliance_ai_events
FOR EACH ROW
BEGIN
    UPDATE compliance_ai_events
    SET updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
    WHERE event_id = NEW.event_id;
END;

CREATE TABLE IF NOT EXISTS compliance_ai_payloads (
    payload_id TEXT PRIMARY KEY,
    event_id TEXT NOT NULL REFERENCES compliance_ai_events(event_id) ON DELETE CASCADE,
    payload_kind TEXT NOT NULL CHECK(payload_kind IN ('prompt','response','system','tool_input','tool_output')),
    content_hash TEXT NOT NULL,
    content_text TEXT NOT NULL,
    content_redacted INTEGER NOT NULL DEFAULT 0 CHECK(content_redacted IN (0,1)),
    token_count INTEGER NULL CHECK(token_count IS NULL OR token_count >= 0),
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
);
CREATE INDEX IF NOT EXISTS idx_compliance_ai_payloads_event
    ON compliance_ai_payloads(event_id, payload_kind);

CREATE TABLE IF NOT EXISTS compliance_ai_sources (
    source_id TEXT PRIMARY KEY,
    event_id TEXT NOT NULL REFERENCES compliance_ai_events(event_id) ON DELETE CASCADE,
    source_kind TEXT NOT NULL CHECK(source_kind IN ('rag','file','url','database','addon','memory','vector','other')),
    source_ref TEXT NOT NULL,
    source_hash TEXT NOT NULL DEFAULT '',
    title TEXT NOT NULL DEFAULT '',
    excerpt_hash TEXT NOT NULL DEFAULT '',
    excerpt_text TEXT NOT NULL DEFAULT '',
    score REAL NULL,
    metadata_cbor BLOB NOT NULL DEFAULT X'',
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
);
CREATE INDEX IF NOT EXISTS idx_compliance_ai_sources_event
    ON compliance_ai_sources(event_id, source_kind);

CREATE TABLE IF NOT EXISTS compliance_ai_tool_calls (
    tool_call_id TEXT PRIMARY KEY,
    event_id TEXT NOT NULL REFERENCES compliance_ai_events(event_id) ON DELETE CASCADE,
    addon_id TEXT NULL,
    tool_name TEXT NOT NULL,
    input_hash TEXT NOT NULL DEFAULT '',
    output_hash TEXT NOT NULL DEFAULT '',
    status TEXT NOT NULL CHECK(status IN ('running','success','failed')),
    started_at TEXT NOT NULL,
    finished_at TEXT NULL,
    error_message TEXT NULL
);
CREATE INDEX IF NOT EXISTS idx_compliance_ai_tool_calls_event
    ON compliance_ai_tool_calls(event_id, status);

CREATE TABLE IF NOT EXISTS compliance_ai_policy_decisions (
    decision_id TEXT PRIMARY KEY,
    event_id TEXT NOT NULL REFERENCES compliance_ai_events(event_id) ON DELETE CASCADE,
    decision_kind TEXT NOT NULL CHECK(decision_kind IN ('allow','deny','redact','retain','legal_hold','risk_class')),
    decision_value TEXT NOT NULL,
    reason TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
);
CREATE INDEX IF NOT EXISTS idx_compliance_ai_policy_decisions_event
    ON compliance_ai_policy_decisions(event_id, decision_kind);

CREATE TABLE IF NOT EXISTS compliance_data_subjects (
    subject_id TEXT PRIMARY KEY,
    org_id TEXT NOT NULL REFERENCES organizations(org_id) ON DELETE CASCADE,
    subject_type TEXT NOT NULL CHECK(subject_type IN ('user','contact','customer','employee','external_person','unknown')),
    display_name_translations TEXT NOT NULL DEFAULT '{}',
    email_hash TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    CHECK(json_valid(display_name_translations))
);
CREATE INDEX IF NOT EXISTS idx_compliance_data_subjects_org
    ON compliance_data_subjects(org_id, subject_type, email_hash);

CREATE TRIGGER IF NOT EXISTS compliance_data_subjects_updated_at
AFTER UPDATE ON compliance_data_subjects
FOR EACH ROW
BEGIN
    UPDATE compliance_data_subjects
    SET updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
    WHERE subject_id = NEW.subject_id;
END;

CREATE TABLE IF NOT EXISTS compliance_data_subject_links (
    link_id TEXT PRIMARY KEY,
    subject_id TEXT NOT NULL REFERENCES compliance_data_subjects(subject_id) ON DELETE CASCADE,
    resource_kind TEXT NOT NULL,
    resource_id TEXT NOT NULL,
    addon_id TEXT NULL,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_compliance_data_subject_links_unique
    ON compliance_data_subject_links(subject_id, resource_kind, resource_id, COALESCE(addon_id, ''));
CREATE INDEX IF NOT EXISTS idx_compliance_data_subject_links_resource
    ON compliance_data_subject_links(resource_kind, resource_id, addon_id);

CREATE TABLE IF NOT EXISTS compliance_dsar_requests (
    dsar_id TEXT PRIMARY KEY,
    org_id TEXT NOT NULL REFERENCES organizations(org_id) ON DELETE CASCADE,
    subject_id TEXT NOT NULL REFERENCES compliance_data_subjects(subject_id) ON DELETE CASCADE,
    request_type TEXT NOT NULL CHECK(request_type IN ('access','rectification','erasure','restriction','portability','objection')),
    status TEXT NOT NULL DEFAULT 'open' CHECK(status IN ('open','in_progress','completed','rejected','cancelled')),
    requested_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    due_at TEXT NOT NULL,
    completed_at TEXT NULL,
    handled_by_user_id TEXT NULL REFERENCES user_accounts(id) ON DELETE SET NULL,
    notes TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_compliance_dsar_requests_org
    ON compliance_dsar_requests(org_id, status, due_at);

CREATE TABLE IF NOT EXISTS compliance_dsar_exports (
    export_id TEXT PRIMARY KEY,
    dsar_id TEXT NOT NULL REFERENCES compliance_dsar_requests(dsar_id) ON DELETE CASCADE,
    artifact_path TEXT NOT NULL,
    artifact_hash TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
);

CREATE TABLE IF NOT EXISTS compliance_consent_records (
    consent_id TEXT PRIMARY KEY,
    org_id TEXT NOT NULL REFERENCES organizations(org_id) ON DELETE CASCADE,
    subject_id TEXT NOT NULL REFERENCES compliance_data_subjects(subject_id) ON DELETE CASCADE,
    activity_id TEXT NOT NULL REFERENCES compliance_processing_activities(activity_id) ON DELETE CASCADE,
    consent_version TEXT NOT NULL,
    status TEXT NOT NULL CHECK(status IN ('granted','withdrawn','expired')),
    granted_at TEXT NULL,
    withdrawn_at TEXT NULL,
    evidence_hash TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
);
CREATE INDEX IF NOT EXISTS idx_compliance_consent_records_subject
    ON compliance_consent_records(org_id, subject_id, activity_id, status);

CREATE TABLE IF NOT EXISTS compliance_dpia_records (
    dpia_id TEXT PRIMARY KEY,
    org_id TEXT NOT NULL REFERENCES organizations(org_id) ON DELETE CASCADE,
    activity_id TEXT NOT NULL REFERENCES compliance_processing_activities(activity_id) ON DELETE CASCADE,
    status TEXT NOT NULL DEFAULT 'draft' CHECK(status IN ('draft','review','approved','rejected','retired')),
    risk_class TEXT NOT NULL DEFAULT 'standard' CHECK(risk_class IN ('low','standard','high','critical')),
    owner_user_id TEXT NULL REFERENCES user_accounts(id) ON DELETE SET NULL,
    summary TEXT NOT NULL DEFAULT '',
    reviewed_at TEXT NULL,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
);
CREATE INDEX IF NOT EXISTS idx_compliance_dpia_records_org
    ON compliance_dpia_records(org_id, status, risk_class);

CREATE TRIGGER IF NOT EXISTS compliance_dpia_records_updated_at
AFTER UPDATE ON compliance_dpia_records
FOR EACH ROW
BEGIN
    UPDATE compliance_dpia_records
    SET updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
    WHERE dpia_id = NEW.dpia_id;
END;

CREATE TABLE IF NOT EXISTS compliance_breach_incidents (
    breach_id TEXT PRIMARY KEY,
    org_id TEXT NOT NULL REFERENCES organizations(org_id) ON DELETE CASCADE,
    detected_at TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'open' CHECK(status IN ('open','investigating','notified','closed')),
    severity TEXT NOT NULL DEFAULT 'medium' CHECK(severity IN ('low','medium','high','critical')),
    title_translations TEXT NOT NULL DEFAULT '{}',
    summary_translations TEXT NOT NULL DEFAULT '{}',
    dpa_notified_at TEXT NULL,
    subjects_notified_at TEXT NULL,
    created_by_user_id TEXT NULL REFERENCES user_accounts(id) ON DELETE SET NULL,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    CHECK(json_valid(title_translations)),
    CHECK(json_valid(summary_translations))
);
CREATE INDEX IF NOT EXISTS idx_compliance_breach_incidents_org
    ON compliance_breach_incidents(org_id, status, detected_at DESC);

CREATE TRIGGER IF NOT EXISTS compliance_breach_incidents_updated_at
AFTER UPDATE ON compliance_breach_incidents
FOR EACH ROW
BEGIN
    UPDATE compliance_breach_incidents
    SET updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
    WHERE breach_id = NEW.breach_id;
END;

CREATE TABLE IF NOT EXISTS compliance_processors (
    processor_id TEXT PRIMARY KEY,
    org_id TEXT NOT NULL REFERENCES organizations(org_id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    role TEXT NOT NULL CHECK(role IN ('processor','subprocessor','joint_controller')),
    country TEXT NOT NULL DEFAULT '',
    transfer_mechanism TEXT NOT NULL DEFAULT '',
    dpa_reference TEXT NOT NULL DEFAULT '',
    is_active INTEGER NOT NULL DEFAULT 1 CHECK(is_active IN (0,1)),
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    UNIQUE(org_id, name)
);
CREATE INDEX IF NOT EXISTS idx_compliance_processors_org
    ON compliance_processors(org_id, is_active);

CREATE TRIGGER IF NOT EXISTS compliance_processors_updated_at
AFTER UPDATE ON compliance_processors
FOR EACH ROW
BEGIN
    UPDATE compliance_processors
    SET updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
    WHERE processor_id = NEW.processor_id;
END;
"#;

// v62 — `compliance_ai_tool_calls.llm_tool_call_id`: model-issued call id
// (`LlmToolCall.id`) recorded next to the execution result. Kept apart from
// the UUID primary key because prompt-mode ids are deterministic content
// hashes and may repeat across events.
const COMPLIANCE_AI_TOOL_CALLS_LLM_CALL_ID: &str = r#"
ALTER TABLE compliance_ai_tool_calls ADD COLUMN llm_tool_call_id TEXT NULL;
"#;

// v66 — agent + cross-event correlation on AI audit events (Harness §3.1 /
// §3.4): the gateway-aware LlmDispatcherImpl opens one compliance_ai_events row
// per `execute_chat`, so a single agent run that loops the model N times
// stitches all N events together by `agent_run_id`. For the common (non-agent)
// chat path there is no agent_run_id, yet routing still writes a session event
// AND the flow's `llm` node writes a per-call event for the same user turn. The
// `correlation_id` column links those: routing seeds it with the session
// event's own `request_id`, and every per-call event copies that value, so one
// user turn's rows share a correlation key even though `UNIQUE(org_id,
// request_id)` forces each row a distinct `request_id`. All plain TEXT (no FK):
// the event must survive an agent definition edit/delete and may be written
// before the agent row syncs in from another node. Indexes on `agent_run_id`
// and `correlation_id` so the dashboard can fetch one run's / one turn's AI-call
// timeline cheaply.
//
// Fresh installs get the columns from the inlined v50 foundation DDL, so the
// ADD COLUMN must be idempotent (a plain SQL migration would fail with
// "duplicate column name" on a fresh DB where v50 already added them). Each
// ALTER is gated by a PRAGMA table_info probe; the indexes use IF NOT EXISTS.
fn add_ai_events_agent_context(conn: &Connection, version: i64, name: &str) -> Result<()> {
    if !column_exists(conn, "compliance_ai_events", "agent_id")? {
        conn.execute_batch("ALTER TABLE compliance_ai_events ADD COLUMN agent_id TEXT NULL;")?;
    }
    if !column_exists(conn, "compliance_ai_events", "agent_run_id")? {
        conn.execute_batch("ALTER TABLE compliance_ai_events ADD COLUMN agent_run_id TEXT NULL;")?;
    }
    if !column_exists(conn, "compliance_ai_events", "correlation_id")? {
        conn.execute_batch(
            "ALTER TABLE compliance_ai_events ADD COLUMN correlation_id TEXT NULL;",
        )?;
    }
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_compliance_ai_events_agent_run \
             ON compliance_ai_events(agent_run_id, started_at DESC);
         CREATE INDEX IF NOT EXISTS idx_compliance_ai_events_correlation \
             ON compliance_ai_events(correlation_id, started_at DESC);",
    )?;
    conn.execute(
        "INSERT OR IGNORE INTO _migrations (version, name) VALUES (?1, ?2)",
        rusqlite::params![version, name],
    )?;
    Ok(())
}

// v40 — `platform_locales`: katalog jezykow interfejsu per organizacja.
// Tabela trzyma kody ISO 639-1 dostepne dla danej organizacji oraz wskazuje
// jezyk domyslny. Dane sluza warstwie aplikacyjnej do walidacji kompletnosci
// tlumaczen w `role_catalog.name_translations` i pozniej w innych tabelach
// platformy (stanowiska, etykiety addonow). SQLite nie wymusza obecnosci
// kluczy JSON dla wszystkich locale — kontrolka jest po stronie Rust
// (services/role_catalog), tu seedujemy startowy zestaw pl + en.
//
// Czesciowy unikalny indeks `idx_platform_locales_one_default_per_org`
// gwarantuje ze w danej organizacji jest dokladnie jeden jezyk z
// `is_default = 1` — bez tego UI mialby ambiwalentny fallback.
const PLATFORM_LOCALES_SCHEMA: &str = r#"
CREATE TABLE platform_locales (
  id            TEXT PRIMARY KEY,
  org_id        TEXT NOT NULL REFERENCES organizations(org_id) ON DELETE CASCADE,
  code          TEXT NOT NULL,
  display_name  TEXT NOT NULL,
  is_default    INTEGER NOT NULL DEFAULT 0 CHECK (is_default IN (0, 1)),
  is_active     INTEGER NOT NULL DEFAULT 1 CHECK (is_active IN (0, 1)),
  created_at    TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
  UNIQUE (org_id, code)
);

CREATE INDEX idx_platform_locales_org ON platform_locales(org_id);
CREATE UNIQUE INDEX idx_platform_locales_one_default_per_org
  ON platform_locales(org_id) WHERE is_default = 1;

INSERT INTO platform_locales (id, org_id, code, display_name, is_default, is_active)
VALUES
  (lower(hex(randomblob(4))) || '-' || lower(hex(randomblob(2))) || '-4' || substr(lower(hex(randomblob(2))),2) || '-' || substr('89ab',abs(random()) % 4 + 1, 1) || substr(lower(hex(randomblob(2))),2) || '-' || lower(hex(randomblob(6))),
   'org-default', 'pl', 'Polski', 1, 1),
  (lower(hex(randomblob(4))) || '-' || lower(hex(randomblob(2))) || '-4' || substr(lower(hex(randomblob(2))),2) || '-' || substr('89ab',abs(random()) % 4 + 1, 1) || substr(lower(hex(randomblob(2))),2) || '-' || lower(hex(randomblob(6))),
   'org-default', 'en', 'English', 0, 1);
"#;

// v41 — `role_catalog`: globalny, administrowalny katalog rol funkcjonalnych
// w organizacji. Rola opisuje *funkcje* osoby (np. handlowiec, PM techniczny,
// architekt) niezaleznie od stanowiska w drzewie organizacyjnym. Stanowiska
// i tabele typu `responsible_persons` w CRM bedaca referencja do wpisu z tego
// katalogu.
//
// `name_translations` i `description_translations` trzymane jako JSON
// (`{"pl":"...","en":"..."}`) — SQLite nie ma JSONB, walidujemy `json_valid`,
// pelna walidacja kompletnosci kluczy wzgledem `platform_locales` zyje w
// `services/role_catalog/repo.rs`. Seed zapewnia komplet pl + en dla wszystkich
// 14 rol z dokumentu `00-platform-roles-catalog.md`.
//
// `default_visibility_scope` dziedziczy sie do reguly P2 (permissions) jako
// startowa propozycja — admin moze zmienic w UI bez modyfikacji katalogu.
// `is_manager` jest uzywane przez O1 do layoutu drzewa stanowisk; nie wplywa
// bezposrednio na uprawnienia (te zyja w P2 jako reguly `flag:is_manager`).
const ROLE_CATALOG_SCHEMA: &str = r#"
CREATE TABLE role_catalog (
  id                          TEXT PRIMARY KEY,
  org_id                      TEXT NOT NULL REFERENCES organizations(org_id) ON DELETE CASCADE,
  slug                        TEXT NOT NULL CHECK (slug GLOB '[a-z][a-z0-9_]*' AND length(slug) <= 50),
  kind                        TEXT NOT NULL CHECK (kind IN ('sales','technical','management','external','other')),
  name_translations           TEXT NOT NULL DEFAULT '{}',
  description_translations    TEXT NOT NULL DEFAULT '{}',
  icon                        TEXT,
  color_hint                  TEXT,
  is_manager                  INTEGER NOT NULL DEFAULT 0 CHECK (is_manager IN (0,1)),
  default_visibility_scope    TEXT NOT NULL DEFAULT 'assigned'
    CHECK (default_visibility_scope IN ('assigned','own','section','department','all')),
  is_active                   INTEGER NOT NULL DEFAULT 1 CHECK (is_active IN (0,1)),
  created_at                  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
  updated_at                  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
  created_by                  TEXT,
  UNIQUE (org_id, slug),
  CHECK (json_valid(name_translations)),
  CHECK (json_valid(description_translations))
);

CREATE INDEX idx_role_catalog_org_active ON role_catalog(org_id, is_active);
CREATE INDEX idx_role_catalog_org_kind   ON role_catalog(org_id, kind);

CREATE TRIGGER role_catalog_updated_at
AFTER UPDATE ON role_catalog
FOR EACH ROW
BEGIN
  UPDATE role_catalog SET updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now') WHERE id = NEW.id;
END;

INSERT INTO role_catalog (id, org_id, slug, kind, name_translations, description_translations, icon, is_manager, default_visibility_scope) VALUES
(lower(hex(randomblob(4))) || '-' || lower(hex(randomblob(2))) || '-4' || substr(lower(hex(randomblob(2))),2) || '-' || substr('89ab',abs(random()) % 4 + 1, 1) || substr(lower(hex(randomblob(2))),2) || '-' || lower(hex(randomblob(6))),
 'org-default','handlowiec_l1','sales',
 json_object('pl','Handlowiec L1','en','Sales Rep L1'),
 json_object('pl','Junior — podstawowa rola sprzedazowa, prowadzi wlasne dealy','en','Junior — entry-level sales role, owns their own deals'),
 'i-briefcase', 0, 'assigned'),
(lower(hex(randomblob(4))) || '-' || lower(hex(randomblob(2))) || '-4' || substr(lower(hex(randomblob(2))),2) || '-' || substr('89ab',abs(random()) % 4 + 1, 1) || substr(lower(hex(randomblob(2))),2) || '-' || lower(hex(randomblob(6))),
 'org-default','handlowiec_l2','sales',
 json_object('pl','Handlowiec L2','en','Sales Rep L2'),
 json_object('pl','Mid — samodzielnie prowadzi dealy oraz wspiera juniorow','en','Mid-level — runs deals independently and supports juniors'),
 'i-briefcase', 0, 'own'),
(lower(hex(randomblob(4))) || '-' || lower(hex(randomblob(2))) || '-4' || substr(lower(hex(randomblob(2))),2) || '-' || substr('89ab',abs(random()) % 4 + 1, 1) || substr(lower(hex(randomblob(2))),2) || '-' || lower(hex(randomblob(6))),
 'org-default','sales_lead','sales',
 json_object('pl','Lider sprzedazy','en','Sales Lead'),
 json_object('pl','Kierownik sekcji sprzedazowej, prowadzi zespol handlowcow','en','Manages a sales section and a team of sales reps'),
 'i-users', 1, 'section'),
(lower(hex(randomblob(4))) || '-' || lower(hex(randomblob(2))) || '-4' || substr(lower(hex(randomblob(2))),2) || '-' || substr('89ab',abs(random()) % 4 + 1, 1) || substr(lower(hex(randomblob(2))),2) || '-' || lower(hex(randomblob(6))),
 'org-default','pm_technical','technical',
 json_object('pl','PM techniczny','en','Technical PM'),
 json_object('pl','Project Manager po stronie technicznej dealu/projektu','en','Project Manager on the technical side of a deal or project'),
 'i-clipboard', 0, 'assigned'),
(lower(hex(randomblob(4))) || '-' || lower(hex(randomblob(2))) || '-4' || substr(lower(hex(randomblob(2))),2) || '-' || substr('89ab',abs(random()) % 4 + 1, 1) || substr(lower(hex(randomblob(2))),2) || '-' || lower(hex(randomblob(6))),
 'org-default','architect_senior','technical',
 json_object('pl','Architekt senior','en','Senior Architect'),
 json_object('pl','Architekt rozwiazan, odpowiada za design techniczny','en','Solutions architect responsible for technical design'),
 'i-cube', 0, 'assigned'),
(lower(hex(randomblob(4))) || '-' || lower(hex(randomblob(2))) || '-4' || substr(lower(hex(randomblob(2))),2) || '-' || substr('89ab',abs(random()) % 4 + 1, 1) || substr(lower(hex(randomblob(2))),2) || '-' || lower(hex(randomblob(6))),
 'org-default','consultant_technical','technical',
 json_object('pl','Konsultant techniczny','en','Technical Consultant'),
 json_object('pl','Konsultant doradzajacy klientowi w obszarze technicznym','en','Consultant advising the client on technical matters'),
 'i-headset', 0, 'assigned'),
(lower(hex(randomblob(4))) || '-' || lower(hex(randomblob(2))) || '-4' || substr(lower(hex(randomblob(2))),2) || '-' || substr('89ab',abs(random()) % 4 + 1, 1) || substr(lower(hex(randomblob(2))),2) || '-' || lower(hex(randomblob(6))),
 'org-default','developer','technical',
 json_object('pl','Programista','en','Developer'),
 json_object('pl','Programista realizujacy implementacje na projekcie','en','Developer implementing project work'),
 'i-code', 0, 'assigned'),
(lower(hex(randomblob(4))) || '-' || lower(hex(randomblob(2))) || '-4' || substr(lower(hex(randomblob(2))),2) || '-' || substr('89ab',abs(random()) % 4 + 1, 1) || substr(lower(hex(randomblob(2))),2) || '-' || lower(hex(randomblob(6))),
 'org-default','qa','technical',
 json_object('pl','Tester QA','en','QA Engineer'),
 json_object('pl','Odpowiada za testy i jakosc dostarczanych rozwiazan','en','Responsible for testing and quality of delivered solutions'),
 'i-bug', 0, 'assigned'),
(lower(hex(randomblob(4))) || '-' || lower(hex(randomblob(2))) || '-4' || substr(lower(hex(randomblob(2))),2) || '-' || substr('89ab',abs(random()) % 4 + 1, 1) || substr(lower(hex(randomblob(2))),2) || '-' || lower(hex(randomblob(6))),
 'org-default','section_director','management',
 json_object('pl','Dyrektor sekcji','en','Section Director'),
 json_object('pl','Dyrektor odpowiedzialny za sekcje organizacyjna','en','Director responsible for an organizational section'),
 'i-building', 1, 'section'),
(lower(hex(randomblob(4))) || '-' || lower(hex(randomblob(2))) || '-4' || substr(lower(hex(randomblob(2))),2) || '-' || substr('89ab',abs(random()) % 4 + 1, 1) || substr(lower(hex(randomblob(2))),2) || '-' || lower(hex(randomblob(6))),
 'org-default','sales_director','management',
 json_object('pl','Dyrektor sprzedazy','en','Sales Director'),
 json_object('pl','Dyrektor pionu sprzedazy, nadzoruje wiele sekcji','en','Sales department director, oversees multiple sections'),
 'i-chart', 1, 'department'),
(lower(hex(randomblob(4))) || '-' || lower(hex(randomblob(2))) || '-4' || substr(lower(hex(randomblob(2))),2) || '-' || substr('89ab',abs(random()) % 4 + 1, 1) || substr(lower(hex(randomblob(2))),2) || '-' || lower(hex(randomblob(6))),
 'org-default','ceo','management',
 json_object('pl','Prezes','en','CEO'),
 json_object('pl','Prezes zarzadu, najwyzszy poziom decyzyjny','en','Chief Executive Officer, top decision-making level'),
 'i-crown', 1, 'all'),
(lower(hex(randomblob(4))) || '-' || lower(hex(randomblob(2))) || '-4' || substr(lower(hex(randomblob(2))),2) || '-' || substr('89ab',abs(random()) % 4 + 1, 1) || substr(lower(hex(randomblob(2))),2) || '-' || lower(hex(randomblob(6))),
 'org-default','decision_maker','external',
 json_object('pl','Decydent klienta','en','Client Decision Maker'),
 json_object('pl','Osoba po stronie klienta podejmujaca decyzje zakupowe','en','Person on the client side making purchasing decisions'),
 'i-user-check', 0, 'assigned'),
(lower(hex(randomblob(4))) || '-' || lower(hex(randomblob(2))) || '-4' || substr(lower(hex(randomblob(2))),2) || '-' || substr('89ab',abs(random()) % 4 + 1, 1) || substr(lower(hex(randomblob(2))),2) || '-' || lower(hex(randomblob(6))),
 'org-default','influencer','external',
 json_object('pl','Influencer po stronie klienta','en','Client Influencer'),
 json_object('pl','Osoba wplywajaca na decyzje klienta, bez bezposredniej decyzyjnosci','en','Person influencing client decisions without direct authority'),
 'i-user', 0, 'assigned'),
(lower(hex(randomblob(4))) || '-' || lower(hex(randomblob(2))) || '-4' || substr(lower(hex(randomblob(2))),2) || '-' || substr('89ab',abs(random()) % 4 + 1, 1) || substr(lower(hex(randomblob(2))),2) || '-' || lower(hex(randomblob(6))),
 'org-default','power_user_sponsor','external',
 json_object('pl','Sponsor wewnetrzny u klienta','en','Internal Sponsor at Client'),
 json_object('pl','Kluczowy uzytkownik i sponsor wdrozenia po stronie klienta','en','Key user and rollout sponsor on the client side'),
 'i-user-cog', 0, 'assigned');
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    /// Regex-free heuristic: does a column name look like a core-identity FK?
    /// Matches the discovery pattern from FAZA B krok 1A.
    fn looks_like_identity_fk(table: &str, column: &str) -> bool {
        if column == "id"
            && matches!(
                table,
                "flows" | "flow_model_bindings" | "flow_versions" | "user_accounts" | "user_groups"
            )
        {
            return true;
        }
        const SUFFIXES: &[&str] = &["_user_id", "_group_id"];
        const EXACT: &[&str] = &[
            "user_id",
            "group_id",
            "flow_id",
            "subject_id",
            "granted_by",
            "approved_by",
            "created_by",
            "updated_by",
            "owner_user_id",
            "actor_user_id",
            "manager_user_id",
            "assigned_user_id",
            "default_group_id",
            "created_by_user_id",
            "handled_by_user_id",
            "released_by_user_id",
            "generated_by_user_id",
            "changed_by_user_id",
            "caller_user_id",
            "grant_decided_by_user_id",
        ];
        EXACT.contains(&column) || SUFFIXES.iter().any(|s| column.ends_with(s))
    }

    /// PRAGMA-introspect a live DB: returns (table, column, declared_type) for
    /// every column whose name matches the identity-FK pattern.
    fn pattern_integer_columns(conn: &Connection) -> Vec<(String, String, String)> {
        let mut tables: Vec<String> = Vec::new();
        {
            let mut stmt = conn
                .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'")
                .unwrap();
            let rows = stmt.query_map([], |r| r.get::<_, String>(0)).unwrap();
            for r in rows {
                tables.push(r.unwrap());
            }
        }
        let mut out = Vec::new();
        for table in tables {
            let mut stmt = conn
                .prepare(&format!("PRAGMA table_info({table})"))
                .unwrap();
            let rows = stmt
                .query_map([], |r| Ok((r.get::<_, String>(1)?, r.get::<_, String>(2)?)))
                .unwrap();
            for r in rows {
                let (col, ty) = r.unwrap();
                if looks_like_identity_fk(&table, &col) {
                    out.push((table.clone(), col, ty));
                }
            }
        }
        out
    }

    /// Fresh install: run all migrations, then assert every identity-FK-pattern
    /// column is either migrated to TEXT (per the allowlist) or explicitly
    /// flagged intentionally-local INTEGER. A column matching the pattern that
    /// is neither TEXT-allowlisted nor exempt fails the build — this is the
    /// guard that stops a future column from silently keeping an INTEGER id.
    ///
    /// This guard runs on a FRESH install, where INITIAL_SCHEMA declares the
    /// remapped child columns TEXT. An UPGRADED DB intentionally differs: v56
    /// value-remaps those columns (INTEGER id -> UUID text) but does NOT rebuild
    /// the table to flip the declared type, so they keep INTEGER affinity while
    /// holding UUID text. That asymmetry is safe (SQLite affinity never coerces a
    /// UUID string) and is verified separately by
    /// `migration_remaps_integer_ids_to_uuid`.
    #[test]
    fn allowlist_guard_no_unaccounted_identity_integer() {
        let conn = Connection::open_in_memory().unwrap();
        run(&conn).unwrap();

        // Build the migrated-to-text set: identity PKs + every child remap.
        let mut migrated: std::collections::HashSet<(String, String)> =
            std::collections::HashSet::new();
        for (t, c) in identity_pk_columns() {
            migrated.insert((t.to_string(), c.to_string()));
        }
        for r in child_remaps() {
            migrated.insert((r.table.to_string(), r.column.to_string()));
        }
        // Every exemption must carry a WHY reason; an empty one is a doc gap.
        let exempt: std::collections::HashSet<(String, String)> = intentionally_local_integers()
            .into_iter()
            .map(|e| {
                assert!(
                    !e.reason.is_empty(),
                    "intentionally-local {}.{} needs a WHY reason",
                    e.table,
                    e.column
                );
                (e.table.to_string(), e.column.to_string())
            })
            .collect();
        let text_non_identity: std::collections::HashSet<(String, String)> =
            intentionally_text_non_identity()
                .into_iter()
                .map(|e| {
                    assert!(
                        !e.reason.is_empty(),
                        "intentionally-text {}.{} needs a WHY reason",
                        e.table,
                        e.column
                    );
                    (e.table.to_string(), e.column.to_string())
                })
                .collect();

        for (table, column, ty) in pattern_integer_columns(&conn) {
            let key = (table.clone(), column.clone());
            if migrated.contains(&key) {
                assert!(
                    ty.eq_ignore_ascii_case("TEXT"),
                    "{table}.{column} is on the migrated allowlist but is declared {ty}, not TEXT"
                );
                continue;
            }
            if exempt.contains(&key) {
                assert!(
                    ty.eq_ignore_ascii_case("INTEGER"),
                    "{table}.{column} is flagged intentionally-local INTEGER but is declared {ty}"
                );
                continue;
            }
            if text_non_identity.contains(&key) {
                assert!(
                    ty.eq_ignore_ascii_case("TEXT"),
                    "{table}.{column} is flagged intentionally-text-non-identity but is declared {ty}"
                );
                continue;
            }
            panic!(
                "{table}.{column} ({ty}) matches the identity-FK pattern but is on neither the \
                 migrated-to-text allowlist nor the intentionally-local-integer list. \
                 Add it to child_remaps() (and flip its schema to TEXT) or to \
                 intentionally_local_integers() with a WHY comment."
            );
        }
    }

    /// Seeds a DB at the pre-UUID schema (INTEGER ids) by running migrations up
    /// to the flip, then exercises the flip and verifies PK rewrite + referential
    /// integrity.
    #[test]
    fn migration_remaps_integer_ids_to_uuid() {
        let conn = Connection::open_in_memory().unwrap();
        // Run the historical migrations only up to (excluding) the flip so identity
        // tables still hold INTEGER ids when we seed.
        seed_pre_uuid_schema(&conn);

        // Seed users, groups, flows and dependent children with INTEGER ids.
        conn.execute_batch(
            r#"
            INSERT INTO user_accounts (id, username, password_hash) VALUES
                (10, 'alice', 'h'), (11, 'bob', 'h');
            INSERT INTO user_groups (id, name) VALUES (100, 'eng'), (101, 'ops');
            INSERT INTO group_members (group_id, user_id) VALUES (100, 10), (101, 11);
            INSERT INTO flows (id, name, flow_json) VALUES
                (5, 'f1', '{}'), (6, 'f2', '{}');
            INSERT INTO flow_model_bindings (id, flow_id, model_pattern) VALUES
                (1, 5, 'gpt-*');
            INSERT INTO flow_versions (id, flow_id, version_num, flow_json, name) VALUES
                (1, 5, 1, '{}', 'v1');
            INSERT INTO flow_executions (flow_id, status) VALUES (5, 'success');
            INSERT INTO api_keys (key_hash, key_prefix, name, owner_user_id)
                VALUES ('kh', 'kp', 'k', 11);
            INSERT INTO addon_permissions (addon_id, subject_type, subject_id, permission_id)
                VALUES ('a', 'user', 10, 'p1'), ('a', 'group', 100, 'p2');
            INSERT INTO audit_log (action, user_id) VALUES ('login', 10);
            INSERT INTO sso_providers
                (name, provider_type, client_id, client_secret_encrypted, discovery_url, default_group_id)
                VALUES ('idp', 'oidc', 'cid', 'sec', 'http://x', 101);
            INSERT INTO flow_invocations
                (id, addon_id, flow_id, started_at, status, operators_total, actor_user_id)
                VALUES ('inv-1', 'a', '5', 'now', 'running', 1, '10'),
                       ('inv-2', 'a', '5', 'now', 'running', 1, NULL);
            "#,
        )
        .unwrap();

        // inv-3 models a row whose actor account ('999') was hard-deleted before
        // the migration: a dangling stringified-int the legacy schema never
        // enforced. Seed it with FK enforcement off (the migration itself runs
        // with `foreign_keys = OFF`) so the dangling value can exist pre-flip.
        conn.execute_batch("PRAGMA foreign_keys = OFF;").unwrap();
        conn.execute(
            "INSERT INTO flow_invocations \
             (id, addon_id, flow_id, started_at, status, operators_total, actor_user_id) \
             VALUES ('inv-3', 'a', '5', 'now', 'running', 1, '999')",
            [],
        )
        .unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();

        // Legacy `users` carries the only pre-mgmt language preference. Match by
        // username so the flip backfill can copy it into user_accounts.
        conn.execute_batch(
            r#"
            INSERT INTO users (username, password_hash, role, preferred_language)
            VALUES ('alice', 'h', 'admin', 'pl');
            "#,
        )
        .unwrap();

        // org_memberships.user_id is TEXT (CAST(id AS TEXT)). Seed legacy ints.
        conn.execute_batch(
            r#"
            INSERT INTO org_memberships (org_id, user_id, role_id, granted_at, granted_by)
            VALUES ('org-default', '10', 'role-org-admin', 'now', 'system');
            "#,
        )
        .unwrap();

        let users_before: i64 = conn
            .query_row("SELECT COUNT(*) FROM user_accounts", [], |r| r.get(0))
            .unwrap();
        let flows_before: i64 = conn
            .query_row("SELECT COUNT(*) FROM flows", [], |r| r.get(0))
            .unwrap();

        // Run the identity flip.
        core_identity_int_to_uuid(
            &conn,
            CORE_IDENTITY_FLIP_VERSION,
            "core_identity_int_to_uuid",
        )
        .unwrap();

        // (a) PKs are now TEXT UUIDs.
        for table in [
            "flows",
            "flow_model_bindings",
            "flow_versions",
            "user_accounts",
            "user_groups",
        ] {
            assert!(
                column_is_text(&conn, table, "id").unwrap(),
                "{table}.id should be TEXT after migration"
            );
        }
        let alice_id: String = conn
            .query_row(
                "SELECT id FROM user_accounts WHERE username='alice'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(alice_id.len(), 36, "user id should be a UUID");

        // (b) foreign_key_check is empty.
        let violations = foreign_key_check(&conn).unwrap();
        assert!(violations.is_empty(), "FK violations: {violations:?}");

        // (c) row counts preserved.
        let users_after: i64 = conn
            .query_row("SELECT COUNT(*) FROM user_accounts", [], |r| r.get(0))
            .unwrap();
        let flows_after: i64 = conn
            .query_row("SELECT COUNT(*) FROM flows", [], |r| r.get(0))
            .unwrap();
        assert_eq!(users_before, users_after);
        assert_eq!(flows_before, flows_after);

        // (d) child FKs point at existing parent UUIDs.
        let dangling_bindings: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM flow_model_bindings b \
                 LEFT JOIN flows f ON f.id = b.flow_id WHERE f.id IS NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(dangling_bindings, 0, "binding.flow_id must resolve");
        // group_members both columns resolve.
        let dangling_members: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM group_members m \
                 LEFT JOIN user_accounts u ON u.id = m.user_id \
                 LEFT JOIN user_groups g ON g.id = m.group_id \
                 WHERE u.id IS NULL OR g.id IS NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(dangling_members, 0, "group_members must resolve both sides");

        // polymorphic subject_id: user row -> alice uuid, group row -> eng uuid.
        let perm_user: String = conn
            .query_row(
                "SELECT subject_id FROM addon_permissions WHERE permission_id='p1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(perm_user, alice_id);
        let eng_id: String = conn
            .query_row("SELECT id FROM user_groups WHERE name='eng'", [], |r| {
                r.get(0)
            })
            .unwrap();
        let perm_group: String = conn
            .query_row(
                "SELECT subject_id FROM addon_permissions WHERE permission_id='p2'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(perm_group, eng_id);

        // org_memberships.user_id remapped from '10' to alice uuid.
        let membership_user: String = conn
            .query_row(
                "SELECT user_id FROM org_memberships WHERE org_id='org-default'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(membership_user, alice_id);

        // audit_log.user_id remapped.
        let audit_user: String = conn
            .query_row(
                "SELECT user_id FROM audit_log WHERE action='login'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(audit_user, alice_id);

        // flow_invocations.actor_user_id remapped from legacy '10' to alice uuid;
        // the NULL-actor row stays NULL.
        let inv_actor: String = conn
            .query_row(
                "SELECT actor_user_id FROM flow_invocations WHERE id='inv-1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(inv_actor, alice_id);
        let inv_null: Option<String> = conn
            .query_row(
                "SELECT actor_user_id FROM flow_invocations WHERE id='inv-2'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(inv_null.is_none(), "NULL-actor invocation stays NULL");
        // inv-3 referenced a deleted actor ('999', no matching user_accounts row):
        // its enforced FK forces the unresolved id to NULL, not a dangling string.
        let inv_unresolved: Option<String> = conn
            .query_row(
                "SELECT actor_user_id FROM flow_invocations WHERE id='inv-3'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            inv_unresolved.is_none(),
            "unresolved actor id must be set to NULL to satisfy the enforced FK"
        );

        // preferred_language backfilled from legacy users (matched by username).
        let alice_lang: Option<String> = conn
            .query_row(
                "SELECT preferred_language FROM user_accounts WHERE username='alice'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            alice_lang.as_deref(),
            Some("pl"),
            "alice's language preference must survive the UUID flip"
        );

        // Upgrade path keeps a value-remapped child column at its declared
        // INTEGER affinity (we no longer rebuild the table to flip the type).
        // Assert the VALUES are correct, not the declared type: the remapped
        // value is a parent UUID and a JOIN over the column resolves even though
        // the column affinity is still INTEGER. flow_executions is NOT a rebuilt
        // identity table, so its `flow_id` (INTEGER REFERENCES flows(id) in the
        // pre-UUID schema) is the canonical INTEGER-affinity-holding-UUID case.
        assert!(
            !column_is_text(&conn, "flow_executions", "flow_id").unwrap(),
            "upgrade path leaves flow_executions.flow_id at INTEGER affinity"
        );
        let f1_id: String = conn
            .query_row("SELECT id FROM flows WHERE name='f1'", [], |r| r.get(0))
            .unwrap();
        let exec_flow: String = conn
            .query_row(
                "SELECT f.id FROM flow_executions e JOIN flows f ON f.id = e.flow_id \
                 WHERE e.status='success'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            exec_flow, f1_id,
            "JOIN over an INTEGER-affinity column holding UUID text must resolve"
        );

        // Writing a fresh UUID into the INTEGER-affinity column and reading it
        // back must round-trip losslessly (no integer coercion), and a JOIN over
        // the new value must find the parent (its enforced FK must accept it).
        let f2_id: String = conn
            .query_row("SELECT id FROM flows WHERE name='f2'", [], |r| r.get(0))
            .unwrap();
        conn.execute(
            "INSERT INTO flow_executions (flow_id, status) VALUES (?1, 'completed')",
            rusqlite::params![f2_id],
        )
        .unwrap();
        let roundtrip: String = conn
            .query_row(
                "SELECT flow_id FROM flow_executions WHERE status='completed'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            roundtrip, f2_id,
            "UUID text written to an INTEGER-affinity column must read back verbatim"
        );
        let joined_new: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM flow_executions e JOIN flows f ON f.id = e.flow_id \
                 WHERE e.status='completed'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            joined_new, 1,
            "JOIN over the freshly written UUID must match"
        );
    }

    /// Regresja v58: migracja repair_admin_non_uuid_id naprawia stara instalacje,
    /// w ktorej seed wstawil literal '1' do user_accounts.id (zamiast UUID).
    /// Budujemy czysty schemat przez run(), cofamy wersje ponizej 58, recznie
    /// odtwarzamy zepsuty stan (id='1' + group_members.user_id='1'), po czym
    /// uruchamiamy repair i weryfikujemy remap + brak naruszen FK + idempotencje.
    #[test]
    fn migration_v58_repairs_non_uuid_admin_id() {
        const REPAIRED: &str = "00000000-0000-4000-8000-000000000002";

        let conn = Connection::open_in_memory().unwrap();
        run(&conn).unwrap();

        // Cofnij stan migracji ponizej 58, tak jak wygladalaby stara instalacja
        // sprzed wprowadzenia naprawy.
        conn.execute("DELETE FROM _migrations WHERE version >= 58", [])
            .unwrap();

        // Odtworz zepsuty stan: admin z id='1' oraz jego czlonkostwo w grupie z
        // user_id='1'. FK wylaczone, bo '1' nie jest poprawnym UUID rodzica i przy
        // wlaczonych FK insert dziecka by sie nie powiodl (dokladnie stan, jaki
        // realnie istnial po wadliwym seedzie).
        conn.execute_batch("PRAGMA foreign_keys = OFF;").unwrap();
        conn.execute(
            "INSERT INTO user_accounts (id, username, password_hash, display_name, is_admin) \
             VALUES ('1', 'admin', 'h', 'Administrator', 1)",
            [],
        )
        .unwrap();
        let admins_group_id: String = conn
            .query_row(
                "SELECT id FROM user_groups WHERE name = 'admins'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        conn.execute(
            "INSERT INTO group_members (group_id, user_id) VALUES (?1, '1')",
            rusqlite::params![admins_group_id],
        )
        .unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();

        // Stan przed naprawa: faktycznie istnieje zepsuty wiersz id='1'.
        let broken_before: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM user_accounts WHERE id = '1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(broken_before, 1, "stan testowy: powinien byc wiersz id='1'");

        // Uruchom naprawe.
        repair_admin_non_uuid_id(&conn, 58, "repair_admin_non_uuid_id").unwrap();

        // (a) brak wiersza id='1'.
        let broken_after: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM user_accounts WHERE id = '1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(broken_after, 0, "id='1' powinno zniknac po naprawie");

        // (b) istnieje wiersz admina z poprawnym, stalym UUID.
        let admin_id: String = conn
            .query_row(
                "SELECT id FROM user_accounts WHERE username = 'admin'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(admin_id, REPAIRED, "id admina musi byc REPAIRED_ADMIN_ID");
        uuid::Uuid::parse_str(&admin_id)
            .unwrap_or_else(|e| panic!("naprawione id '{admin_id}' nie jest UUID: {e}"));

        // (c) group_members.user_id zaktualizowane na ten sam UUID.
        let member_user_id: String = conn
            .query_row(
                "SELECT user_id FROM group_members WHERE group_id = ?1",
                rusqlite::params![admins_group_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            member_user_id, REPAIRED,
            "group_members.user_id musi zostac zremapowane na UUID admina"
        );

        // (d) brak naruszen integralnosci referencyjnej.
        let violations = foreign_key_check(&conn).unwrap();
        assert!(
            violations.is_empty(),
            "naruszenia FK po naprawie: {violations:?}"
        );

        // (e) migracja zostala oznaczona jako wykonana.
        let stamped: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM _migrations WHERE version = 58",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(stamped, 1, "v58 powinno byc zapisane w _migrations");

        // (f) idempotencja: drugie wywolanie na juz naprawionej bazie nie wybucha
        // i nie zmienia stanu.
        repair_admin_non_uuid_id(&conn, 58, "repair_admin_non_uuid_id").unwrap();
        let admin_id_again: String = conn
            .query_row(
                "SELECT id FROM user_accounts WHERE username = 'admin'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(admin_id_again, REPAIRED);
        let violations_again = foreign_key_check(&conn).unwrap();
        assert!(
            violations_again.is_empty(),
            "powtorne wywolanie nie moze wprowadzic naruszen FK: {violations_again:?}"
        );
    }

    /// Regresja v61: migracja repair_default_flow_random_id przepina "Default
    /// Chat", ktory na starych instalacjach mial losowy `Uuid::new_v4()` (rozny
    /// per node), na wspolny staly id. Budujemy schemat przez run(), cofamy
    /// wersje ponizej 61, odtwarzamy stan z losowym id + dzieckiem
    /// (flow_model_bindings) na to id, po czym uruchamiamy repair i weryfikujemy
    /// remap rodzica + dziecka, brak naruszen FK i idempotencje.
    #[test]
    fn migration_v61_repairs_random_default_flow_id() {
        const CANONICAL: &str = "00000000-0000-4000-8000-000000000010";

        let conn = Connection::open_in_memory().unwrap();
        run(&conn).unwrap();

        conn.execute("DELETE FROM _migrations WHERE version >= 61", [])
            .unwrap();

        // Odtworz stary stan: Default Chat z losowym id + binding na to id.
        let random_id = uuid::Uuid::new_v4().to_string();
        conn.execute_batch("PRAGMA foreign_keys = OFF;").unwrap();
        conn.execute(
            "INSERT INTO flows (id, name, flow_json, is_default, status) \
             VALUES (?1, 'Default Chat', '{}', 1, 'active')",
            rusqlite::params![random_id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO flow_model_bindings (id, flow_id, model_pattern, priority) \
             VALUES (?1, ?2, 'gpt-*', 0)",
            rusqlite::params![uuid::Uuid::new_v4().to_string(), random_id],
        )
        .unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();

        repair_default_flow_random_id(&conn, 61, "repair_default_flow_random_id").unwrap();

        // (a) flow ma kanoniczny id, losowy zniknal.
        let flow_id: String = conn
            .query_row(
                "SELECT id FROM flows WHERE name = 'Default Chat'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(flow_id, CANONICAL, "Default Chat musi miec kanoniczny id");
        assert_eq!(
            random_id_count(&conn, &random_id),
            0,
            "losowy id nie moze juz istniec"
        );

        // (b) dziecko (binding) zremapowane na ten sam id.
        let binding_flow_id: String = conn
            .query_row("SELECT flow_id FROM flow_model_bindings LIMIT 1", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(
            binding_flow_id, CANONICAL,
            "flow_model_bindings.flow_id musi zostac zremapowane"
        );

        // (c) brak naruszen FK.
        let violations = foreign_key_check(&conn).unwrap();
        assert!(
            violations.is_empty(),
            "naruszenia FK po naprawie: {violations:?}"
        );

        // (d) v61 zapisane w _migrations.
        let stamped: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM _migrations WHERE version = 61",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(stamped, 1, "v61 powinno byc zapisane w _migrations");

        // (e) idempotencja: drugie wywolanie nic nie psuje.
        repair_default_flow_random_id(&conn, 61, "repair_default_flow_random_id").unwrap();
        let flow_id_again: String = conn
            .query_row(
                "SELECT id FROM flows WHERE name = 'Default Chat'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(flow_id_again, CANONICAL);
        let violations_again = foreign_key_check(&conn).unwrap();
        assert!(
            violations_again.is_empty(),
            "powtorne wywolanie nie moze wprowadzic naruszen FK: {violations_again:?}"
        );
    }

    fn random_id_count(conn: &Connection, id: &str) -> i64 {
        conn.query_row(
            "SELECT COUNT(*) FROM flows WHERE id = ?1",
            rusqlite::params![id],
            |r| r.get(0),
        )
        .unwrap()
    }

    #[test]
    fn migration_v65_widens_retention_scope_and_seeds_agent_runs() {
        let conn = Connection::open_in_memory().unwrap();
        run(&conn).unwrap();

        // Reproduce a pre-v65 database: rebuild the retention table with the old
        // 7-value CHECK and drop the seeded agent_runs policy + the v65 stamp.
        // Scopes introduced after v65 go with it — the old CHECK rejects them.
        conn.execute_batch("PRAGMA foreign_keys = OFF;").unwrap();
        conn.execute_batch(
            "
            CREATE TABLE compliance_retention_policies_old (
                retention_policy_id TEXT PRIMARY KEY,
                org_id TEXT NOT NULL REFERENCES organizations(org_id) ON DELETE CASCADE,
                slug TEXT NOT NULL,
                name_translations TEXT NOT NULL DEFAULT '{}',
                scope_kind TEXT NOT NULL CHECK(scope_kind IN \
                    ('audit','ai_audit','data_category','document','dsar','breach','general')),
                category_id TEXT NULL REFERENCES compliance_data_categories(category_id) ON DELETE SET NULL,
                retention_days INTEGER NOT NULL,
                minimum_days INTEGER NOT NULL DEFAULT 0,
                action_after_retention TEXT NOT NULL DEFAULT 'delete'
                    CHECK(action_after_retention IN ('delete','anonymize','archive')),
                is_default INTEGER NOT NULL DEFAULT 0 CHECK(is_default IN (0,1)),
                is_active INTEGER NOT NULL DEFAULT 1 CHECK(is_active IN (0,1)),
                created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
                updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
                CHECK(retention_days >= minimum_days),
                UNIQUE(org_id, slug),
                CHECK(json_valid(name_translations))
            );
            INSERT INTO compliance_retention_policies_old
                SELECT retention_policy_id, org_id, slug, name_translations, scope_kind, \
                       category_id, retention_days, minimum_days, action_after_retention, \
                       is_default, is_active, created_at, updated_at \
                FROM compliance_retention_policies \
                WHERE scope_kind NOT IN ('agent_runs', 'events');
            DROP TABLE compliance_retention_policies;
            ALTER TABLE compliance_retention_policies_old RENAME TO compliance_retention_policies;
            ",
        )
        .unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        conn.execute("DELETE FROM _migrations WHERE version = 65", [])
            .unwrap();

        // The old CHECK rejects the new scope, proving we reproduced pre-v65.
        let pre = conn.execute(
            "INSERT INTO compliance_retention_policies \
                (retention_policy_id, org_id, slug, name_translations, scope_kind, \
                 retention_days, is_default, is_active) \
             VALUES ('probe', 'org-default', 'probe', '{}', 'agent_runs', 30, 0, 1)",
            [],
        );
        assert!(pre.is_err(), "pre-v65 CHECK must reject 'agent_runs'");

        widen_retention_scope_for_agent_runs(&conn, 65, "compliance_retention_agent_runs_scope")
            .unwrap();

        // (a) the widened CHECK now accepts the new scope.
        let policy_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM compliance_retention_policies \
                 WHERE scope_kind = 'agent_runs' AND org_id = 'org-default'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(policy_count, 1, "agent_runs policy must be backfilled");

        // (b) the seeded policy is the 30-day default with the stable id.
        let (id, days, action): (String, i64, String) = conn
            .query_row(
                "SELECT retention_policy_id, retention_days, action_after_retention \
                 FROM compliance_retention_policies \
                 WHERE scope_kind = 'agent_runs' AND org_id = 'org-default'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(id, "ret-core-agent-runs-default");
        assert_eq!(days, 30);
        assert_eq!(action, "delete");

        // (c) v65 stamped, FKs intact.
        let stamped: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM _migrations WHERE version = 65",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(stamped, 1, "v65 must be recorded in _migrations");
        assert!(foreign_key_check(&conn).unwrap().is_empty());

        // (d) idempotency: a second call short-circuits and does not duplicate.
        conn.execute("DELETE FROM _migrations WHERE version = 65", [])
            .unwrap();
        widen_retention_scope_for_agent_runs(&conn, 65, "compliance_retention_agent_runs_scope")
            .unwrap();
        let policy_count_again: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM compliance_retention_policies WHERE scope_kind = 'agent_runs'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(policy_count_again, 1, "no duplicate agent_runs policy");
    }

    #[test]
    fn migration_v66_adds_agent_context_columns_idempotently() {
        let conn = Connection::open_in_memory().unwrap();
        run(&conn).unwrap();

        // Reproduce a pre-v66 database: drop the two columns by rebuilding only
        // the schema shape that matters here is the absence of the columns —
        // simplest is to assert the fresh DB already has them (v50 inlined them),
        // then prove the migration is a no-op (idempotent) when re-run.
        assert!(
            column_exists(&conn, "compliance_ai_events", "agent_id").unwrap(),
            "fresh install must already carry agent_id from the v50 foundation"
        );
        assert!(column_exists(&conn, "compliance_ai_events", "agent_run_id").unwrap());
        assert!(column_exists(&conn, "compliance_ai_events", "correlation_id").unwrap());

        // Re-running v66 on a DB that already has the columns must not fail with
        // "duplicate column name" (the bug the PRAGMA probe guards against).
        conn.execute("DELETE FROM _migrations WHERE version = 66", [])
            .unwrap();
        add_ai_events_agent_context(&conn, 66, "compliance_ai_events_agent_context").unwrap();
        let stamped: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM _migrations WHERE version = 66",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(stamped, 1, "v66 must be recorded in _migrations");

        // The upgrade path (columns missing) also works: simulate a legacy
        // compliance_ai_events lacking the agent columns (but with started_at,
        // which the agent-run index orders by) and run v66 over it.
        conn.execute_batch(
            "DROP INDEX IF EXISTS idx_compliance_ai_events_agent_run;
             DROP TABLE compliance_ai_events;
             CREATE TABLE compliance_ai_events (
                event_id TEXT PRIMARY KEY,
                flow_node_id TEXT NULL,
                started_at TEXT NOT NULL DEFAULT ''
             );",
        )
        .unwrap();
        conn.execute("DELETE FROM _migrations WHERE version = 66", [])
            .unwrap();
        add_ai_events_agent_context(&conn, 66, "compliance_ai_events_agent_context").unwrap();
        assert!(column_exists(&conn, "compliance_ai_events", "agent_id").unwrap());
        assert!(column_exists(&conn, "compliance_ai_events", "agent_run_id").unwrap());
        assert!(column_exists(&conn, "compliance_ai_events", "correlation_id").unwrap());
    }

    fn role_permissions(conn: &Connection, role: &str) -> Vec<String> {
        let json: String = conn
            .query_row(
                "SELECT permissions_json FROM roles WHERE name = ?1",
                rusqlite::params![role],
                |r| r.get(0),
            )
            .unwrap();
        serde_json::from_str(&json).unwrap()
    }

    /// Rebuilds `compliance_retention_policies` with the pre-v133 CHECK list and
    /// strips everything else v133 adds, so the migration can be exercised on a
    /// database that really is at v132 (main's services migration stays applied).
    fn rewind_to_v132(conn: &Connection) {
        conn.execute_batch("PRAGMA foreign_keys = OFF;").unwrap();
        conn.execute_batch(
            "
            CREATE TABLE compliance_retention_policies_v132 (
                retention_policy_id TEXT PRIMARY KEY,
                org_id TEXT NOT NULL REFERENCES organizations(org_id) ON DELETE CASCADE,
                slug TEXT NOT NULL,
                name_translations TEXT NOT NULL DEFAULT '{}',
                scope_kind TEXT NOT NULL CHECK(scope_kind IN \
                    ('audit','ai_audit','data_category','document','dsar','breach','general','agent_runs')),
                category_id TEXT NULL REFERENCES compliance_data_categories(category_id) ON DELETE SET NULL,
                retention_days INTEGER NOT NULL,
                minimum_days INTEGER NOT NULL DEFAULT 0,
                action_after_retention TEXT NOT NULL DEFAULT 'delete'
                    CHECK(action_after_retention IN ('delete','anonymize','archive')),
                is_default INTEGER NOT NULL DEFAULT 0 CHECK(is_default IN (0,1)),
                is_active INTEGER NOT NULL DEFAULT 1 CHECK(is_active IN (0,1)),
                created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
                updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
                CHECK(retention_days >= minimum_days),
                UNIQUE(org_id, slug),
                CHECK(json_valid(name_translations))
            );
            INSERT INTO compliance_retention_policies_v132
                SELECT retention_policy_id, org_id, slug, name_translations, scope_kind, \
                       category_id, retention_days, minimum_days, action_after_retention, \
                       is_default, is_active, created_at, updated_at \
                FROM compliance_retention_policies WHERE scope_kind <> 'events';
            DROP TABLE compliance_retention_policies;
            ALTER TABLE compliance_retention_policies_v132 RENAME TO compliance_retention_policies;
            DROP INDEX IF EXISTS idx_audit_log_correlation;
            ALTER TABLE audit_log DROP COLUMN correlation_id;
            UPDATE roles SET permissions_json = (
                SELECT json_group_array(value) FROM json_each(roles.permissions_json)
                WHERE value NOT IN ('events.read', 'events.read_all')
            );
            DELETE FROM _migrations WHERE version = 133;
            ",
        )
        .unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
    }

    #[test]
    fn migration_v133_fresh_database_has_events_foundation() {
        let conn = Connection::open_in_memory().unwrap();
        run(&conn).unwrap();

        assert!(column_exists(&conn, "audit_log", "correlation_id").unwrap());
        let indexed: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master \
                 WHERE type = 'index' AND name = 'idx_audit_log_correlation'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            indexed, 1,
            "the audit -> timeline deep link needs its index"
        );

        // A fresh install gets the final CHECK straight from INITIAL_SCHEMA, so the
        // v133 probe finds the scope already there and skips the table rebuild.
        // A rebuild ends in ALTER TABLE ... RENAME, which leaves the table name
        // quoted in sqlite_master; an unquoted name proves this is still the
        // table INITIAL_SCHEMA created.
        let table_sql: String = conn
            .query_row(
                "SELECT sql FROM sqlite_master \
                 WHERE type = 'table' AND name = 'compliance_retention_policies'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            table_sql.contains("'events'"),
            "INITIAL_SCHEMA must already accept the events scope: {table_sql}"
        );
        assert!(
            table_sql.contains("CREATE TABLE compliance_retention_policies"),
            "a fresh install must not rebuild the retention table: {table_sql}"
        );

        let admin = role_permissions(&conn, "org_admin");
        assert!(admin.iter().any(|p| p == "events.read"));
        assert!(admin.iter().any(|p| p == "events.read_all"));
        for role in ["org_operator", "org_viewer", "dpo", "supervisor"] {
            let perms = role_permissions(&conn, role);
            assert!(
                perms.iter().any(|p| p == "events.read"),
                "{role} must read its own runs"
            );
            assert!(
                !perms.iter().any(|p| p == "events.read_all"),
                "{role} must not read every run on the node"
            );
        }

        let (days, action, minimum, translations): (i64, String, i64, String) = conn
            .query_row(
                "SELECT retention_days, action_after_retention, minimum_days, name_translations \
                 FROM compliance_retention_policies \
                 WHERE scope_kind = 'events' AND org_id = ?1",
                rusqlite::params![crate::services::org::DEFAULT_ORG_ID],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(days, crate::compliance::EVENTS_RETENTION_DAYS);
        assert_eq!(action, "delete");
        assert_eq!(minimum, 0);
        for locale in ["pl", "en", "de", "es", "fr"] {
            assert!(
                translations.contains(&format!("\"{locale}\"")),
                "missing locale {locale} in the seeded policy name"
            );
        }

        // The event log's own term must not have touched the AI-audit commitment.
        let ai_days: i64 = conn
            .query_row(
                "SELECT retention_days FROM compliance_retention_policies \
                 WHERE scope_kind = 'ai_audit' AND org_id = ?1",
                rusqlite::params![crate::services::org::DEFAULT_ORG_ID],
                |r| r.get(0),
            )
            .unwrap();
        assert!(ai_days >= crate::compliance::MINIMUM_AI_AUDIT_RETENTION_DAYS);
    }

    #[test]
    fn migration_v133_upgrades_v132_database_without_data_loss() {
        let conn = Connection::open_in_memory().unwrap();
        run(&conn).unwrap();
        rewind_to_v132(&conn);

        // The rewound CHECK rejects the new scope, proving we are really at v132.
        assert!(
            conn.execute(
                "INSERT INTO compliance_retention_policies \
                    (retention_policy_id, org_id, slug, name_translations, scope_kind, \
                     retention_days, is_default, is_active) \
                 VALUES ('probe', 'org-default', 'probe', '{}', 'events', 30, 0, 1)",
                [],
            )
            .is_err(),
            "pre-v133 CHECK must reject 'events'"
        );
        assert!(!column_exists(&conn, "audit_log", "correlation_id").unwrap());

        for action in ["login", "flow.run", "addon.install"] {
            conn.execute(
                "INSERT INTO audit_log (action, result) VALUES (?1, 'ok')",
                rusqlite::params![action],
            )
            .unwrap();
        }
        // An operator-tuned policy must survive the table rebuild verbatim.
        conn.execute(
            "UPDATE compliance_retention_policies SET retention_days = 999 \
             WHERE slug = 'general_default'",
            [],
        )
        .unwrap();
        let policies_before: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM compliance_retention_policies",
                [],
                |r| r.get(0),
            )
            .unwrap();

        create_events_tracking_foundation(&conn, 133, "events_tracking_foundation").unwrap();

        // Upgrading a real v132 database DOES rebuild: the narrow CHECK is gone
        // and the renamed table is the one the migration created.
        let table_sql: String = conn
            .query_row(
                "SELECT sql FROM sqlite_master \
                 WHERE type = 'table' AND name = 'compliance_retention_policies'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            table_sql.contains("'events'"),
            "the upgrade must widen the CHECK: {table_sql}"
        );

        let audit_rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM audit_log", [], |r| r.get(0))
            .unwrap();
        assert_eq!(audit_rows, 3, "audit rows must survive the upgrade");
        let null_correlations: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM audit_log WHERE correlation_id IS NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(null_correlations, 3, "backfilled rows stay uncorrelated");

        let tuned: i64 = conn
            .query_row(
                "SELECT retention_days FROM compliance_retention_policies \
                 WHERE slug = 'general_default'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(tuned, 999, "existing retention policies must be untouched");
        let policies_after: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM compliance_retention_policies",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            policies_after,
            policies_before + 1,
            "exactly one new policy"
        );

        assert!(role_permissions(&conn, "org_admin")
            .iter()
            .any(|p| p == "events.read_all"));
        assert!(foreign_key_check(&conn).unwrap().is_empty());

        let stamped: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM _migrations WHERE version = 133",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(stamped, 1);

        // Re-running on an already-migrated database is a no-op, not a duplicate
        // column, a duplicate policy or a second permission entry.
        conn.execute("DELETE FROM _migrations WHERE version = 133", [])
            .unwrap();
        create_events_tracking_foundation(&conn, 133, "events_tracking_foundation").unwrap();
        let events_policies: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM compliance_retention_policies WHERE scope_kind = 'events'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(events_policies, 1);
        assert_eq!(
            role_permissions(&conn, "org_admin")
                .iter()
                .filter(|p| *p == "events.read")
                .count(),
            1
        );
    }

    /// v71 rewrites the existing "Agent Run" flow (`…012`) to the single-graph
    /// inline loop region, in place, idempotently. A legacy row gets the new JSON;
    /// a second run is a no-op; an absent row is a no-op.
    #[test]
    fn migration_v71_rewrites_agent_run_to_inline_region_idempotently() {
        let conn = Connection::open_in_memory().unwrap();
        run(&conn).unwrap();

        let target = crate::db::seed::agent_run_flow_json();

        // Simulate a legacy database row carrying the old loop/subflow JSON so
        // the migration has work to do (bare `run` does not seed harness flows).
        let legacy = r#"{"nodes":[{"id":"t1","type":"trigger","config":{}}],"edges":[]}"#;
        conn.execute(
            "INSERT INTO flows (id, name, flow_json, status, is_default) \
             VALUES (?1, 'Agent Run', ?2, 'active', 0)",
            rusqlite::params![AGENT_RUN_FLOW_ID, legacy],
        )
        .unwrap();

        rewrite_agent_run_to_inline_region(&conn).unwrap();
        let after: String = conn
            .query_row(
                "SELECT flow_json FROM flows WHERE id = ?1",
                rusqlite::params![AGENT_RUN_FLOW_ID],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(after, target, "v71 must install the inline-region graph");

        // Idempotent: re-running changes nothing.
        rewrite_agent_run_to_inline_region(&conn).unwrap();
        let again: String = conn
            .query_row(
                "SELECT flow_json FROM flows WHERE id = ?1",
                rusqlite::params![AGENT_RUN_FLOW_ID],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(again, target);

        // Absent row: no-op, no error.
        conn.execute(
            "DELETE FROM flows WHERE id = ?1",
            rusqlite::params![AGENT_RUN_FLOW_ID],
        )
        .unwrap();
        rewrite_agent_run_to_inline_region(&conn).unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM flows WHERE id = ?1",
                rusqlite::params![AGENT_RUN_FLOW_ID],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 0, "absent row stays absent");
    }

    /// v72 upgrades a v71-shaped "Agent Run" row (inline region, blocking
    /// persist→output) to the region-streaming shape (region exit streams to
    /// output, persist on the blocking finalizer path). The target is
    /// `agent_run_flow_json()`, so a row carrying the older non-streaming graph
    /// is rewritten and a row already on the streaming graph is a no-op.
    #[test]
    fn migration_v72_upgrades_agent_run_to_region_streaming() {
        let conn = Connection::open_in_memory().unwrap();
        run(&conn).unwrap();

        let target = crate::db::seed::agent_run_flow_json();
        // A v71-era row: inline region but the exit feeds persist→output with NO
        // stream edge and output.format=text (the pre-streaming shape).
        let v71_shape = r#"{"nodes":[{"id":"t1","type":"trigger","config":{}},{"id":"x1","type":"tool_exec","region":"agent_turn","config":{}}],"edges":[{"from_node":"t1","to_node":"x1"}]}"#;
        conn.execute(
            "INSERT INTO flows (id, name, flow_json, status, is_default) \
             VALUES (?1, 'Agent Run', ?2, 'active', 0)",
            rusqlite::params![AGENT_RUN_FLOW_ID, v71_shape],
        )
        .unwrap();

        rewrite_agent_run_to_inline_region(&conn).unwrap();
        let after: String = conn
            .query_row(
                "SELECT flow_json FROM flows WHERE id = ?1",
                rusqlite::params![AGENT_RUN_FLOW_ID],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(after, target, "v72 must install the region-streaming graph");
        // The streaming wire is present: the region exit streams to output.
        assert!(
            after.contains(r#""from_port":"stream""#),
            "region-streaming graph must carry a stream edge"
        );
        assert!(
            after.contains(r#""mode":"stream""#),
            "output must be in stream mode"
        );

        // Idempotent re-run is a no-op.
        rewrite_agent_run_to_inline_region(&conn).unwrap();
        let again: String = conn
            .query_row(
                "SELECT flow_json FROM flows WHERE id = ?1",
                rusqlite::params![AGENT_RUN_FLOW_ID],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(again, target);
    }

    /// v74 rewrites a v72-shaped "Agent Run" row (empty config boxes, 200px node
    /// spacing) to the defaults-filled, 360px-spaced graph in place, idempotently.
    /// The target is `agent_run_flow_json()`, so a row carrying the older
    /// empty-config/overlapping-layout graph is rewritten and a row already on the
    /// filled graph is a no-op.
    #[test]
    fn migration_v74_fills_agent_run_defaults_idempotently() {
        let conn = Connection::open_in_memory().unwrap();
        run(&conn).unwrap();

        let target = crate::db::seed::agent_run_flow_json();
        // A v72-era row: correct region-streaming wiring but empty prompt config
        // and the old 200px spacing (the pre-defaults shape).
        let v72_shape = r#"{"nodes":[{"id":"c0","type":"agent_context","position":{"x":400,"y":0},"config":{"agent_id":"","from_vars":true}},{"id":"k1","type":"compact_context","position":{"x":600,"y":0},"region":"agent_turn","config":{"threshold_percent":50}}],"edges":[{"from_node":"c0","to_node":"k1"}]}"#;
        conn.execute(
            "INSERT INTO flows (id, name, flow_json, status, is_default) \
             VALUES (?1, 'Agent Run', ?2, 'active', 0)",
            rusqlite::params![AGENT_RUN_FLOW_ID, v72_shape],
        )
        .unwrap();

        rewrite_agent_run_to_inline_region(&conn).unwrap();
        let after: String = conn
            .query_row(
                "SELECT flow_json FROM flows WHERE id = ?1",
                rusqlite::params![AGENT_RUN_FLOW_ID],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(after, target, "v74 must install the defaults-filled graph");
        // The compaction prompt defaults are present (not empty boxes anymore).
        assert!(
            after.contains("structured \\nhandoff summary")
                || after.contains("structured handoff summary"),
            "filled graph must carry the summary system prompt"
        );
        // 360px spacing: the last node sits at x=2520 (8 nodes spaced 360).
        assert!(
            after.contains(r#""x":2520"#),
            "filled graph must use 360px node spacing"
        );

        // Idempotent re-run is a no-op.
        rewrite_agent_run_to_inline_region(&conn).unwrap();
        let again: String = conn
            .query_row(
                "SELECT flow_json FROM flows WHERE id = ?1",
                rusqlite::params![AGENT_RUN_FLOW_ID],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(again, target);
    }

    /// v113 strips the hidden `pii_filter` from the seeded "Default Chat" flow:
    /// the seeded-pii row is rewritten to the clean `trigger -> llm ->
    /// output(stream)` pipeline, while an admin-customized flow that also
    /// contains a pii_filter is left untouched (matched only against the exact
    /// seeded literal).
    /// Legacy rows: the wizard's container-name suggestion goes away and the
    /// config port is rewritten from `runtime_port` (dropped when the row has no
    /// runtime, e.g. an on-demand engine).
    #[test]
    fn migration_v132_rewrites_service_config_from_runtime_columns() {
        let conn = Connection::open_in_memory().unwrap();
        run(&conn).unwrap();

        conn.execute(
            "INSERT INTO services (engine_id, category, display_name, deploy_method, transport, \
             status, runtime_port, config_json) \
             VALUES ('vllm', 'llm', 'vLLM', 'docker', 'http_direct', 'running', 5005, ?1)",
            rusqlite::params![
                r#"{"port":5003,"container_name":"tentaflow-vllm-8p7iu","model_repo":"speakleash/Bielik"}"#
            ],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO services (engine_id, category, display_name, deploy_method, transport, \
             status, runtime_port, config_json) \
             VALUES ('teams-bot', 'agents', 'Teams Bot', 'docker', 'http_direct', 'stopped', NULL, ?1)",
            rusqlite::params![r#"{"port":5003,"container_name":"tentaflow-teams-bot-ck1mr"}"#],
        )
        .unwrap();

        conn.execute_batch(SERVICES_CONFIG_RUNTIME_TRUTH).unwrap();

        let running: String = conn
            .query_row(
                "SELECT config_json FROM services WHERE engine_id = 'vllm'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let cfg: serde_json::Value = serde_json::from_str(&running).unwrap();
        assert_eq!(cfg.get("port").and_then(|v| v.as_u64()), Some(5005));
        assert!(cfg.get("container_name").is_none(), "{running}");
        assert_eq!(
            cfg.get("model_repo").and_then(|v| v.as_str()),
            Some("speakleash/Bielik")
        );

        let on_demand: String = conn
            .query_row(
                "SELECT config_json FROM services WHERE engine_id = 'teams-bot'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let cfg: serde_json::Value = serde_json::from_str(&on_demand).unwrap();
        assert!(
            cfg.get("port").is_none() && cfg.get("container_name").is_none(),
            "{on_demand}"
        );
    }

    #[test]
    fn migration_v113_strips_pii_from_seeded_default_chat() {
        let conn = Connection::open_in_memory().unwrap();
        run(&conn).unwrap();

        // Reproduce an already-seeded DB: the canonical Default Chat row carrying
        // the hidden pii_filter (the shape shipped before v113).
        conn.execute(
            "INSERT OR REPLACE INTO flows (id, name, description, service_type, flow_json, status, is_default) \
             VALUES (?1, 'Default Chat', 'Streaming chat pipeline: trigger -> LLM -> pii_filter -> output(stream).', 'chat', ?2, 'active', 1)",
            rusqlite::params![DEFAULT_CHAT_FLOW_ID, DEFAULT_CHAT_FLOW_JSON_WITH_PII],
        )
        .unwrap();

        // A separate, admin-customized flow that deliberately keeps pii_filter.
        let custom_id = uuid::Uuid::new_v4().to_string();
        let custom_json = r#"{"nodes":[{"id":"t1","type":"trigger","position":{"x":0,"y":0},"config":{}},{"id":"l1","type":"llm","position":{"x":200,"y":0},"config":{}},{"id":"p1","type":"pii_filter","position":{"x":400,"y":0},"config":{}},{"id":"o1","type":"output","position":{"x":600,"y":0},"config":{"mode":"stream"}}],"edges":[{"from_node":"t1","to_node":"l1","from_port":"text","data_type":"text"},{"from_node":"l1","to_node":"p1","from_port":"text"},{"from_node":"p1","to_node":"o1","from_port":"text","to_port":"text","data_type":"text"}]}"#;
        conn.execute(
            "INSERT INTO flows (id, name, description, service_type, flow_json, status, is_default) \
             VALUES (?1, 'My PII Chat', 'custom', 'chat', ?2, 'active', 0)",
            rusqlite::params![custom_id, custom_json],
        )
        .unwrap();

        strip_pii_from_default_chat_flow(&conn).unwrap();

        // (a) seeded Default Chat lost pii_filter and now streams llm -> output.
        let (default_json, default_desc): (String, String) = conn
            .query_row(
                "SELECT flow_json, description FROM flows WHERE id = ?1",
                rusqlite::params![DEFAULT_CHAT_FLOW_ID],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(default_json, crate::db::seed::LEGACY_DEFAULT_CHAT_FLOW_JSON);
        assert!(!default_json.contains("pii_filter"), "pii_filter removed");
        assert!(!default_desc.contains("pii_filter"), "description updated");
        let parsed: serde_json::Value = serde_json::from_str(&default_json).unwrap();
        let types: Vec<&str> = parsed["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n["type"].as_str().unwrap())
            .collect();
        assert_eq!(types, vec!["trigger", "llm", "output"]);
        let edges = parsed["edges"].as_array().unwrap();
        assert_eq!(edges.len(), 2);
        // The llm -> output edge is the streaming shape (from stream, to text).
        let has_stream_edge = edges.iter().any(|e| {
            e["from_node"] == "l1"
                && e["to_node"] == "o1"
                && e["from_port"] == "stream"
                && e["to_port"] == "text"
        });
        assert!(has_stream_edge, "llm -> output streaming edge preserved");

        // (b) the admin-customized pii flow is untouched.
        let custom_after: String = conn
            .query_row(
                "SELECT flow_json FROM flows WHERE id = ?1",
                rusqlite::params![custom_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(custom_after, custom_json, "custom pii flow untouched");

        // (c) idempotent re-run is a no-op (row no longer matches the pii literal).
        strip_pii_from_default_chat_flow(&conn).unwrap();
        let again: String = conn
            .query_row(
                "SELECT flow_json FROM flows WHERE id = ?1",
                rusqlite::params![DEFAULT_CHAT_FLOW_ID],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(again, crate::db::seed::LEGACY_DEFAULT_CHAT_FLOW_JSON);
    }

    /// A fresh `run(&conn)` (all migrations + nothing else) leaves the seeded
    /// "Agent Run" row compilable as a single inline loop region. This proves the
    /// migration and seed agree on the canonical graph.
    #[test]
    fn fresh_db_agent_run_is_the_inline_region_graph() {
        let conn = Connection::open_in_memory().unwrap();
        run(&conn).unwrap();
        // The seed inserts the row on fresh installs; the migration UPDATE is a
        // no-op there because the JSON already matches.
        let stored: Option<String> = conn
            .query_row(
                "SELECT flow_json FROM flows WHERE id = ?1",
                rusqlite::params![AGENT_RUN_FLOW_ID],
                |r| r.get(0),
            )
            .ok();
        if let Some(json) = stored {
            assert_eq!(json, crate::db::seed::agent_run_flow_json());
        }
    }

    /// An in-process benchmark target (`api_type='local'`) must survive the
    /// CHECK constraint on a fully migrated DB. v107 created the table admitting
    /// only openai/anthropic, so without the v124 rebuild every save of an
    /// embedded / coding-agent / sidecar target fails at the DB layer.
    #[test]
    fn migrated_db_accepts_in_process_benchmark_target() {
        let conn = Connection::open_in_memory().unwrap();
        run(&conn).unwrap();
        let org: String = conn
            .query_row("SELECT org_id FROM organizations LIMIT 1", [], |r| r.get(0))
            .expect("seeded org");
        conn.execute(
            "INSERT INTO benchmarks (id, org_id, name, config_json) VALUES ('b1', ?1, 'b', '{}')",
            rusqlite::params![org],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO benchmark_targets
                 (id, benchmark_id, kind, api_type, host, port, model, label)
             VALUES ('t1', 'b1', 'service', 'local', '', 0, 'bielik-4.5b', 'Bielik')",
            [],
        )
        .expect("in-process target must pass the api_type CHECK");
        // The two endpoint protocols keep working, and an unknown one is still rejected.
        conn.execute(
            "INSERT INTO benchmark_targets
                 (id, benchmark_id, kind, api_type, host, port, model, label)
             VALUES ('t2', 'b1', 'external', 'anthropic', 'api.anthropic.com', 443, 'm', 'A')",
            [],
        )
        .unwrap();
        assert!(conn
            .execute(
                "INSERT INTO benchmark_targets
                     (id, benchmark_id, kind, api_type, host, port, model, label)
                 VALUES ('t3', 'b1', 'external', 'grpc', 'h', 1, 'm', 'G')",
                [],
            )
            .is_err());
    }

    /// Builds a DB at exactly the pre-flip schema state (INTEGER identity ids) by
    /// running every migration except the v56 UUID flip and anything after it.
    /// Mirrors `run` but stops before the flip so the migration test can seed
    /// legacy integer rows.
    fn seed_pre_uuid_schema(conn: &Connection) {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS _migrations (
                version INTEGER PRIMARY KEY,
                name TEXT NOT NULL,
                applied_at TEXT NOT NULL DEFAULT (datetime('now'))
            );",
        )
        .unwrap();
        for (version, name, step) in get_migrations() {
            if version >= CORE_IDENTITY_FLIP_VERSION {
                break;
            }
            let tx = conn.unchecked_transaction().unwrap();
            match step {
                MigrationStep::Sql(sql) => tx.execute_batch(sql).unwrap(),
                MigrationStep::Rust(f) => f(&tx).unwrap(),
                MigrationStep::RustSelfManaged(_) => {
                    unreachable!("no self-managed step below the identity flip")
                }
            }
            tx.execute(
                "INSERT INTO _migrations (version, name) VALUES (?1, ?2)",
                rusqlite::params![version, name],
            )
            .unwrap();
            tx.commit().unwrap();
        }
        // Re-stamp identity tables back to INTEGER PKs: the squashed v1 now
        // declares them TEXT, so for the migration test we must recreate the
        // pre-UUID INTEGER shape that real legacy databases carry.
        rebuild_identity_tables_as_integer(conn);
    }

    /// Drops the TEXT-PK identity tables created by the (already-flipped) v1
    /// schema and recreates them with the historical INTEGER AUTOINCREMENT PKs,
    /// so the migration test reproduces a genuine legacy database.
    fn rebuild_identity_tables_as_integer(conn: &Connection) {
        conn.execute_batch("PRAGMA foreign_keys = OFF;").unwrap();
        conn.execute_batch(
            r#"
            DROP TABLE IF EXISTS group_members;
            DROP TABLE IF EXISTS flow_versions;
            DROP TABLE IF EXISTS flow_model_bindings;
            DROP TABLE IF EXISTS flow_executions;
            DROP TABLE IF EXISTS flows;
            DROP TABLE IF EXISTS user_groups;
            DROP TABLE IF EXISTS user_accounts;

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
            CREATE TABLE flows (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL,
                description TEXT,
                version INTEGER DEFAULT 1,
                is_default INTEGER NOT NULL DEFAULT 0,
                service_type TEXT,
                flow_json TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'draft' CHECK(status IN ('draft','active','decoded')),
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now')),
                published_model_name TEXT NULL
            );
            CREATE TABLE flow_model_bindings (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                flow_id INTEGER NOT NULL REFERENCES flows(id) ON DELETE CASCADE,
                model_pattern TEXT NOT NULL UNIQUE,
                priority INTEGER DEFAULT 0
            );
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
            "#,
        )
        .unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
    }

    fn index_exists(conn: &Connection, name: &str) -> bool {
        let found: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name = ?1",
                rusqlite::params![name],
                |r| r.get(0),
            )
            .unwrap();
        found == 1
    }

    fn index_sql(conn: &Connection, name: &str) -> String {
        conn.query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'index' AND name = ?1",
            rusqlite::params![name],
            |r| r.get(0),
        )
        .unwrap()
    }

    fn graph_path(conn: &Connection, org_id: &str, addon_id: &str, collection: &str) -> String {
        conn.query_row(
            "SELECT file_path FROM addon_graph_collections \
             WHERE org_id = ?1 AND addon_id = ?2 AND collection = ?3",
            rusqlite::params![org_id, addon_id, collection],
            |r| r.get(0),
        )
        .unwrap()
    }

    /// Inserts one graph collection row verbatim, so a test can plant a path the
    /// runtime would never mint today.
    fn seed_graph_collection(
        conn: &Connection,
        org_id: &str,
        addon_id: &str,
        collection: &str,
        file_path: &str,
    ) {
        conn.execute(
            "INSERT INTO addon_graph_collections \
                (org_id, addon_id, collection, file_path, engine, created_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, 'sled', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
            rusqlite::params![org_id, addon_id, collection, file_path],
        )
        .unwrap();
    }

    /// A database whose graph registry still points at the directory the data was
    /// moved AWAY from — the state v134 exists to repair. `addons_data_dir` is the
    /// only input `current_orgs_dir` has, so setting it pins the expected target
    /// without depending on where the test process resolved `tentaflow_home()`.
    const STALE_ORGS_ROOT: &str = "/srv/tentaflow-old/orgs";
    const CURRENT_ORGS_ROOT: &str = "/mnt/big/orgs";

    #[test]
    fn migration_v134_repairs_only_stale_derived_graph_paths() {
        let conn = Connection::open_in_memory().unwrap();
        run(&conn).unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO settings (key, value) VALUES ('addons_data_dir', ?1)",
            rusqlite::params![CURRENT_ORGS_ROOT],
        )
        .unwrap();

        // (a) stale: derived under the OLD root, orphaned by a pre-fix move.
        seed_graph_collection(
            &conn,
            "org-default",
            "notes",
            "notes_kg",
            &format!("{STALE_ORGS_ROOT}/org-default/addons/notes/graph/notes_kg.cozo"),
        );
        // (b) already correct: derived under the CURRENT root.
        seed_graph_collection(
            &conn,
            "org-b",
            "memory",
            "facts",
            &format!("{CURRENT_ORGS_ROOT}/org-b/addons/memory/graph/facts.cozo"),
        );
        // (c) explicit directory (`ensure_collection_at`): outside the
        // organizations tree on purpose, and not shaped like a derived path.
        let custom = "/var/lib/tentaflow-custom/kg_active.cozo";
        seed_graph_collection(&conn, "org-c", "ps-proj-1", "kg_active", custom);

        repair_graph_collection_paths(&conn).unwrap();

        assert_eq!(
            graph_path(&conn, "org-default", "notes", "notes_kg"),
            format!("{CURRENT_ORGS_ROOT}/org-default/addons/notes/graph/notes_kg.cozo"),
            "a derived path under a dead root must be moved to the current one"
        );
        assert_eq!(
            graph_path(&conn, "org-b", "memory", "facts"),
            format!("{CURRENT_ORGS_ROOT}/org-b/addons/memory/graph/facts.cozo"),
            "a row already under the current root must not be touched"
        );
        assert_eq!(
            graph_path(&conn, "org-c", "ps-proj-1", "kg_active"),
            custom,
            "an explicitly placed collection must survive the repair verbatim"
        );

        // Nothing is lost or invented: three rows in, three rows out.
        let rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM addon_graph_collections", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(rows, 3);

        // Idempotency: a second pass changes nothing at all.
        let before: Vec<String> = {
            let mut stmt = conn
                .prepare("SELECT file_path FROM addon_graph_collections ORDER BY org_id")
                .unwrap();
            let out = stmt
                .query_map([], |r| r.get::<_, String>(0))
                .unwrap()
                .map(|r| r.unwrap())
                .collect();
            out
        };
        repair_graph_collection_paths(&conn).unwrap();
        let after: Vec<String> = {
            let mut stmt = conn
                .prepare("SELECT file_path FROM addon_graph_collections ORDER BY org_id")
                .unwrap();
            let out = stmt
                .query_map([], |r| r.get::<_, String>(0))
                .unwrap()
                .map(|r| r.unwrap())
                .collect();
            out
        };
        assert_eq!(before, after, "the repair must be idempotent");
    }

    #[test]
    fn current_orgs_dir_prefers_the_setting_over_the_category_default() {
        let conn = Connection::open_in_memory().unwrap();
        run(&conn).unwrap();

        // No setting: the category default, exactly like `paths::category_dir`.
        assert_eq!(
            current_orgs_dir(&conn).unwrap(),
            crate::paths::StorageCategory::AddonData.default_dir()
        );

        // A blank setting is "no override", matching `paths::override_path`.
        conn.execute(
            "INSERT OR REPLACE INTO settings (key, value) VALUES ('addons_data_dir', '   ')",
            [],
        )
        .unwrap();
        assert_eq!(
            current_orgs_dir(&conn).unwrap(),
            crate::paths::StorageCategory::AddonData.default_dir()
        );

        conn.execute(
            "INSERT OR REPLACE INTO settings (key, value) VALUES ('addons_data_dir', ?1)",
            rusqlite::params![CURRENT_ORGS_ROOT],
        )
        .unwrap();
        assert_eq!(
            current_orgs_dir(&conn).unwrap(),
            std::path::PathBuf::from(CURRENT_ORGS_ROOT)
        );
    }

    #[test]
    fn migration_v135_fresh_database_has_run_provenance() {
        let conn = Connection::open_in_memory().unwrap();
        run(&conn).unwrap();

        for table in ["flow_executions", "agent_runs"] {
            for column in RUN_PROVENANCE_COLUMNS {
                assert!(
                    column_exists(&conn, table, column).unwrap(),
                    "{table} is missing {column}"
                );
                assert!(
                    column_is_text(&conn, table, column).unwrap(),
                    "{table}.{column} must be TEXT"
                );
            }
        }

        for name in [
            "idx_flow_executions_origin",
            "idx_flow_executions_actor",
            "idx_flow_executions_correlation",
            "idx_agent_runs_origin",
            "idx_agent_runs_actor",
            "idx_agent_runs_correlation",
        ] {
            assert!(index_exists(&conn, name), "missing index {name}");
        }

        // The browse queries filter on one value and order by time, so the time
        // column has to be IN the index, not merely available on the row.
        assert!(index_sql(&conn, "idx_flow_executions_origin").contains("started_at"));
        assert!(index_sql(&conn, "idx_agent_runs_origin").contains("created_at"));
        assert!(index_sql(&conn, "idx_flow_executions_actor").contains("actor_id"));
        assert!(index_sql(&conn, "idx_agent_runs_actor").contains("actor_id"));

        // The provenance stamp must never be forged by a column default.
        for table in ["flow_executions", "agent_runs"] {
            let table_sql: String = conn
                .query_row(
                    "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = ?1",
                    rusqlite::params![table],
                    |r| r.get(0),
                )
                .unwrap();
            assert!(
                !table_sql.contains("origin TEXT NOT NULL"),
                "{table}.origin must stay nullable: {table_sql}"
            );
        }

        assert!(foreign_key_check(&conn).unwrap().is_empty());
    }

    /// Strips everything v135 adds, so the migration can be exercised on a
    /// database that really is at v134. `DROP COLUMN` refuses an indexed column,
    /// hence the indexes go first.
    fn rewind_to_v134(conn: &Connection) {
        for name in [
            "idx_flow_executions_origin",
            "idx_flow_executions_actor",
            "idx_flow_executions_correlation",
            "idx_agent_runs_origin",
            "idx_agent_runs_actor",
            "idx_agent_runs_correlation",
        ] {
            conn.execute_batch(&format!("DROP INDEX IF EXISTS {name};"))
                .unwrap();
        }
        for table in ["flow_executions", "agent_runs"] {
            for column in RUN_PROVENANCE_COLUMNS {
                conn.execute_batch(&format!("ALTER TABLE {table} DROP COLUMN {column};"))
                    .unwrap();
            }
        }
        conn.execute("DELETE FROM _migrations WHERE version = 135", [])
            .unwrap();
    }

    #[test]
    fn migration_v135_upgrades_v134_database_without_data_loss() {
        let conn = Connection::open_in_memory().unwrap();
        run(&conn).unwrap();
        rewind_to_v134(&conn);

        for column in RUN_PROVENANCE_COLUMNS {
            assert!(
                !column_exists(&conn, "flow_executions", column).unwrap(),
                "the rewind must really land on v134"
            );
        }

        conn.execute(
            "INSERT INTO flows (id, name, flow_json, status) \
             VALUES ('flow-1', 'chat', '{}', 'active')",
            [],
        )
        .unwrap();
        for (request_id, status, tokens) in [
            ("req-a", "completed", 120),
            ("req-b", "error", 0),
            ("req-c", "running", 45),
        ] {
            conn.execute(
                "INSERT INTO flow_executions \
                    (flow_id, request_id, started_at, status, total_tokens) \
                 VALUES ('flow-1', ?1, '2026-08-01T10:00:00Z', ?2, ?3)",
                rusqlite::params![request_id, status, tokens],
            )
            .unwrap();
        }
        for (id, status, iterations) in [("run-a", "completed", 7), ("run-b", "failed", 2)] {
            conn.execute(
                "INSERT INTO agent_runs (id, agent_id, status, prompt, iterations) \
                 VALUES (?1, 'agent-1', ?2, 'do the thing', ?3)",
                rusqlite::params![id, status, iterations],
            )
            .unwrap();
        }

        let executions_before: i64 = conn
            .query_row("SELECT COUNT(*) FROM flow_executions", [], |r| r.get(0))
            .unwrap();
        let runs_before: i64 = conn
            .query_row("SELECT COUNT(*) FROM agent_runs", [], |r| r.get(0))
            .unwrap();

        add_run_provenance_columns(&conn).unwrap();

        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM flow_executions", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            executions_before,
            "no flow execution may be lost by the upgrade"
        );
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM agent_runs", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            runs_before,
            "no agent run may be lost by the upgrade"
        );

        // Spot-check that pre-existing values came through byte for byte.
        let (status, tokens): (String, i64) = conn
            .query_row(
                "SELECT status, total_tokens FROM flow_executions WHERE request_id = 'req-a'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(status, "completed");
        assert_eq!(tokens, 120);
        let (run_status, iterations): (String, i64) = conn
            .query_row(
                "SELECT status, iterations FROM agent_runs WHERE id = 'run-a'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(run_status, "completed");
        assert_eq!(iterations, 7);

        // Historical rows stay honestly unattributed — a guessed origin would be
        // indistinguishable from an observed one.
        let unstamped_executions: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM flow_executions \
                 WHERE origin IS NULL AND actor_kind IS NULL AND actor_id IS NULL \
                   AND actor_user_id IS NULL AND correlation_id IS NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(unstamped_executions, executions_before);
        let unstamped_runs: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM agent_runs \
                 WHERE origin IS NULL AND actor_kind IS NULL AND actor_id IS NULL \
                   AND actor_user_id IS NULL AND correlation_id IS NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(unstamped_runs, runs_before);

        // That new rows really carry the stamp is NOT asserted here: an INSERT
        // written in this test body would prove only that the column accepts a
        // value, not that the product supplies one. The writers are exercised
        // through their production entry points instead —
        // `flow_engine::executor::provenance_persistence_tests` for
        // `flow_executions` and `agents::run_manager` for `agent_runs`.

        assert!(foreign_key_check(&conn).unwrap().is_empty());

        // Re-running on an already-migrated database is a no-op, not a duplicate
        // column error and not a second index.
        add_run_provenance_columns(&conn).unwrap();
        for name in ["idx_flow_executions_origin", "idx_agent_runs_correlation"] {
            assert!(index_exists(&conn, name));
        }
    }

    #[test]
    fn fresh_database_migrates_to_134_with_clean_foreign_keys() {
        let conn = Connection::open_in_memory().unwrap();
        run(&conn).unwrap();

        let head: i64 = conn
            .query_row("SELECT MAX(version) FROM _migrations", [], |r| r.get(0))
            .unwrap();
        assert_eq!(head, 135, "135 must be the highest applied migration");
        assert!(foreign_key_check(&conn).unwrap().is_empty());

        // Running the whole ladder twice must be a no-op.
        run(&conn).unwrap();
        let head_again: i64 = conn
            .query_row("SELECT MAX(version) FROM _migrations", [], |r| r.get(0))
            .unwrap();
        assert_eq!(head_again, 135);
    }
}
