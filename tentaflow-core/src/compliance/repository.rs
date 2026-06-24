// =============================================================================
// Plik: compliance/repository.rs
// Opis: Repozytorium SQLite dla Compliance Core, ROPA, retencji i AI audit.
// =============================================================================

use anyhow::{anyhow, Result};
use rusqlite::{params, Connection, OptionalExtension};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::models::*;
use super::MINIMUM_AI_AUDIT_RETENTION_DAYS;

fn now_utc() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

fn hash_text(value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    hex::encode(hasher.finalize())
}

fn default_record_id(org_id: &str, base_id: &str) -> String {
    if org_id == crate::services::org::DEFAULT_ORG_ID {
        base_id.to_string()
    } else {
        format!("{org_id}:{base_id}")
    }
}

pub fn default_ai_legal_basis_id(org_id: &str) -> String {
    default_record_id(org_id, "lb-core-ai-legitimate-interest")
}

fn row_risk_class(row: &rusqlite::Row<'_>, idx: usize) -> rusqlite::Result<ComplianceRiskClass> {
    let value: String = row.get(idx)?;
    ComplianceRiskClass::from_str(&value).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            idx,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("nieznana klasa ryzyka compliance: {value}"),
            )),
        )
    })
}

fn row_retention_scope(
    row: &rusqlite::Row<'_>,
    idx: usize,
) -> rusqlite::Result<RetentionScopeKind> {
    let value: String = row.get(idx)?;
    RetentionScopeKind::from_str(&value).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            idx,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("nieznany zakres retencji compliance: {value}"),
            )),
        )
    })
}

fn row_ai_status(row: &rusqlite::Row<'_>, idx: usize) -> rusqlite::Result<AiEventStatus> {
    let value: String = row.get(idx)?;
    AiEventStatus::from_str(&value).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            idx,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("nieznany status AI audit: {value}"),
            )),
        )
    })
}

fn retention_policy_from_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<ComplianceRetentionPolicy> {
    Ok(ComplianceRetentionPolicy {
        retention_policy_id: row.get(0)?,
        org_id: row.get(1)?,
        slug: row.get(2)?,
        name_translations: row.get(3)?,
        scope_kind: row_retention_scope(row, 4)?,
        category_id: row.get(5)?,
        retention_days: row.get(6)?,
        minimum_days: row.get(7)?,
        action_after_retention: row.get(8)?,
        is_default: row.get::<_, i64>(9)? != 0,
        is_active: row.get::<_, i64>(10)? != 0,
    })
}

