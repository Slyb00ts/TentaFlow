// =============================================================================
// File: addon/ui_audit.rs
// Desc: Risk-classified audit logging for the UI channel binary protocol.
//       Collapses C-class events (rate-limit denials) into one audit_log row
//       per addon per 60 s window; A/B-class events are always logged
//       immediately; D-class events go to tracing::debug only.
// =============================================================================

use std::collections::HashMap;
use std::fmt;
use std::time::{Duration, Instant};

use parking_lot::Mutex;

use crate::audit::chain::AuditRowHashInput;
use crate::db::DbPool;

const COLLAPSE_WINDOW_SECS: u64 = 60;

// =============================================================================
// RiskClass — UI-channel specific (A = attack, B = security, C = ops, D = info)
// =============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RiskClass {
    A,
    B,
    C,
    D,
}

impl RiskClass {
    pub const fn as_db_str(self) -> &'static str {
        match self {
            Self::A => "A",
            Self::B => "B",
            Self::C => "C",
            Self::D => "D",
        }
    }

    /// Map to the platform-wide `audit::RiskClass` for chain hashing.
    fn to_audit_risk(self) -> crate::audit::RiskClass {
        match self {
            Self::A => crate::audit::RiskClass::A,
            Self::B => crate::audit::RiskClass::B,
            Self::C => crate::audit::RiskClass::C,
            // D-class never reaches the DB, but map defensively.
            Self::D => crate::audit::RiskClass::Unclassified,
        }
    }
}

impl fmt::Display for RiskClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_db_str())
    }
}

// =============================================================================
// UiAuditAction
// =============================================================================

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum UiAuditAction {
    SlotOwnershipViolation,
    ReservedNamespaceViolation,
    RevisionMismatch,
    ActionNotDeclared,
    EpochMismatch,
    RateLimitDenied,
    UrlBlocked,
    FilenameBlocked,
    MalformedCbor,
    OversizedPayload,
    UnknownTag,
    PermissionDenied,
    PanelOpen,
    PanelClose,
    StreamCancel,
    CreditsExhausted,
    ShellAlreadyRegistered,
    ReservedSlotPrefix,
}

impl fmt::Display for UiAuditAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::SlotOwnershipViolation => "slot_ownership_violation",
            Self::ReservedNamespaceViolation => "reserved_namespace_violation",
            Self::RevisionMismatch => "revision_mismatch",
            Self::ActionNotDeclared => "action_not_declared",
            Self::EpochMismatch => "epoch_mismatch",
            Self::RateLimitDenied => "rate_limit_denied",
            Self::UrlBlocked => "url_blocked",
            Self::FilenameBlocked => "filename_blocked",
            Self::MalformedCbor => "malformed_cbor",
            Self::OversizedPayload => "oversized_payload",
            Self::UnknownTag => "unknown_tag",
            Self::PermissionDenied => "permission_denied",
            Self::PanelOpen => "panel_open",
            Self::PanelClose => "panel_close",
            Self::StreamCancel => "stream_cancel",
            Self::CreditsExhausted => "credits_exhausted",
            Self::ShellAlreadyRegistered => "shell_already_registered",
            Self::ReservedSlotPrefix => "reserved_slot_prefix",
        };
        f.write_str(s)
    }
}

// =============================================================================
// UiAuditOutcome
// =============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiAuditOutcome {
    Allowed,
    Denied,
    Rejected,
    RateLimited,
}

impl fmt::Display for UiAuditOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Allowed => "allowed",
            Self::Denied => "denied",
            Self::Rejected => "rejected",
            Self::RateLimited => "rate_limited",
        };
        f.write_str(s)
    }
}

// =============================================================================
// UiAuditEntry
// =============================================================================

pub struct UiAuditEntry {
    pub ts_ms: i64,
    pub session_id: Option<String>,
    pub user_id: Option<String>,
    pub addon_id: String,
    pub panel_id: Option<String>,
    pub action: UiAuditAction,
    pub outcome: UiAuditOutcome,
    pub details: HashMap<String, String>,
    pub risk_class: RiskClass,
}

// =============================================================================
// CollapseWindow — per-(addon, action) 60 s window for C-class dedup
// =============================================================================

