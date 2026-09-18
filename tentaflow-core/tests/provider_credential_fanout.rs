// =============================================================================
// File: tests/provider_credential_fanout.rs — an agent-account credential
//       travelling between real nodes
// =============================================================================
//
// Four independent installations in one process: each has its OWN SQLite
// database, its OWN `SettingsCipher` (so "re-encrypted for the receiving node"
// is measured, not assumed), its OWN mesh identity and its OWN ledger signer.
// Nothing is shared between them but the bytes the test hands over.
//
// Two channels carry the feature and both are exercised here as they run in
// production:
//
//   * the account ROW — a genuine `SyncOperation` minted from the writer's
//     capture journal, signed, serialised to CBOR, decoded on the receiver and
//     applied by `sync::core_materializer::apply_core_operation`;
//   * the credential MATERIAL — a genuine `ProviderCredentialsSync` frame built
//     by `mesh::provider_credentials::build_for_peer`, sealed for exactly one
//     peer, CBOR-encoded, decoded and ingested on the other side.
//
// What this test does NOT stand up, because other tests do: the iroh transport
// and the ledger's own admission (hash chain, signature verification, outbox
// targeting) — `tests/process_four_node_sync.rs` runs those against real child
// processes. What is proved here is the decision layer: who is offered a
// credential, who is refused, and what happens to a copy after a revocation.

use std::collections::BTreeMap;
use std::sync::Arc;

use ed25519_dalek::SigningKey;
use tentaflow_core::crypto::SettingsCipher;
use tentaflow_core::db::repository::{self as core_repository, SyncNodeProfileUpdate};
use tentaflow_core::db::DbPool;
use tentaflow_core::mesh::provider_credentials;
use tentaflow_core::mesh::security::MeshSecurity;
use tentaflow_core::provider_accounts::repository as store;
use tentaflow_core::provider_accounts::{
    AccountUpdate, CredentialMeta, CredentialWrite, NewAccount,
};
use tentaflow_core::sync::core_capture::drain_pending_core_captures_with;
use tentaflow_core::sync::core_materializer::apply_core_operation;
use tentaflow_core::sync::core_registry::{descriptor_for_table, CORE_SYNC_ADDON_ID};
use tentaflow_core::sync::ledger::{
    ActionType, BaselineEpoch, Ed25519OperationSigner, NewSyncOperation, NodeEnvironment,
    SyncOperation,
};
use tentaflow_core::sync::runtime::SqlWriteAction;
use tentaflow_protocol::mesh::ProviderCredentialsSyncPayload;

const ORG: &str = "default";
const ACCOUNT: &str = "acc-shared-codex";

/// One installation: its database, its key material and its mesh identity.
struct Node {
    name: &'static str,
    db: DbPool,
    security: Arc<MeshSecurity>,
    id: String,
    signer: Ed25519OperationSigner,
    next_seq: std::cell::Cell<u64>,
    _home: tempfile::TempDir,
}

impl Node {
    fn new(name: &'static str, cipher_key: u8) -> Self {
        let home = tempfile::tempdir().expect("node home");
        let db = tentaflow_core::db::init(&home.path().join("tentaflow.db")).expect("db");
        let cipher = Arc::new(SettingsCipher::new(&[cipher_key; 32]));
        let security = Arc::new(MeshSecurity::new(db.clone(), cipher).expect("mesh identity"));
        let id = security.ed25519_public_key_hex();
        core_repository::set_setting(&db, core_repository::LOCAL_NODE_ID_SETTING, &id)
            .expect("local node id");
        {
            let conn = db.write().expect("db");
            conn.execute(
                "INSERT OR IGNORE INTO sync_nodes (node_id, public_key, display_name) \
                 VALUES (?1, ?2, ?3)",
                rusqlite::params![id, security.public_key_hex(), name],
            )
            .expect("node row");
        }
        let signer =
            Ed25519OperationSigner::new(id.clone(), SigningKey::from_bytes(&[cipher_key; 32]))
                .expect("signer");
        Self {
            name,
            db,
            security,
            id,
            signer,
            next_seq: std::cell::Cell::new(1),
            _home: home,
        }
    }