pub fn ensure_org_defaults(conn: &Connection, org_id: &str) -> Result<()> {
    let category_user_account = default_record_id(org_id, "cat-core-user-account");
    let category_contact = default_record_id(org_id, "cat-core-contact");
    let category_ai_prompt = default_record_id(org_id, "cat-core-ai-prompt");
    let category_secret = default_record_id(org_id, "cat-core-secret");
    let category_audit = default_record_id(org_id, "cat-core-audit");

    let activity_auth = default_record_id(org_id, "act-core-auth");
    let activity_ai = default_record_id(org_id, "act-core-ai");
    let activity_crm = default_record_id(org_id, "act-core-crm");
    let activity_audit = default_record_id(org_id, "act-core-audit");

    let legal_auth_contract = default_record_id(org_id, "lb-core-auth-contract");
    let legal_ai = default_ai_legal_basis_id(org_id);
    let legal_audit = default_record_id(org_id, "lb-core-audit-legal-obligation");

    conn.execute(
        "INSERT OR IGNORE INTO compliance_data_categories \
            (category_id, org_id, slug, name_translations, description_translations, personal_data, sensitive_data, risk_class, source_scope) \
         VALUES (?1, ?2, 'user_account', json_object('pl','Konta użytkowników','en','User accounts'), \
                 json_object('pl','Tożsamość, logowanie i role użytkowników TentaFlow.','en','Identity, login and TentaFlow user roles.'), \
                 1, 0, 'standard', 'core')",
        params![category_user_account, org_id],
    )?;
    conn.execute(
        "INSERT OR IGNORE INTO compliance_data_categories \
            (category_id, org_id, slug, name_translations, description_translations, personal_data, sensitive_data, risk_class, source_scope) \
         VALUES (?1, ?2, 'contact', json_object('pl','Kontakty CRM','en','CRM contacts'), \
                 json_object('pl','Dane osób i firm zarządzane przez addony CRM/kontaktów.','en','People and company data managed by CRM/contact addons.'), \
                 1, 0, 'standard', 'addon')",
        params![category_contact, org_id],
    )?;
    conn.execute(
        "INSERT OR IGNORE INTO compliance_data_categories \
            (category_id, org_id, slug, name_translations, description_translations, personal_data, sensitive_data, risk_class, source_scope) \
         VALUES (?1, ?2, 'ai_prompt', json_object('pl','Prompty i odpowiedzi AI','en','AI prompts and responses'), \
                 json_object('pl','Treści przekazywane do modeli AI, odpowiedzi, źródła RAG i wywołania narzędzi.','en','Content sent to AI models, responses, RAG sources and tool calls.'), \
                 1, 0, 'high', 'core')",
        params![category_ai_prompt, org_id],
    )?;
    conn.execute(
        "INSERT OR IGNORE INTO compliance_data_categories \
            (category_id, org_id, slug, name_translations, description_translations, personal_data, sensitive_data, risk_class, source_scope) \
         VALUES (?1, ?2, 'external_secret', json_object('pl','Klucze i sekrety zewnętrzne','en','External keys and secrets'), \
                 json_object('pl','Tokeny i dane dostępowe do usług zewnętrznych.','en','Tokens and access data for external services.'), \
                 0, 0, 'critical', 'core')",
        params![category_secret, org_id],
    )?;
    conn.execute(
        "INSERT OR IGNORE INTO compliance_data_categories \
            (category_id, org_id, slug, name_translations, description_translations, personal_data, sensitive_data, risk_class, source_scope) \
         VALUES (?1, ?2, 'audit_trail', json_object('pl','Log audytowy','en','Audit trail'), \
                 json_object('pl','Techniczne logi operacji, integralności i decyzji bezpieczeństwa.','en','Technical logs for operations, integrity and security decisions.'), \
                 1, 0, 'high', 'core')",
        params![category_audit, org_id],
    )?;

    conn.execute(
        "INSERT OR IGNORE INTO compliance_processing_activities \
            (activity_id, org_id, slug, name_translations, purpose_translations, controller_role, system_scope, status) \
         VALUES (?1, ?2, 'identity_and_access', json_object('pl','Tożsamość i dostęp','en','Identity and access'), \
                 json_object('pl','Uwierzytelnianie użytkowników, role, grupy, uprawnienia i synchronizacja polityk.','en','User authentication, roles, groups, permissions and policy synchronization.'), \
                 'controller', 'core', 'active')",
        params![activity_auth, org_id],
    )?;
    conn.execute(
        "INSERT OR IGNORE INTO compliance_processing_activities \
            (activity_id, org_id, slug, name_translations, purpose_translations, controller_role, system_scope, status) \
         VALUES (?1, ?2, 'ai_inference', json_object('pl','Przetwarzanie AI','en','AI processing'), \
                 json_object('pl','Obsługa zapytań AI, tool calling, RAG oraz audyt promptów i odpowiedzi.','en','AI requests, tool calling, RAG and prompt/response audit.'), \
                 'controller', 'core', 'active')",
        params![activity_ai, org_id],
    )?;
    conn.execute(
        "INSERT OR IGNORE INTO compliance_processing_activities \
            (activity_id, org_id, slug, name_translations, purpose_translations, controller_role, system_scope, status) \
         VALUES (?1, ?2, 'crm_contacts', json_object('pl','Zarządzanie kontaktami CRM','en','CRM contact management'), \
                 json_object('pl','Przechowywanie i synchronizacja kontaktów, firm i relacji biznesowych.','en','Storage and synchronization of contacts, companies and business relationships.'), \
                 'controller', 'addon', 'active')",
        params![activity_crm, org_id],
    )?;
    conn.execute(
        "INSERT OR IGNORE INTO compliance_processing_activities \
            (activity_id, org_id, slug, name_translations, purpose_translations, controller_role, system_scope, status) \
         VALUES (?1, ?2, 'audit_and_security', json_object('pl','Audyt i bezpieczeństwo','en','Audit and security'), \
                 json_object('pl','Rejestrowanie zdarzeń bezpieczeństwa, zgodności i integralności systemu.','en','Recording security, compliance and system integrity events.'), \
                 'controller', 'core', 'active')",
        params![activity_audit, org_id],
    )?;

    for (activity_id, category_id) in [
        (&activity_auth, &category_user_account),
        (&activity_auth, &category_secret),
        (&activity_ai, &category_ai_prompt),
        (&activity_ai, &category_secret),
        (&activity_crm, &category_contact),
        (&activity_audit, &category_audit),
        (&activity_audit, &category_user_account),
    ] {
        conn.execute(
            "INSERT OR IGNORE INTO compliance_activity_categories(activity_id, category_id) VALUES (?1, ?2)",
            params![activity_id, category_id],
        )?;
    }

    conn.execute(
        "INSERT OR IGNORE INTO compliance_legal_basis \
            (legal_basis_id, org_id, activity_id, category_id, basis_kind, basis_reference, description_translations, is_active) \
         VALUES (?1, ?2, ?3, ?4, 'contract', 'RODO art. 6 ust. 1 lit. b', \
                 json_object('pl','Niezbędne do obsługi konta i dostępu do usługi.','en','Required to provide account and service access.'), 1)",
        params![legal_auth_contract, org_id, activity_auth, category_user_account],
    )?;
    conn.execute(
        "INSERT OR IGNORE INTO compliance_legal_basis \
            (legal_basis_id, org_id, activity_id, category_id, basis_kind, basis_reference, description_translations, is_active) \
         VALUES (?1, ?2, ?3, ?4, 'legitimate_interest', 'RODO art. 6 ust. 1 lit. f', \
                 json_object('pl','Audyt, bezpieczeństwo i rozliczalność wywołań AI.','en','Audit, security and accountability of AI calls.'), 1)",
        params![legal_ai, org_id, activity_ai, category_ai_prompt],
    )?;
    conn.execute(
        "INSERT OR IGNORE INTO compliance_legal_basis \
            (legal_basis_id, org_id, activity_id, category_id, basis_kind, basis_reference, description_translations, is_active) \
         VALUES (?1, ?2, ?3, ?4, 'legal_obligation', 'RODO art. 6 ust. 1 lit. c', \
                 json_object('pl','Prowadzenie niezbędnych zapisów audytowych i dowodowych.','en','Maintaining required audit and evidence records.'), 1)",
        params![legal_audit, org_id, activity_audit, category_audit],
    )?;

    for (base_id, slug, name_pl, name_en, scope, days, minimum, action) in [
        (
            "ret-core-ai-audit-default",
            "ai_audit_default",
            "AI audit minimum 6 miesięcy",
            "AI audit minimum 6 months",
            RetentionScopeKind::AiAudit,
            183,
            183,
            "archive",
        ),
        (
            "ret-core-audit-default",
            "audit_default",
            "Audit trail minimum 6 miesięcy",
            "Audit trail minimum 6 months",
            RetentionScopeKind::Audit,
            183,
            183,
            "archive",
        ),
        (
            "ret-core-general-default",
            "general_default",
            "Retencja ogólna",
            "General retention",
            RetentionScopeKind::General,
            365,
            0,
            "delete",
        ),
        (
            "ret-core-document-default",
            "documents_default",
            "Dokumenty compliance",
            "Compliance documents",
            RetentionScopeKind::Document,
            2190,
            0,
            "archive",
        ),
        (
            "ret-core-dsar-default",
            "dsar_default",
            "Wnioski DSAR",
            "DSAR requests",
            RetentionScopeKind::Dsar,
            1095,
            0,
            "archive",
        ),
        (
            "ret-core-breach-default",
            "breach_default",
            "Rejestr naruszeń",
            "Breach register",
            RetentionScopeKind::Breach,
            2190,
            0,
            "archive",
        ),
        // Agent run logs (Harness §3.3): tool outputs in `run_log` may be PII
        // (CRM/memory), so the row's text columns are purged after the term while
        // the statistical row stays. 30-day default; the purge job lands in phase 6/7.
        (
            "ret-core-agent-runs-default",
            "agent_runs_default",
            "Przebiegi agentów",
            "Agent runs",
            RetentionScopeKind::AgentRuns,
            30,
            0,
            "delete",
        ),
    ] {
        conn.execute(
            "INSERT OR IGNORE INTO compliance_retention_policies \
                (retention_policy_id, org_id, slug, name_translations, scope_kind, category_id, retention_days, minimum_days, action_after_retention, is_default, is_active) \
             VALUES (?1, ?2, ?3, json_object('pl',?4,'en',?5), ?6, NULL, ?7, ?8, ?9, 1, 1)",
            params![
                default_record_id(org_id, base_id),
                org_id,
                slug,
                name_pl,
                name_en,
                scope.as_str(),
                days,
                minimum,
                action,
            ],
        )?;
    }

    Ok(())
}

