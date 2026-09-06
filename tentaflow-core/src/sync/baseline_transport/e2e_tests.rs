// =============================================================================
// Plik: sync/baseline_transport/e2e_tests.rs
// Opis: End-to-end (in-process) tests for the full sync-identity redesign:
//       two INDEPENDENTLY installed nodes (each with its own DbPool + UUID
//       primary keys) pair over the fake DuplexFrameStream, the joiner adopts
//       the donor baseline, and both converge into ONE logical organization
//       with no UNIQUE collisions. These exercise the donor/joiner state
//       machine over a real (in-memory) stream, the same path production uses
//       over iroh, and assert the brief's payoff: "two nodes, each with a flow
//       version 2 -> after adopt no UNIQUE error".
// =============================================================================

use super::*;
use crate::crypto::SettingsCipher;
use crate::db::models::FlowParams;
use crate::db::{self, repository, DbPool};
use crate::mesh::security::MeshSecurity;
use crate::sync::core_baseline::{load_adopt_state, BaselinePhase, BaselineRole};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};

// =============================================================================
// Fake stream (in-memory duplex) — same wire format as iroh, no sockets.
// =============================================================================

struct DuplexFrameStream {
    inner: DuplexStream,
}

impl DuplexFrameStream {
    fn pair() -> (Self, Self) {
        let (a, b) = tokio::io::duplex(1024 * 1024);
        (Self { inner: a }, Self { inner: b })
    }
}

#[async_trait]
impl FrameStream for DuplexFrameStream {
    async fn read_raw(&mut self, label: &str) -> LedgerResult<Vec<u8>> {
        let mut len_buf = [0u8; 4];
        self.inner
            .read_exact(&mut len_buf)
            .await
            .map_err(|e| transport_err(label, format!("read len: {e}")))?;
        let len = u32::from_be_bytes(len_buf) as usize;
        if len > MAX_BASELINE_FRAME_BYTES {
            return Err(transport_err(
                label,
                format!("frame too large: {len} bytes"),
            ));
        }
        let mut body = vec![0u8; len];
        self.inner
            .read_exact(&mut body)
            .await
            .map_err(|e| transport_err(label, format!("read body: {e}")))?;
        Ok(body)
    }

    async fn write_raw(&mut self, body: &[u8], label: &str) -> LedgerResult<()> {
        self.inner
            .write_all(&(body.len() as u32).to_be_bytes())
            .await
            .map_err(|e| transport_err(label, format!("write len: {e}")))?;
        self.inner
            .write_all(body)
            .await
            .map_err(|e| transport_err(label, format!("write body: {e}")))?;
        Ok(())
    }

    async fn finish(&mut self) -> LedgerResult<()> {
        self.inner
            .shutdown()
            .await
            .map_err(|e| transport_err("finish", format!("{e}")))?;
        Ok(())
    }
}

// =============================================================================
// Fixtures — each node boots from a fresh migrated DB, then keeps only ONE
// organization (a real single-org node before its first pairing).
// =============================================================================

fn test_cipher() -> Arc<SettingsCipher> {
    Arc::new(SettingsCipher::new(&[7u8; 32]))
}

/// The single org id every node ships with: migrations seed it, and the core
/// capture journal is FK-bound to it, so the real `create_*` helpers (which emit
/// captures) require it to exist. Two independently-installed nodes therefore
/// share this org id by construction — the redesign's claim is that UUID
/// resource ids stay collision-free EVEN THOUGH the org id matches.
const NODE_ORG_ID: &str = "org-default";

