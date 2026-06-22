// =============================================================================
// File: addon/ui_session.rs
// Per-connection UI session state for the addon CBOR binary protocol (Faza 6
// Krok 4). Tracks open panels, slot declarations, state revisions, declared
// actions, event topic patterns, local capabilities and credit-based flow
// control.
// =============================================================================

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};

use parking_lot::{Mutex, RwLock};
use thiserror::Error;

// =============================================================================
// Constants
// =============================================================================

/// Root state keys reserved by the platform (§8.3). Addons cannot write to
/// paths starting with any of these unless the write originates from a local
/// action handler (which the host itself drives).
pub const RESERVED_STATE_ROOTS: &[&str] = &[
    "__system",
    "__user",
    "__draft",
    "__optimistic",
    "__committed",
];

/// Slot id prefix reserved for shell-injected slots (§8.4). Addons must not
/// declare slots whose id starts with this prefix — those are managed by the
/// host shell.
pub const RESERVED_SLOT_PREFIX: &str = "__shell:";

/// Initial UI channel credits (§8.2 flow control).
const INITIAL_UI_CREDITS: u32 = 256;

// =============================================================================
// TopicPattern — compiled event topic pattern
// =============================================================================

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TopicPatternSegment {
    Literal(String),
    /// Matches exactly one segment.
    Wildcard,
}

/// Compiled event topic pattern. Segments are separated by `.` in the manifest
/// declaration; `*` is a single-segment wildcard. No runtime glob — the
/// pattern is pre-compiled at registration time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TopicPattern {
    pub segments: Vec<TopicPatternSegment>,
}

impl TopicPattern {
    /// Parse a dotted topic string into segments. `*` becomes `Wildcard`,
    /// everything else becomes `Literal`.
    pub fn parse(raw: &str) -> Self {
        let segments = raw
            .split('.')
            .map(|s| {
                if s == "*" {
                    TopicPatternSegment::Wildcard
                } else {
                    TopicPatternSegment::Literal(s.to_owned())
                }
            })
            .collect();
        Self { segments }
    }

    /// Checks whether this pattern matches a concrete topic represented as
    /// `(kind, value)` segment pairs. Match rules:
    /// - Segment count must be equal.
    /// - A `Wildcard` pattern segment matches any topic segment.
    /// - A `Literal` pattern segment matches only when the value is equal.
    pub fn matches_topic_segments(&self, segments: &[(String, String)]) -> bool {
        if self.segments.len() != segments.len() {
            return false;
        }
        self.segments
            .iter()
            .zip(segments.iter())
            .all(|(pat, (_kind, value))| match pat {
                TopicPatternSegment::Wildcard => true,
                TopicPatternSegment::Literal(lit) => lit == value,
            })
    }
}

// =============================================================================
// LocalCapability
// =============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LocalCapability {
    Clipboard,
    Download,
    NavigateExternal,
    FormReset,
}

// =============================================================================
// UiCredits — credit-based flow control
// =============================================================================

#[derive(Debug, Clone)]
pub struct UiCredits {
    available: u32,
    initial: u32,
    consumed_in_window: u32,
}

impl UiCredits {
    fn new(initial: u32) -> Self {
        Self {
            available: initial,
            initial,
            consumed_in_window: 0,
        }
    }

    fn try_consume(&mut self) -> Result<(), SessionError> {
        if self.available == 0 {
            return Err(SessionError::CreditsExhausted);
        }
        self.available -= 1;
        self.consumed_in_window += 1;
        Ok(())
    }

    fn grant(&mut self, amount: u32) {
        self.available = self.available.saturating_add(amount);
        self.consumed_in_window = 0;
    }

    fn should_grant(&self) -> bool {
        self.consumed_in_window >= self.initial / 2
    }
}

// =============================================================================
// PanelOwnership
// =============================================================================

#[derive(Debug, Clone)]
pub struct PanelOwnership {
    pub panel_epoch: u64,
    pub state_revision: u64,
    pub declared_slots: HashSet<String>,
    pub declared_actions: HashSet<String>,
    pub declared_event_publish: Vec<TopicPattern>,
    pub declared_event_subscribe: Vec<TopicPattern>,
    pub declared_local_capabilities: HashSet<LocalCapability>,
    /// Set to `true` after `register_shell` succeeds. Prevents double
    /// registration for the same panel open cycle.
    shell_registered: bool,
}

// =============================================================================
// SessionError
// =============================================================================

