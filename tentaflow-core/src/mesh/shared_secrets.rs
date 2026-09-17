// =============================================================================
// File: mesh/shared_secrets.rs
// Purpose: Replication of fleet-wide secret settings (`hf_token`, `ngc_api_key`,
//          `api_key_pepper`) between trusted nodes. Each secret travels sealed
//          for the one peer that receives it and never enters the sync ledger,
//          whose operation bodies are stored and relayed in plaintext.
// =============================================================================

use anyhow::Result;
use tentaflow_protocol::mesh::{SharedSecretEntry, SharedSecretsSyncPayload};
use tracing::warn;

use crate::db::repository::{self, SharedSecretVersion};
use crate::mesh::security::MeshSecurity;
use crate::sync::ledger::HybridLogicalTimestamp;

/// A version stamped further ahead than the clock window would win every later
/// comparison, so one bad frame could pin a secret for good.
const MAX_VERSION_LEAD_MS: i64 = 120_000;

/// Binds a sealed value to the setting and version it belongs to.
fn seal_context(key: &str, hlc: &HybridLogicalTimestamp) -> Vec<u8> {
    format!(
        "shared-secret|{key}|{}|{}|{}",
        hlc.wall_time_ms, hlc.logical, hlc.node_id
    )
    .into_bytes()
}

/// Every secret this node holds, sealed for `peer_node_id`. `None` when there is
/// nothing to send.
pub fn build_for_peer(
    security: &MeshSecurity,
    peer_node_id: &str,
) -> Result<Option<SharedSecretsSyncPayload>> {
    let secrets = repository::list_shared_secret_versions(&security.db, security.settings_cipher())?;
    let mut entries = Vec::with_capacity(secrets.len());
    for secret in secrets {
        let sealed = security.seal_for_peer(
            peer_node_id,
            &seal_context(&secret.key, &secret.hlc),
            secret.value.as_bytes(),
        )?;
        entries.push(SharedSecretEntry {
            key: secret.key,
            hlc_wall_ms: secret.hlc.wall_time_ms,
            hlc_logical: secret.hlc.logical,
            hlc_node: secret.hlc.node_id,
            sealed,
        });
    }
    Ok((!entries.is_empty()).then_some(SharedSecretsSyncPayload { entries }))
}

