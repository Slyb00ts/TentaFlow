//! Organizational structure: units, positions, reporting lines and who holds
//! which position, with effective dating (docs/ORG_STRUCTURE_PLAN.md §1).
//!
//! `validate` holds the pure rules, `repo` the database layer that enforces
//! them inside the write transaction, `replication` the sync plumbing shared by
//! all eleven tables. `availability`, `escalation` and `privacy` answer who is
//! present, who is asked when somebody is not, and who may see what (WP9).

pub mod availability;
pub mod batch;
pub mod change_set;
pub mod error;
pub mod escalation;
pub mod handover;
pub mod history;
pub mod import;
pub mod nightly;
pub mod privacy;
pub mod projection;
pub mod query;
pub mod replication;
pub mod repo;
pub mod types;
pub mod validate;

pub use error::{OrgStructureError, Result};
pub use repo::*;
pub use types::*;

#[cfg(test)]
mod batch_tests;
#[cfg(test)]
mod change_set_tests;
#[cfg(test)]
mod cover_tests;
#[cfg(test)]
mod history_tests;
#[cfg(test)]
mod query_tests;
#[cfg(test)]
mod tests;