#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum SessionError {
    #[error("panel not open: addon={addon_id} panel={panel_id}")]
    PanelNotOpen { addon_id: String, panel_id: String },

    #[error("epoch mismatch: expected {expected}, got {got}")]
    EpochMismatch { expected: u64, got: u64 },

    #[error("slot ownership violation: addon={addon_id} panel={panel_id} slot={slot_id}")]
    SlotOwnershipViolation {
        addon_id: String,
        panel_id: String,
        slot_id: String,
    },

    #[error("reserved slot prefix in slot_id: {slot_id}")]
    ReservedSlotPrefix { slot_id: String },

    #[error("state revision mismatch: expected {expected}, got {got}")]
    RevisionMismatch { expected: u64, got: u64 },

    #[error("action not declared: addon={addon_id} panel={panel_id} action={action_id}")]
    ActionNotDeclared {
        addon_id: String,
        panel_id: String,
        action_id: String,
    },

    #[error("reserved namespace: {path_root}")]
    ReservedNamespace { path_root: String },

    #[error("event topic not declared: addon={addon_id}")]
    EventTopicNotDeclared { addon_id: String },

    #[error("UI credits exhausted")]
    CreditsExhausted,

    #[error("panel already open: addon={addon_id} panel={panel_id}")]
    PanelAlreadyOpen { addon_id: String, panel_id: String },

    #[error("shell already registered: addon={addon_id} panel={panel_id}")]
    ShellAlreadyRegistered { addon_id: String, panel_id: String },
}

// =============================================================================
// SessionState
// =============================================================================

/// Entry in the open-panels list. Vec-based for zero-allocation lookups
/// (typical session has 1-3 open panels; linear scan beats HashMap).
#[derive(Debug)]
struct OpenPanel {
    addon_id: String,
    panel_id: String,
    ownership: PanelOwnership,
}

#[derive(Debug)]
pub struct SessionState {
    open_panels: Vec<OpenPanel>,
    ui_credits: UiCredits,
    next_epoch: u64,
}

impl SessionState {
    pub fn new() -> Self {
        Self {
            open_panels: Vec::with_capacity(4),
            ui_credits: UiCredits::new(INITIAL_UI_CREDITS),
            next_epoch: 1,
        }
    }

    #[inline]
    fn find_panel(&self, addon_id: &str, panel_id: &str) -> Option<usize> {
        self.open_panels
            .iter()
            .position(|p| p.addon_id == addon_id && p.panel_id == panel_id)
    }

    /// Registers a panel as open, assigns a monotonically increasing epoch.
    pub fn open_panel(&mut self, addon_id: &str, panel_id: &str) -> Result<u64, SessionError> {
        if self.find_panel(addon_id, panel_id).is_some() {
            return Err(SessionError::PanelAlreadyOpen {
                addon_id: addon_id.to_owned(),
                panel_id: panel_id.to_owned(),
            });
        }
        let epoch = self.next_epoch;
        self.next_epoch += 1;
        self.open_panels.push(OpenPanel {
            addon_id: addon_id.to_owned(),
            panel_id: panel_id.to_owned(),
            ownership: PanelOwnership {
                panel_epoch: epoch,
                state_revision: 0,
                declared_slots: HashSet::new(),
                declared_actions: HashSet::new(),
                declared_event_publish: Vec::new(),
                declared_event_subscribe: Vec::new(),
                declared_local_capabilities: HashSet::new(),
                shell_registered: false,
            },
        });
        Ok(epoch)
    }

    /// Removes a panel, returning its ownership data for cleanup.
    pub fn close_panel(&mut self, addon_id: &str, panel_id: &str) -> Option<PanelOwnership> {
        let idx = self.find_panel(addon_id, panel_id)?;
        Some(self.open_panels.swap_remove(idx).ownership)
    }

    pub fn get_panel(&self, addon_id: &str, panel_id: &str) -> Option<&PanelOwnership> {
        let idx = self.find_panel(addon_id, panel_id)?;
        Some(&self.open_panels[idx].ownership)
    }

    /// Czy ta sesja (połączenie) ma OTWARTY jakikolwiek panel danego addona.
    /// Generyczny upload plików z panelu nie zna panel_id w protokole, więc
    /// autoryzacja sprawdza, że połączenie faktycznie ma otwarty panel tego
    /// addona — ten sam model własności co ścieżka `Action` (panel musi być
    /// otwarty na tym socketcie), bez wiązania uploadu do konkretnego panel_id.
    pub fn has_open_panel_for_addon(&self, addon_id: &str) -> bool {
        self.open_panels.iter().any(|p| p.addon_id == addon_id)
    }

