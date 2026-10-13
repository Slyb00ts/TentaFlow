// ===== File: processes/tool_host.rs — addon tool access for BPMN Service tasks =====

use anyhow::Result;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

/// What the process engine pins about a tool at publication.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessToolDescriptor {
    pub input_schema: Value,
    pub bundle_hash: String,
}

/// Why an initiator may not run an addon tool. Only `PermissionDenied` is a
/// decision a person can change (grant the permission and resume), so it alone
/// becomes a human task; every other variant means the source is gone.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ToolAccessError {
    #[error("addon tool is not available")]
    Unavailable,
    // Same text as `Unavailable` on purpose: a caller outside the addon's
    // organization must not learn that the addon exists.
    #[error("addon tool is not available")]
    OtherOrganization,
    #[error("initiator may not call this addon tool")]
    PermissionDenied,
    #[error("process initiator is inactive or outside the organization")]
    Initiator,
}

/// The slice of the addon runtime a Service task needs. A trait so the process
/// engine is tested against a scripted addon instead of a WASM module.
pub trait ProcessToolHost: Send + Sync {
    /// Registered, enabled, same-organization and permitted for `user_id`.
    fn authorize(
        &self,
        addon_id: &str,
        tool_name: &str,
        user_id: &str,
        org_id: &str,
    ) -> Result<ProcessToolDescriptor, ToolAccessError>;

    /// Blocking call. `request_id` is the invocation's stable request id.
    fn call(
        &self,
        addon_id: &str,
        tool_name: &str,
        params: Value,
        user_id: &str,
        org_id: &str,
        request_id: &str,
    ) -> Result<Value>;
}

/// The organization rule for process calls. The process path never starts an
/// addon (a main instance started here would be created under the initiator's
/// identity and organization), so a stopped addon is unavailable and a running
/// one must belong to the initiator's organization. An instance started at
/// boot has no organization and serves the default one.
pub fn check_addon_organization(
    running: bool,
    instance_org: Option<&str>,
    requested_org: &str,
) -> Result<(), ToolAccessError> {
    if !running {
        return Err(ToolAccessError::Unavailable);
    }
    if instance_org.unwrap_or(crate::services::org::DEFAULT_ORG_ID) != requested_org {
        return Err(ToolAccessError::OtherOrganization);
    }
    Ok(())
}

impl ProcessToolHost for crate::addon::AddonManager {
    fn authorize(
        &self,
        addon_id: &str,
        tool_name: &str,
        user_id: &str,
        org_id: &str,
    ) -> Result<ProcessToolDescriptor, ToolAccessError> {
        let tool = self
            .list_tools()
            .into_iter()
            .find(|tool| tool.addon_id == addon_id && tool.tool_name == tool_name)
            .ok_or(ToolAccessError::Unavailable)?;
        check_addon_organization(
            self.has_running_instance(addon_id),
            self.instance_org_id(addon_id).as_deref(),
            org_id,
        )?;
        if !self
            .permission_checker()
            .check(addon_id, user_id, "llm", None)
            .is_granted()
        {
            return Err(ToolAccessError::PermissionDenied);
        }
        Ok(ProcessToolDescriptor {
            input_schema: tool.parameters_schema,
            bundle_hash: self
                .installed_bundle_hash(addon_id)
                .map_err(|_| ToolAccessError::Unavailable)?,
        })
    }

    fn call(
        &self,
        addon_id: &str,
        tool_name: &str,
        params: Value,
        user_id: &str,
        org_id: &str,
        request_id: &str,
    ) -> Result<Value> {
        // Decided again right before the call: the claim-time check can be
        // minutes old, and this is the last point before WASM runs.
        self.authorize(addon_id, tool_name, user_id, org_id)?;
        self.call_tool_process(addon_id, tool_name, params, user_id, org_id, request_id)
    }
}

/// Caps the WASM calls the process engine has in flight per addon, running or
/// abandoned. A timed-out call cannot be interrupted, so its permit stays with
/// the blocking closure until the guest really returns; without a cap, tasks
/// that keep timing out would pile up threads and worker instances.
pub struct ToolCallGate {
    limit: std::sync::atomic::AtomicUsize,
    in_flight: parking_lot::Mutex<std::collections::HashMap<String, usize>>,
}

pub const DEFAULT_TOOL_CALLS_PER_ADDON: usize = 4;

impl Default for ToolCallGate {
    fn default() -> Self {
        Self {
            limit: std::sync::atomic::AtomicUsize::new(DEFAULT_TOOL_CALLS_PER_ADDON),
            in_flight: parking_lot::Mutex::new(std::collections::HashMap::new()),
        }
    }
}

impl ToolCallGate {
    #[cfg(test)]
    pub fn set_limit(&self, limit: usize) {
        self.limit.store(limit, std::sync::atomic::Ordering::SeqCst);
    }