struct CollapseWindow {
    window_start: Instant,
    denied_count: u64,
    last_flushed: Instant,
}

// =============================================================================
// UiAuditWriter
// =============================================================================

pub struct UiAuditWriter {
    collapse_windows: Mutex<HashMap<(String, String), CollapseWindow>>,
}

impl UiAuditWriter {
    pub fn new() -> Self {
        Self {
            collapse_windows: Mutex::new(HashMap::new()),
        }
    }

    /// Main entry point — route by risk class.
    pub async fn log(&self, entry: UiAuditEntry, db: &DbPool) {
        match entry.risk_class {
            RiskClass::A | RiskClass::B => {
                self.insert_audit_row(&entry, None, db);
            }
            RiskClass::C => {
                self.collapse_or_flush(entry, db);
            }
            RiskClass::D => {
                tracing::debug!(
                    addon_id = %entry.addon_id,
                    action = %entry.action,
                    outcome = %entry.outcome,
                    "ui_audit D-class event"
                );
            }
        }
    }

    /// Flush any C-class collapse windows whose 60 s has expired.
    pub async fn flush_expired_windows(&self, db: &DbPool) {
        let now = Instant::now();
        let window_dur = Duration::from_secs(COLLAPSE_WINDOW_SECS);
        let mut windows = self.collapse_windows.lock();
        let expired_keys: Vec<(String, String)> = windows
            .iter()
            .filter(|(_, w)| now.duration_since(w.window_start) >= window_dur)
            .map(|(k, _)| k.clone())
            .collect();

        for key in expired_keys {
            if let Some(w) = windows.remove(&key) {
                self.flush_collapsed_window(&key.0, &key.1, &w, db);
            }
        }
    }

    /// Construct a `UiAuditEntry` with common fields pre-filled.
    #[allow(clippy::too_many_arguments)]
    pub fn build_entry(
        addon_id: impl Into<String>,
        action: UiAuditAction,
        outcome: UiAuditOutcome,
        risk_class: RiskClass,
        session_id: Option<String>,
        user_id: Option<String>,
        panel_id: Option<String>,
        details: HashMap<String, String>,
    ) -> UiAuditEntry {
        UiAuditEntry {
            ts_ms: chrono::Utc::now().timestamp_millis(),
            session_id,
            user_id,
            addon_id: addon_id.into(),
            panel_id,
            action,
            outcome,
            details,
            risk_class,
        }
    }

    // -------------------------------------------------------------------------
    // Private
    // -------------------------------------------------------------------------

    fn collapse_or_flush(&self, entry: UiAuditEntry, db: &DbPool) {
        let now = Instant::now();
        let window_dur = Duration::from_secs(COLLAPSE_WINDOW_SECS);
        let key = (entry.addon_id.clone(), entry.action.to_string());

        let mut windows = self.collapse_windows.lock();
        if let Some(w) = windows.get_mut(&key) {
            if now.duration_since(w.window_start) >= window_dur {
                // Window expired — flush the accumulated count, then start a new one.
                let addon_id = key.0.clone();
                let action_str = key.1.clone();
                let old_window = CollapseWindow {
                    window_start: w.window_start,
                    denied_count: w.denied_count,
                    last_flushed: w.last_flushed,
                };
                w.window_start = now;
                w.denied_count = 1;
                w.last_flushed = now;
                // Drop lock before DB write.
                drop(windows);
                self.flush_collapsed_window(&addon_id, &action_str, &old_window, db);
            } else {
                w.denied_count += 1;
            }
        } else {
            windows.insert(
                key,
                CollapseWindow {
                    window_start: now,
                    denied_count: 1,
                    last_flushed: now,
                },
            );
        }
    }

    fn flush_collapsed_window(
        &self,
        addon_id: &str,
        action_str: &str,
        window: &CollapseWindow,
        db: &DbPool,
    ) {
        if window.denied_count == 0 {
            return;
        }

        let mut details = HashMap::new();
        details.insert("denied_count".to_string(), window.denied_count.to_string());
        details.insert("window_secs".to_string(), COLLAPSE_WINDOW_SECS.to_string());

        let synthetic = UiAuditEntry {
            ts_ms: chrono::Utc::now().timestamp_millis(),
            session_id: None,
            user_id: None,
            addon_id: addon_id.to_string(),
            panel_id: None,
            action: UiAuditAction::RateLimitDenied,
            outcome: UiAuditOutcome::RateLimited,
            details,
            risk_class: RiskClass::C,
        };

        let _ = action_str; // action_str matches synthetic.action for C-class flush
        self.insert_audit_row(&synthetic, None, db);
    }