    pub fn get_panel_mut(&mut self, addon_id: &str, panel_id: &str) -> Option<&mut PanelOwnership> {
        let idx = self.find_panel(addon_id, panel_id)?;
        Some(&mut self.open_panels[idx].ownership)
    }

    /// Called when the host receives a PanelShell declaration from the addon.
    /// Validates epoch, rejects reserved slot prefixes, and stores all
    /// declarations on the panel ownership record.
    #[allow(clippy::too_many_arguments)]
    pub fn register_shell(
        &mut self,
        addon_id: &str,
        panel_id: &str,
        panel_epoch: u64,
        slots: HashSet<String>,
        actions: HashSet<String>,
        publish_topics: Vec<TopicPattern>,
        subscribe_topics: Vec<TopicPattern>,
        capabilities: HashSet<LocalCapability>,
    ) -> Result<(), SessionError> {
        let idx =
            self.find_panel(addon_id, panel_id)
                .ok_or_else(|| SessionError::PanelNotOpen {
                    addon_id: addon_id.to_owned(),
                    panel_id: panel_id.to_owned(),
                })?;
        let ownership = &mut self.open_panels[idx].ownership;

        if ownership.shell_registered {
            return Err(SessionError::ShellAlreadyRegistered {
                addon_id: addon_id.to_owned(),
                panel_id: panel_id.to_owned(),
            });
        }

        if ownership.panel_epoch != panel_epoch {
            return Err(SessionError::EpochMismatch {
                expected: ownership.panel_epoch,
                got: panel_epoch,
            });
        }

        for slot_id in &slots {
            if slot_id.starts_with(RESERVED_SLOT_PREFIX) {
                return Err(SessionError::ReservedSlotPrefix {
                    slot_id: slot_id.clone(),
                });
            }
        }

        ownership.declared_slots = slots;
        ownership.declared_actions = actions;
        ownership.declared_event_publish = publish_topics;
        ownership.declared_event_subscribe = subscribe_topics;
        ownership.declared_local_capabilities = capabilities;
        ownership.shell_registered = true;

        Ok(())
    }

    /// Checks that `slot_id` belongs to the declared slots of the given panel.
    pub fn validate_slot_ownership(
        &self,
        addon_id: &str,
        panel_id: &str,
        slot_id: &str,
    ) -> Result<(), SessionError> {
        let ownership =
            self.get_panel(addon_id, panel_id)
                .ok_or_else(|| SessionError::PanelNotOpen {
                    addon_id: addon_id.to_owned(),
                    panel_id: panel_id.to_owned(),
                })?;
        if !ownership.declared_slots.contains(slot_id) {
            return Err(SessionError::SlotOwnershipViolation {
                addon_id: addon_id.to_owned(),
                panel_id: panel_id.to_owned(),
                slot_id: slot_id.to_owned(),
            });
        }
        Ok(())
    }

    /// Validates that `base_revision` matches the panel's current
    /// `state_revision`. Returns `Ok(())` on match, or `Err` with the
    /// current revision on mismatch.
    pub fn validate_state_revision(
        &self,
        addon_id: &str,
        panel_id: &str,
        base_revision: u64,
    ) -> Result<(), SessionError> {
        let ownership =
            self.get_panel(addon_id, panel_id)
                .ok_or_else(|| SessionError::PanelNotOpen {
                    addon_id: addon_id.to_owned(),
                    panel_id: panel_id.to_owned(),
                })?;
        if ownership.state_revision != base_revision {
            return Err(SessionError::RevisionMismatch {
                expected: ownership.state_revision,
                got: base_revision,
            });
        }
        Ok(())
    }

    /// Advances the panel's state revision after a successful state patch.
    pub fn advance_state_revision(
        &mut self,
        addon_id: &str,
        panel_id: &str,
        new_revision: u64,
    ) -> Result<(), SessionError> {
        let ownership =
            self.get_panel_mut(addon_id, panel_id)
                .ok_or_else(|| SessionError::PanelNotOpen {
                    addon_id: addon_id.to_owned(),
                    panel_id: panel_id.to_owned(),
                })?;
        ownership.state_revision = new_revision;
        Ok(())
    }

    /// Checks that `action_id` is among the panel's declared actions.
    pub fn validate_action(
        &self,
        addon_id: &str,
        panel_id: &str,
        action_id: &str,
    ) -> Result<(), SessionError> {
        let ownership =
            self.get_panel(addon_id, panel_id)
                .ok_or_else(|| SessionError::PanelNotOpen {
                    addon_id: addon_id.to_owned(),
                    panel_id: panel_id.to_owned(),
                })?;
        if !ownership.declared_actions.contains(action_id) {
            return Err(SessionError::ActionNotDeclared {
                addon_id: addon_id.to_owned(),
                panel_id: panel_id.to_owned(),
                action_id: action_id.to_owned(),
            });
        }
        Ok(())
    }

