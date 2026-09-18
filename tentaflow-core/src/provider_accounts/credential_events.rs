// ===== File: provider_accounts/credential_events.rs — what Core does when a bridge's credential moves =====
//
// A bridge publishes two events about the account's canonical credential and
// nothing else about it: `credential_changed {engine, sha256}` when a session's
// rotated copy passed every gate and became the account's, and
// `credential_rejected {engine, reason, sha256}` when it did not. The material
// is never on either event — the bridge holds it, Core asks for it over the
// authenticated loopback channel it already uses for everything else.
//
// Until now both were logged and forgotten, which meant a Codex refresh token
// rotated on one node stayed on that node's disk: the store still held the
// token the provider had already retired, and every other node materialized it.
// This module is what makes the store follow the rotation.
//
// The write is a revision CAS, not a plain update. A node that adopts a
// rotation is claiming "the credential after the one I was given", and if
// somebody else already claimed that revision with different material, the
// conflict is real: two live tokens exist, one of them is already dead at the
// provider, and picking one would burn the other. The account goes to
// `needs_login` instead.

use std::sync::Arc;

use crate::crypto::SettingsCipher;
use crate::db::DbPool;
use crate::provider_accounts::{repository as store, CredentialMeta, CredentialWrite};
use crate::services::agent_runtime;

/// The two entry points every session-watching loop calls, with nothing but
/// what the event carried.
///
/// The node context is resolved here rather than threaded through every poll
/// loop: an event is about an ACCOUNT, and the loops that observe one (the
/// delegation node, the `/v1` chat path) have a service row and a user, not a
/// cipher and a node identity. A process that has not published its node state
/// yet is not an error — it cannot have a running bridge either.
pub async fn observed_change(account_id: &str, engine_id: &str, sha256: &str) {
    let Some(state) = crate::code_studio::remote_proxy::node_state() else {
        return;
    };
    credential_changed(
        &state.db,
        &state.settings_cipher,
        state.local_node_id.as_ref(),
        account_id,
        engine_id,
        sha256,
    )
    .await;
}

pub fn observed_rejection(account_id: &str, engine_id: &str, reason: &str, sha256: &str) {
    let Some(state) = crate::code_studio::remote_proxy::node_state() else {
        return;
    };
    credential_rejected(&state.db, account_id, engine_id, reason, sha256);
}

/// Adopts a credential the bridge of `account_id` just published.
///
/// `sha256` is what the bridge says it now holds; the material is fetched only
/// when the store does not already have that digest, so the ordinary case (a
/// node reacting to its own write) costs one comparison.
pub async fn credential_changed(
    db: &DbPool,
    cipher: &Arc<SettingsCipher>,
    node_id: &str,
    account_id: &str,
    engine_id: &str,
    sha256: &str,
) {
    if let Err(error) = adopt(db, cipher, node_id, account_id, sha256).await {
        // A rotation this node could not store is not a failed turn: the CLI
        // is working with the token it just refreshed. What it IS is a node
        // whose store now lags, which is why it is a warning and why the
        // account's node state records it.
        tracing::warn!(
            %account_id,
            engine = %engine_id,
            error = %format!("{error:#}"),
            "a rotated provider credential was not adopted into the account store"
        );
        // The revision this node already holds is NOT reset: it is still
        // running on that credential, and claiming to hold nothing would make
        // the next rotation mint a revision the store already has — a CAS
        // conflict that moves the whole shared account to `needs_login`.
        let _ = store::set_node_error(db, account_id, node_id, &format!("{error:#}"));
    }
}