    fn material(&self) -> Option<String> {
        store::credential_material(&self.db, self.security.settings_cipher(), ACCOUNT)
            .expect("credential read")
    }

    fn revision(&self) -> Option<i64> {
        store::credential_summary(&self.db, ACCOUNT)
            .expect("summary")
            .map(|row| row.revision)
    }

    fn status(&self) -> String {
        store::get_account(&self.db, ACCOUNT)
            .expect("account")
            .expect("row")
            .status
    }

    fn refusal_reasons(&self) -> Vec<String> {
        let conn = self.db.read().expect("db");
        let mut stmt = conn
            .prepare(
                "SELECT details FROM audit_log \
                 WHERE action = 'provider_account.credential_refused' ORDER BY id",
            )
            .expect("audit query");
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .expect("audit rows")
            .collect::<Result<Vec<_>, _>>()
            .expect("audit rows");
        rows.into_iter()
            .map(|details| {
                serde_json::from_str::<serde_json::Value>(&details).expect("details")["reason"]
                    .as_str()
                    .expect("reason")
                    .to_string()
            })
            .collect()
    }
}

/// Makes `peer` a trusted, operator-grade node in `local`'s registry — the state
/// first-contact pairing leaves behind, and the precondition for every frame in
/// this test.
fn trust(local: &Node, peer: &Node) {
    local
        .security
        .add_trusted_key(&peer.id, &peer.security.public_key_hex(), "peer", None)
        .expect("trust");
    core_repository::update_sync_node_profile(
        &local.db,
        &peer.id,
        &SyncNodeProfileUpdate {
            node_kind: None,
            operator: Some(true),
        },
        None,
    )
    .expect("profile");
}

/// The tables this test replicates. Everything else a paired fleet exchanges —
/// the node registry, trust, identity — is set up directly on each node here,
/// because how THOSE rows travel is not what is being measured and each carries
/// its own admission rules.
const REPLICATED_TABLES: &[&str] = &[
    "provider_accounts",
    "provider_account_grants",
    "agent_runtime_nodes",
    "agent_runtime_engines",
];

/// Drains everything `from` wrote into its capture journal, mints a real signed
/// ledger operation for each, and applies it on every node in `to` after a CBOR
/// round-trip — which is what the ledger does between two nodes.
fn replicate(from: &Node, to: &[&Node]) -> usize {
    drain_pending_core_captures_with(&from.db, 256, |capture| {
        if !REPLICATED_TABLES.contains(&capture.table_name.as_str()) {
            return Ok(Some(tentaflow_core::sync::ledger::OperationId::from_hash(
                [0u8; 32],
            )));
        }
        let descriptor = descriptor_for_table(&capture.table_name).expect("core sync descriptor");
        let mut changed_fields: BTreeMap<_, _> = capture.changed_fields.clone();
        changed_fields.insert(
            "capture_id".to_string(),
            tentaflow_core::sync::ledger::FieldValue::String(capture.capture_id.clone()),
        );
        let new_operation = NewSyncOperation {
            org_id: capture.org_id.clone(),
            partition_id: descriptor
                .partition_id(&capture.org_id, None, NodeEnvironment::default())
                .expect("partition"),
            addon_id: CORE_SYNC_ADDON_ID.to_string(),
            resource_type: capture.resource_type.clone(),
            resource_id: capture.resource_id.clone(),
            table_name: capture.table_name.clone(),
            primary_key: capture.primary_key.clone(),
            action: match capture.action {
                SqlWriteAction::Insert => ActionType::Insert,
                SqlWriteAction::Update => ActionType::Update,
                SqlWriteAction::Delete => ActionType::Delete,
            },
            changed_fields,
            before_hash: None,
            after_hash: None,
            actor_user_id: capture
                .actor_user_id
                .clone()
                .unwrap_or_else(|| "system".to_string()),
            actor_device_id: from.id.clone(),
            actor_node_id: from.id.clone(),
            hlc_timestamp: tentaflow_core::sync::ledger::HybridLogicalTimestamp {
                wall_time_ms: capture.hlc.wall_time_ms,
                logical: capture.hlc.logical,
                node_id: from.id.clone(),
            },
            epoch: BaselineEpoch::default(),
            environment: NodeEnvironment::default(),
            payload_hash: [0u8; 32],
            acl_snapshot_hash: [0u8; 32],
            policy_epoch: 0,
            encryption_info: None,
        };
        let seq = from.next_seq.get();
        from.next_seq.set(seq + 1);
        let operation =
            SyncOperation::from_new(new_operation, seq, None, &from.signer).expect("operation");
        let wire = tentaflow_core::mesh::cbor::encode(&operation).expect("encode operation");
        for target in to {
            let decoded: SyncOperation =
                tentaflow_core::mesh::cbor::decode(&wire).expect("decode operation");
            apply_core_operation(&target.db, &decoded).unwrap_or_else(|error| {
                panic!(
                    "{} could not apply {}: {error}",
                    target.name, capture.table_name
                )
            });
        }
        Ok(Some(operation.op_id))
    })
    .expect("replicate")
}

