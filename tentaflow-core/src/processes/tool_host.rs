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
    #[error("addon runs in another organization")]
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
        if self.has_running_instance(addon_id) {
            // A boot-started instance has no organization and serves the default one.
            let instance_org = self
                .instance_org_id(addon_id)
                .unwrap_or_else(|| crate::services::org::DEFAULT_ORG_ID.to_owned());
            if instance_org != org_id {
                return Err(ToolAccessError::OtherOrganization);
            }
        }
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
        if !self.has_running_instance(addon_id) {
            self.start_addon(addon_id, Some(user_id.to_owned()), Some(org_id.to_owned()))?;
        }
        self.call_tool_process(addon_id, tool_name, params, user_id, request_id)
    }
}

/// Canonical JSON of the tool pin and its SHA-256. Mirrors the graph JSON/hash
/// pair a Flow pin stores, so the publication code treats both alike.
pub fn tool_pin(
    addon_id: &str,
    tool_name: &str,
    descriptor: &ProcessToolDescriptor,
) -> Result<(String, String)> {
    let schema = super::repository::canonical_json_value(&descriptor.input_schema);
    let digest = hex::encode(Sha256::digest(serde_json::to_vec(&schema)?));
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
                bundle_hash: "b".repeat(64),
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
