//! `HistoryListRequest` and `HistoryDiffRequest`: what changed in the
//! structure, and what differs between two days of it.
//!
//! Both are open to every member of the organization; what an answer leaves
//! out for anyone but an administrator (the position history of other people)
//! is decided by `svc::history::Privacy`, and the answer says so
//! (`personal_visible`) so the screen can tell "nothing changed" from "you
//! do not see it".

use tentaflow_protocol::org_structure as wire;
use tentaflow_protocol::org_structure::OrgStructurePayload as P;
use tentaflow_protocol::{MessageBody, ProtocolError};

use super::{db_error, opt_day, read_error, require_member, PERM_ADMIN};
use crate::dispatch::HandlerContext;
use crate::services::org_structure as svc;
use crate::services::org_structure::history::{DiffContext, HistoryQuery, Privacy};

fn non_empty(value: &Option<String>) -> Option<String> {
    value.clone().filter(|v| !v.trim().is_empty())
}

pub(super) async fn list(
    ctx: &HandlerContext,
    from: &Option<String>,
    to: &Option<String>,
    unit_id: &Option<String>,
    offset: u32,
    limit: u32,
) -> Result<MessageBody, ProtocolError> {
    let org = require_member(ctx)?;
    let query = HistoryQuery {
        from: opt_day(from.as_deref())?,
        to: opt_day(to.as_deref())?,
        unit_id: non_empty(unit_id),
        offset: offset as usize,
        limit: limit as usize,
    };
    let (pool, org_id, viewer) = (
        ctx.state.db.clone(),
        org.org_id.clone(),
        org.user_id.clone(),
    );
    let personal_visible = org.has(PERM_ADMIN);
    let page = tokio::task::spawn_blocking(move || {
        let privacy = Privacy {
            personal_visible,
            viewer_user_id: &viewer,
        };
        svc::history::list_changes(&pool, &org_id, &privacy, &query)
    })
    .await
    .map_err(|e| db_error(format!("history task: {e}")))?
    .map_err(read_error)?;
    Ok(MessageBody::OrgStructureBody(P::HistoryListResponse {
        entries: page.entries,
        total: page.total as u32,
        personal_visible,
        today: svc::validate::format_date(page.today),
    }))
}

/// The structure on `day` as the wire carries it.
pub(super) fn view_on(
    ctx: &HandlerContext,
    org_id: &str,
    day: Option<chrono::NaiveDate>,
) -> Result<wire::OrgStructureView, ProtocolError> {
    Ok(svc::query::structure_as_of(&ctx.state.db, org_id, day)
        .map_err(read_error)?
        .into())
}

pub(super) fn diff(
    ctx: &HandlerContext,
    from: &str,
    to: &str,
    unit_id: &Option<String>,
) -> Result<MessageBody, ProtocolError> {
    let org = require_member(ctx)?;
    let (day_from, day_to) = (opt_day(Some(from))?, opt_day(Some(to))?);
    if day_from.is_none() || day_to.is_none() {
        return Err(ProtocolError::bad_request("both days are required"));
    }
    let a = view_on(ctx, &org.org_id, day_from)?;
    let b = view_on(ctx, &org.org_id, day_to)?;
    let personal_visible = org.has(PERM_ADMIN);
    let privacy = Privacy {
        personal_visible,
        viewer_user_id: &org.user_id,
    };
    let unit = non_empty(unit_id);
    let items = svc::history::diff_views(
        &a,
        &b,
        &privacy,
        &DiffContext {
            type_names: svc::history::unit_type_names(&ctx.state.db, &org.org_id)
                .map_err(read_error)?,
            unit_id: unit.as_deref(),
        },
    );
    Ok(MessageBody::OrgStructureBody(P::HistoryDiffResponse {
        from: a.at,
        to: b.at,
        items,
        personal_visible,
    }))
}