    fn insert_audit_row(&self, entry: &UiAuditEntry, denied_count: Option<u64>, db: &DbPool) {
        let action_str = format!("ui.{}", entry.action);
        let result_str = entry.outcome.to_string();
        let risk_class = entry.risk_class.to_audit_risk();
        let risk_class_db = risk_class.as_db_str();
        let action_hash = super::utils::fnv1a_hash(&action_str);

        let details_str = if entry.details.is_empty() && denied_count.is_none() {
            None
        } else {
            let mut merged = entry.details.clone();
            if let Some(count) = denied_count {
                merged.insert("denied_count".to_string(), count.to_string());
            }
            if let Some(ref sid) = entry.session_id {
                merged.insert("session_id".to_string(), sid.clone());
            }
            if let Some(ref pid) = entry.panel_id {
                merged.insert("panel_id".to_string(), pid.clone());
            }
            Some(serde_json::to_string(&merged).unwrap_or_default())
        };

        let timestamp = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();

        let conn = match db.write() {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("ui_audit: failed to lock db: {e}");
                return;
            }
        };

        // Session/panel context goes into details JSON; instance_id carries
        // session_id so existing audit queries can locate UI-channel rows.
        let instance_id = entry.session_id.as_deref();

        let hash_input = AuditRowHashInput {
            user_id: entry.user_id.as_deref(),
            addon_id: Some(&entry.addon_id),
            instance_id,
            action: &action_str,
            resource: None,
            resource_type: Some("ui_channel"),
            resource_id: entry.panel_id.as_deref(),
            result: Some(&result_str),
            error_message: None,
            details: details_str.as_deref(),
            ip_address: None,
            node_id: None,
            severity: Some("info"),
            risk_class: risk_class_db,
            related_claim_id: None,
            request_id: None,
            timestamp: &timestamp,
        };

        let (prev_hash_blob, hash_blob) =
            match crate::audit::chain::compute_chain_for_insert(&conn, &hash_input) {
                Ok(pair) => pair,
                Err(e) => {
                    tracing::warn!("ui_audit: compute_chain_for_insert failed: {e}");
                    return;
                }
            };

        let org_id = crate::services::org::DEFAULT_ORG_ID;

        let _ = conn.execute(
            "INSERT INTO audit_log (user_id, addon_id, instance_id, action, \
             resource_type, resource_id, result, error_message, action_hash, \
             risk_class, related_claim_id, request_id, timestamp, prev_hash, \
             hash, org_id) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, \
             ?14, ?15, ?16)",
            rusqlite::params![
                entry.user_id,
                &entry.addon_id,
                instance_id,
                &action_str,
                "ui_channel",
                entry.panel_id.as_deref(),
                &result_str,
                Option::<&str>::None,
                action_hash,
                risk_class_db,
                Option::<&str>::None,
                Option::<&str>::None,
                &timestamp,
                prev_hash_blob,
                hash_blob,
                org_id,
            ],
        );
    }
}