async fn adopt(
    db: &DbPool,
    cipher: &Arc<SettingsCipher>,
    node_id: &str,
    account_id: &str,
    sha256: &str,
) -> anyhow::Result<()> {
    if store::get_account(db, account_id)?.is_none() {
        // The bridge belongs to a `services`-era account that has no row in the
        // registry yet. Nothing to update, and inventing a row here would mint
        // an account nobody created.
        return Ok(());
    }
    let stored = store::credential_summary(db, account_id)?;
    if stored
        .as_ref()
        .is_some_and(|row| row.material_sha256 == sha256)
    {
        return Ok(());
    }
    let Some(bridge) = agent_runtime::running_bridge(account_id).await else {
        anyhow::bail!("the bridge that published it is no longer running");
    };
    let (material, published_sha, identity) = agent_runtime::read_bridge_credential(&bridge)
        .await?
        .ok_or_else(|| anyhow::anyhow!("the bridge holds no credential for this account"))?;
    if published_sha != sha256 {
        anyhow::bail!("the bridge moved on to a different credential while this one was read");
    }
    // The revision this rotation follows. It is the STORE's revision, because
    // that is the one the CAS is against: the bridge rotated the credential
    // this node is running on, and the next number after the one the store
    // holds is what that rotation is.
    //
    // The node's own applied revision is only a floor. Using it as the base
    // instead (which is what this did) meant that one failed adoption — which
    // records no revision at all — made the NEXT rotation claim revision 1
    // against a store already past it: a conflict, and `needs_login` for every
    // user of a shared account, over a transient error.
    let base = stored.as_ref().map(|row| row.revision).unwrap_or(0).max(
        store::node_state(db, account_id, node_id)?
            .map(|row| row.applied_revision)
            .unwrap_or(0),
    );
    let meta = CredentialMeta {
        provider_subject: identity,
        expires_at: None,
        refreshed_by_node: Some(node_id.to_string()),
        // Nobody asked for this write: the provider rotated a token while a
        // session was running, and the audit row says so by naming no actor.
        actor: None,
    };
    match store::set_credential(db, cipher, account_id, base + 1, &material, &meta)? {
        CredentialWrite::Applied { revision } | CredentialWrite::Unchanged { revision } => {
            store::set_node_state(db, account_id, node_id, revision, "ready", None)?;
            // The provider rotated the token while a session was running, so
            // every node still holding the previous one is now working with a
            // credential the provider has retired. On the home node this is the
            // fan-out; on a satellite it is the submission the home node applies
            // the CAS to — both are the same frame, aimed at the peers the store
            // says may have it.
            super::credential_sync::publish_to_fleet();
            Ok(())
        }
        CredentialWrite::Conflict { revision } => anyhow::bail!(
            "revision {revision} was already minted with different material elsewhere"
        ),
        CredentialWrite::Stale { revision } => {
            anyhow::bail!("the store is already at revision {revision}")
        }
    }
}