    /// Dynamically adds new action_ids to a panel's declared set. Called when
    /// SlotContent pushes components with new handlers not present in the
    /// original layout.
    pub fn extend_declared_actions(
        &mut self,
        addon_id: &str,
        panel_id: &str,
        new_actions: HashSet<String>,
    ) {
        if let Some(idx) = self.find_panel(addon_id, panel_id) {
            self.open_panels[idx]
                .ownership
                .declared_actions
                .extend(new_actions);
        }
    }

    /// Enforces §8.3 namespace rules. Reserved root paths (`__system`,
    /// `__user`, etc.) are writable only from local action handlers
    /// (`from_local_action = true`). Addon-initiated state patches must not
    /// touch them.
    pub fn validate_state_path_writable(
        path_root: &str,
        from_local_action: bool,
    ) -> Result<(), SessionError> {
        if !from_local_action && RESERVED_STATE_ROOTS.contains(&path_root) {
            return Err(SessionError::ReservedNamespace {
                path_root: path_root.to_owned(),
            });
        }
        Ok(())
    }

    /// Validates that the addon has at least one open panel whose
    /// `declared_event_publish` patterns match the given topic segments.
    pub fn validate_event_publish(
        &self,
        addon_id: &str,
        topic_segments: &[(String, String)],
    ) -> Result<(), SessionError> {
        let any_match = self
            .open_panels
            .iter()
            .filter(|p| p.addon_id == addon_id)
            .any(|p| {
                p.ownership
                    .declared_event_publish
                    .iter()
                    .any(|pat| pat.matches_topic_segments(topic_segments))
            });
        if !any_match {
            return Err(SessionError::EventTopicNotDeclared {
                addon_id: addon_id.to_owned(),
            });
        }
        Ok(())
    }

    /// Consumes one UI credit. Returns `Err(CreditsExhausted)` when none
    /// remain.
    pub fn try_consume_credit(&mut self) -> Result<(), SessionError> {
        self.ui_credits.try_consume()
    }

    /// Grants additional credits to the session. Typically called by the
    /// receiver side when it has processed enough frames.
    pub fn grant_credits(&mut self, amount: u32) {
        self.ui_credits.grant(amount);
    }

    /// Returns `true` when the receiver should send a credit grant — i.e.
    /// when >=50% of the initial credits have been consumed.
    pub fn should_grant_credits(&self) -> bool {
        self.ui_credits.should_grant()
    }
}

impl Default for SessionState {
    fn default() -> Self {
        Self::new()
    }
}

// =============================================================================
// SessionRegistry — per-WS-connection UI session state
// =============================================================================

/// Process-wide registry of UI panel session state, keyed by connection_id.
/// Each WS connection gets its own `SessionState` tracking open panels, slot
/// declarations, state revisions, etc. Fine-grained Mutex per session avoids
/// contention between independent connections.
pub struct SessionRegistry {
    sessions: RwLock<HashMap<u64, Arc<Mutex<SessionState>>>>,
    /// Maps (addon_id, user_id) to connection_id so host functions can look up
    /// which SessionState owns the panel they are rendering to.
    addon_connections: RwLock<HashMap<(String, String), u64>>,
}

impl Default for SessionRegistry {
    fn default() -> Self {
        Self {
            sessions: RwLock::new(HashMap::new()),
            addon_connections: RwLock::new(HashMap::new()),
        }
    }
}