/// Hands `from`'s credentials to `to` the way the mesh does: one frame, sealed
/// for that peer alone, CBOR on the wire.
fn deliver_credentials(from: &Node, to: &Node) -> Vec<provider_credentials::Adopted> {
    let Some(payload) =
        provider_credentials::build_for_peer(&from.security, &to.id).expect("build frame")
    else {
        return Vec::new();
    };
    let wire = tentaflow_core::mesh::cbor::encode(&payload).expect("encode frame");
    let decoded: ProviderCredentialsSyncPayload =
        tentaflow_core::mesh::cbor::decode(&wire).expect("decode frame");
    provider_credentials::ingest(&to.security, &from.id, decoded).expect("ingest")
}

/// Waits for the reconcile the materializer starts when a revocation — or a
/// withdrawn runtime flag — lands, because that is the production path: nothing
/// in the product calls the purge by hand.
async fn wait_for_purge(node: &Node) {
    for _ in 0..100 {
        if node.material().is_none() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(40)).await;
    }
    panic!("{} kept a credential it may no longer hold", node.name);
}

/// The whole package in one run: a credential minted on the home node reaches
/// the node flagged for accounts and no other, a rotation observed on a
/// satellite gets back to the home node, a satellite offering a rotation to
/// another satellite is refused with an audit, and clearing the credential
/// removes it everywhere it was replicated.
#[tokio::test(flavor = "multi_thread")]
async fn a_credential_reaches_every_account_node_and_leaves_them_all_when_it_is_cleared() {
    let home = Node::new("home", 1);
    let satellite = Node::new("satellite", 2);
    let excluded = Node::new("excluded", 3);
    let other = Node::new("other-satellite", 4);
    let fleet = [&satellite, &excluded, &other];
    for peer in fleet {
        trust(&home, peer);
        trust(peer, &home);
    }
    trust(&satellite, &other);
    trust(&other, &satellite);

    // The administrator's fleet decision, made on the home node and replicated:
    // three nodes hold credentials, `excluded` does not.
    for node in [&home, &satellite, &other] {
        store::set_receives_accounts(&home.db, &node.id, true, Some("admin")).expect("flag");
    }
    store::create_account(
        &home.db,
        &NewAccount {
            account_id: ACCOUNT.to_string(),
            org_id: ORG.to_string(),
            engine_id: "codex".to_string(),
            display_name: "Shared codex".to_string(),
            scope: "global".to_string(),
            owner_user_id: None,
            credential_kind: "provider_login".to_string(),
            created_by: "admin".to_string(),
        },
    )
    .expect("account");
    // What a sign-in on this node produces: the credential and the home node.
    store::mint_credential(
        &home.db,
        home.security.settings_cipher(),
        ACCOUNT,
        "{\"tokens\":{\"account_id\":\"acct-7\",\"refresh_token\":\"first\"}}",
        &CredentialMeta {
            actor: Some("alice".to_string()),
            ..CredentialMeta::default()
        },
    )
    .expect("sign-in");
    assert!(replicate(&home, &[&satellite, &excluded, &other]) >= 2);
    assert_eq!(
        store::get_account(&satellite.db, ACCOUNT)
            .unwrap()
            .unwrap()
            .home_node_id,
        Some(home.id.clone()),
        "the node that signed in is the account's home everywhere"
    );

    // ---------------------------------------------------------------- fan-out
    let adopted = deliver_credentials(&home, &satellite);
    assert_eq!(adopted.len(), 1);
    assert_eq!(
        satellite.material().as_deref(),
        Some("{\"tokens\":{\"account_id\":\"acct-7\",\"refresh_token\":\"first\"}}"),
        "the satellite opens the credential with its own key"
    );
    assert_eq!(
        satellite.revision(),
        Some(1),
        "the CAS revision is carried over"
    );
    assert_eq!(satellite.status(), "active");

    assert!(
        provider_credentials::build_for_peer(&home.security, &excluded.id)
            .expect("build")
            .is_none(),
        "a node outside the account fleet is offered nothing"
    );
    assert_eq!(excluded.material(), None);
    // Even handed the satellite's frame, the excluded node refuses it outright.
    let frame_for_satellite = provider_credentials::build_for_peer(&home.security, &satellite.id)
        .expect("build")
        .expect("frame");
    assert!(
        provider_credentials::ingest(&excluded.security, &home.id, frame_for_satellite).is_err(),
        "a node that may not hold accounts refuses the frame"
    );
    assert_eq!(excluded.material(), None);

    // ------------------------------------------------- rotation on a satellite
    // The provider rotated the token under a session running on the satellite;
    // its store follows, and the new revision has to get back to the home node.
    assert_eq!(
        store::set_credential(
            &satellite.db,
            satellite.security.settings_cipher(),
            ACCOUNT,
            2,
            "{\"tokens\":{\"account_id\":\"acct-7\",\"refresh_token\":\"rotated\"}}",
            &CredentialMeta {
                refreshed_by_node: Some(satellite.id.clone()),
                ..CredentialMeta::default()
            },
        )
        .expect("rotation"),
        CredentialWrite::Applied { revision: 2 }
    );
    let submitted = deliver_credentials(&satellite, &home);
    assert_eq!(
        submitted.len(),
        1,
        "the home node takes its satellite's rotation"
    );
    assert_eq!(home.revision(), Some(2));
    assert_eq!(
        home.material().as_deref(),
        Some("{\"tokens\":{\"account_id\":\"acct-7\",\"refresh_token\":\"rotated\"}}")
    );

    // The same satellite may NOT hand that rotation to another satellite: it is
    // not the home node, and two satellites taking each other's material is how
    // a fleet starts overwriting one revision with another.
    assert!(
        provider_credentials::build_for_peer(&satellite.security, &other.id)
            .expect("build")
            .is_none()
    );
    let forged = ProviderCredentialsSyncPayload {
        entries: provider_credentials::build_for_peer(&satellite.security, &home.id)
            .expect("build")
            .expect("frame")
            .entries
            .into_iter()
            .map(|entry| {
                // Sealed for `other` this time: the seal is not what refuses it,
                // the home-node rule is.
                let sealed = satellite
                    .security
                    .seal_for_peer(
                        &other.id,
                        format!(
                            "provider-credential|{}|{}|{}",
                            entry.account_id, entry.revision, entry.material_sha256
                        )
                        .as_bytes(),
                        "{\"tokens\":{\"account_id\":\"acct-7\",\"refresh_token\":\"rotated\"}}"
                            .as_bytes(),
                    )
                    .expect("seal");
                tentaflow_protocol::mesh::ProviderCredentialEntry { sealed, ..entry }
            })
            .collect(),
    };
    let refused = provider_credentials::ingest(&other.security, &satellite.id, forged)
        .expect("a refused entry is not a broken frame");
    assert!(refused.is_empty());
    assert_eq!(other.material(), None);
    assert_eq!(other.refusal_reasons(), vec!["not_home_node"]);

    // The home node fans the rotation out to the other satellite itself.
    assert_eq!(deliver_credentials(&home, &other).len(), 1);
    assert_eq!(other.revision(), Some(2));

    // ------------------------------------------------------------- revocation
    assert!(store::clear_credential(&home.db, ACCOUNT, Some("alice")).expect("clear"));
    assert_eq!(home.material(), None);
    replicate(&home, &[&satellite, &excluded, &other]);
    for node in [&satellite, &other] {
        assert_eq!(
            node.status(),
            "needs_login",
            "{} must ask for a new sign-in",
            node.name
        );
        // The revocation mark on the account row is the whole signal: applying
        // it triggers the reconcile, and the copy leaves without anybody asking.
        wait_for_purge(node).await;
        assert!(
            store::node_state(&node.db, ACCOUNT, &node.id)
                .expect("node state")
                .is_none(),
            "{} must not keep claiming a revision it purged",
            node.name
        );
    }
    // And nothing offers the revoked revision again.
    assert!(
        provider_credentials::build_for_peer(&home.security, &satellite.id)
            .expect("build")
            .is_none()
    );
}

