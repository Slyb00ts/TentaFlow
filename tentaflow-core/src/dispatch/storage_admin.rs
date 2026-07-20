// =============================================================================
// File: dispatch/storage_admin.rs
// Purpose: Admin binary RPCs for Ustawienia → Magazyn danych. Dispatches the
//          `StorageAdminBody` payload family to `services::storage_admin`:
//          overview (paths + sizes + disk), directory browse + mkdir for the
//          picker tree, and migration (live move with progress on the deploy
//          log stream, or a boot-time pending move for data/sync).
// =============================================================================

use tentaflow_macros::{handler, observed, policy};
use tentaflow_protocol::{
    MessageBody, ProtocolError, StorageAdminPayload, StorageCreateDirResponse,
};

use super::HandlerContext;
use crate::services::storage_admin::{self, MigrationDeps, StorageAdminError};

fn map_err(e: StorageAdminError) -> ProtocolError {
    match e {
        StorageAdminError::BadRequest(m) => ProtocolError::bad_request(m),
        StorageAdminError::Internal(m) => ProtocolError::internal(m),
    }
}

#[handler(variant = "StorageAdminBody", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub async fn storage_admin_dispatch(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::StorageAdminBody(p) => p,
        _ => return Err(ProtocolError::bad_request("expected StorageAdminBody")),
    };
    match payload {
        StorageAdminPayload::OverviewRequest => {
            let resp = storage_admin::overview().await;
            Ok(MessageBody::StorageAdminBody(
                StorageAdminPayload::OverviewResponse(resp),
            ))
        }
        StorageAdminPayload::BrowseRequest(r) => {
            let resp = storage_admin::browse(r.path.clone())
                .await
                .map_err(map_err)?;
            Ok(MessageBody::StorageAdminBody(
                StorageAdminPayload::BrowseResponse(resp),
            ))
        }
        StorageAdminPayload::CreateDirRequest(r) => {
            let path = storage_admin::create_dir(&r.parent, &r.name).map_err(map_err)?;
            Ok(MessageBody::StorageAdminBody(
                StorageAdminPayload::CreateDirResponse(StorageCreateDirResponse { path }),
            ))
        }
        StorageAdminPayload::MigrateRequest(r) => {
            let deps = MigrationDeps {
                db: ctx.state.db.clone(),
                port_allocator: ctx.state.port_allocator.clone(),
                settings_cipher: ctx.state.settings_cipher.clone(),
            };
            let resp = storage_admin::start_migration(deps, &r.key, &r.new_path, r.move_data)
                .map_err(map_err)?;
            let _ = crate::db::repository::log_audit(
                &ctx.state.db,
                None,
                None,
                "storage.migrate.request",
                Some("settings"),
                Some(&format!("{} -> {} (move={})", r.key, r.new_path, r.move_data)),
                None,
                Some(&ctx.state.local_node_id),
            );
            Ok(MessageBody::StorageAdminBody(
                StorageAdminPayload::MigrateResponse(resp),
            ))
        }
        StorageAdminPayload::OverviewResponse(_)
        | StorageAdminPayload::BrowseResponse(_)
        | StorageAdminPayload::CreateDirResponse(_)
        | StorageAdminPayload::MigrateResponse(_) => Err(ProtocolError::bad_request(
            "response variant cannot be sent as a request",
        )),
    }
}

macro_rules! register_storage_admin_variant {
    ($variant:literal, $metric:literal) => {
        ::inventory::submit! {
            crate::dispatch::HandlerMeta {
                variant_name: $variant,
                since_major: 1,
                since_minor: 0,
                required_auth: crate::dispatch::SessionAuthKind::Admin,
                metric_name: $metric,
                dispatch_fn: __tentaflow_dispatch_storage_admin_dispatch,
            }
        }
    };
}

register_storage_admin_variant!("StorageOverviewRequest", "tentaflow_ws_handler_storage_overview");
register_storage_admin_variant!("StorageBrowseRequest", "tentaflow_ws_handler_storage_browse");
register_storage_admin_variant!(
    "StorageCreateDirRequest",
    "tentaflow_ws_handler_storage_create_dir"
);
register_storage_admin_variant!("StorageMigrateRequest", "tentaflow_ws_handler_storage_migrate");
