// ===== File: provider_accounts/credential_sync.rs — the two ends of a credential's travel =====
//
// The mesh transport lives in `mesh/provider_credentials.rs` and the rules in
// `repository.rs`. What is left is WHEN: a credential that has just been minted
// here has to reach the fleet without waiting for the next reconnect, and a
// credential this node may no longer hold has to leave its disk without waiting
// for somebody to use the account.
//
// Both are best-effort by nature and neither may fail the operation that
// triggered it: a sign-in that succeeded did succeed even if a peer is
// unreachable, and a revocation that could not stop a bridge is still a
// revocation. The anti-entropy push on every reconnect
// (`mesh/pipeline.rs`) and this reconcile are what make the fleet converge
// afterwards.

use anyhow::Result;

use crate::db::DbPool;
use crate::provider_accounts::repository as store;

/// Hands whatever this node holds to every trusted peer allowed to take it.
///
/// Called right after a local credential write (a sign-in, a pasted API key, a
/// rotation a bridge published). Spawned rather than awaited: the caller is
/// answering a user, and fanning out to a peer whose connection is slow is not
/// that user's business. A node with no mesh — the single-node install — simply
/// has nobody to tell.
pub fn publish_to_fleet() {
    let Some(state) = crate::code_studio::remote_proxy::node_state() else {
        return;
    };
    let Some(mesh) = state.quic_mesh.clone() else {
        return;
    };
    // The callers are synchronous functions inside async handlers, so a runtime
    // is normally right there — but a credential write must never PANIC for
    // want of one, and a fleet that was not told now is told on the next
    // reconnect by the anti-entropy push.
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        return;
    };
    handle.spawn(async move { mesh.push_provider_credentials_to_trusted(None).await });
}

/// Removes every credential copy this node may no longer hold, and returns how
/// many were removed.
///
/// Two reasons, both decided elsewhere and both arriving on the ledger: the
/// credential was cleared for the whole organisation (the revocation mark on the
/// account row), or this node was taken out of the account fleet (the flag on
/// the runtime-node row). The store row goes and the bridge is told to drop the
/// file it owns — a purge that left either half behind would leave a retired
/// token in front of the next session.
pub async fn reconcile_local(db: &DbPool, node_id: &str) -> Result<usize> {
    let stale = store::stale_local_credentials(db, node_id)?;
    let mut purged = 0usize;
    for credential in stale {
        // The bridge first: with the row already gone a failure here would leave
        // the file with nothing left to say it should not be there.
        if let Err(error) =
            crate::services::agent_runtime::drop_account_credential(&credential.account_id).await
        {
            tracing::warn!(
                account_id = %credential.account_id,
                reason = credential.reason,
                error = %format!("{error:#}"),
                "a revoked agent credential could not be dropped from its bridge"
            );
        }
        if store::purge_local_credential(db, &credential.account_id, node_id, credential.reason)? {
            purged += 1;
            tracing::info!(
                account_id = %credential.account_id,
                revision = credential.revision,
                reason = credential.reason,
                "an agent account credential was purged from this node"
            );
        }
    }
    Ok(purged)
}

/// Drops the bridge copy of an account that no longer exists here.
///
/// The account's own row took its credential row with it through the FK
/// cascade, so nothing in the store can name this file any more — which is
/// precisely why it has to be dropped from where the deletion was observed
/// rather than by the reconcile.
pub fn spawn_account_drop(account_id: &str) {
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        return;
    };
    let account_id = account_id.to_string();
    handle.spawn(async move {
        if let Err(error) =
            crate::services::agent_runtime::drop_account_credential(&account_id).await
        {
            tracing::warn!(
                %account_id,
                error = %format!("{error:#}"),
                "a deleted agent account left a credential in its bridge"
            );
        }
    });
}

/// Runs `reconcile_local` off the caller's thread.
///
/// The callers are synchronous and hold no runtime of their own: the sync
/// materializer, which has just committed an account row carrying a revocation
/// or a runtime-node row carrying a flag, and startup. Outside a tokio runtime
/// (a migration-only process, a test) there is nothing to spawn onto and the
/// next reconcile does the work instead.
pub fn spawn_reconcile(db: &DbPool) {
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        return;
    };
    let db = db.clone();
    handle.spawn(async move {
        let Ok(Some(node_id)) =
            crate::db::repository::get_setting(&db, crate::db::repository::LOCAL_NODE_ID_SETTING)
        else {
            return;
        };
        if let Err(error) = reconcile_local(&db, &node_id).await {
            tracing::warn!(
                error = %format!("{error:#}"),
                "the agent credential reconcile failed"
            );
        }
    });
}
