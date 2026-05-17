// =============================================================================
// File: flow_runtime/types.rs — DAG schema, compiled form, and compile errors
// =============================================================================
//
// Wire types deserialized from `*.flow.json` (`FlowDefinition`, `OperatorDef`,
// `EdgeDef`) plus the post-validation `CompiledFlow` returned by `parser::
// compile`. `OperatorType` is a closed enum: unknown values produce a typed
// `FlowCompileError::UnknownOperator` instead of being silently accepted.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Hard cap on operators per flow. Enforced at compile time so the scheduler
/// can size per-edge buffers and per-operator task vectors without unbounded
/// growth. 64 is a deliberately small ceiling — flows above this size point
/// at modeling issues, not legitimate complexity.
pub const MAX_OPERATORS_PER_FLOW: usize = 64;

/// Schema version this implementation accepts. A document with any other
/// version is rejected at parse time so a future v2 cannot be silently
/// mis-interpreted by a v1-only Core.
pub const SUPPORTED_SCHEMA_VERSION: u32 = 1;

/// Allowed port labels on edges leaving a `Branch` operator. Any other value
/// — or any port on a non-branch edge — is rejected by `parser::compile`.
pub const BRANCH_PORTS: &[&str] = &["true", "false", "error"];

/// Operator kind. Closed enum — additions require an explicit code change in
/// both `flow_runtime` and the scheduler.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum OperatorType {
    Source,
    Predict,
    Threshold,
    Branch,
    Aggregate,
    Sink,
}

/// One node in the DAG as declared by an addon. `params` is opaque to the
/// runtime at compile time — each operator implementation interprets its own
/// shape in chunk C.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperatorDef {
    pub id: String,
    #[serde(rename = "type")]
    pub op_type: OperatorType,
    #[serde(default = "default_params")]
    pub params: toml::Value,
}

fn default_params() -> toml::Value {
    toml::Value::Table(toml::value::Table::new())
}

/// Directed edge `from -> to`. `port` is `Some` only for edges leaving a
/// `Branch` operator and must then be one of `BRANCH_PORTS`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EdgeDef {
    pub from: String,
    pub to: String,
    #[serde(default)]
    pub port: Option<String>,
}

/// Raw flow document as found on disk. Validation happens in `parser::
/// compile`; this struct is intentionally a 1:1 mirror of the JSON shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlowDefinition {
    pub schema_version: u32,
    pub id: String,
    pub operators: Vec<OperatorDef>,
    pub edges: Vec<EdgeDef>,
    #[serde(default)]
    pub is_long_running: bool,
    /// 0 means "no per-invocation wall clock limit" (only meaningful when
    /// `is_long_running` is true). The scheduler in chunk B enforces the
    /// budget — at compile time it is carried verbatim.
    #[serde(default)]
    pub max_runtime_ms: u32,
}

/// Output of `parser::compile`: the original definition plus an adjacency
/// map keyed by operator id and a topological order. Stored behind `Arc` in
/// the registry so individual invocations can clone cheaply.
#[derive(Debug, Clone)]
pub struct CompiledFlow {
    pub def: FlowDefinition,
    pub topo_order: Vec<String>,
    pub adjacency: HashMap<String, Vec<String>>,
}

/// Compile-time errors. Each variant points to the failing item so the
/// install path can surface a precise diagnostic to the operator.
#[derive(Debug, thiserror::Error)]
pub enum FlowCompileError {
    #[error("unsupported schema_version: {found} (this Core accepts {})", SUPPORTED_SCHEMA_VERSION)]
    UnsupportedSchemaVersion { found: u32 },

    #[error("flow has no operators")]
    EmptyFlow,

    #[error("flow has {count} operators, exceeds limit of {}", MAX_OPERATORS_PER_FLOW)]
    TooManyOperators { count: usize },

    #[error("operator id '{0}' referenced but not declared")]
    UnknownOperator(String),

    #[error("duplicate operator id '{0}'")]
    DuplicateOperator(String),

    #[error("edge[{edge_idx}] references unknown operator '{op_id}'")]
    EdgeReferencesUnknownOperator { edge_idx: usize, op_id: String },

    #[error("edge[{edge_idx}] has port='{port}' but source operator '{op_id}' is not a Branch")]
    PortOnNonBranch {
        edge_idx: usize,
        op_id: String,
        port: String,
    },

    #[error("edge[{edge_idx}] has invalid port value '{port}' (allowed: true|false|error)")]
    InvalidPort { edge_idx: usize, port: String },

    #[error("cycle detected involving operators: {involved:?}")]
    Cycle { involved: Vec<String> },

    #[error("parse error: {0}")]
    Parse(String),

    #[error("path resolution failed: {0}")]
    Path(String),

    #[error("I/O error reading flow file: {0}")]
    Io(String),
}