pub fn list_data_categories(
    conn: &Connection,
    org_id: &str,
) -> Result<Vec<ComplianceDataCategory>> {
    let mut stmt = conn.prepare_cached(
        "SELECT category_id, org_id, slug, name_translations, description_translations, \
                personal_data, sensitive_data, risk_class, source_scope, addon_id \
         FROM compliance_data_categories \
         WHERE org_id = ?1 \
         ORDER BY slug",
    )?;
    let rows = stmt
        .query_map(params![org_id], |row| {
            Ok(ComplianceDataCategory {
                category_id: row.get(0)?,
                org_id: row.get(1)?,
                slug: row.get(2)?,
                name_translations: row.get(3)?,
                description_translations: row.get(4)?,
                personal_data: row.get::<_, i64>(5)? != 0,
                sensitive_data: row.get::<_, i64>(6)? != 0,
                risk_class: row_risk_class(row, 7)?,
                source_scope: row.get(8)?,
                addon_id: row.get(9)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn list_retention_policies(
    conn: &Connection,
    org_id: &str,
) -> Result<Vec<ComplianceRetentionPolicy>> {
    let mut stmt = conn.prepare_cached(
        "SELECT retention_policy_id, org_id, slug, name_translations, scope_kind, category_id, \
                retention_days, minimum_days, action_after_retention, is_default, is_active \
         FROM compliance_retention_policies \
         WHERE org_id = ?1 \
         ORDER BY scope_kind, is_default DESC, slug",
    )?;
    let rows = stmt
        .query_map(params![org_id], retention_policy_from_row)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn resolve_retention_policy(
    conn: &Connection,
    org_id: &str,
    scope_kind: RetentionScopeKind,
    category_id: Option<&str>,
) -> Result<ComplianceRetentionPolicy> {
    let category_policy = if let Some(category_id) = category_id {
        conn.query_row(
            "SELECT retention_policy_id, org_id, slug, name_translations, scope_kind, category_id, \
                    retention_days, minimum_days, action_after_retention, is_default, is_active \
             FROM compliance_retention_policies \
             WHERE org_id = ?1 AND scope_kind = ?2 AND category_id = ?3 AND is_active = 1 \
             ORDER BY is_default DESC, retention_days DESC \
             LIMIT 1",
            params![org_id, scope_kind.as_str(), category_id],
            retention_policy_from_row,
        )
        .optional()?
    } else {
        None
    };

    let policy = if let Some(policy) = category_policy {
        policy
    } else {
        conn.query_row(
            "SELECT retention_policy_id, org_id, slug, name_translations, scope_kind, category_id, \
                    retention_days, minimum_days, action_after_retention, is_default, is_active \
             FROM compliance_retention_policies \
             WHERE org_id = ?1 AND scope_kind = ?2 AND category_id IS NULL AND is_active = 1 \
             ORDER BY is_default DESC, retention_days DESC \
             LIMIT 1",
            params![org_id, scope_kind.as_str()],
            retention_policy_from_row,
        )?
    };

    if scope_kind == RetentionScopeKind::AiAudit
        && policy.retention_days < MINIMUM_AI_AUDIT_RETENTION_DAYS
    {
        return Err(anyhow!(
            "retencja AI audit {} dni jest ponizej minimum {} dni",
            policy.retention_days,
            MINIMUM_AI_AUDIT_RETENTION_DAYS
        ));
    }

    Ok(policy)
}

pub fn start_ai_event(conn: &Connection, event: &NewAiEvent<'_>) -> Result<String> {
    let retention =
        resolve_retention_policy(conn, event.org_id, RetentionScopeKind::AiAudit, None)?;
    if let Some(legal_basis_id) = event.legal_basis_id {
        let legal_basis_org = conn
            .query_row(
                "SELECT org_id FROM compliance_legal_basis WHERE legal_basis_id = ?1",
                params![legal_basis_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .ok_or_else(|| {
                anyhow!("nie znaleziono podstawy prawnej compliance: {legal_basis_id}")
            })?;
        if legal_basis_org != event.org_id {
            return Err(anyhow!(
                "podstawa prawna compliance {legal_basis_id} nalezy do innej organizacji"
            ));
        }
    }
    let event_id = Uuid::new_v4().to_string();
    let started_at = now_utc();
    let affected = conn.execute(
        "INSERT INTO compliance_ai_events \
            (event_id, org_id, user_id, node_id, addon_id, instance_id, flow_id, flow_node_id, \
             agent_id, agent_run_id, request_id, correlation_id, model_id, backend, started_at, finished_at, status, \
             risk_class, legal_basis_id, retention_policy_id, prompt_hash, response_hash, audit_log_id, error_message) \
         VALUES \
            (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, NULL, 'running', ?16, ?17, ?18, '', '', NULL, NULL)",
        params![
            event_id,
            event.org_id,
            event.user_id,
            event.node_id,
            event.addon_id,
            event.instance_id,
            event.flow_id,
            event.flow_node_id,
            event.agent_id,
            event.agent_run_id,
            event.request_id,
            event.correlation_id,
            event.model_id,
            event.backend,
            started_at,
            event.risk_class.as_str(),
            event.legal_basis_id,
            retention.retention_policy_id,
        ],
    )?;
    if affected != 1 {
        return Err(anyhow!(
            "compliance_ai_events insert affected {affected} rows"
        ));
    }
    Ok(event_id)
}

pub fn get_ai_event(conn: &Connection, event_id: &str) -> Result<Option<ComplianceAiEvent>> {
    conn.query_row(
        "SELECT event_id, org_id, user_id, node_id, addon_id, instance_id, flow_id, flow_node_id, \
                agent_id, agent_run_id, request_id, model_id, backend, started_at, finished_at, \
                status, risk_class, legal_basis_id, retention_policy_id, prompt_hash, response_hash, \
                audit_log_id, error_message \
         FROM compliance_ai_events \
         WHERE event_id = ?1",
        params![event_id],
        |row| {
            Ok(ComplianceAiEvent {
                event_id: row.get(0)?,
                org_id: row.get(1)?,
                user_id: row.get(2)?,
                node_id: row.get(3)?,
                addon_id: row.get(4)?,
                instance_id: row.get(5)?,
                flow_id: row.get(6)?,
                flow_node_id: row.get(7)?,
                agent_id: row.get(8)?,
                agent_run_id: row.get(9)?,
                request_id: row.get(10)?,
                model_id: row.get(11)?,
                backend: row.get(12)?,
                started_at: row.get(13)?,
                finished_at: row.get(14)?,
                status: row_ai_status(row, 15)?,
                risk_class: row_risk_class(row, 16)?,
                legal_basis_id: row.get(17)?,
                retention_policy_id: row.get(18)?,
                prompt_hash: row.get(19)?,
                response_hash: row.get(20)?,
                audit_log_id: row.get(21)?,
                error_message: row.get(22)?,
            })
        },
    )
    .optional()
    .map_err(Into::into)
}

/// Returns the `event_id` of the most recent `compliance_ai_events` row for one
/// agent run. The harness records a tool execution against the LLM call that
/// requested it; that call's event carries the same `agent_run_id`, so the
/// tool_exec block attaches executions to the latest such event (the call it is
/// answering). `None` when the run has no events yet (audit then no-ops).
pub fn latest_ai_event_id_for_run(conn: &Connection, agent_run_id: &str) -> Result<Option<String>> {
    conn.query_row(
        "SELECT event_id FROM compliance_ai_events \
         WHERE agent_run_id = ?1 ORDER BY started_at DESC, rowid DESC LIMIT 1",
        params![agent_run_id],
        |row| row.get::<_, String>(0),
    )
    .optional()
    .map_err(Into::into)
}

fn ai_event_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ComplianceAiEvent> {
    Ok(ComplianceAiEvent {
        event_id: row.get(0)?,
        org_id: row.get(1)?,
        user_id: row.get(2)?,
        node_id: row.get(3)?,
        addon_id: row.get(4)?,
        instance_id: row.get(5)?,
        flow_id: row.get(6)?,
        flow_node_id: row.get(7)?,
        agent_id: row.get(8)?,
        agent_run_id: row.get(9)?,
        request_id: row.get(10)?,
        model_id: row.get(11)?,
        backend: row.get(12)?,
        started_at: row.get(13)?,
        finished_at: row.get(14)?,
        status: row_ai_status(row, 15)?,
        risk_class: row_risk_class(row, 16)?,
        legal_basis_id: row.get(17)?,
        retention_policy_id: row.get(18)?,
        prompt_hash: row.get(19)?,
        response_hash: row.get(20)?,
        audit_log_id: row.get(21)?,
        error_message: row.get(22)?,
    })
}

pub fn list_ai_events(
    conn: &Connection,
    org_id: &str,
    filter: &AiEventListFilter,
) -> Result<Vec<ComplianceAiEvent>> {
    let limit = i64::from(filter.limit.clamp(1, 500));
    let offset = i64::from(filter.offset);
    let mut sql = "SELECT event_id, org_id, user_id, node_id, addon_id, instance_id, flow_id, flow_node_id, \
                          agent_id, agent_run_id, request_id, model_id, backend, started_at, finished_at, \
                          status, risk_class, legal_basis_id, retention_policy_id, prompt_hash, response_hash, \
                          audit_log_id, error_message \
                   FROM compliance_ai_events \
                   WHERE org_id = ?1"
        .to_string();
    let status_value = filter.status.map(|status| status.as_str().to_string());
    let user_id_value = filter.user_id.as_deref();
    let addon_id_value = filter.addon_id.as_deref();
    let mut next_param = 2;

    if status_value.is_some() {
        sql.push_str(&format!(" AND status = ?{next_param}"));
        next_param += 1;
    }
    if user_id_value.is_some() {
        sql.push_str(&format!(" AND user_id = ?{next_param}"));
        next_param += 1;
    }
    if addon_id_value.is_some() {
        sql.push_str(&format!(" AND addon_id = ?{next_param}"));
        next_param += 1;
    }
    sql.push_str(&format!(
        " ORDER BY started_at DESC LIMIT ?{next_param} OFFSET ?{}",
        next_param + 1
    ));

    let mut values: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(6);
    values.push(&org_id);
    if let Some(status) = status_value.as_ref() {
        values.push(status);
    }
    if let Some(user_id) = user_id_value.as_ref() {
        values.push(user_id);
    }
    if let Some(addon_id) = addon_id_value.as_ref() {
        values.push(addon_id);
    }
    values.push(&limit);
    values.push(&offset);

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(values.as_slice(), ai_event_from_row)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn add_ai_payload(conn: &Connection, payload: &NewAiPayload<'_>) -> Result<String> {
    let payload_id = Uuid::new_v4().to_string();
    let content_hash = hash_text(payload.content_text);
    let tx = conn.unchecked_transaction()?;
    let affected = tx.execute(
        "INSERT INTO compliance_ai_payloads \
            (payload_id, event_id, payload_kind, content_hash, content_text, content_redacted, token_count) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            payload_id,
            payload.event_id,
            payload.payload_kind.as_str(),
            content_hash,
            payload.content_text,
            if payload.content_redacted { 1 } else { 0 },
            payload.token_count,
        ],
    )?;
    if affected != 1 {
        return Err(anyhow!(
            "compliance_ai_payloads insert affected {affected} rows"
        ));
    }
    match payload.payload_kind {
        AiPayloadKind::Prompt => {
            tx.execute(
                "UPDATE compliance_ai_events SET prompt_hash = ?1 WHERE event_id = ?2",
                params![content_hash, payload.event_id],
            )?;
        }
        AiPayloadKind::Response => {
            tx.execute(
                "UPDATE compliance_ai_events SET response_hash = ?1 WHERE event_id = ?2",
                params![content_hash, payload.event_id],
            )?;
        }
        AiPayloadKind::System | AiPayloadKind::ToolInput | AiPayloadKind::ToolOutput => {}
    }
    tx.commit()?;
    Ok(payload_id)
}

pub fn add_ai_source(conn: &Connection, source: &NewAiSource<'_>) -> Result<String> {
    let source_id = Uuid::new_v4().to_string();
    let source_hash = hash_text(source.source_text);
    let excerpt_hash = hash_text(source.excerpt_text);
    let affected = conn.execute(
        "INSERT INTO compliance_ai_sources \
            (source_id, event_id, source_kind, source_ref, source_hash, title, excerpt_hash, excerpt_text, score, metadata_cbor) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            source_id,
            source.event_id,
            source.source_kind.as_str(),
            source.source_ref,
            source_hash,
            source.title,
            excerpt_hash,
            source.excerpt_text,
            source.score,
            source.metadata_cbor,
        ],
    )?;
    if affected != 1 {
        return Err(anyhow!(
            "compliance_ai_sources insert affected {affected} rows"
        ));
    }
    Ok(source_id)
}

pub fn add_ai_tool_call(conn: &Connection, tool_call: &NewAiToolCall<'_>) -> Result<String> {
    let tool_call_id = Uuid::new_v4().to_string();
    let input_hash = hash_text(tool_call.input_text);
    let output_hash = hash_text(tool_call.output_text);
    let now = now_utc();
    let started_at = tool_call.started_at.unwrap_or(now.as_str());
    let finished_at = if tool_call.status == ToolCallStatus::Running {
        None
    } else {
        Some(now.as_str())
    };
    let affected = conn.execute(
        "INSERT INTO compliance_ai_tool_calls \
            (tool_call_id, event_id, llm_tool_call_id, addon_id, tool_name, input_hash, output_hash, status, started_at, finished_at, error_message) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            tool_call_id,
            tool_call.event_id,
            tool_call.llm_tool_call_id,
            tool_call.addon_id,
            tool_call.tool_name,
            input_hash,
            output_hash,
            tool_call.status.as_str(),
            started_at,
            finished_at,
            tool_call.error_message,
        ],
    )?;
    if affected != 1 {
        return Err(anyhow!(
            "compliance_ai_tool_calls insert affected {affected} rows"
        ));
    }
    Ok(tool_call_id)
}

pub fn finish_ai_event(
    conn: &Connection,
    event_id: &str,
    status: AiEventStatus,
    audit_log_id: Option<i64>,
    error_message: Option<&str>,
) -> Result<()> {
    if status == AiEventStatus::Running {
        return Err(anyhow!("finish_ai_event nie moze ustawic statusu running"));
    }
    let affected = conn.execute(
        "UPDATE compliance_ai_events \
         SET status = ?1, finished_at = ?2, audit_log_id = ?3, error_message = ?4 \
         WHERE event_id = ?5",
        params![
            status.as_str(),
            now_utc(),
            audit_log_id,
            error_message,
            event_id
        ],
    )?;
    if affected != 1 {
        return Err(anyhow!(
            "compliance_ai_events update affected {affected} rows"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let conn = Connection::open_in_memory().expect("baza testowa");
        crate::db::migrations::run(&conn).expect("migracje");
        conn
    }

    #[test]
    fn seedowane_kategorie_maja_tlumaczenia() {
        let conn = db();
        let categories = list_data_categories(&conn, crate::services::org::DEFAULT_ORG_ID)
            .expect("lista kategorii");

        assert!(categories
            .iter()
            .any(|c| c.name_translations.contains("Konta użytkowników")
                && c.name_translations.contains("User accounts")));
    }

    #[test]
    fn retencja_ai_ma_minimum_szesciu_miesiecy() {
        let conn = db();
        let policy = resolve_retention_policy(
            &conn,
            crate::services::org::DEFAULT_ORG_ID,
            RetentionScopeKind::AiAudit,
            None,
        )
        .expect("polityka retencji");

        assert!(policy.retention_days >= MINIMUM_AI_AUDIT_RETENTION_DAYS);
    }

    #[test]
    fn retencja_agent_runs_domyslnie_trzydziesci_dni() {
        let conn = db();
        let policy = resolve_retention_policy(
            &conn,
            crate::services::org::DEFAULT_ORG_ID,
            RetentionScopeKind::AgentRuns,
            None,
        )
        .expect("polityka retencji agent_runs");

        assert_eq!(policy.scope_kind, RetentionScopeKind::AgentRuns);
        assert_eq!(policy.retention_days, 30);
        assert_eq!(policy.action_after_retention, "delete");
    }

    #[test]
    fn nowa_organizacja_dostaje_domyslne_polityki_compliance() {
        let pool = std::sync::Arc::new(crate::db::Db::from_connection(db()));
        let org = crate::services::org::repo::create_organization(
            &pool,
            "Druga organizacja",
            "druga-organizacja",
            None,
            None,
            None,
            None,
        )
        .expect("organizacja");
        let conn = pool.read().expect("db read");
        let categories = list_data_categories(&conn, &org.org_id).expect("kategorie");
        let policy =
            resolve_retention_policy(&conn, &org.org_id, RetentionScopeKind::AiAudit, None)
                .expect("polityka AI");

        assert!(categories
            .iter()
            .any(|category| category.slug == "ai_prompt"));
        assert_eq!(policy.org_id, org.org_id);
        assert!(policy.retention_days >= MINIMUM_AI_AUDIT_RETENTION_DAYS);
    }

    #[test]
    fn ai_event_zapisuje_payloady_i_hash() {
        let conn = db();
        let event_id = start_ai_event(
            &conn,
            &NewAiEvent {
                org_id: crate::services::org::DEFAULT_ORG_ID,
                user_id: None,
                node_id: "node-a",
                addon_id: Some("contacts"),
                instance_id: Some("instance-a"),
                flow_id: None,
                flow_node_id: None,
                agent_id: None,
                agent_run_id: None,
                request_id: "request-a",
                correlation_id: None,
                model_id: "model-a",
                backend: "test",
                risk_class: ComplianceRiskClass::High,
                legal_basis_id: Some("lb-core-ai-legitimate-interest"),
            },
        )
        .expect("start event");
        add_ai_payload(
            &conn,
            &NewAiPayload {
                event_id: &event_id,
                payload_kind: AiPayloadKind::Prompt,
                content_text: "Załóż kontakt dla Jana Kowalskiego",
                content_redacted: false,
                token_count: Some(7),
            },
        )
        .expect("prompt");
        add_ai_payload(
            &conn,
            &NewAiPayload {
                event_id: &event_id,
                payload_kind: AiPayloadKind::Response,
                content_text: "Kontakt został utworzony",
                content_redacted: false,
                token_count: Some(4),
            },
        )
        .expect("response");
        finish_ai_event(&conn, &event_id, AiEventStatus::Success, None, None).expect("finish");
        let event = get_ai_event(&conn, &event_id).expect("get").expect("event");

        assert_eq!(event.status, AiEventStatus::Success);
        assert!(!event.prompt_hash.is_empty());
        assert!(!event.response_hash.is_empty());
    }
}
