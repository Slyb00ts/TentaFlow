// =============================================================================
// Plik: api/dashboard/handlers_my_accounts.rs
// Opis: Handler #17 — MyOAuthAccountsListRequest. Zwraca wpisy widoku
//       "Moje polaczone konta": pary (addon, provider) w trybie `individual`
//       widoczne dla biezacego uzytkownika wraz ze statusem polaczenia.
// =============================================================================

use tentaflow_macros::{handler, observed, policy};
use tentaflow_protocol::{
    MessageBody, MyOAuthAccountsListResponse, MyOAuthEntry, ProtocolError, ProtocolErrorCode,
    SessionAuth,
};

use crate::db::repository;
use crate::dispatch::HandlerContext;

fn db_err(e: impl std::fmt::Display) -> ProtocolError {
    ProtocolError::internal(format!("database error: {}", e))
}

fn current_user_id(ctx: &HandlerContext) -> Option<i64> {
    match &ctx.session {
        SessionAuth::UserSession { user_id, .. } => {
            if user_id[0] != 0xFF {
                return None;
            }
            let mut le = [0u8; 8];
            le.copy_from_slice(&user_id[8..]);
            Some(i64::from_le_bytes(le))
        }
        _ => None,
    }
}

#[handler(variant = "MyOAuthAccountsListRequest", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub fn my_oauth_accounts_list(
    _req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let uid = current_user_id(ctx).ok_or_else(|| {
        ProtocolError::new(ProtocolErrorCode::AuthRequired, "brak user_id w sesji")
    })?;
    let rows = repository::list_my_oauth_entries(&ctx.state.db, uid).map_err(db_err)?;
    let accounts = rows
        .into_iter()
        .map(|r| MyOAuthEntry {
            addon_id: r.addon_id,
            addon_name: r.addon_name,
            addon_icon: r.addon_icon,
            addon_description: r.addon_description,
            addon_version: r.addon_version,
            provider_id: r.provider_id,
            provider_display_name: r.provider_display_name,
            status: r.status,
            account_id: r.account_id,
            account_email: r.account_email,
            account_display_name: r.account_display_name,
            scopes: r.scopes,
            connected_at_epoch: r.connected_at_epoch,
            last_used_at_epoch: r.last_used_at_epoch,
            expires_at_epoch: r.expires_at_epoch,
        })
        .collect();
    Ok(MessageBody::MyOAuthAccountsListResponseBody(
        MyOAuthAccountsListResponse { accounts },
    ))
}