    /// `None` when `addon_id` already has the maximum number of calls in flight.
    pub fn try_acquire(self: &std::sync::Arc<Self>, addon_id: &str) -> Option<ToolCallPermit> {
        let limit = self.limit.load(std::sync::atomic::Ordering::SeqCst);
        let mut in_flight = self.in_flight.lock();
        let count = in_flight.entry(addon_id.to_owned()).or_insert(0);
        if *count >= limit {
            return None;
        }
        *count += 1;
        Some(ToolCallPermit {
            gate: self.clone(),
            addon_id: addon_id.to_owned(),
        })
    }

    #[cfg(test)]
    pub fn in_flight(&self, addon_id: &str) -> usize {
        self.in_flight.lock().get(addon_id).copied().unwrap_or(0)
    }
}

/// Released when dropped, which for a call is when its blocking closure ends.
pub struct ToolCallPermit {
    gate: std::sync::Arc<ToolCallGate>,
    addon_id: String,
}

impl Drop for ToolCallPermit {
    fn drop(&mut self) {
        let mut in_flight = self.gate.in_flight.lock();
        if let Some(count) = in_flight.get_mut(&self.addon_id) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                in_flight.remove(&self.addon_id);
            }
        }
    }
}

/// SHA-256 of the canonical JSON of a tool's input schema.
pub fn tool_schema_digest(schema: &Value) -> Result<String> {
    let canonical = super::repository::canonical_json_value(schema);
    Ok(hex::encode(Sha256::digest(serde_json::to_vec(&canonical)?)))
}

/// Canonical JSON of the tool pin and its SHA-256. Mirrors the graph JSON/hash
/// pair a Flow pin stores, so the publication code treats both alike.
pub fn tool_pin(
    addon_id: &str,
    tool_name: &str,
    descriptor: &ProcessToolDescriptor,
) -> Result<(String, String)> {
    let digest = tool_schema_digest(&descriptor.input_schema)?;
    let pin = serde_json::to_string(&json!({
        "addon_id": addon_id,
        "tool_name": tool_name,
        "bundle_hash": descriptor.bundle_hash,
        "input_schema_digest": digest,
    }))?;
    let sha256 = hex::encode(Sha256::digest(pin.as_bytes()));
    Ok((pin, sha256))
}

/// Arguments of the tool call: the mapped activity variables, plus the payload
/// when the mapping set one explicitly. An unmapped payload is the whole
/// variable set, which is no argument at all.
pub fn tool_arguments(
    input: &Value,
    mapping: &std::collections::BTreeMap<String, String>,
) -> Result<Value> {
    let mut arguments = input
        .get("variables")
        .and_then(Value::as_object)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("service input variables missing"))?;
    if mapping.contains_key("payload") {
        arguments.insert(
            "payload".to_owned(),
            input
                .get("payload")
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("service input payload missing"))?,
        );
    }
    Ok(Value::Object(arguments))
}