/// Freshly migrated pool reduced to a single controlled organization
/// (`org-default`, kept so the core capture journal FK holds). Everything else
/// (users, flows, groups, extra orgs, trust) is wiped so the test author owns
/// the exact starting state of each independently-installed node.
fn fresh_node_pool() -> DbPool {
    let pool = db::init(std::path::Path::new(":memory:")).expect("init test DB");
    {
        let conn = pool.write().unwrap();
        for table in [
            "node_user_assignments",
            "user_identity_keys",
            "sync_explicit_shares",
            "org_memberships",
            "sync_user_org_profiles",
            "sync_resource_acl",
            "sync_policies",
            "group_members",
            "flow_model_bindings",
            "flows",
            "user_groups",
            "sync_nodes",
            "user_accounts",
            "roles",
        ] {
            conn.execute(&format!("DELETE FROM {table}"), []).unwrap();
        }
        // Reduce to exactly one active org so `primary_donor_org` accepts the
        // snapshot. Keep `org-default` (the capture-journal FK target).
        conn.execute(
            "DELETE FROM organizations WHERE org_id <> ?1",
            rusqlite::params![NODE_ORG_ID],
        )
        .unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO organizations (org_id, name, slug, status, created_at) \
             VALUES (?1, 'Node Org', 'node', 'active', strftime('%Y-%m-%dT%H:%M:%SZ','now'))",
            rusqlite::params![NODE_ORG_ID],
        )
        .unwrap();
        // One non-privileged role so joiner users adopted into the donor org can
        // be granted a safe membership during the merge.
        conn.execute(
            "INSERT OR REPLACE INTO roles (role_id, name, permissions_json, created_at) \
             VALUES ('role-user', 'user', '[]', strftime('%Y-%m-%dT%H:%M:%SZ','now'))",
            [],
        )
        .unwrap();
    }
    pool
}

/// Inserts `peer_node_id` straight into `trusted_nodes` so the donor's
/// `MeshSecurity::is_trusted` returns true without firing policy side-effects
/// that depend on a default org wiped by `fresh_node_pool`.
fn insert_trusted_node(pool: &DbPool, node_id: &str, public_key_hex: &str) {
    let conn = pool.write().unwrap();
    conn.execute(
        "INSERT OR REPLACE INTO trusted_nodes \
            (node_id, public_key, hostname, approved_by, approved_at, is_active) \
         VALUES (?1, ?2, 'peer-host', 'test', datetime('now'), 1)",
        rusqlite::params![node_id, public_key_hex],
    )
    .unwrap();
}

/// Real Ed25519 identity; returns `(node_id_hex, public_key_hex)` where
/// `public_key_hex` is the 128-hex `ed25519 || x25519-placeholder` form the
/// trust store expects. The first 64 hex equal the node_id.
fn gen_identity() -> (String, String) {
    let signing = ed25519_dalek::SigningKey::generate(&mut rand_core_06::OsRng);
    let node_id = hex::encode(signing.verifying_key().as_bytes());
    let public_key_hex = format!("{node_id}{}", "00".repeat(32));
    (node_id, public_key_hex)
}

/// Builds the donor `MeshSecurity` from `donor_pool` while re-rolling its
/// Ed25519 identity until it sorts strictly below `joiner_node_id`. The baseline
/// election makes the lexicographically-lower node the donor, so this guarantees
/// the donor pool really plays the donor role without depending on luck. Returns
/// `(security, donor_node_id)`.
fn donor_security_below(
    donor_pool: DbPool,
    joiner_node_id: &str,
    joiner_pubkey: &str,
) -> (Arc<MeshSecurity>, String) {
    loop {
        // `MeshSecurity::new` persists the minted identity into `settings` and
        // reuses it on subsequent calls. To re-roll a different node_id we must
        // clear the stored keys first, otherwise this would loop forever once the
        // first identity sorts above the joiner.
        let probe = MeshSecurity::new(donor_pool.clone(), test_cipher()).expect("probe security");
        let donor_node_id = probe.ed25519_public_key_hex();
        drop(probe);
        if donor_node_id >= joiner_node_id.to_string() {
            let conn = donor_pool.write().unwrap();
            conn.execute(
                "DELETE FROM settings WHERE key IN ('node_private_key', 'node_x25519_private_key')",
                [],
            )
            .unwrap();
            continue;
        }
        insert_trusted_node(&donor_pool, joiner_node_id, joiner_pubkey);
        // Real pairing always stamps the peer's declared environment
        // (`MeshSecurity::confirm_pairing`, P1-2); `run_donor_session`'s
        // environment fence (N4) fails closed on an unstamped peer, so this
        // e2e fixture — which exercises the happy path, not the fence —
        // must stamp it explicitly, same as `prod`-default `donor_security`.
        repository::set_trusted_node_environment(
            &donor_pool,
            joiner_node_id,
            tentaflow_protocol::environment::NodeEnvironment::Prod,
        )
        .unwrap();
        let security = MeshSecurity::new(donor_pool, test_cipher()).expect("donor security");
        let donor_node_id = security.ed25519_public_key_hex();
        assert!(donor_node_id < joiner_node_id.to_string());
        return (Arc::new(security), donor_node_id);
    }
}

