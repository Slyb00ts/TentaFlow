// =============================================================================
// File: mesh/provider_credentials.rs
// Purpose: Replication of agent-account credentials between the nodes that are
//          allowed to hold them. Each credential travels sealed for the one peer
//          that receives it and never enters the sync ledger, whose operation
//          bodies are stored and relayed in plaintext by every node on the path.
// =============================================================================
//
// The account ROW, its grants and the runtime matrix are ordinary ledger
// resources — they are metadata, and every node that renders the screens needs
// them. The material is not: it is a provider token whose blast radius is a paid
// subscription, and `docs/agent-accounts-technical-design.md` binding correction
// 13 keeps it off the ledger for the same reason `hf_token` was taken off it
// (`sync/runtime.rs::carries_shared_secret`).
//
// Three gates decide whether a credential crosses, and all three live in the
// store (`provider_accounts::repository`) rather than here: the peer must be
// flagged `receives_accounts`, one of the two ends must be the account's home
// node, and the revision must be above the account's revocation mark. This file
// is the transport and the audit of what it refused.

use anyhow::Result;
use sha2::{Digest, Sha256};
use tentaflow_protocol::mesh::{ProviderCredentialEntry, ProviderCredentialsSyncPayload};
use tracing::warn;

use crate::mesh::security::MeshSecurity;
use crate::provider_accounts::repository as store;
use crate::provider_accounts::CredentialMeta;

/// Binds a sealed credential to the account, the revision and the digest it
/// claims to be. An entry cannot be replanted under another account, nor passed
/// off as a different revision of the same one.
fn seal_context(account_id: &str, revision: i64, material_sha256: &str) -> Vec<u8> {
    format!("provider-credential|{account_id}|{revision}|{material_sha256}").into_bytes()
}

fn digest(material: &str) -> String {
    hex::encode(Sha256::digest(material.as_bytes()))
}

/// Which node this installation is, as the account store records it.
fn local_node_id(security: &MeshSecurity) -> Result<String> {
    crate::db::repository::get_setting(&security.db, crate::db::repository::LOCAL_NODE_ID_SETTING)?
        .ok_or_else(|| anyhow::anyhow!("this installation has no node identity yet"))
}

/// Every credential this node may hand to `peer_node_id`, sealed for it. `None`
/// when there is nothing that peer may receive — which is the ordinary answer
/// for a node the administrator has not put in the account fleet.
pub fn build_for_peer(
    security: &MeshSecurity,
    peer_node_id: &str,
) -> Result<Option<ProviderCredentialsSyncPayload>> {
    let local = local_node_id(security)?;
    let credentials = store::publishable_credentials(
        &security.db,
        security.settings_cipher(),
        &local,
        peer_node_id,
    )?;
    let mut entries = Vec::with_capacity(credentials.len());
    for credential in credentials {
        let sealed = security.seal_for_peer(
            peer_node_id,
            &seal_context(
                &credential.account_id,
                credential.revision,
                &credential.material_sha256,
            ),
            credential.material.as_bytes(),
        )?;
        entries.push(ProviderCredentialEntry {
            account_id: credential.account_id,
            revision: credential.revision,
            material_sha256: credential.material_sha256,
            provider_subject: credential.provider_subject,
            expires_at: credential.expires_at,
            home_node_id: credential.home_node_id,
            refreshed_by_node: credential.refreshed_by_node,
            sealed,
        });
    }
    Ok((!entries.is_empty()).then_some(ProviderCredentialsSyncPayload { entries }))
}

/// One account whose stored credential moved, so the caller can bring a running
/// bridge up to the revision the store now holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Adopted {
    pub account_id: String,
    pub revision: i64,
}