/// Checks the arguments against the part of the tool's JSON Schema the engine
/// can decide cheaply: an object carrying every `required` property.
pub fn check_tool_arguments(schema: &Value, arguments: &Value) -> Result<()> {
    let object = arguments
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("tool arguments must be an object"))?;
    if let Some(required) = schema.get("required").and_then(Value::as_array) {
        for name in required.iter().filter_map(Value::as_str) {
            anyhow::ensure!(
                object.contains_key(name),
                "tool argument {name} is required"
            );
        }
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::sync::{Arc, Mutex};

    use super::*;

    /// addon, tool, params, user, request id
    pub type RecordedCall = (String, String, Value, String, String);

    /// A scripted addon: permissions and the call outcome are mutable so a test
    /// can revoke access between the claim and the result.
    pub struct ScriptedToolHost {
        pub allow: Mutex<Result<(), ToolAccessError>>,
        pub reply: Mutex<Result<Value, String>>,
        pub delay: Mutex<std::time::Duration>,
        pub calls: Mutex<Vec<RecordedCall>>,
        pub schema: Value,
        /// Bundle hash the scripted addon reports.
        pub bundle: Mutex<String>,
        /// Organization the scripted addon runs in.
        pub org: String,
        /// Revoke the initiator's permission while the call is running.
        pub revoke_during_call: bool,
    }

    impl ScriptedToolHost {
        pub fn new(reply: Value) -> Arc<Self> {
            Arc::new(Self {
                allow: Mutex::new(Ok(())),
                reply: Mutex::new(Ok(reply)),
                delay: Mutex::new(std::time::Duration::ZERO),
                calls: Mutex::new(Vec::new()),
                schema: json!({"type": "object", "required": ["query"]}),
                bundle: Mutex::new("b".repeat(64)),
                org: crate::services::org::DEFAULT_ORG_ID.to_owned(),
                revoke_during_call: false,
            })
        }
    }

    impl ProcessToolHost for ScriptedToolHost {
        fn authorize(
            &self,
            _addon_id: &str,
            _tool_name: &str,
            _user_id: &str,
            org_id: &str,
        ) -> Result<ProcessToolDescriptor, ToolAccessError> {
            if org_id != self.org {
                return Err(ToolAccessError::OtherOrganization);
            }
            self.allow.lock().unwrap().clone()?;
            Ok(ProcessToolDescriptor {
                input_schema: self.schema.clone(),
                bundle_hash: self.bundle.lock().unwrap().clone(),
            })
        }

        fn call(
            &self,
            addon_id: &str,
            tool_name: &str,
            params: Value,
            user_id: &str,
            _org_id: &str,
            request_id: &str,
        ) -> Result<Value> {
            self.calls.lock().unwrap().push((
                addon_id.to_owned(),
                tool_name.to_owned(),
                params,
                user_id.to_owned(),
                request_id.to_owned(),
            ));
            std::thread::sleep(*self.delay.lock().unwrap());
            if self.revoke_during_call {
                *self.allow.lock().unwrap() = Err(ToolAccessError::PermissionDenied);
            }
            self.reply
                .lock()
                .unwrap()
                .clone()
                .map_err(|e| anyhow::anyhow!(e))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::addon::{AddonManager, ToolDefinition};

    fn manager() -> Arc<AddonManager> {
        let directory = tempfile::tempdir().unwrap();
        let db = crate::db::init(&directory.path().join("addons.db")).unwrap();
        // The database outlives the directory handle only through the pool; keep
        // the files for the whole test.
        std::mem::forget(directory);
        let cipher = Arc::new(crate::crypto::SettingsCipher::new(&[0u8; 32]));
        let manager = Arc::new(AddonManager::new(db, cipher).expect("addon manager"));
        manager.register_tool_for_test(ToolDefinition {
            addon_id: "notes-1a2b3c4d".into(),
            package_id: "notes".into(),
            tool_name: "search".into(),
            description: String::new(),
            parameters_schema: json!({"type": "object"}),
            return_schema: None,
            keywords: Vec::new(),
            read_only: false,
        });
        manager
    }

    #[test]
    fn organization_rule_needs_a_running_addon_of_the_same_organization() {
        use crate::services::org::DEFAULT_ORG_ID;
        assert_eq!(
            check_addon_organization(false, Some("org-a"), "org-a"),
            Err(ToolAccessError::Unavailable)
        );
        assert_eq!(
            check_addon_organization(true, Some("org-a"), "org-a"),
            Ok(())
        );
        assert_eq!(
            check_addon_organization(true, Some("org-a"), "org-b"),
            Err(ToolAccessError::OtherOrganization)
        );
        // A boot-started instance has no organization and serves the default one.
        assert_eq!(check_addon_organization(true, None, DEFAULT_ORG_ID), Ok(()));
        assert_eq!(
            check_addon_organization(true, None, "org-b"),
            Err(ToolAccessError::OtherOrganization)
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn real_host_refuses_a_stopped_addon_and_never_starts_it() {
        let manager = manager();
        let host: &dyn ProcessToolHost = &*manager;
        assert_eq!(
            host.authorize("notes-1a2b3c4d", "search", "u1", "org-default")
                .unwrap_err(),
            ToolAccessError::Unavailable
        );
        let error = host
            .call(
                "notes-1a2b3c4d",
                "search",
                json!({}),
                "u1",
                "org-default",
                "req-1",
            )
            .unwrap_err();
        assert!(error.to_string().contains("not available"), "{error:#}");
        assert!(
            !manager.has_running_instance("notes-1a2b3c4d"),
            "the process path must not start an addon"
        );
        // An unknown tool answers exactly like a stopped addon.
        assert_eq!(
            host.authorize("notes-1a2b3c4d", "missing", "u1", "org-default")
                .unwrap_err(),
            ToolAccessError::Unavailable
        );
    }

    #[test]
    fn denial_texts_do_not_reveal_whether_an_addon_exists_in_another_organization() {
        assert_eq!(
            ToolAccessError::OtherOrganization.to_string(),
            ToolAccessError::Unavailable.to_string()
        );
    }

    #[test]
    fn gate_counts_calls_per_addon_and_releases_on_drop() {
        let gate = Arc::new(ToolCallGate::default());
        gate.set_limit(2);
        let first = gate.try_acquire("a").unwrap();
        let _second = gate.try_acquire("a").unwrap();
        assert!(gate.try_acquire("a").is_none());
        assert!(gate.try_acquire("b").is_some(), "the cap is per addon");
        drop(first);
        assert_eq!(gate.in_flight("a"), 1);
        assert!(gate.try_acquire("a").is_some());
    }
}
