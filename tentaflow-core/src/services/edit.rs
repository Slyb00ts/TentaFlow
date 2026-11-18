//! One service edit, shared by the local `service_update` handler and the mesh
//! `ServiceUpdateRemote` executor: the read, the merge and the write happen in
//! one immediate transaction, so two concurrent saves cannot overwrite each
//! other's keys, and whether the edit needs a restart is decided in one place.

use serde_json::{Map, Value};

use crate::db::DbPool;
use crate::deploy::launch_dialect::{apply_service_tuning, ServiceTuningPatch};
use crate::services_repo::services::{self, ServiceRow, ServiceStatus};

/// Config keys that apply without a restart.
const LIMIT_KEYS: [&str; 2] = ["max_concurrent", "timeout_ms"];

#[derive(Debug, Clone, Default)]
pub struct ServiceEdit {
    pub model_repo: Option<String>,
    pub model_preset_id: Option<String>,
    pub tuning: ServiceTuningPatch,
    pub pinned: Option<bool>,
    pub paused: Option<bool>,
    /// `Some(0)` clears the limit.
    pub max_concurrent: Option<u32>,
    pub timeout_ms: Option<u32>,
}

impl ServiceEdit {
    fn has_limits(&self) -> bool {
        self.max_concurrent.is_some() || self.timeout_ms.is_some()
    }
}

#[derive(Debug)]
pub enum EditError {
    NotFound(String),
    Invalid(String),
    Db(String),
}

impl EditError {
    fn db(e: impl std::fmt::Display) -> Self {
        Self::Db(e.to_string())
    }
}

#[derive(Debug)]
pub struct EditApplied {
    /// The row as it was before the edit.
    pub before: ServiceRow,
    /// The config as stored.
    pub config_json: String,
    /// Something other than the two limit keys changed (config, pinned or
    /// paused). Only such an edit can ask for a restart.
    pub changed_beyond_limits: bool,
}

impl EditApplied {
    /// The service is stopped and respawned only when the caller asked for it,
    /// something other than the limits actually changed, and it was running.
    pub fn restart_wanted(&self, restart_after_save: bool) -> bool {
        restart_after_save
            && self.changed_beyond_limits
            && matches!(
                self.before.status,
                ServiceStatus::Running | ServiceStatus::Degraded | ServiceStatus::Starting
            )
    }
}