/// Adopts the entries of a frame from `sender_node_id` that this node is allowed
/// to take, and returns the accounts whose stored credential changed.
///
/// The frame is refused outright when this node may not hold credentials at all.
/// That is the same decision the SENDER already applied, checked again here
/// because the flag is an administrator's and must not depend on every peer
/// having the current copy of it.
pub fn ingest(
    security: &MeshSecurity,
    sender_node_id: &str,
    payload: ProviderCredentialsSyncPayload,
) -> Result<Vec<Adopted>> {
    if !security.is_trusted(sender_node_id) {
        anyhow::bail!("provider credentials from untrusted node {sender_node_id}");
    }
    let local = local_node_id(security)?;
    if !store::receives_accounts(&security.db, &local)? {
        anyhow::bail!("this node does not receive agent accounts");
    }
    let mut adopted = Vec::new();
    for entry in payload.entries {
        match adopt_entry(security, sender_node_id, &local, &entry) {
            Ok(Some(revision)) => adopted.push(Adopted {
                account_id: entry.account_id,
                revision,
            }),
            Ok(None) => {}
            Err(e) => warn!(
                peer = %sender_node_id,
                account_id = %entry.account_id,
                "provider credentials: {e}"
            ),
        }
    }
    Ok(adopted)
}

/// `Some(revision)` when the store moved, `None` when the entry was legally
/// skipped (already held, older, revoked, refused).
fn adopt_entry(
    security: &MeshSecurity,
    sender_node_id: &str,
    local_node_id: &str,
    entry: &ProviderCredentialEntry,
) -> Result<Option<i64>> {
    let Some(account) = store::get_account(&security.db, &entry.account_id)? else {
        // The account row has not arrived through the ledger yet. Not an error
        // and not a refusal: the next push carries the credential again, and by
        // then the row it belongs to is here.
        return Ok(None);
    };
    // The single-refresher rule. A peer that is neither the home node nor
    // talking TO the home node is offering material it has no title to mint, and
    // taking it is how two nodes start overwriting each other's revision.
    if !store::credential_exchange_allowed(
        local_node_id,
        sender_node_id,
        account.home_node_id.as_deref(),
    ) {
        store::record_credential_refusal(
            &security.db,
            &entry.account_id,
            sender_node_id,
            "not_home_node",
            entry.revision,
        )?;
        return Ok(None);
    }
    if entry.revision <= account.credential_revoked_revision {
        // Material for a credential the fleet was told to stop using. The sender
        // has not seen the revocation yet; it will, through the account row.
        return Ok(None);
    }
    let material = String::from_utf8(security.open_from_peer(
        sender_node_id,
        &seal_context(&entry.account_id, entry.revision, &entry.material_sha256),
        &entry.sealed,
    )?)
    .map_err(|_| anyhow::anyhow!("the sealed credential is not UTF-8"))?;
    // The digest is what the seal is bound to AND what the store records, so a
    // sender whose two halves disagree is naming one credential and handing over
    // another.
    if digest(&material) != entry.material_sha256 {
        store::record_credential_refusal(
            &security.db,
            &entry.account_id,
            sender_node_id,
            "digest_mismatch",
            entry.revision,
        )?;
        return Ok(None);
    }
    let meta = CredentialMeta {
        provider_subject: entry.provider_subject.clone(),
        expires_at: entry.expires_at.clone(),
        refreshed_by_node: entry
            .refreshed_by_node
            .clone()
            .or_else(|| Some(sender_node_id.to_string())),
        // Nobody on THIS node asked for the write: it is the fan-out of a
        // decision (a sign-in, a rotation) that was made elsewhere, and the
        // audit row says so by naming no actor.
        actor: None,
    };
    match store::set_credential(
        &security.db,
        security.settings_cipher(),
        &entry.account_id,
        entry.revision,
        &material,
        &meta,
    )? {
        crate::provider_accounts::CredentialWrite::Applied { revision } => Ok(Some(revision)),
        // A replay of what is already here, an older revision, or two writers
        // that minted one revision — the last of which `set_credential` has
        // already audited and answered by asking for a new sign-in.
        crate::provider_accounts::CredentialWrite::Unchanged { .. }
        | crate::provider_accounts::CredentialWrite::Stale { .. }
        | crate::provider_accounts::CredentialWrite::Conflict { .. } => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::repository::SyncNodeProfileUpdate;
    use crate::provider_accounts::{AccountUpdate, NewAccount};
    use std::sync::Arc;

    /// A node with its own database, its own settings cipher and its own mesh
    /// identity — which is what makes "re-encrypted for the receiving node" a
    /// measurement here rather than a claim.
    fn node(key: u8) -> MeshSecurity {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::migrations::run(&conn).unwrap();
        let db = Arc::new(crate::db::Db::from_connection(conn));
        let security =
            MeshSecurity::new(db, Arc::new(crate::crypto::SettingsCipher::new(&[key; 32])))
                .unwrap();
        let id = security.ed25519_public_key_hex();
        let conn = security.db.write().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)",
            rusqlite::params![crate::db::repository::LOCAL_NODE_ID_SETTING, id],
        )
        .unwrap();
        drop(conn);
        security
    }

    fn trust(local: &MeshSecurity, peer: &MeshSecurity) {
        let peer_id = peer.ed25519_public_key_hex();
        local
            .add_trusted_key(&peer_id, &peer.public_key_hex(), "peer", None)
            .unwrap();
        crate::db::repository::update_sync_node_profile(
            &local.db,
            &peer_id,
            &SyncNodeProfileUpdate {
                node_kind: None,
                operator: Some(true),
            },
            None,
        )
        .unwrap();
    }

    /// The same account row on every node, as the ledger would have delivered
    /// it, homed on `home`.
    fn seed_account(security: &MeshSecurity, account_id: &str, home: &str) {
        store::create_account(
            &security.db,
            &NewAccount {
                account_id: account_id.to_string(),
                org_id: crate::services::org::DEFAULT_ORG_ID.to_string(),
                engine_id: "codex".to_string(),
                display_name: "Shared codex".to_string(),
                scope: "global".to_string(),
                owner_user_id: None,
                credential_kind: "provider_login".to_string(),
                created_by: "admin".to_string(),
            },
        )
        .unwrap();
        store::update_account(
            &security.db,
            account_id,
            &AccountUpdate {
                home_node_id: Some(home.to_string()),
                ..Default::default()
            },
            None,
        )
        .unwrap();
    }

    fn receives(security: &MeshSecurity, node_id: &str, enabled: bool) {
        store::set_receives_accounts(&security.db, node_id, enabled, Some("admin")).unwrap();
    }

    fn material_on(security: &MeshSecurity, account_id: &str) -> Option<String> {
        store::credential_material(&security.db, security.settings_cipher(), account_id).unwrap()
    }

    fn audit_reasons(security: &MeshSecurity, account_id: &str) -> Vec<String> {
        let conn = security.db.read().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT details FROM audit_log \
                 WHERE action = 'provider_account.credential_refused' AND resource = ?1 \
                 ORDER BY id",
            )
            .unwrap();
        let rows = stmt
            .query_map(rusqlite::params![account_id], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        rows.into_iter()
            .map(|details| {
                serde_json::from_str::<serde_json::Value>(&details).unwrap()["reason"]
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect()
    }

    /// The whole point of the feature: a credential minted on the home node is
    /// readable on a receiving satellite, under that satellite's OWN key, at the
    /// same revision — and the frame built for the satellite is worthless to a
    /// node that is not in the account fleet.
    #[test]
    fn a_credential_reaches_a_receiving_node_and_no_other() {
        let (home, satellite, excluded) = (node(1), node(2), node(3));
        let (home_id, sat_id, out_id) = (
            home.ed25519_public_key_hex(),
            satellite.ed25519_public_key_hex(),
            excluded.ed25519_public_key_hex(),
        );
        for pair in [
            (&home, &satellite),
            (&satellite, &home),
            (&home, &excluded),
            (&excluded, &home),
        ] {
            trust(pair.0, pair.1);
        }
        for holder in [&home, &satellite, &excluded] {
            seed_account(holder, "acc-1", &home_id);
            receives(holder, &sat_id, true);
            receives(holder, &home_id, true);
        }
        store::mint_credential(
            &home.db,
            home.settings_cipher(),
            "acc-1",
            "{\"tokens\":{\"refresh_token\":\"first\"}}",
            &CredentialMeta::default(),
        )
        .unwrap();

        let for_satellite = build_for_peer(&home, &sat_id).unwrap().expect("payload");
        assert!(
            build_for_peer(&home, &out_id).unwrap().is_none(),
            "a node that does not receive accounts is offered nothing"
        );
        let adopted = ingest(&satellite, &home_id, for_satellite.clone()).unwrap();

        assert_eq!(adopted.len(), 1);
        assert_eq!(adopted[0].revision, 1);
        assert_eq!(
            material_on(&satellite, "acc-1").as_deref(),
            Some("{\"tokens\":{\"refresh_token\":\"first\"}}"),
            "the material opens under the satellite's own key"
        );
        assert_eq!(
            store::credential_summary(&satellite.db, "acc-1")
                .unwrap()
                .unwrap()
                .revision,
            1,
            "the CAS revision is preserved across the mesh"
        );
        // The frame sealed for the satellite is not material for anybody else,
        // and a node outside the fleet refuses the frame before it even tries.
        let refused = ingest(&excluded, &home_id, for_satellite).unwrap_err();
        assert!(refused.to_string().contains("does not receive"));
        assert_eq!(material_on(&excluded, "acc-1"), None);
        // Re-ingesting the same revision changes nothing, which ends the relay.
        let again = ingest(
            &satellite,
            &home_id,
            build_for_peer(&home, &sat_id).unwrap().expect("payload"),
        )
        .unwrap();
        assert!(again.is_empty());
    }

    /// A satellite may not fan a rotation out to another satellite: it is not
    /// the home node, and two satellites taking each other's material is exactly
    /// the ping-pong the single-refresher rule exists to prevent. The refusal is
    /// audited, because nothing in the resulting state records it.
    #[test]
    fn a_rotation_offered_by_a_node_that_is_not_home_is_refused_with_an_audit() {
        let (home, first, second) = (node(4), node(5), node(6));
        let (home_id, first_id, second_id) = (
            home.ed25519_public_key_hex(),
            first.ed25519_public_key_hex(),
            second.ed25519_public_key_hex(),
        );
        for pair in [
            (&first, &second),
            (&second, &first),
            (&home, &first),
            (&first, &home),
        ] {
            trust(pair.0, pair.1);
        }
        for holder in [&home, &first, &second] {
            seed_account(holder, "acc-2", &home_id);
            receives(holder, &home_id, true);
            receives(holder, &first_id, true);
            receives(holder, &second_id, true);
        }
        store::mint_credential(
            &first.db,
            first.settings_cipher(),
            "acc-2",
            "rotated-on-a-satellite",
            &CredentialMeta::default(),
        )
        .unwrap();

        // The sender's own gate already refuses to build the frame for a peer
        // that is not the home node…
        assert!(build_for_peer(&first, &second_id).unwrap().is_none());
        // …and a peer that ignores its own gate is refused on arrival.
        let forged = ProviderCredentialsSyncPayload {
            entries: vec![ProviderCredentialEntry {
                account_id: "acc-2".to_string(),
                revision: 1,
                material_sha256: digest("rotated-on-a-satellite"),
                provider_subject: None,
                expires_at: None,
                home_node_id: Some(first_id.clone()),
                refreshed_by_node: Some(first_id.clone()),
                sealed: first
                    .seal_for_peer(
                        &second_id,
                        &seal_context("acc-2", 1, &digest("rotated-on-a-satellite")),
                        b"rotated-on-a-satellite",
                    )
                    .unwrap(),
            }],
        };
        let adopted = ingest(&second, &first_id, forged).unwrap();

        assert!(adopted.is_empty());
        assert_eq!(material_on(&second, "acc-2"), None);
        assert_eq!(audit_reasons(&second, "acc-2"), vec!["not_home_node"]);
        // Towards the home node the same satellite MAY submit what its CLI
        // rotated — that is how a rotation reaches the single refresher.
        assert!(build_for_peer(&first, &home_id).unwrap().is_some());
    }

    /// A credential cleared on the home node leaves the satellites too. The
    /// removal travels as the revocation mark on the account row (the material
    /// never travels through the ledger, so its ABSENCE carries nothing), and the
    /// satellite purges what it holds when it sees a mark that covers it.
    #[test]
    fn a_cleared_credential_is_purged_where_it_was_replicated() {
        let (home, satellite) = (node(7), node(8));
        let (home_id, sat_id) = (
            home.ed25519_public_key_hex(),
            satellite.ed25519_public_key_hex(),
        );
        trust(&home, &satellite);
        trust(&satellite, &home);
        for holder in [&home, &satellite] {
            seed_account(holder, "acc-3", &home_id);
            receives(holder, &sat_id, true);
            receives(holder, &home_id, true);
        }
        store::mint_credential(
            &home.db,
            home.settings_cipher(),
            "acc-3",
            "material",
            &CredentialMeta::default(),
        )
        .unwrap();
        ingest(
            &satellite,
            &home_id,
            build_for_peer(&home, &sat_id).unwrap().expect("payload"),
        )
        .unwrap();
        assert!(material_on(&satellite, "acc-3").is_some());

        store::clear_credential(&home.db, "acc-3", Some("admin")).unwrap();
        // What the ledger delivers: the account row with the mark raised.
        store::update_account(
            &satellite.db,
            "acc-3",
            &AccountUpdate {
                status: Some("needs_login".to_string()),
                ..Default::default()
            },
            None,
        )
        .unwrap();
        satellite
            .db
            .write()
            .unwrap()
            .execute(
                "UPDATE provider_accounts SET credential_revoked_revision = 1 \
                 WHERE account_id = 'acc-3'",
                [],
            )
            .unwrap();

        let stale = store::stale_local_credentials(&satellite.db, &sat_id).unwrap();
        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0].reason, "revoked");
        assert!(store::purge_local_credential(&satellite.db, "acc-3", &sat_id, "revoked").unwrap());
        assert_eq!(material_on(&satellite, "acc-3"), None);
        // And the home node offers nothing for the revoked revision afterwards.
        assert!(build_for_peer(&home, &sat_id).unwrap().is_none());
    }

    /// Taking a SATELLITE out of the account fleet takes the material off its
    /// disk: the flag is a gate, not a label, and a replicated credential
    /// already written there has to go on the next reconcile. The home node
    /// cannot be taken out this way at all — `set_receives_accounts` refuses
    /// while a node is somebody's home, because there the purge would drop the
    /// copy every other node's is fanned out from.
    #[test]
    fn a_satellite_that_stops_receiving_accounts_purges_what_it_holds() {
        let holder = node(9);
        let holder_id = holder.ed25519_public_key_hex();
        seed_account(&holder, "acc-4", "node-home-elsewhere");
        receives(&holder, &holder_id, true);
        // What the fan-out leaves behind: the home node's revision 1, stored
        // through the CAS path rather than minted here.
        store::set_credential(
            &holder.db,
            holder.settings_cipher(),
            "acc-4",
            1,
            "material",
            &CredentialMeta::default(),
        )
        .unwrap();
        assert!(store::stale_local_credentials(&holder.db, &holder_id)
            .unwrap()
            .is_empty());

        receives(&holder, &holder_id, false);
        let stale = store::stale_local_credentials(&holder.db, &holder_id).unwrap();

        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0].reason, "not_receiving");
        assert!(
            store::purge_local_credential(&holder.db, "acc-4", &holder_id, "not_receiving")
                .unwrap()
        );
        assert_eq!(material_on(&holder, "acc-4"), None);
    }
}