/// Taking a node out of the account fleet is a gate, not a label: the material
/// already on its disk goes at the next reconcile, and the node is offered
/// nothing afterwards.
#[tokio::test(flavor = "multi_thread")]
async fn a_node_removed_from_the_account_fleet_loses_the_credential_it_held() {
    let home = Node::new("home", 5);
    let satellite = Node::new("satellite", 6);
    trust(&home, &satellite);
    trust(&satellite, &home);
    for node in [&home, &satellite] {
        store::set_receives_accounts(&home.db, &node.id, true, Some("admin")).expect("flag");
    }
    store::create_account(
        &home.db,
        &NewAccount {
            account_id: ACCOUNT.to_string(),
            org_id: ORG.to_string(),
            engine_id: "codex".to_string(),
            display_name: "Shared codex".to_string(),
            scope: "global".to_string(),
            owner_user_id: None,
            credential_kind: "provider_login".to_string(),
            created_by: "admin".to_string(),
        },
    )
    .expect("account");
    store::update_account(
        &home.db,
        ACCOUNT,
        &AccountUpdate {
            home_node_id: Some(home.id.clone()),
            ..Default::default()
        },
        None,
    )
    .expect("home node");
    store::mint_credential(
        &home.db,
        home.security.settings_cipher(),
        ACCOUNT,
        "material",
        &CredentialMeta::default(),
    )
    .expect("credential");
    replicate(&home, &[&satellite]);
    assert_eq!(deliver_credentials(&home, &satellite).len(), 1);
    assert!(satellite.material().is_some());

    // The administrator takes the satellite out of the fleet on the HOME node;
    // the decision reaches the satellite on the ledger.
    store::set_receives_accounts(&home.db, &satellite.id, false, Some("admin")).expect("flag");
    replicate(&home, &[&satellite]);
    assert!(
        !store::receives_accounts(&satellite.db, &satellite.id).expect("flag"),
        "the withdrawal reached the satellite"
    );

    // Nobody asks: materializing the runtime-node row is what triggers the
    // reconcile, and the material leaves the disk on its own.
    wait_for_purge(&satellite).await;
    assert_eq!(
        tentaflow_core::provider_accounts::credential_sync::reconcile_local(
            &satellite.db,
            &satellite.id
        )
        .await
        .expect("reconcile"),
        0,
        "the reconcile is idempotent once the material is gone"
    );
    assert!(
        provider_credentials::build_for_peer(&home.security, &satellite.id)
            .expect("build")
            .is_none(),
        "a node out of the fleet is offered nothing afterwards"
    );
    // And it refuses a frame even if one is handed to it.
    assert!(provider_credentials::ingest(
        &satellite.security,
        &home.id,
        ProviderCredentialsSyncPayload { entries: vec![] }
    )
    .is_err());
}