/// Runs the full donor<->joiner baseline-adopt over the fake stream and returns
/// the joiner's import report. Both sessions run concurrently, exactly as the
/// two endpoints would over iroh.
async fn run_pairing(
    security: Arc<MeshSecurity>,
    donor_node_id: String,
    joiner_pool: DbPool,
    joiner_node_id: String,
) -> BaselineImportReport {
    let (mut joiner_stream, mut donor_stream) = DuplexFrameStream::pair();
    let cipher = test_cipher();

    let donor_task = {
        let donor_node_id = donor_node_id.clone();
        let joiner_node_id = joiner_node_id.clone();
        tokio::spawn(async move {
            run_donor_session(
                &mut donor_stream,
                &security,
                &donor_node_id,
                &joiner_node_id,
            )
            .await
        })
    };
    let joiner_task = tokio::spawn(async move {
        run_joiner_session(
            &mut joiner_stream,
            &joiner_pool,
            &joiner_node_id,
            &donor_node_id,
            &cipher,
            0,
        )
        .await
    });

    donor_task
        .await
        .expect("donor join")
        .expect("donor session ok");
    joiner_task
        .await
        .expect("joiner join")
        .expect("joiner session ok")
}

fn count(pool: &DbPool, sql: &str) -> i64 {
    let conn = pool.read().unwrap();
    conn.query_row(sql, [], |r| r.get(0)).unwrap()
}

fn exists(pool: &DbPool, sql: &str) -> bool {
    let conn = pool.read().unwrap();
    conn.query_row(sql, [], |r| r.get(0)).unwrap()
}

fn flow_params<'a>(name: &'a str, model: Option<&'a str>) -> FlowParams<'a> {
    FlowParams {
        name,
        description: None,
        is_default: false,
        service_type: None,
        flow_json: "{\"nodes\":[]}",
        status: "active",
        published_model_name: model,
        actor_user_id: None,
    }
}

// =============================================================================
// Scenario 1: independent bootstrap — colliding "first" resources get distinct
// UUIDs, NOT a colliding autoincrement id.
// =============================================================================

#[tokio::test]
async fn e2e_independent_bootstrap_first_resources_get_distinct_uuids() {
    let node_a = fresh_node_pool();
    let node_b = fresh_node_pool();

    // Each node creates its very FIRST flow, user and group. Under the old
    // autoincrement scheme both would be id=1 and collide on pairing. Under the
    // UUID scheme (phase B) every id is globally unique on creation.
    let flow_a = repository::create_flow(&node_a, &flow_params("First Flow", None)).unwrap();
    let flow_b = repository::create_flow(&node_b, &flow_params("First Flow", None)).unwrap();

    let user_a =
        repository::create_user_account(&node_a, "alice", "h", "Alice", "alice@a.io").unwrap();
    let user_b = repository::create_user_account(&node_b, "bob", "h", "Bob", "bob@b.io").unwrap();

    let group_a = repository::create_group(&node_a, "Team", "first group").unwrap();
    let group_b = repository::create_group(&node_b, "Team", "first group").unwrap();

    // The "first" resources are NOT id=1 — they are UUIDs, and they differ
    // across the two independent installs.
    assert_ne!(
        flow_a, flow_b,
        "first-flow ids must not collide across nodes"
    );
    assert_ne!(
        user_a, user_b,
        "first-user ids must not collide across nodes"
    );
    assert_ne!(
        group_a, group_b,
        "first-group ids must not collide across nodes"
    );
    for id in [&flow_a, &flow_b, &user_a, &user_b, &group_a, &group_b] {
        assert_ne!(id.as_str(), "1", "ids must be UUIDs, not autoincrement");
        assert_eq!(id.len(), 36, "uuid v4 string length");
    }
}

// =============================================================================
// Scenario 2: pairing + baseline adopt end-to-end — two independent nodes merge
// into the donor's single logical organization with no duplicated system roles.
// =============================================================================

