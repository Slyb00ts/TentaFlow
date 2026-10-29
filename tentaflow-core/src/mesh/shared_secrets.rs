// =============================================================================
// File: mesh/shared_secrets.rs
// Purpose: Replication of fleet-wide secret settings (`hf_token`, `ngc_api_key`,
//          `api_key_pepper`) between trusted nodes, and of the per-organization
//          keys of hashed bus fields (`bus::field_hash_key`, TentaBus F4 B7).
//          Each secret travels sealed for the one peer that receives it and
//          never enters the sync ledger, whose operation bodies are stored and
//          relayed in plaintext.
// =============================================================================

use anyhow::Result;
use tentaflow_protocol::mesh::{BusHashKeyEntry, SharedSecretEntry, SharedSecretsSyncPayload};
use tracing::warn;
use zeroize::Zeroizing;

use crate::bus::field_hash_key::{FieldHashKeyVersion, FieldHashKeys};
use crate::db::repository::{self, SharedSecretVersion};
use crate::mesh::security::MeshSecurity;
use crate::sync::ledger::HybridLogicalTimestamp;

/// A version stamped further ahead than the clock window would win every later
/// comparison, so one bad frame could pin a secret for good.
const MAX_VERSION_LEAD_MS: i64 = 120_000;

/// Longest organization id a hash key frame may name.
const MAX_ORG_ID_BYTES: usize = 128;

/// Binds a sealed value to the setting and version it belongs to.
fn seal_context(key: &str, hlc: &HybridLogicalTimestamp) -> Vec<u8> {
    format!(
        "shared-secret|{key}|{}|{}|{}",
        hlc.wall_time_ms, hlc.logical, hlc.node_id
    )
    .into_bytes()
}

/// Binds a sealed hash key to the organization and version it belongs to.
fn hash_key_seal_context(org_id: &str, key_version: i64, rotated_at_ms: i64) -> Vec<u8> {
    format!("bus-hash-key|{org_id}|{key_version}|{rotated_at_ms}").into_bytes()
}

fn hash_keys(security: &MeshSecurity) -> FieldHashKeys {
    FieldHashKeys::new(security.db.clone(), security.settings_cipher().clone())
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
    let mut bus_hash_keys = Vec::new();
    // A table of hash keys that cannot be listed must not take the fleet's
    // other secrets down with it: they go out without the keys.
    let exported = match hash_keys(security).export() {
        Ok(keys) => keys,
        Err(e) => {
            warn!(peer = %peer_node_id, "shared secrets: hash keys not sent: {e}");
            Vec::new()
        }
    };
    for key in exported {
        let sealed = security.seal_for_peer(
            peer_node_id,
            &hash_key_seal_context(&key.org_id, key.key_version, key.rotated_at_ms),
            key.key_hex.as_bytes(),
        )?;
        bus_hash_keys.push(BusHashKeyEntry {
            org_id: key.org_id,
            key_version: key.key_version,
            created_at_ms: key.created_at_ms,
            rotated_at_ms: key.rotated_at_ms,
            sealed,
        });
    }
    Ok((!entries.is_empty() || !bus_hash_keys.is_empty()).then_some(SharedSecretsSyncPayload {
        entries,
        bus_hash_keys,
    }))
}