/// Adopts the entries of a frame from `sender_node_id` that are newer than what
/// this node holds, and returns how many were adopted.
///
/// Only an operator may change a fleet secret. A node trusted transitively still
/// receives secrets, but what it sends is ignored until an admin promotes it —
/// otherwise any such node could replace the API-key pepper for the whole fleet.
pub fn ingest(
    security: &MeshSecurity,
    sender_node_id: &str,
    payload: SharedSecretsSyncPayload,
    now_ms: i64,
) -> Result<usize> {
    if !security.is_trusted(sender_node_id) {
        anyhow::bail!("shared secrets from untrusted node {sender_node_id}");
    }
    if !repository::node_is_operator(&security.db, sender_node_id)? {
        anyhow::bail!(
            "shared secrets from {sender_node_id} ignored: the node is not an operator"
        );
    }
    let mut adopted = 0usize;
    for entry in payload.entries {
        if !repository::is_shared_secret_setting_key(&entry.key) {
            warn!(peer = %sender_node_id, key = %entry.key, "shared secrets: key is not a shared secret, skipped");
            continue;
        }
        if entry.hlc_wall_ms > now_ms + MAX_VERSION_LEAD_MS {
            warn!(peer = %sender_node_id, key = %entry.key, "shared secrets: version is ahead of the local clock, skipped");
            continue;
        }
        let hlc = HybridLogicalTimestamp {
            wall_time_ms: entry.hlc_wall_ms,
            logical: entry.hlc_logical,
            node_id: entry.hlc_node,
        };
        let opened = match security.open_from_peer(
            sender_node_id,
            &seal_context(&entry.key, &hlc),
            &entry.sealed,
        ) {
            Ok(opened) => opened,
            Err(e) => {
                warn!(peer = %sender_node_id, key = %entry.key, "shared secrets: {e}");
                continue;
            }
        };
        let Ok(value) = String::from_utf8(opened) else {
            warn!(peer = %sender_node_id, key = %entry.key, "shared secrets: value is not UTF-8, skipped");
            continue;
        };
        let secret = SharedSecretVersion {
            key: entry.key,
            value,
            hlc,
        };
        if repository::adopt_shared_secret_version(&security.db, security.settings_cipher(), &secret)? {
            adopted += 1;
        }
    }
    Ok(adopted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::repository::SyncNodeProfileUpdate;
    use std::sync::Arc;

    fn node() -> MeshSecurity {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::migrations::run(&conn).unwrap();
        let db = Arc::new(crate::db::Db::from_connection(conn));
        MeshSecurity::new(db, Arc::new(crate::crypto::SettingsCipher::new(&[7u8; 32]))).unwrap()
    }

    fn trust(local: &MeshSecurity, peer: &MeshSecurity, operator: bool) {
        let peer_id = peer.ed25519_public_key_hex();
        local
            .add_trusted_key(&peer_id, &peer.public_key_hex(), "peer", None)
            .unwrap();
        repository::update_sync_node_profile(
            &local.db,
            &peer_id,
            &SyncNodeProfileUpdate {
                node_kind: None,
                operator: Some(operator),
            },
            None,
        )
        .unwrap();
    }

    fn stored(node: &MeshSecurity, key: &str) -> Option<String> {
        repository::get_setting_secure(&node.db, key, node.settings_cipher()).unwrap()
    }

    fn now_ms() -> i64 {
        crate::mesh::proto_conv::now_unix_ms()
    }

    #[test]
    fn a_secret_set_on_one_node_is_adopted_by_a_peer_that_sees_it_as_operator() {
        let (origin, receiver) = (node(), node());
        trust(&origin, &receiver, false);
        trust(&receiver, &origin, true);
        repository::set_shared_secret_setting_secure(
            &origin.db,
            "hf_token",
            "hf_value",
            origin.settings_cipher(),
        )
        .unwrap();

        let payload = build_for_peer(&origin, &receiver.ed25519_public_key_hex())
            .unwrap()
            .expect("payload");
        let adopted =
            ingest(&receiver, &origin.ed25519_public_key_hex(), payload.clone(), now_ms()).unwrap();

        assert_eq!(adopted, 1);
        assert_eq!(stored(&receiver, "hf_token").as_deref(), Some("hf_value"));
        // The same version a second time changes nothing, which ends the relay.
        let again = ingest(&receiver, &origin.ed25519_public_key_hex(), payload, now_ms()).unwrap();
        assert_eq!(again, 0);
    }

    #[test]
    fn a_node_that_is_not_an_operator_cannot_change_a_fleet_secret() {
        let (sender, receiver) = (node(), node());
        trust(&sender, &receiver, false);
        trust(&receiver, &sender, false);
        repository::set_shared_secret_setting_secure(
            &sender.db,
            "api_key_pepper",
            "attacker-pepper",
            sender.settings_cipher(),
        )
        .unwrap();
        let payload = build_for_peer(&sender, &receiver.ed25519_public_key_hex())
            .unwrap()
            .expect("payload");

        assert!(ingest(&receiver, &sender.ed25519_public_key_hex(), payload, now_ms()).is_err());
        assert_eq!(stored(&receiver, "api_key_pepper"), None);
    }

    #[test]
    fn an_older_version_does_not_replace_a_newer_local_secret() {
        let (stale, fresh) = (node(), node());
        trust(&stale, &fresh, true);
        trust(&fresh, &stale, true);
        repository::set_shared_secret_setting_secure(&stale.db, "hf_token", "old", stale.settings_cipher())
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        repository::set_shared_secret_setting_secure(&fresh.db, "hf_token", "new", fresh.settings_cipher())
            .unwrap();

        let payload = build_for_peer(&stale, &fresh.ed25519_public_key_hex())
            .unwrap()
            .expect("payload");
        let adopted = ingest(&fresh, &stale.ed25519_public_key_hex(), payload, now_ms()).unwrap();

        assert_eq!(adopted, 0);
        assert_eq!(stored(&fresh, "hf_token").as_deref(), Some("new"));
    }

    #[test]
    fn an_entry_sealed_for_another_node_or_moved_to_another_key_is_skipped() {
        let (origin, receiver, bystander) = (node(), node(), node());
        trust(&origin, &receiver, false);
        trust(&origin, &bystander, false);
        trust(&receiver, &origin, true);
        trust(&bystander, &origin, true);
        repository::set_shared_secret_setting_secure(
            &origin.db,
            "hf_token",
            "hf_value",
            origin.settings_cipher(),
        )
        .unwrap();
        let for_receiver = build_for_peer(&origin, &receiver.ed25519_public_key_hex())
            .unwrap()
            .expect("payload");

        let origin_id = origin.ed25519_public_key_hex();
        assert_eq!(ingest(&bystander, &origin_id, for_receiver.clone(), now_ms()).unwrap(), 0);

        let mut moved = for_receiver;
        moved.entries[0].key = "ngc_api_key".to_string();
        assert_eq!(ingest(&receiver, &origin_id, moved, now_ms()).unwrap(), 0);
        assert_eq!(stored(&receiver, "ngc_api_key"), None);
    }

    #[test]
    fn a_version_far_ahead_of_the_local_clock_is_skipped() {
        let (origin, receiver) = (node(), node());
        trust(&origin, &receiver, false);
        trust(&receiver, &origin, true);
        repository::set_shared_secret_setting_secure(
            &origin.db,
            "hf_token",
            "hf_value",
            origin.settings_cipher(),
        )
        .unwrap();
        let payload = build_for_peer(&origin, &receiver.ed25519_public_key_hex())
            .unwrap()
            .expect("payload");

        let long_ago = now_ms() - 10 * MAX_VERSION_LEAD_MS;
        let adopted = ingest(&receiver, &origin.ed25519_public_key_hex(), payload, long_ago).unwrap();

        assert_eq!(adopted, 0);
        assert_eq!(stored(&receiver, "hf_token"), None);
    }
}
