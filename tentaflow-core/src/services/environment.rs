// =============================================================================
// File: services/environment.rs — node environment identity (ROADMAP Z12)
// =============================================================================
//
// A node's environment (Dev/Test/Prod) is an identity attribute, not a
// separate instance — one mesh, but direct sync only ever happens between
// nodes declaring the SAME environment (enforced independently in the sync
// envelope + ledger admission `sync::ledger`, the pairing handshake
// `net::iroh::pairing`, and the alias resolver
// `services::runtime::resolver`). This module is the single place that reads
// and writes the two `settings` keys that carry that identity — every other
// call site goes through here rather than hard-coding the key strings.
//
// Not to be confused with Project Studio's `environments.rs` (test-runner
// target environments, `env_type ∈ {web, api}`) — a completely different,
// unrelated concept that happens to share the English word.
// =============================================================================

use anyhow::Result;

use tentaflow_protocol::environment::NodeEnvironment;

use crate::db::{repository, DbPool};

/// `settings` key holding the node's declared environment (`dev`/`test`/
/// `prod`). Generic key-value table — no migration needed to add it. Missing
/// key = `NodeEnvironment::default()` (Prod), the conservative choice for a
/// pre-Z12 installation.
pub const NODE_ENVIRONMENT_SETTING_KEY: &str = "node_environment";

/// `settings` key holding the strict cross-environment pairing isolation flag
/// (`"1"` = strict, anything else/absent = off). `strict` makes the pairing
/// handshake reject a peer declaring a different environment BEFORE PIN
/// validation; without it, pairing across environments is still allowed (sync
/// stays fenced regardless), but the UI shows a warning.
pub const ENVIRONMENT_ISOLATION_STRICT_SETTING_KEY: &str = "environment_isolation";

/// Reads the local node's declared environment from `settings`, defaulting to
/// `Prod` when the key is absent or holds an unrecognized value — fail closed
/// toward the tightest routing/sync boundary rather than guessing.
pub fn get_node_environment(pool: &DbPool) -> NodeEnvironment {
    repository::get_setting(pool, NODE_ENVIRONMENT_SETTING_KEY)
        .ok()
        .flatten()
        .and_then(|v| NodeEnvironment::parse(&v))
        .unwrap_or_default()
}

/// Persists the node's declared environment. Callers are responsible for the
/// accompanying ledger wipe+reseed (`sync::runtime::switch_node_environment`)
/// — this function only updates the `settings` row.
pub fn set_node_environment(pool: &DbPool, environment: NodeEnvironment) -> Result<()> {
    repository::set_setting(pool, NODE_ENVIRONMENT_SETTING_KEY, environment.as_str())
}

/// Whether strict cross-environment pairing isolation is enabled.
pub fn is_isolation_strict(pool: &DbPool) -> bool {
    matches!(
        repository::get_setting(pool, ENVIRONMENT_ISOLATION_STRICT_SETTING_KEY).ok().flatten(),
        Some(v) if v == "1"
    )
}

/// Persists the strict-isolation flag.
pub fn set_isolation_strict(pool: &DbPool, strict: bool) -> Result<()> {
    repository::set_setting(
        pool,
        ENVIRONMENT_ISOLATION_STRICT_SETTING_KEY,
        if strict { "1" } else { "0" },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn test_pool() -> DbPool {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::migrations::run(&conn).unwrap();
        Arc::new(crate::db::Db::from_connection(conn))
    }

    #[test]
    fn missing_key_defaults_to_prod() {
        let pool = test_pool();
        assert_eq!(get_node_environment(&pool), NodeEnvironment::Prod);
    }

    #[test]
    fn set_then_get_roundtrips() {
        let pool = test_pool();
        set_node_environment(&pool, NodeEnvironment::Test).expect("set");
        assert_eq!(get_node_environment(&pool), NodeEnvironment::Test);
    }

    #[test]
    fn strict_isolation_defaults_off() {
        let pool = test_pool();
        assert!(!is_isolation_strict(&pool));
        set_isolation_strict(&pool, true).expect("set");
        assert!(is_isolation_strict(&pool));
        set_isolation_strict(&pool, false).expect("set");
        assert!(!is_isolation_strict(&pool));
    }

    #[test]
    fn unrecognized_stored_value_defaults_to_prod() {
        let pool = test_pool();
        repository::set_setting(&pool, NODE_ENVIRONMENT_SETTING_KEY, "staging").expect("set raw");
        assert_eq!(get_node_environment(&pool), NodeEnvironment::Prod);
    }
}