#[tokio::test]
async fn e2e_pairing_adopt_merges_into_single_org_no_role_duplication() {
    let (joiner_node_id, joiner_pubkey) = gen_identity();

    let donor_pool = fresh_node_pool();
    let joiner_pool = fresh_node_pool();

    // System role shared by exact id on both nodes (deterministic seed). After
    // adopt it must NOT be duplicated — same role_id collapses on upsert.
    {
        let conn = donor_pool.write().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO roles (role_id, name, permissions_json, created_at) \
             VALUES ('role-admin', 'admin', '[\"org.read\"]', strftime('%Y-%m-%dT%H:%M:%SZ','now'))",
            [],
        )
        .unwrap();
    }
    {
        let conn = joiner_pool.write().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO roles (role_id, name, permissions_json, created_at) \
             VALUES ('role-admin', 'admin', '[\"org.read\"]', strftime('%Y-%m-%dT%H:%M:%SZ','now'))",
            [],
        )
        .unwrap();
    }

    // Donor user-created account (must reach the joiner) and joiner user-created
    // account with a DIFFERENT email (must survive as a member of the donor org).
    let donor_user = repository::create_user_account(
        &donor_pool,
        "donor_alice",
        "h",
        "Donor Alice",
        "alice@donor.io",
    )
    .unwrap();
    {
        let conn = donor_pool.write().unwrap();
        conn.execute(
            "INSERT INTO org_memberships (org_id, user_id, role_id, granted_at, granted_by) \
             VALUES (?1, ?2, 'role-user', strftime('%Y-%m-%dT%H:%M:%SZ','now'), 'seed')",
            rusqlite::params![NODE_ORG_ID, donor_user],
        )
        .unwrap();
    }
    let joiner_user = repository::create_user_account(
        &joiner_pool,
        "joiner_bob",
        "h",
        "Joiner Bob",
        "bob@joiner.io",
    )
    .unwrap();
    {
        let conn = joiner_pool.write().unwrap();
        conn.execute(
            "INSERT INTO org_memberships (org_id, user_id, role_id, granted_at, granted_by) \
             VALUES (?1, ?2, 'role-user', strftime('%Y-%m-%dT%H:%M:%SZ','now'), 'seed')",
            rusqlite::params![NODE_ORG_ID, joiner_user],
        )
        .unwrap();
    }

    let (security, donor_node_id) =
        donor_security_below(donor_pool.clone(), &joiner_node_id, &joiner_pubkey);
    let donor_epoch_before = crate::sync::runtime::core_epoch();

    let report = run_pairing(
        security.clone(),
        donor_node_id.clone(),
        joiner_pool.clone(),
        joiner_node_id.clone(),
    )
    .await;
    assert_eq!(report.donor_org_id, NODE_ORG_ID);

    // After adopt there is exactly ONE logical organization, and every
    // membership lives in it — donor and joiner have merged.
    assert_eq!(
        count(
            &joiner_pool,
            "SELECT COUNT(*) FROM organizations WHERE status <> 'deleted'"
        ),
        1,
        "exactly one active org after adopt"
    );
    assert_eq!(
        count(
            &joiner_pool,
            &format!("SELECT COUNT(*) FROM org_memberships WHERE org_id <> '{NODE_ORG_ID}'")
        ),
        0,
        "all memberships belong to the single merged org"
    );

    // System role NOT duplicated — exactly one row keyed by the shared id.
    assert_eq!(
        count(
            &joiner_pool,
            "SELECT COUNT(*) FROM roles WHERE role_id = 'role-admin'"
        ),
        1,
        "shared system role collapses to a single row"
    );

    // Donor's user-created account reached the joiner.
    assert!(
        exists(
            &joiner_pool,
            &format!("SELECT EXISTS(SELECT 1 FROM user_accounts WHERE id = '{donor_user}')")
        ),
        "donor user-created account present on joiner"
    );

    // Joiner's own user-created account survived and is now a member of the donor org.
    assert!(
        exists(
            &joiner_pool,
            &format!("SELECT EXISTS(SELECT 1 FROM user_accounts WHERE id = '{joiner_user}')")
        ),
        "joiner user-created account survived adopt"
    );
    let joiner_user_org: String = {
        let conn = joiner_pool.read().unwrap();
        conn.query_row(
            "SELECT org_id FROM org_memberships WHERE user_id = ?1",
            rusqlite::params![joiner_user],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert_eq!(
        joiner_user_org, NODE_ORG_ID,
        "joiner user is in the merged org"
    );

    // Epoch parity: after adopt both report the same baseline epoch counter.
    // (In-process there is no runtime ledger, so the adopt epoch is the donor's
    // current core_epoch the donor session captured.)
    let donor_state = load_adopt_state(&security.db)
        .unwrap()
        .expect("donor state");
    let joiner_state = load_adopt_state(&joiner_pool)
        .unwrap()
        .expect("joiner state");
    assert_eq!(donor_state.role, BaselineRole::Donor);
    assert_eq!(joiner_state.role, BaselineRole::Joiner);
    assert_eq!(donor_state.phase, BaselinePhase::Completed);
    assert_eq!(joiner_state.phase, BaselinePhase::Completed);
    assert_eq!(
        joiner_state.epoch.counter, donor_epoch_before.counter,
        "joiner adopted the donor's epoch counter"
    );
}

// =============================================================================
// Scenario 3 (brief core): both nodes hold a flow with the SAME name AND
// published_model_name ("version 2" locally). After pairing+adopt there is NO
// `UNIQUE constraint failed`; BOTH flows survive under distinct UUIDs; donor
// keeps the published name, joiner's flow is unpublished by the collision policy.
// =============================================================================

#[tokio::test]
async fn e2e_both_nodes_published_version2_flow_no_unique_error_after_adopt() {
    let (joiner_node_id, joiner_pubkey) = gen_identity();

    let donor_pool = fresh_node_pool();
    let joiner_pool = fresh_node_pool();

    // Each node independently published a flow as "version 2" advertising the
    // same model name — the exact symptom from the original bug report.
    let donor_flow =
        repository::create_flow(&donor_pool, &flow_params("version 2", Some("shared/model")))
            .unwrap();
    let joiner_flow = repository::create_flow(
        &joiner_pool,
        &flow_params("version 2", Some("shared/model")),
    )
    .unwrap();
    assert_ne!(donor_flow, joiner_flow, "distinct UUIDs on each node");

    let (security, donor_node_id) =
        donor_security_below(donor_pool.clone(), &joiner_node_id, &joiner_pubkey);

    // The whole point: the adopt import runs to completion WITHOUT a UNIQUE error
    // on flows.published_model_name. `run_pairing` panics if either session errs.
    let report = run_pairing(security, donor_node_id, joiner_pool.clone(), joiner_node_id).await;
    assert_eq!(report.donor_org_id, NODE_ORG_ID);

    // Both flows kept under their own UUIDs.
    assert_eq!(
        count(
            &joiner_pool,
            "SELECT COUNT(*) FROM flows WHERE name = 'version 2'"
        ),
        2,
        "both 'version 2' flows survive after adopt"
    );
    assert!(exists(
        &joiner_pool,
        &format!("SELECT EXISTS(SELECT 1 FROM flows WHERE id = '{donor_flow}')")
    ));
    assert!(exists(
        &joiner_pool,
        &format!("SELECT EXISTS(SELECT 1 FROM flows WHERE id = '{joiner_flow}')")
    ));

    // Collision policy: donor keeps the published model name, joiner is unpublished.
    let donor_model: Option<String> = {
        let conn = joiner_pool.read().unwrap();
        conn.query_row(
            "SELECT published_model_name FROM flows WHERE id = ?1",
            rusqlite::params![donor_flow],
            |r| r.get(0),
        )
        .unwrap()
    };
    let joiner_model: Option<String> = {
        let conn = joiner_pool.read().unwrap();
        conn.query_row(
            "SELECT published_model_name FROM flows WHERE id = ?1",
            rusqlite::params![joiner_flow],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert_eq!(
        donor_model.as_deref(),
        Some("shared/model"),
        "donor stays published"
    );
    assert_eq!(
        joiner_model, None,
        "joiner flow unpublished on model-name collision"
    );

    // No published_model_name appears twice — the UNIQUE invariant holds post-merge.
    assert_eq!(
        count(
            &joiner_pool,
            "SELECT COUNT(*) FROM flows WHERE published_model_name = 'shared/model'"
        ),
        1,
        "exactly one flow keeps the published model name"
    );
}