impl SessionRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the session state for `connection_id`, creating a fresh one if
    /// this connection hasn't been seen yet.
    pub fn get_or_create(&self, connection_id: u64) -> Arc<Mutex<SessionState>> {
        {
            let read = self.sessions.read();
            if let Some(s) = read.get(&connection_id) {
                return s.clone();
            }
        }
        let mut write = self.sessions.write();
        write
            .entry(connection_id)
            .or_insert_with(|| Arc::new(Mutex::new(SessionState::new())))
            .clone()
    }

    /// Removes the session for `connection_id` (called on WS disconnect).
    /// Also drops every (addon_id, user_id) → connection_id mapping owned by
    /// this connection — PanelClose does the same cleanup explicitly, but on
    /// an abrupt disconnect no PanelClose arrives and stale mappings would
    /// keep routing ticks/renders to a dead connection (`panel_not_open`
    /// warnings) until the same addon+user reopened a panel. Mappings owned
    /// by other live connections are untouched.
    pub fn remove(&self, connection_id: u64) {
        self.sessions.write().remove(&connection_id);
        self.addon_connections
            .write()
            .retain(|_, conn_id| *conn_id != connection_id);
    }

    /// Records which connection_id is serving a given addon+user panel session.
    pub fn register_addon_connection(&self, addon_id: &str, user_id: &str, connection_id: u64) {
        tracing::info!(
            addon = addon_id,
            user_id,
            connection_id,
            registry_ptr = format_args!("{:p}", self),
            "register_addon_connection"
        );
        self.addon_connections
            .write()
            .insert((addon_id.to_owned(), user_id.to_owned()), connection_id);
    }

    /// Removes the addon+user → connection_id mapping (panel close / disconnect).
    pub fn unregister_addon_connection(&self, addon_id: &str, user_id: &str) {
        self.addon_connections
            .write()
            .retain(|(aid, uid), _| !(aid == addon_id && uid == user_id));
    }

    /// Looks up the connection_id serving a given addon+user panel.
    /// Uses linear scan to avoid String allocation on every lookup.
    /// Typical session count is < 50, so linear scan is faster than
    /// HashMap hashing + allocation overhead.
    pub fn find_connection(&self, addon_id: &str, user_id: &str) -> Option<u64> {
        let read = self.addon_connections.read();
        let count = read.len();
        tracing::debug!(
            addon = addon_id,
            user_id,
            entries = count,
            "find_connection lookup"
        );
        for ((aid, uid), conn_id) in read.iter() {
            tracing::debug!(stored_addon = %aid, stored_uid = %uid, stored_conn = conn_id, "find_connection entry");
            if uid == user_id && aid == addon_id {
                return Some(*conn_id);
            }
        }
        None
    }
}

// =============================================================================
// Global SessionRegistry — accessible from host functions without threading
// through AddonState.
// =============================================================================

static GLOBAL_SESSION_REGISTRY: OnceLock<Arc<SessionRegistry>> = OnceLock::new();

/// Initializes the process-wide global registry. Called once at server startup.
pub fn init_global_registry(registry: Arc<SessionRegistry>) {
    if GLOBAL_SESSION_REGISTRY.set(registry).is_err() {
        tracing::warn!("init_global_registry called twice — ignoring second call");
    }
}

