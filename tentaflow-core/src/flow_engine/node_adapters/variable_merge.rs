// ===== File: flow_engine/node_adapters/variable_merge.rs — shared per-key
// variable merge policy (§3.12). `combine` merges variables across fan-in
// branches and `map` merges variables across element bodies; both need the SAME
// deterministic semantics: a conflicting value for one key on two sources is a
// node error unless the node declares a `variable_merge_policy`. Factored here
// so the two blocks cannot drift apart. Sources are merged in the order the
// caller supplies (combine sorts branches by from_node_id; map uses element
// input index), so `last_wins` / `collect` are deterministic. =====

use std::collections::BTreeMap;

use anyhow::{anyhow, Result};
use serde_json::Value;

use crate::flow_engine::envelope::FlowValue;
use crate::flow_engine::types::FlowNode;

/// Per-key conflict policy parsed from `node.config.variable_merge_policy`.
/// Default (no entry) = a different value for the same key on two sources is a
/// node error — deterministic and debuggable.
pub enum MergePolicy {
    /// Last source (in caller order) wins on conflict.
    LastWins,
    /// The source whose port label equals the configured port wins. Only
    /// meaningful for `combine` (branches carry a `from_port`); `map` element
    /// sources have no port, so a preferred port simply never matches and the
    /// hold-until-preferred fallback keeps the first value.
    PreferPort(String),
    /// Collect all source values into a JSON array (caller order).
    Collect,
}

/// One merge source: its variables plus an optional port label used by the
/// `prefer_port` policy. `combine` passes the branch `from_port`; `map` passes
/// `None`.
pub struct MergeSource<'a> {
    pub port: Option<&'a str>,
    pub variables: &'a BTreeMap<String, FlowValue>,
}

/// Parses `node.config.variable_merge_policy` ({key: policy-string}) into the
/// per-key policy map. `"last_wins"` | `"collect"` | `"prefer_port:<port>"`.
pub fn parse_policies(node: &FlowNode) -> Result<BTreeMap<String, MergePolicy>> {
    let Some(obj) = node
        .config
        .get("variable_merge_policy")
        .and_then(|v| v.as_object())
    else {
        return Ok(BTreeMap::new());
    };
    let mut policies = BTreeMap::new();
    for (key, spec) in obj {
        let spec = spec.as_str().ok_or_else(|| {
            anyhow!(
                "node '{}': variable_merge_policy['{key}'] must be a string",
                node.id
            )
        })?;
        let policy = if spec == "last_wins" {
            MergePolicy::LastWins
        } else if spec == "collect" {
            MergePolicy::Collect
        } else if let Some(port) = spec.strip_prefix("prefer_port:") {
            if port.is_empty() {
                return Err(anyhow!(
                    "node '{}': prefer_port policy for '{key}' needs a port name",
                    node.id
                ));
            }
            MergePolicy::PreferPort(port.to_string())
        } else {
            return Err(anyhow!(
                "node '{}': unknown merge policy '{spec}' for variable '{key}' \
                 (expected last_wins, collect or prefer_port:<port>)",
                node.id
            ));
        };
        policies.insert(key.clone(), policy);
    }
    Ok(policies)
}

/// Merges `variables` from every source using the per-key policy. Sources are
/// consumed in the order given, so `last_wins` / `collect` are deterministic.
/// `what` names the node for error messages (e.g. "combine node 'c1'").
pub fn merge_ordered(
    node: &FlowNode,
    what: &str,
    sources: &[MergeSource<'_>],
) -> Result<BTreeMap<String, FlowValue>> {
    let policies = parse_policies(node)?;
    let mut merged: BTreeMap<String, FlowValue> = BTreeMap::new();
    // For `collect` we accumulate arrays; for `prefer_port` we track whether the
    // winning port already supplied a value so a later source cannot override it.
    let mut collected: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    let mut prefer_locked: BTreeMap<String, bool> = BTreeMap::new();

    for source in sources {
        for (key, value) in source.variables {
            match policies.get(key) {
                Some(MergePolicy::Collect) => {
                    collected
                        .entry(key.clone())
                        .or_default()
                        .push(crate::flow_engine::expr::flow_value_to_json(value));
                }
                Some(MergePolicy::PreferPort(port)) => {
                    let locked = prefer_locked.entry(key.clone()).or_insert(false);
                    if source.port == Some(port.as_str()) {
                        merged.insert(key.clone(), value.clone());
                        *locked = true;
                    } else if !*locked && !merged.contains_key(key) {
                        // Hold a non-preferred value only until the preferred
                        // port supplies one; the preferred port always wins.
                        merged.insert(key.clone(), value.clone());
                    }
                }
                Some(MergePolicy::LastWins) => {
                    merged.insert(key.clone(), value.clone());
                }
                None => match merged.get(key) {
                    Some(existing) if existing != value => {
                        return Err(anyhow!(
                            "{what}: conflicting values for variable '{key}' across sources \
                             and no merge policy configured \
                             (set variable_merge_policy['{key}'] to last_wins, \
                             prefer_port:<port> or collect)"
                        ));
                    }
                    Some(_) => {}
                    None => {
                        merged.insert(key.clone(), value.clone());
                    }
                },
            }
        }
    }

    for (key, values) in collected {
        merged.insert(key, FlowValue::Json(Value::Array(values)));
    }
    Ok(merged)
}