/// The config without the limit keys and without null entries (absent and
/// null read the same), for the before/after comparison.
fn comparable(cfg: &Map<String, Value>) -> Map<String, Value> {
    cfg.iter()
        .filter(|(k, v)| !LIMIT_KEYS.contains(&k.as_str()) && !v.is_null())
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

/// Validates and applies `edit`, then puts the stored limits in force on this
/// node's limiter. Nothing is written when validation fails.
pub fn apply_edit(
    db: &DbPool,
    local_node_id: &str,
    service_id: i64,
    edit: &ServiceEdit,
) -> Result<EditApplied, EditError> {
    services::validate_service_limits(edit.max_concurrent, edit.timeout_ms)
        .map_err(EditError::Invalid)?;

    let applied = {
        let mut conn = db
            .write()
            .map_err(|_| EditError::Db("db pool poisoned".into()))?;
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(EditError::db)?;
        let before = services::get(&tx, service_id)
            .map_err(EditError::db)?
            .ok_or_else(|| EditError::NotFound(format!("service id={service_id} not found")))?;

        let mut cfg: Value =
            serde_json::from_str(&before.config_json).unwrap_or_else(|_| serde_json::json!({}));
        let cfg_obj = cfg
            .as_object_mut()
            .ok_or_else(|| EditError::Invalid("service config_json is not an object".into()))?;
        let config_before = comparable(cfg_obj);

        if let Some(repo) = edit.model_repo.as_ref() {
            cfg_obj.insert("model_repo".into(), Value::String(repo.clone()));
            cfg_obj.insert("model_preset_id".into(), Value::Null);
        }
        if let Some(preset_id) = edit.model_preset_id.as_ref() {
            cfg_obj.insert("model_preset_id".into(), Value::String(preset_id.clone()));
            cfg_obj.insert("model_repo".into(), Value::Null);
        }
        // Scalar keys plus the matching flags inside `vllm_args`: the runtime
        // builds argv from that string, so scalar-only edits would be inert.
        apply_service_tuning(&before.engine_id, cfg_obj, &edit.tuning);
        services::merge_service_limits(cfg_obj, edit.max_concurrent, edit.timeout_ms);

        let changed_beyond_limits = comparable(cfg_obj) != config_before
            || edit.pinned.is_some_and(|p| p != before.pinned)
            || edit.paused.is_some_and(|p| p != before.paused);

        let config_json = serde_json::to_string(&cfg)
            .map_err(|e| EditError::Db(format!("serialize config: {e}")))?;
        services::update_config_json(&tx, service_id, &config_json).map_err(EditError::db)?;
        if let Some(p) = edit.pinned {
            services::set_pinned(&tx, service_id, p).map_err(EditError::db)?;
        }
        if let Some(p) = edit.paused {
            services::set_paused(&tx, service_id, p).map_err(EditError::db)?;
        }
        tx.commit().map_err(EditError::db)?;
        EditApplied {
            before,
            config_json,
            changed_beyond_limits,
        }
    };

    if edit.has_limits() {
        crate::services::supervisor::apply_limits_from_row(db, local_node_id, service_id);
    }
    Ok(applied)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch::state::AppState;
    use crate::services::transport::Transport;
    use crate::services_repo::services::{DeployMethod, NewService};

    fn service(db: &DbPool, config: &str, pad: usize) -> i64 {
        let conn = db.write().unwrap();
        let mut id = 0;
        for _ in 0..=pad {
            let mut new =
                NewService::minimal("vllm", DeployMethod::NativeBinary, Transport::HttpDirect);
            new.status = ServiceStatus::Running;
            new.config_json = config.into();
            id = services::insert(&conn, &new).unwrap();
        }
        id
    }

    #[test]
    fn resending_unchanged_fields_with_new_limits_asks_for_no_restart() {
        let state = AppState::for_test();
        let id = service(&state.db, r#"{"max_num_seqs":4,"model_repo":"a/b"}"#, 40);
        let same = ServiceEdit {
            model_repo: Some("a/b".into()),
            tuning: ServiceTuningPatch {
                max_num_seqs: Some(4),
                ..Default::default()
            },
            max_concurrent: Some(3),
            ..Default::default()
        };
        // The first save may also materialise derived keys such as `vllm_args`;
        // the resend after it is the case under test.
        apply_edit(&state.db, "edit-test-a", id, &same).unwrap();
        let applied = apply_edit(&state.db, "edit-test-a", id, &same).unwrap();
        assert!(!applied.changed_beyond_limits);
        assert!(!applied.restart_wanted(true));

        let changed = ServiceEdit {
            tuning: ServiceTuningPatch {
                max_num_seqs: Some(8),
                ..Default::default()
            },
            ..Default::default()
        };
        let applied = apply_edit(&state.db, "edit-test-a", id, &changed).unwrap();
        assert!(applied.changed_beyond_limits);
        assert!(applied.restart_wanted(true));
        assert!(!applied.restart_wanted(false));
    }

    #[test]
    fn a_pinned_change_counts_but_an_unchanged_pinned_does_not() {
        let state = AppState::for_test();
        let id = service(&state.db, "{}", 41);
        let pin = |pinned| ServiceEdit {
            pinned: Some(pinned),
            ..Default::default()
        };
        assert!(
            apply_edit(&state.db, "edit-test-b", id, &pin(true))
                .unwrap()
                .changed_beyond_limits
        );
        assert!(
            !apply_edit(&state.db, "edit-test-b", id, &pin(true))
                .unwrap()
                .changed_beyond_limits
        );
    }

    #[test]
    fn bad_limits_write_nothing_and_a_missing_service_is_not_found() {
        let state = AppState::for_test();
        let id = service(&state.db, r#"{"max_concurrent":2}"#, 42);
        let bad = ServiceEdit {
            max_concurrent: Some(5000),
            pinned: Some(true),
            ..Default::default()
        };
        assert!(matches!(
            apply_edit(&state.db, "edit-test-c", id, &bad),
            Err(EditError::Invalid(_))
        ));
        let conn = state.db.read().unwrap();
        let row = services::get(&conn, id).unwrap().unwrap();
        assert!(!row.pinned && row.config_json.contains("\"max_concurrent\":2"));
        drop(conn);
        assert!(matches!(
            apply_edit(&state.db, "edit-test-c", 9_999, &ServiceEdit::default()),
            Err(EditError::NotFound(_))
        ));
    }

    #[test]
    fn two_edits_in_a_row_keep_each_others_keys() {
        let state = AppState::for_test();
        let id = service(&state.db, "{}", 43);
        let limits = ServiceEdit {
            timeout_ms: Some(2_000),
            ..Default::default()
        };
        let tuning = ServiceEdit {
            tuning: ServiceTuningPatch {
                max_num_seqs: Some(6),
                ..Default::default()
            },
            ..Default::default()
        };
        apply_edit(&state.db, "edit-test-d", id, &limits).unwrap();
        let applied = apply_edit(&state.db, "edit-test-d", id, &tuning).unwrap();
        let cfg: Value = serde_json::from_str(&applied.config_json).unwrap();
        assert_eq!(
            (cfg["timeout_ms"].clone(), cfg["max_num_seqs"].clone()),
            (2_000.into(), 6.into())
        );
    }
}