/// Returns the process-wide global registry (None before `init_global_registry`).
pub fn global_registry() -> Option<&'static Arc<SessionRegistry>> {
    GLOBAL_SESSION_REGISTRY.get()
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_close_panel_lifecycle() {
        let mut state = SessionState::new();
        let epoch = state.open_panel("addon-a", "main").unwrap();
        assert_eq!(epoch, 1);
        assert!(state.get_panel("addon-a", "main").is_some());

        let ownership = state.close_panel("addon-a", "main");
        assert!(ownership.is_some());
        assert_eq!(ownership.unwrap().panel_epoch, 1);
        assert!(state.get_panel("addon-a", "main").is_none());
    }

    #[test]
    fn open_panel_already_open() {
        let mut state = SessionState::new();
        state.open_panel("addon-a", "main").unwrap();
        let err = state.open_panel("addon-a", "main").unwrap_err();
        assert!(matches!(err, SessionError::PanelAlreadyOpen { .. }));
    }

    #[test]
    fn close_nonexistent_panel_returns_none() {
        let mut state = SessionState::new();
        assert!(state.close_panel("nope", "nope").is_none());
    }

    #[test]
    fn epoch_monotonicity() {
        let mut state = SessionState::new();
        let e1 = state.open_panel("a", "p1").unwrap();
        let e2 = state.open_panel("a", "p2").unwrap();
        let e3 = state.open_panel("b", "p1").unwrap();
        assert!(e1 < e2);
        assert!(e2 < e3);

        // Close and reopen — epoch must still increase.
        state.close_panel("a", "p1");
        let e4 = state.open_panel("a", "p1").unwrap();
        assert!(e4 > e3);
    }

    #[test]
    fn slot_ownership_valid() {
        let mut state = SessionState::new();
        let epoch = state.open_panel("addon-a", "main").unwrap();

        let mut slots = HashSet::new();
        slots.insert("content".to_owned());
        slots.insert("sidebar".to_owned());

        state
            .register_shell(
                "addon-a",
                "main",
                epoch,
                slots,
                HashSet::new(),
                Vec::new(),
                Vec::new(),
                HashSet::new(),
            )
            .unwrap();

        assert!(state
            .validate_slot_ownership("addon-a", "main", "content")
            .is_ok());
        assert!(state
            .validate_slot_ownership("addon-a", "main", "sidebar")
            .is_ok());
    }

    #[test]
    fn slot_ownership_violation() {
        let mut state = SessionState::new();
        let epoch = state.open_panel("addon-a", "main").unwrap();

        let mut slots = HashSet::new();
        slots.insert("content".to_owned());

        state
            .register_shell(
                "addon-a",
                "main",
                epoch,
                slots,
                HashSet::new(),
                Vec::new(),
                Vec::new(),
                HashSet::new(),
            )
            .unwrap();

        let err = state
            .validate_slot_ownership("addon-a", "main", "other")
            .unwrap_err();
        assert!(matches!(err, SessionError::SlotOwnershipViolation { .. }));
    }

    #[test]
    fn reserved_slot_prefix_rejected() {
        let mut state = SessionState::new();
        let epoch = state.open_panel("addon-a", "main").unwrap();

        let mut slots = HashSet::new();
        slots.insert("__shell:nav".to_owned());

        let err = state
            .register_shell(
                "addon-a",
                "main",
                epoch,
                slots,
                HashSet::new(),
                Vec::new(),
                Vec::new(),
                HashSet::new(),
            )
            .unwrap_err();
        assert!(matches!(err, SessionError::ReservedSlotPrefix { .. }));
    }

    #[test]
    fn state_revision_match_and_mismatch() {
        let mut state = SessionState::new();
        state.open_panel("addon-a", "main").unwrap();

        // Initially at revision 0.
        assert!(state.validate_state_revision("addon-a", "main", 0).is_ok());

        // Advance to 1.
        state.advance_state_revision("addon-a", "main", 1).unwrap();

        // Now base_revision 0 is stale.
        let err = state
            .validate_state_revision("addon-a", "main", 0)
            .unwrap_err();
        assert!(matches!(
            err,
            SessionError::RevisionMismatch {
                expected: 1,
                got: 0
            }
        ));

        // Matching revision works.
        assert!(state.validate_state_revision("addon-a", "main", 1).is_ok());
    }

    #[test]
    fn reserved_namespace_enforcement() {
        for root in RESERVED_STATE_ROOTS {
            let err = SessionState::validate_state_path_writable(root, false).unwrap_err();
            assert!(matches!(err, SessionError::ReservedNamespace { .. }));

            // From a local action handler, reserved roots are writable.
            assert!(SessionState::validate_state_path_writable(root, true).is_ok());
        }

        // Non-reserved roots are always writable.
        assert!(SessionState::validate_state_path_writable("items", false).is_ok());
        assert!(SessionState::validate_state_path_writable("items", true).is_ok());
    }

    #[test]
    fn credit_consumption_and_exhaustion() {
        let mut state = SessionState::new();

        // Consume all credits.
        for _ in 0..INITIAL_UI_CREDITS {
            state.try_consume_credit().unwrap();
        }

        // Next consume must fail.
        let err = state.try_consume_credit().unwrap_err();
        assert!(matches!(err, SessionError::CreditsExhausted));
    }

    #[test]
    fn credit_grant_and_should_grant_threshold() {
        let mut state = SessionState::new();

        // Consume less than 50% — should_grant is false.
        for _ in 0..(INITIAL_UI_CREDITS / 2 - 1) {
            state.try_consume_credit().unwrap();
        }
        assert!(!state.should_grant_credits());

        // Consume one more to reach exactly 50%.
        state.try_consume_credit().unwrap();
        assert!(state.should_grant_credits());

        // Grant resets the window.
        state.grant_credits(INITIAL_UI_CREDITS);
        assert!(!state.should_grant_credits());
    }

    #[test]
    fn action_validation() {
        let mut state = SessionState::new();
        let epoch = state.open_panel("addon-a", "main").unwrap();

        let mut actions = HashSet::new();
        actions.insert("save".to_owned());
        actions.insert("delete".to_owned());

        state
            .register_shell(
                "addon-a",
                "main",
                epoch,
                HashSet::new(),
                actions,
                Vec::new(),
                Vec::new(),
                HashSet::new(),
            )
            .unwrap();

        assert!(state.validate_action("addon-a", "main", "save").is_ok());
        assert!(state.validate_action("addon-a", "main", "delete").is_ok());

        let err = state
            .validate_action("addon-a", "main", "hack")
            .unwrap_err();
        assert!(matches!(err, SessionError::ActionNotDeclared { .. }));
    }

    #[test]
    fn register_shell_epoch_mismatch() {
        let mut state = SessionState::new();
        let _epoch = state.open_panel("addon-a", "main").unwrap();

        let err = state
            .register_shell(
                "addon-a",
                "main",
                999,
                HashSet::new(),
                HashSet::new(),
                Vec::new(),
                Vec::new(),
                HashSet::new(),
            )
            .unwrap_err();
        assert!(matches!(err, SessionError::EpochMismatch { .. }));
    }

    #[test]
    fn register_shell_already_registered() {
        let mut state = SessionState::new();
        let epoch = state.open_panel("addon-a", "main").unwrap();

        state
            .register_shell(
                "addon-a",
                "main",
                epoch,
                HashSet::new(),
                HashSet::new(),
                Vec::new(),
                Vec::new(),
                HashSet::new(),
            )
            .unwrap();

        let err = state
            .register_shell(
                "addon-a",
                "main",
                epoch,
                HashSet::new(),
                HashSet::new(),
                Vec::new(),
                Vec::new(),
                HashSet::new(),
            )
            .unwrap_err();
        assert!(matches!(err, SessionError::ShellAlreadyRegistered { .. }));
    }

    #[test]
    fn register_shell_panel_not_open() {
        let mut state = SessionState::new();

        let err = state
            .register_shell(
                "addon-a",
                "main",
                1,
                HashSet::new(),
                HashSet::new(),
                Vec::new(),
                Vec::new(),
                HashSet::new(),
            )
            .unwrap_err();
        assert!(matches!(err, SessionError::PanelNotOpen { .. }));
    }

    #[test]
    fn get_panel_mut_modifies_ownership() {
        let mut state = SessionState::new();
        state.open_panel("addon-a", "main").unwrap();

        let ownership = state.get_panel_mut("addon-a", "main").unwrap();
        ownership.state_revision = 42;

        assert_eq!(
            state.get_panel("addon-a", "main").unwrap().state_revision,
            42
        );
    }

    #[test]
    fn topic_pattern_parse() {
        let p = TopicPattern::parse("addon.*.updated");
        assert_eq!(p.segments.len(), 3);
        assert_eq!(
            p.segments[0],
            TopicPatternSegment::Literal("addon".to_owned())
        );
        assert_eq!(p.segments[1], TopicPatternSegment::Wildcard);
        assert_eq!(
            p.segments[2],
            TopicPatternSegment::Literal("updated".to_owned())
        );
    }

    #[test]
    fn validate_on_nonexistent_panel() {
        let state = SessionState::new();

        assert!(matches!(
            state.validate_slot_ownership("x", "y", "z"),
            Err(SessionError::PanelNotOpen { .. })
        ));
        assert!(matches!(
            state.validate_state_revision("x", "y", 0),
            Err(SessionError::PanelNotOpen { .. })
        ));
        assert!(matches!(
            state.validate_action("x", "y", "a"),
            Err(SessionError::PanelNotOpen { .. })
        ));
    }

    #[test]
    fn advance_revision_panel_not_open() {
        let mut state = SessionState::new();
        let err = state.advance_state_revision("x", "y", 1).unwrap_err();
        assert!(matches!(err, SessionError::PanelNotOpen { .. }));
    }

    #[test]
    fn grant_credits_saturates() {
        let mut state = SessionState::new();
        state.grant_credits(u32::MAX);
        // Should not panic on overflow — saturating add.
        state.grant_credits(1);
        assert!(state.try_consume_credit().is_ok());
    }

    // =========================================================================
    // SessionRegistry tests
    // =========================================================================

    #[test]
    fn registry_get_or_create_and_remove() {
        let reg = SessionRegistry::new();
        let s1 = reg.get_or_create(1);
        let s2 = reg.get_or_create(1);
        // Same Arc for same connection_id.
        assert!(Arc::ptr_eq(&s1, &s2));

        reg.remove(1);
        let s3 = reg.get_or_create(1);
        // After removal a fresh session is created.
        assert!(!Arc::ptr_eq(&s1, &s3));
    }

    #[test]
    fn registry_addon_connection_lifecycle() {
        let reg = SessionRegistry::new();

        assert!(reg.find_connection("contacts", "1").is_none());

        reg.register_addon_connection("contacts", "1", 42);
        assert_eq!(reg.find_connection("contacts", "1"), Some(42));

        // Different user_id — not found.
        assert!(reg.find_connection("contacts", "2").is_none());

        reg.unregister_addon_connection("contacts", "1");
        assert!(reg.find_connection("contacts", "1").is_none());
    }

    #[test]
    fn registry_remove_purges_only_own_addon_mappings() {
        let reg = SessionRegistry::new();
        reg.get_or_create(1);
        reg.get_or_create(2);
        reg.register_addon_connection("tentavision", "1", 1);
        reg.register_addon_connection("contacts", "1", 1);
        reg.register_addon_connection("tentavision", "2", 2);

        // Disconnect of connection 1 drops both of its mappings...
        reg.remove(1);
        assert!(reg.find_connection("tentavision", "1").is_none());
        assert!(reg.find_connection("contacts", "1").is_none());

        // ...while connection 2's mapping survives.
        assert_eq!(reg.find_connection("tentavision", "2"), Some(2));
    }

    #[test]
    fn registry_addon_connection_overwrite() {
        let reg = SessionRegistry::new();
        reg.register_addon_connection("a", "1", 10);
        reg.register_addon_connection("a", "1", 20);
        assert_eq!(reg.find_connection("a", "1"), Some(20));
    }

    // =========================================================================
    // TopicPattern matching tests
    // =========================================================================

    #[test]
    fn topic_pattern_matches_exact_literals() {
        let p = TopicPattern::parse("addon.contacts.updated");
        let segments = vec![
            ("literal".into(), "addon".into()),
            ("literal".into(), "contacts".into()),
            ("literal".into(), "updated".into()),
        ];
        assert!(p.matches_topic_segments(&segments));
    }

    #[test]
    fn topic_pattern_wildcard_matches_any_value() {
        let p = TopicPattern::parse("addon.*.updated");
        let segments = vec![
            ("literal".into(), "addon".into()),
            ("id".into(), "anything-here".into()),
            ("literal".into(), "updated".into()),
        ];
        assert!(p.matches_topic_segments(&segments));
    }

    #[test]
    fn topic_pattern_length_mismatch_rejects() {
        let p = TopicPattern::parse("addon.contacts");
        let segments = vec![
            ("literal".into(), "addon".into()),
            ("literal".into(), "contacts".into()),
            ("literal".into(), "extra".into()),
        ];
        assert!(!p.matches_topic_segments(&segments));
    }

    #[test]
    fn topic_pattern_literal_mismatch_rejects() {
        let p = TopicPattern::parse("addon.contacts.deleted");
        let segments = vec![
            ("literal".into(), "addon".into()),
            ("literal".into(), "contacts".into()),
            ("literal".into(), "updated".into()),
        ];
        assert!(!p.matches_topic_segments(&segments));
    }

    // =========================================================================
    // validate_event_publish tests
    // =========================================================================

    #[test]
    fn validate_event_publish_allowed() {
        let mut state = SessionState::new();
        let epoch = state.open_panel("addon-a", "main").unwrap();

        state
            .register_shell(
                "addon-a",
                "main",
                epoch,
                HashSet::new(),
                HashSet::new(),
                vec![TopicPattern::parse("addon-a.*.updated")],
                Vec::new(),
                HashSet::new(),
            )
            .unwrap();

        let segments = vec![
            ("literal".into(), "addon-a".into()),
            ("id".into(), "entity-123".into()),
            ("literal".into(), "updated".into()),
        ];
        assert!(state.validate_event_publish("addon-a", &segments).is_ok());
    }

    #[test]
    fn validate_event_publish_denied() {
        let mut state = SessionState::new();
        let epoch = state.open_panel("addon-a", "main").unwrap();

        state
            .register_shell(
                "addon-a",
                "main",
                epoch,
                HashSet::new(),
                HashSet::new(),
                vec![TopicPattern::parse("addon-a.contacts.updated")],
                Vec::new(),
                HashSet::new(),
            )
            .unwrap();

        let segments = vec![
            ("literal".into(), "addon-a".into()),
            ("literal".into(), "contacts".into()),
            ("literal".into(), "deleted".into()),
        ];
        let err = state
            .validate_event_publish("addon-a", &segments)
            .unwrap_err();
        assert!(matches!(err, SessionError::EventTopicNotDeclared { .. }));
    }

    #[test]
    fn validate_event_publish_no_panels_open() {
        let state = SessionState::new();
        let segments = vec![("literal".into(), "x".into())];
        let err = state
            .validate_event_publish("addon-a", &segments)
            .unwrap_err();
        assert!(matches!(err, SessionError::EventTopicNotDeclared { .. }));
    }
}