/// Records a credential the bridge refused to publish.
///
/// Two of the four reasons are about one session's own copy (`stale_baseline`,
/// `unsafe_credential_file`) and change nothing about the account. The other
/// two mean the material could not be tied to this account's provider identity,
/// and a repeat of one of those is the shape of an account whose stored
/// credential no longer matches what the CLI is producing — the GUI has to say
/// "sign in again" rather than keep showing an active account that fails every
/// turn.
pub fn credential_rejected(
    db: &DbPool,
    account_id: &str,
    engine_id: &str,
    reason: &str,
    sha256: &str,
) {
    if store::get_account(db, account_id)
        .map(|account| account.is_none())
        .unwrap_or(true)
    {
        return;
    }
    match store::record_credential_rejection(db, account_id, engine_id, reason, sha256) {
        Ok(flagged) => tracing::warn!(
            %account_id,
            engine = %engine_id,
            %reason,
            credential_sha256 = %sha256,
            needs_login = flagged,
            "the bridge refused a credential a session handed back"
        ),
        Err(error) => tracing::warn!(
            %account_id,
            %reason,
            error = %format!("{error:#}"),
            "a refused credential could not be recorded"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider_accounts::NewAccount;

    /// A bridge holding `material`, addressed on loopback exactly as a real one
    /// is, and registered as the running bridge of `account_id`.
    async fn bridge_holding(account_id: &str, material: &str, identity: &str) {
        let handle = agent_runtime::testing::fake_bridge(
            "codex",
            vec![(
                "/account/credential",
                serde_json::json!({
                    "present": true,
                    "engine": "codex",
                    "sha256": digest(material),
                    "identity": identity,
                    "material": material,
                })
                .to_string(),
            )],
        )
        .await;
        agent_runtime::testing::register(account_id, handle);
    }

    fn digest(material: &str) -> String {
        use sha2::Digest;
        hex::encode(sha2::Sha256::digest(material.as_bytes()))
    }

    fn audit_details(db: &DbPool, action: &str, account_id: &str) -> Option<serde_json::Value> {
        let conn = db.read().expect("db");
        conn.query_row(
            "SELECT details FROM audit_log WHERE action = ?1 AND resource = ?2 ORDER BY id DESC \
             LIMIT 1",
            rusqlite::params![action, account_id],
            |row| row.get::<_, String>(0),
        )
        .ok()
        .map(|details| serde_json::from_str(&details).expect("audit details"))
    }

    fn account(db: &DbPool, account_id: &str) {
        store::create_account(
            db,
            &NewAccount {
                account_id: account_id.to_string(),
                org_id: crate::services::org::DEFAULT_ORG_ID.to_string(),
                engine_id: "codex".to_string(),
                display_name: account_id.to_string(),
                scope: "global".to_string(),
                owner_user_id: None,
                credential_kind: "provider_login".to_string(),
                created_by: "admin".to_string(),
            },
        )
        .expect("account");
    }

    /// A comparison that SUCCEEDED and disagreed is conclusive: the session's
    /// credential named a different provider account than this one's, and only
    /// a sign-in can say which identity the account is supposed to be. One is
    /// enough. A comparison that could not be made at all takes a second one
    /// within the day — see `record_credential_rejection`.
    #[test]
    fn one_identity_mismatch_asks_for_a_new_sign_in() {
        let db = crate::db::init(std::path::Path::new(":memory:")).expect("db");
        let cipher = SettingsCipher::new(&[5u8; 32]);
        account(&db, "acc-1");
        account(&db, "acc-2");
        for id in ["acc-1", "acc-2"] {
            store::mint_credential(&db, &cipher, id, "material", &CredentialMeta::default())
                .expect("credential");
        }

        credential_rejected(&db, "acc-1", "codex", "identity_mismatch", "aa");
        assert_eq!(
            store::get_account(&db, "acc-1").unwrap().unwrap().status,
            "needs_login"
        );

        credential_rejected(&db, "acc-2", "codex", "identity_unverifiable", "aa");
        assert_eq!(
            store::get_account(&db, "acc-2").unwrap().unwrap().status,
            "active",
            "a format with no comparable subject is not an account's problem yet"
        );
        credential_rejected(&db, "acc-2", "codex", "identity_unverifiable", "bb");
        assert_eq!(
            store::get_account(&db, "acc-2").unwrap().unwrap().status,
            "needs_login"
        );
    }

    /// A session that lost a rotation race, or corrupted its own copy, says
    /// nothing about the account's credential — which is still working.
    #[test]
    fn a_session_local_refusal_never_disables_the_account() {
        let db = crate::db::init(std::path::Path::new(":memory:")).expect("db");
        let cipher = SettingsCipher::new(&[5u8; 32]);
        account(&db, "acc-1");
        store::mint_credential(
            &db,
            &cipher,
            "acc-1",
            "material",
            &CredentialMeta::default(),
        )
        .expect("credential");

        for _ in 0..4 {
            credential_rejected(&db, "acc-1", "codex", "stale_baseline", "aa");
            credential_rejected(&db, "acc-1", "codex", "unsafe_credential_file", "bb");
        }
        assert_eq!(
            store::get_account(&db, "acc-1").unwrap().unwrap().status,
            "active"
        );
    }

    /// The whole adoption, end to end against a bridge: the rotation becomes
    /// the next revision, the material the bridge holds is what the store
    /// keeps, the audit row names no actor (nobody asked — the provider did
    /// it), and this node records that it holds the new revision.
    #[tokio::test]
    async fn a_rotation_becomes_the_next_revision_of_the_account() {
        let db = crate::db::init(std::path::Path::new(":memory:")).expect("db");
        let cipher = Arc::new(SettingsCipher::new(&[6u8; 32]));
        account(&db, "acc-rot");
        store::mint_credential(
            &db,
            &cipher,
            "acc-rot",
            "{\"tokens\":{\"account_id\":\"acct-7\",\"refresh_token\":\"first\"}}",
            &CredentialMeta {
                actor: Some("alice".into()),
                ..CredentialMeta::default()
            },
        )
        .expect("first credential");

        let rotated = "{\"tokens\":{\"account_id\":\"acct-7\",\"refresh_token\":\"second\"}}";
        bridge_holding("acc-rot", rotated, "account:acct-7").await;
        credential_changed(&db, &cipher, "node-1", "acc-rot", "codex", &digest(rotated)).await;
        agent_runtime::testing::forget("acc-rot");

        let summary = store::credential_summary(&db, "acc-rot")
            .expect("summary")
            .expect("row");
        assert_eq!(summary.revision, 2);
        assert_eq!(summary.material_sha256, digest(rotated));
        assert_eq!(summary.refreshed_by_node.as_deref(), Some("node-1"));
        assert_eq!(
            store::credential_material(&db, &cipher, "acc-rot").expect("material"),
            Some(rotated.to_string()),
        );
        assert_eq!(
            store::get_account(&db, "acc-rot").unwrap().unwrap().status,
            "active"
        );
        let state = store::node_state(&db, "acc-rot", "node-1")
            .expect("state")
            .expect("row");
        assert_eq!(
            (state.applied_revision, state.runtime_state.as_str()),
            (2, "ready")
        );
        let details =
            audit_details(&db, "provider_account.credential_set", "acc-rot").expect("audit row");
        assert_eq!(details["revision"], serde_json::json!(2));
        assert_eq!(details["rotated"], serde_json::json!(true));
        assert!(
            !details.to_string().contains("second"),
            "the audit row must not carry the material"
        );
        let conn = db.read().expect("db");
        let actor: Option<String> = conn
            .query_row(
                "SELECT user_id FROM audit_log WHERE action = 'provider_account.credential_set' \
                 AND resource = 'acc-rot' ORDER BY id DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .expect("audit row");
        assert_eq!(actor, None, "nobody asked for a rotation the provider made");
    }

    /// One failed adoption must not poison the next one. The failure records an
    /// error WITHOUT resetting the revision this node holds, so the rotation
    /// that follows claims the revision after the STORE's — not revision 1
    /// against a store that is already past it, which conflicted and disabled
    /// the account for every user of it.
    #[tokio::test]
    async fn an_earlier_failure_does_not_turn_the_next_rotation_into_a_conflict() {
        let db = crate::db::init(std::path::Path::new(":memory:")).expect("db");
        let cipher = Arc::new(SettingsCipher::new(&[7u8; 32]));
        account(&db, "acc-err");
        store::mint_credential(&db, &cipher, "acc-err", "first", &CredentialMeta::default())
            .expect("first");
        store::mint_credential(
            &db,
            &cipher,
            "acc-err",
            "second",
            &CredentialMeta::default(),
        )
        .expect("rotation elsewhere");
        store::set_node_state(&db, "acc-err", "node-1", 2, "ready", None).expect("state");

        // No bridge is running: the adoption fails exactly as a transient error
        // does, and the node's state records it.
        credential_changed(&db, &cipher, "node-1", "acc-err", "codex", &digest("third")).await;
        let state = store::node_state(&db, "acc-err", "node-1")
            .expect("state")
            .expect("row");
        assert_eq!(state.runtime_state, "error");
        assert_eq!(state.applied_revision, 2, "the held revision survives");
        assert_eq!(
            store::get_account(&db, "acc-err").unwrap().unwrap().status,
            "active",
            "a node that could not store a rotation does not disable the account"
        );

        // The real rotation right after it is adopted as revision 3.
        bridge_holding("acc-err", "third", "account:acct-7").await;
        credential_changed(&db, &cipher, "node-1", "acc-err", "codex", &digest("third")).await;
        agent_runtime::testing::forget("acc-err");
        let summary = store::credential_summary(&db, "acc-err")
            .expect("summary")
            .expect("row");
        assert_eq!(summary.revision, 3);
        assert_eq!(summary.material_sha256, digest("third"));
        assert_eq!(
            store::get_account(&db, "acc-err").unwrap().unwrap().status,
            "active"
        );
        assert!(
            audit_details(&db, "provider_account.credential_conflict", "acc-err").is_none(),
            "no conflict may be recorded for a rotation nobody raced"
        );
    }

    /// A bridge that moved on between the event and the read is not adopted:
    /// storing what it holds NOW under the digest the event named would file
    /// material the store never verified.
    #[tokio::test]
    async fn a_credential_that_moved_since_the_event_is_not_adopted() {
        let db = crate::db::init(std::path::Path::new(":memory:")).expect("db");
        let cipher = Arc::new(SettingsCipher::new(&[8u8; 32]));
        account(&db, "acc-race");
        store::mint_credential(
            &db,
            &cipher,
            "acc-race",
            "first",
            &CredentialMeta::default(),
        )
        .expect("first");
        bridge_holding("acc-race", "newer", "account:acct-7").await;

        credential_changed(
            &db,
            &cipher,
            "node-1",
            "acc-race",
            "codex",
            &digest("older"),
        )
        .await;
        agent_runtime::testing::forget("acc-race");
        let summary = store::credential_summary(&db, "acc-race")
            .expect("summary")
            .expect("row");
        assert_eq!(summary.revision, 1, "nothing was adopted");
        assert_eq!(
            store::node_state(&db, "acc-race", "node-1")
                .expect("state")
                .expect("row")
                .runtime_state,
            "error"
        );
    }

    /// An event about an account this node does not have is ignored, not
    /// invented: a bridge from the old per-service model publishes exactly
    /// that, and a row minted from it would be an account nobody created.
    #[tokio::test]
    async fn an_event_about_an_unknown_account_changes_nothing() {
        let db = crate::db::init(std::path::Path::new(":memory:")).expect("db");
        let cipher = Arc::new(SettingsCipher::new(&[5u8; 32]));
        credential_changed(&db, &cipher, "node-1", "no-such-account", "codex", "aa").await;
        credential_rejected(&db, "no-such-account", "codex", "identity_mismatch", "aa");
        assert!(store::node_state(&db, "no-such-account", "node-1")
            .unwrap()
            .is_none());
    }
}