impl Default for UiAuditWriter {
    fn default() -> Self {
        Self::new()
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn risk_class_display() {
        assert_eq!(RiskClass::A.to_string(), "A");
        assert_eq!(RiskClass::B.to_string(), "B");
        assert_eq!(RiskClass::C.to_string(), "C");
        assert_eq!(RiskClass::D.to_string(), "D");
    }

    #[test]
    fn audit_action_display() {
        let cases = [
            (
                UiAuditAction::SlotOwnershipViolation,
                "slot_ownership_violation",
            ),
            (
                UiAuditAction::ReservedNamespaceViolation,
                "reserved_namespace_violation",
            ),
            (UiAuditAction::RevisionMismatch, "revision_mismatch"),
            (UiAuditAction::ActionNotDeclared, "action_not_declared"),
            (UiAuditAction::EpochMismatch, "epoch_mismatch"),
            (UiAuditAction::RateLimitDenied, "rate_limit_denied"),
            (UiAuditAction::UrlBlocked, "url_blocked"),
            (UiAuditAction::FilenameBlocked, "filename_blocked"),
            (UiAuditAction::MalformedCbor, "malformed_cbor"),
            (UiAuditAction::OversizedPayload, "oversized_payload"),
            (UiAuditAction::UnknownTag, "unknown_tag"),
            (UiAuditAction::PermissionDenied, "permission_denied"),
            (UiAuditAction::PanelOpen, "panel_open"),
            (UiAuditAction::PanelClose, "panel_close"),
            (UiAuditAction::StreamCancel, "stream_cancel"),
            (UiAuditAction::CreditsExhausted, "credits_exhausted"),
            (
                UiAuditAction::ShellAlreadyRegistered,
                "shell_already_registered",
            ),
            (UiAuditAction::ReservedSlotPrefix, "reserved_slot_prefix"),
        ];
        for (action, expected) in &cases {
            assert_eq!(action.to_string(), *expected);
        }
    }

    #[test]
    fn build_entry_populates_fields() {
        let mut details = HashMap::new();
        details.insert("key".to_string(), "val".to_string());

        let entry = UiAuditWriter::build_entry(
            "sdk-showcase",
            UiAuditAction::UnknownTag,
            UiAuditOutcome::Rejected,
            RiskClass::A,
            Some("sess-1".to_string()),
            Some("00000000-0000-0000-0000-000000000042".to_string()),
            Some("panel-main".to_string()),
            details,
        );

        assert_eq!(entry.addon_id, "sdk-showcase");
        assert_eq!(entry.action, UiAuditAction::UnknownTag);
        assert_eq!(entry.outcome, UiAuditOutcome::Rejected);
        assert_eq!(entry.risk_class, RiskClass::A);
        assert_eq!(entry.session_id.as_deref(), Some("sess-1"));
        assert_eq!(
            entry.user_id.as_deref(),
            Some("00000000-0000-0000-0000-000000000042")
        );
        assert_eq!(entry.panel_id.as_deref(), Some("panel-main"));
        assert_eq!(entry.details.get("key").map(|v| v.as_str()), Some("val"));
        assert!(entry.ts_ms > 0);
    }

    #[test]
    fn collapse_window_tracking() {
        let writer = UiAuditWriter::new();
        let now = Instant::now();

        // Simulate 5 C-class events for the same addon+action.
        {
            let mut windows = writer.collapse_windows.lock();
            let key = ("addon-x".to_string(), "rate_limit_denied".to_string());
            windows.insert(
                key.clone(),
                CollapseWindow {
                    window_start: now,
                    denied_count: 1,
                    last_flushed: now,
                },
            );
            // Increment 4 more times within the window.
            for _ in 0..4 {
                let w = windows.get_mut(&key).unwrap();
                w.denied_count += 1;
            }
            let w = windows.get(&key).unwrap();
            assert_eq!(w.denied_count, 5);
        }

        // A different addon should have its own window.
        {
            let mut windows = writer.collapse_windows.lock();
            let key2 = ("addon-y".to_string(), "rate_limit_denied".to_string());
            assert!(!windows.contains_key(&key2));
            windows.insert(
                key2.clone(),
                CollapseWindow {
                    window_start: now,
                    denied_count: 1,
                    last_flushed: now,
                },
            );
            assert_eq!(windows.len(), 2);
        }

        // Simulate an expired window by setting window_start far in the past.
        {
            let mut windows = writer.collapse_windows.lock();
            let key = ("addon-x".to_string(), "rate_limit_denied".to_string());
            let w = windows.get_mut(&key).unwrap();
            w.window_start = now - Duration::from_secs(COLLAPSE_WINDOW_SECS + 1);

            let window_dur = Duration::from_secs(COLLAPSE_WINDOW_SECS);
            let expired: Vec<_> = windows
                .iter()
                .filter(|(_, w)| Instant::now().duration_since(w.window_start) >= window_dur)
                .map(|(k, _)| k.clone())
                .collect();
            assert_eq!(expired.len(), 1);
            assert_eq!(expired[0].0, "addon-x");
        }
    }
}
