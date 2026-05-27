// =============================================================================
// File: flow_runtime/mod.rs — DAG executor for addon-declared Flow templates
// =============================================================================
//
// Foundation (chunk A): types, registry, parser, cycle detection. The
// scheduler, operator implementations, and host functions (flow_invoke_v1 /
// flow_status_v1) are added in subsequent chunks (B/C/D). At install time
// `addon::lifecycle` calls `parser::load_from_addon_dir` for every declared
// `[[flow_template]]` and registers the `CompiledFlow` in the process-wide
// `FlowRegistry` singleton; uninstall drops every flow owned by the addon.

pub mod boot;
pub mod bounded_drop_oldest;
pub mod operators;
pub mod parser;
pub mod registry;
pub mod scheduler;
pub mod types;

#[cfg(test)]
mod tests;