/// Adopts the entries of a frame from `sender_node_id` that are newer than what
/// this node holds, and returns how many were adopted (secrets and hash keys).
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
    let keys = hash_keys(security);
    for entry in payload.bus_hash_keys {
        if entry.org_id.is_empty() || entry.org_id.len() > MAX_ORG_ID_BYTES {
            warn!(peer = %sender_node_id, "shared secrets: hash key names an invalid organization, skipped");
            continue;
        }
        // The stamp of a hash key is not a reason to refuse it: a rotation is
        // a higher version whatever the sender's clock says, and a stamp that
        // is far off only makes an equal-version key lose the tie
        // (`FieldHashKeys::adopt_at`). Refusing it would leave the receiver on
        // another key than the operator's for as long as the clock is wrong.
        let opened = match security.open_from_peer(
            sender_node_id,
            &hash_key_seal_context(&entry.org_id, entry.key_version, entry.rotated_at_ms),
            &entry.sealed,
        ) {
            Ok(opened) => Zeroizing::new(opened),
            Err(e) => {
                warn!(peer = %sender_node_id, org = %entry.org_id, "shared secrets: hash key: {e}");
                continue;
            }
        };
        let Ok(key_hex) = std::str::from_utf8(&opened).map(|text| Zeroizing::new(text.to_string()))
        else {
            warn!(peer = %sender_node_id, org = %entry.org_id, "shared secrets: hash key is not UTF-8, skipped");
            continue;
        };
        let incoming = FieldHashKeyVersion {
            org_id: entry.org_id,
            key_version: entry.key_version,
            created_at_ms: entry.created_at_ms,
            rotated_at_ms: entry.rotated_at_ms,
            key_hex,
        };
        match keys.adopt_at(&incoming, now_ms) {
            Ok(true) => adopted += 1,
            Ok(false) => {}
            Err(e) => warn!(peer = %sender_node_id, org = %incoming.org_id, "shared secrets: {e}"),
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

    // ---- TentaBus F4 B7: per-organization hash keys -------------------------

    fn keys(node: &MeshSecurity) -> FieldHashKeys {
        hash_keys(node)
    }

    fn hash_of(node: &MeshSecurity, org: &str, value: &[u8]) -> String {
        keys(node).get_or_create(org).unwrap().hash_hex(value)
    }

    #[test]
    fn a_hash_key_made_on_one_node_gives_a_peer_the_same_hashes() {
        let (origin, receiver) = (node(), node());
        trust(&origin, &receiver, false);
        trust(&receiver, &origin, true);
        let before = hash_of(&origin, "org-1", b"Jan");

        let payload = build_for_peer(&origin, &receiver.ed25519_public_key_hex())
            .unwrap()
            .expect("payload");
        // The key never travels in the clear.
        let wire = crate::mesh::cbor::encode(&payload).unwrap();
        let key_hex = keys(&origin).export().unwrap()[0].key_hex.to_string();
        assert!(!String::from_utf8_lossy(&wire).contains(&key_hex));
        assert!(!wire.windows(32).any(|w| w == hex::decode(&key_hex).unwrap().as_slice()));

        let adopted =
            ingest(&receiver, &origin.ed25519_public_key_hex(), payload.clone(), now_ms()).unwrap();
        assert_eq!(adopted, 1);
        assert_eq!(hash_of(&receiver, "org-1", b"Jan"), before);
        // The same frame again changes nothing, which ends the relay.
        assert_eq!(
            ingest(&receiver, &origin.ed25519_public_key_hex(), payload, now_ms()).unwrap(),
            0
        );
    }

    #[test]
    fn two_nodes_that_each_made_a_key_converge_after_exchanging_them() {
        let (a, b) = (node(), node());
        trust(&a, &b, true);
        trust(&b, &a, true);
        hash_of(&a, "org-1", b"x");
        std::thread::sleep(std::time::Duration::from_millis(3));
        hash_of(&b, "org-1", b"x");
        assert_ne!(hash_of(&a, "org-1", b"Jan"), hash_of(&b, "org-1", b"Jan"));

        let from_a = build_for_peer(&a, &b.ed25519_public_key_hex()).unwrap().unwrap();
        let from_b = build_for_peer(&b, &a.ed25519_public_key_hex()).unwrap().unwrap();
        ingest(&b, &a.ed25519_public_key_hex(), from_a, now_ms()).unwrap();
        ingest(&a, &b.ed25519_public_key_hex(), from_b, now_ms()).unwrap();

        assert_eq!(hash_of(&a, "org-1", b"Jan"), hash_of(&b, "org-1", b"Jan"));
    }

    /// F4 B7 review R1 P2-1: a node that has just joined may hold a key it made
    /// itself before the organization's reached it. The organization's is the
    /// older one, and it wins on the joiner — and never the other way round, so
    /// the joiner cannot talk the fleet into a new key either.
    #[test]
    fn a_fresh_local_key_gives_way_to_the_older_key_of_the_fleet() {
        let (fleet, joiner) = (node(), node());
        trust(&fleet, &joiner, true);
        trust(&joiner, &fleet, true);
        hash_of(&fleet, "org-1", b"x");
        std::thread::sleep(std::time::Duration::from_millis(3));
        hash_of(&joiner, "org-1", b"x");
        let fleet_hash = hash_of(&fleet, "org-1", b"Jan");
        assert_ne!(hash_of(&joiner, "org-1", b"Jan"), fleet_hash);

        let from_joiner = build_for_peer(&joiner, &fleet.ed25519_public_key_hex())
            .unwrap()
            .unwrap();
        assert_eq!(
            ingest(&fleet, &joiner.ed25519_public_key_hex(), from_joiner, now_ms()).unwrap(),
            0,
            "the newer key does not replace the fleet's"
        );
        assert_eq!(hash_of(&fleet, "org-1", b"Jan"), fleet_hash);

        let from_fleet = build_for_peer(&fleet, &joiner.ed25519_public_key_hex())
            .unwrap()
            .unwrap();
        assert_eq!(
            ingest(&joiner, &fleet.ed25519_public_key_hex(), from_fleet, now_ms()).unwrap(),
            1
        );
        assert_eq!(hash_of(&joiner, "org-1", b"Jan"), fleet_hash);
        assert_eq!(
            keys(&joiner).info("org-1").unwrap().unwrap().key_version,
            1,
            "the version did not move"
        );
    }

    /// F4 B7 review R1 P3-1: a rotation raises the version and an independent
    /// older v1 cannot take it back; a version no `u32` holds is refused.
    #[test]
    fn a_rotated_key_is_not_replaced_by_an_unrelated_older_version_and_a_huge_version_is_refused() {
        let (a, b) = (node(), node());
        trust(&a, &b, true);
        trust(&b, &a, true);
        hash_of(&a, "org-1", b"x");
        keys(&a).rotate("org-1", 1).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(3));
        hash_of(&b, "org-1", b"x");
        let rotated = hash_of(&a, "org-1", b"Jan");

        let from_a = build_for_peer(&a, &b.ed25519_public_key_hex()).unwrap().unwrap();
        ingest(&b, &a.ed25519_public_key_hex(), from_a, now_ms()).unwrap();
        assert_eq!(hash_of(&b, "org-1", b"Jan"), rotated);
        let from_b = build_for_peer(&b, &a.ed25519_public_key_hex()).unwrap().unwrap();
        assert_eq!(ingest(&a, &b.ed25519_public_key_hex(), from_b, now_ms()).unwrap(), 0);
        assert_eq!(keys(&a).info("org-1").unwrap().unwrap().key_version, 2);

        // A frame naming a version beyond u32::MAX is skipped, whatever sealed it.
        let sealed_for_b = |version: i64| {
            let key = keys(&a).export().unwrap().remove(0);
            let sealed = a
                .seal_for_peer(
                    &b.ed25519_public_key_hex(),
                    &hash_key_seal_context("org-9", version, key.rotated_at_ms),
                    key.key_hex.as_bytes(),
                )
                .unwrap();
            SharedSecretsSyncPayload {
                entries: vec![],
                bus_hash_keys: vec![BusHashKeyEntry {
                    org_id: "org-9".to_string(),
                    key_version: version,
                    created_at_ms: key.created_at_ms,
                    rotated_at_ms: key.rotated_at_ms,
                    sealed,
                }],
            }
        };
        assert_eq!(
            ingest(&b, &a.ed25519_public_key_hex(), sealed_for_b(i64::MAX), now_ms()).unwrap(),
            0
        );
        assert!(keys(&b).info("org-9").unwrap().is_none());
        assert_eq!(
            ingest(&b, &a.ed25519_public_key_hex(), sealed_for_b(3), now_ms()).unwrap(),
            1
        );
    }

    /// F4 B7 review R1 P3-5: a table of hash keys that cannot be listed does
    /// not take the fleet's other secrets down with it.
    #[test]
    fn a_broken_hash_key_table_does_not_stop_the_other_secrets_from_going_out() {
        let origin = node();
        let peer = node();
        trust(&origin, &peer, false);
        repository::set_shared_secret_setting_secure(
            &origin.db,
            "hf_token",
            "hf_value",
            origin.settings_cipher(),
        )
        .unwrap();
        origin
            .db
            .write()
            .unwrap()
            .execute("DROP TABLE bus_field_hash_keys", [])
            .unwrap();
        let payload = build_for_peer(&origin, &peer.ed25519_public_key_hex())
            .unwrap()
            .expect("the secrets still go out");
        assert_eq!(payload.entries.len(), 1);
        assert!(payload.bus_hash_keys.is_empty());
    }

    #[test]
    fn a_rotation_reaches_the_peers_and_old_hashes_stop_matching_everywhere() {
        let (a, b) = (node(), node());
        trust(&a, &b, true);
        trust(&b, &a, true);
        let old = hash_of(&a, "org-1", b"Jan");
        let first = build_for_peer(&a, &b.ed25519_public_key_hex()).unwrap().unwrap();
        ingest(&b, &a.ed25519_public_key_hex(), first, now_ms()).unwrap();
        assert_eq!(hash_of(&b, "org-1", b"Jan"), old);

        keys(&a).rotate("org-1", 1).unwrap();
        let rotated = build_for_peer(&a, &b.ed25519_public_key_hex()).unwrap().unwrap();
        assert_eq!(
            ingest(&b, &a.ed25519_public_key_hex(), rotated, now_ms()).unwrap(),
            1
        );
        let new = hash_of(&b, "org-1", b"Jan");
        assert_ne!(new, old);
        assert_eq!(new, hash_of(&a, "org-1", b"Jan"));
    }

    #[test]
    fn a_node_that_is_not_an_operator_cannot_change_a_hash_key() {
        let (sender, receiver) = (node(), node());
        trust(&sender, &receiver, false);
        trust(&receiver, &sender, false);
        hash_of(&sender, "org-1", b"x");
        let payload = build_for_peer(&sender, &receiver.ed25519_public_key_hex())
            .unwrap()
            .unwrap();
        assert!(ingest(&receiver, &sender.ed25519_public_key_hex(), payload, now_ms()).is_err());
        assert!(keys(&receiver).info("org-1").unwrap().is_none());
    }

    #[test]
    fn a_hash_key_sealed_for_another_node_or_moved_to_another_organization_is_skipped() {
        let (origin, receiver, bystander) = (node(), node(), node());
        trust(&origin, &receiver, false);
        trust(&origin, &bystander, false);
        trust(&receiver, &origin, true);
        trust(&bystander, &origin, true);
        hash_of(&origin, "org-1", b"x");
        let for_receiver = build_for_peer(&origin, &receiver.ed25519_public_key_hex())
            .unwrap()
            .unwrap();
        let origin_id = origin.ed25519_public_key_hex();

        assert_eq!(ingest(&bystander, &origin_id, for_receiver.clone(), now_ms()).unwrap(), 0);
        assert!(keys(&bystander).info("org-1").unwrap().is_none());

        let mut moved = for_receiver.clone();
        moved.bus_hash_keys[0].org_id = "org-2".to_string();
        assert_eq!(ingest(&receiver, &origin_id, moved, now_ms()).unwrap(), 0);
        let mut renumbered = for_receiver;
        renumbered.bus_hash_keys[0].key_version = 9;
        assert_eq!(ingest(&receiver, &origin_id, renumbered, now_ms()).unwrap(), 0);
        assert!(keys(&receiver).info("org-1").unwrap().is_none());
        assert!(keys(&receiver).info("org-2").unwrap().is_none());
    }

    /// F4 B7 review R2 P3-7: the clock window is not a reason to refuse a hash
    /// key (a skewed operator clock must not keep its rotations away); only an
    /// organization-less entry is skipped.
    #[test]
    fn a_hash_key_far_ahead_of_the_local_clock_is_still_adopted_and_one_for_no_organization_is_skipped() {
        let (origin, receiver) = (node(), node());
        trust(&origin, &receiver, false);
        trust(&receiver, &origin, true);
        hash_of(&origin, "org-1", b"x");
        let payload = build_for_peer(&origin, &receiver.ed25519_public_key_hex())
            .unwrap()
            .unwrap();
        let mut nameless = payload.clone();
        nameless.bus_hash_keys[0].org_id = String::new();
        assert_eq!(
            ingest(&receiver, &origin.ed25519_public_key_hex(), nameless, now_ms()).unwrap(),
            0
        );
        assert!(keys(&receiver).info("org-1").unwrap().is_none());
        // The receiver's clock is far behind the stamp of the key.
        let long_ago = now_ms() - 10 * MAX_VERSION_LEAD_MS;
        assert_eq!(
            ingest(&receiver, &origin.ed25519_public_key_hex(), payload, long_ago).unwrap(),
            1
        );
        assert_eq!(
            hash_of(&receiver, "org-1", b"Jan"),
            hash_of(&origin, "org-1", b"Jan")
        );
    }

    #[test]
    fn a_frame_from_a_peer_that_predates_hash_keys_still_decodes() {
        #[derive(serde::Serialize)]
        struct Legacy {
            entries: Vec<SharedSecretEntry>,
        }
        let bytes = crate::mesh::cbor::encode(&Legacy { entries: vec![] }).unwrap();
        let decoded: SharedSecretsSyncPayload = crate::mesh::cbor::decode(&bytes).unwrap();
        assert!(decoded.entries.is_empty() && decoded.bus_hash_keys.is_empty());
    }
}
