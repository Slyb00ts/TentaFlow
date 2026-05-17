// ============ File: services/rbac/mod.rs — F2 P1.b RBAC middleware + permission matrix ============
//
// Layered on top of P1.a's `services::org` repository. The permission matrix
// caches per-(user, org) permission sets so the hot read path does not hit
// SQLite on every host-fn / dispatch call. `OrgContext` is the per-request
// snapshot threaded through dispatch handlers and (later) HTTP layers.
//
// Cache invalidation is driven by membership / role mutations in
// `services::org::repo::{add_membership, remove_membership}` — those call
// `PermissionMatrix::global().invalidate(...)` directly so callers (CLI,
// dashboard admin, future bulk import) cannot accidentally leave a stale
// entry behind.

pub mod middleware;
pub mod permissions;

pub use middleware::{resolve_org_context, OrgContext, OrgContextError};
pub use permissions::{PermissionDecision, PermissionError, PermissionMatrix};
