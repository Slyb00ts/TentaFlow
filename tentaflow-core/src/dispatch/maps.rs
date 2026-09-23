// =============================================================================
// File: dispatch/maps.rs — shared map: places, scenes and device placement
// =============================================================================
//
// `MessageBody::MapBody` is the control plane of the persistent map: the places
// an organization manages, the reconstructions inside them, which node owns
// each one and which device may write into it. Geometry never passes through
// here — chunks live on the owning node's disk and reach a viewer over the
// `map:` stream.
//
// Every handler is org-scoped and answers `NotFound` for a row of another
// organization, so error codes cannot be used to probe what else exists. The
// wire tier is `UserSession`; the actual gate is the RBAC permission each
// handler checks (`map.read` / `map.write` / `map.admin`, migration 166),
// because a viewer and an administrator use the same socket.

use tentaflow_macros::{handler, observed, policy};
use tentaflow_protocol::map::{MapPayload, MapScene, MapSite};
use tentaflow_protocol::{MessageBody, ProtocolError, ProtocolErrorCode};

use super::HandlerContext;
use crate::services::rbac::OrgContext;

const PERM_READ: &str = "map.read";
const PERM_WRITE: &str = "map.write";
const PERM_ADMIN: &str = "map.admin";

fn require_org(ctx: &HandlerContext) -> Result<&OrgContext, ProtocolError> {
    ctx.org_context.as_ref().ok_or_else(|| {
        ProtocolError::new(ProtocolErrorCode::AuthRequired, "org context required")
    })
}

fn require_perm<'a>(ctx: &'a HandlerContext, perm: &str) -> Result<&'a OrgContext, ProtocolError> {
    let org = require_org(ctx)?;
    if !org.has(perm) {
        return Err(ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            format!("{perm} permission required"),
        ));
    }
    Ok(org)
}

fn db_error(e: anyhow::Error) -> ProtocolError {
    ProtocolError::new(ProtocolErrorCode::Internal, format!("map: {e:#}"))
}

#[handler(variant = "MapBody", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub async fn map_dispatch(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::MapBody(p) => p,
        _ => return Err(ProtocolError::bad_request("expected MapBody")),
    };
    match payload {
        MapPayload::SiteListRequest {} => site_list(ctx),
        MapPayload::SiteUpsertRequest { site } => site_upsert(ctx, site),
        MapPayload::SiteDeleteRequest { site_id } => site_delete(ctx, site_id),
        MapPayload::SceneListRequest { site_id } => scene_list(ctx, site_id.as_deref()),
        MapPayload::SceneUpsertRequest { scene } => scene_upsert(ctx, scene),
        MapPayload::SceneDeleteRequest { scene_id, force } => scene_delete(ctx, scene_id, *force),
        MapPayload::SceneSetOwnerRequest { scene_id, node_id, .. } => {
            scene_set_owner(ctx, scene_id, node_id)
        }
        MapPayload::SceneGeoAnchorSetRequest {
            scene_id,
            lat,
            lon,
            alt,
            heading,
        } => scene_set_geo(ctx, scene_id, *lat, *lon, *alt, *heading),
        MapPayload::DeviceListRequest { scene_id } => device_list(ctx, scene_id.as_deref()),
        MapPayload::DeviceAssignRequest {
            scene_id,
            node_id,
            device_id,
        } => device_assign(ctx, scene_id, node_id, device_id),
        MapPayload::DeviceUnassignRequest { node_id, device_id } => {
            device_unassign(ctx, node_id, device_id)
        }
        MapPayload::SiteListResponse { .. }
        | MapPayload::SiteResponse { .. }
        | MapPayload::SceneListResponse { .. }
        | MapPayload::SceneResponse { .. }
        | MapPayload::DeviceListResponse { .. }
        | MapPayload::DeviceResponse { .. } => {
            Err(ProtocolError::bad_request("that variant is a reply, not a request"))
        }
    }
}

fn site_list(ctx: &HandlerContext) -> Result<MessageBody, ProtocolError> {
    let org = require_perm(ctx, PERM_READ)?;
    let mut sites =
        crate::db::repository::map_site_list(&ctx.state.db, &org.org_id).map_err(db_error)?;
    let devices =
        crate::db::repository::map_device_list(&ctx.state.db, &org.org_id, None).map_err(db_error)?;
    let scenes =
        crate::db::repository::map_scene_list(&ctx.state.db, &org.org_id, None).map_err(db_error)?;
    for site in &mut sites {
        site.devices_online = scenes
            .iter()
            .filter(|s| s.site_id == site.site_id)
            .flat_map(|s| {
                devices
                    .iter()
                    .filter(move |d| d.scene_id.as_deref() == Some(s.scene_id.as_str()))
            })
            .filter(|d| d.state == "placed" || d.state == "relocalizing")
            .count() as u32;
    }
    let my_permissions = [PERM_READ, PERM_WRITE, PERM_ADMIN]
        .into_iter()
        .filter(|p| org.has(p))
        .map(|p| p.to_string())
        .collect();
    Ok(MessageBody::MapBody(MapPayload::SiteListResponse {
        sites,
        my_permissions,
    }))
}

fn site_upsert(ctx: &HandlerContext, site: &MapSite) -> Result<MessageBody, ProtocolError> {
    let org = require_perm(ctx, PERM_ADMIN)?;
    if site.name.trim().is_empty() {
        return Err(ProtocolError::bad_request("a site needs a name"));
    }
    let stored = crate::db::repository::map_site_upsert(
        &ctx.state.db,
        &org.org_id,
        &org.user_id,
        site,
    )
    .map_err(db_error)?;
    match stored {
        Some(site) => Ok(MessageBody::MapBody(MapPayload::SiteResponse {
            ok: true,
            error: None,
            site: Some(site),
        })),
        None => Err(ProtocolError::new(
            ProtocolErrorCode::NotFound,
            "no such site",
        )),
    }
}

fn site_delete(ctx: &HandlerContext, site_id: &str) -> Result<MessageBody, ProtocolError> {
    let org = require_perm(ctx, PERM_ADMIN)?;
    let deleted = crate::db::repository::map_site_delete(&ctx.state.db, &org.org_id, site_id)
        .map_err(db_error)?;
    if !deleted {
        return Err(ProtocolError::new(
            ProtocolErrorCode::NotFound,
            "no such site",
        ));
    }
    Ok(MessageBody::MapBody(MapPayload::SiteResponse {
        ok: true,
        error: None,
        site: None,
    }))
}

fn scene_list(ctx: &HandlerContext, site_id: Option<&str>) -> Result<MessageBody, ProtocolError> {
    let org = require_perm(ctx, PERM_READ)?;
    let local = ctx.state.local_node_id.as_ref();
    let mut scenes = crate::db::repository::map_scene_list(&ctx.state.db, &org.org_id, site_id)
        .map_err(db_error)?;
    for scene in &mut scenes {
        let owned_here = scene.owner_node_id == local;
        scene.owner_online = owned_here;
        scene.replica_state = if owned_here { "owner" } else { "none" }.to_string();
    }
    Ok(MessageBody::MapBody(MapPayload::SceneListResponse { scenes }))
}

fn scene_upsert(ctx: &HandlerContext, scene: &MapScene) -> Result<MessageBody, ProtocolError> {
    let org = require_perm(ctx, PERM_ADMIN)?;
    if scene.name.trim().is_empty() {
        return Err(ProtocolError::bad_request("a scene needs a name"));
    }
    if scene.voxel_res_m <= 0.0 || scene.voxel_res_m > 1.0 {
        return Err(ProtocolError::bad_request(
            "voxel resolution must be between 0 and 1 m",
        ));
    }
    let mut scene = scene.clone();
    if scene.scene_id.trim().is_empty() && scene.owner_node_id.trim().is_empty() {
        // Created without naming an owner: this node takes it. The admin can
        // hand it over later, but a scene with no owner has no writer at all.
        scene.owner_node_id = ctx.state.local_node_id.as_ref().to_string();
    }
    let stored = crate::db::repository::map_scene_upsert(
        &ctx.state.db,
        &org.org_id,
        &org.user_id,
        &scene,
    )
    .map_err(db_error)?;
    match stored {
        Some(scene) => Ok(MessageBody::MapBody(MapPayload::SceneResponse {
            ok: true,
            error: None,
            scene: Some(scene),
        })),
        None => Err(ProtocolError::new(
            ProtocolErrorCode::NotFound,
            "no such scene or site",
        )),
    }
}

fn scene_delete(
    ctx: &HandlerContext,
    scene_id: &str,
    force: bool,
) -> Result<MessageBody, ProtocolError> {
    let org = require_perm(ctx, PERM_ADMIN)?;
    if crate::db::repository::map_scene_get(&ctx.state.db, &org.org_id, scene_id)
        .map_err(db_error)?
        .is_none()
    {
        return Err(ProtocolError::new(
            ProtocolErrorCode::NotFound,
            "no such scene",
        ));
    }
    if !force
        && crate::db::repository::map_scene_has_geometry(&ctx.state.db, scene_id)
            .map_err(db_error)?
    {
        return Err(ProtocolError::bad_request(
            "the scene still holds mapped geometry — confirm the deletion",
        ));
    }
    crate::db::repository::map_scene_delete(&ctx.state.db, &org.org_id, scene_id)
        .map_err(db_error)?;
    Ok(MessageBody::MapBody(MapPayload::SceneResponse {
        ok: true,
        error: None,
        scene: None,
    }))
}

fn scene_set_owner(
    ctx: &HandlerContext,
    scene_id: &str,
    node_id: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_perm(ctx, PERM_ADMIN)?;
    if node_id.trim().is_empty() {
        return Err(ProtocolError::bad_request("the new owner needs a node id"));
    }
    if !crate::db::repository::map_scene_set_owner(&ctx.state.db, &org.org_id, scene_id, node_id)
        .map_err(db_error)?
    {
        return Err(ProtocolError::new(
            ProtocolErrorCode::NotFound,
            "no such scene",
        ));
    }
    let scene = crate::db::repository::map_scene_get(&ctx.state.db, &org.org_id, scene_id)
        .map_err(db_error)?;
    Ok(MessageBody::MapBody(MapPayload::SceneResponse {
        ok: true,
        error: None,
        scene,
    }))
}

fn scene_set_geo(
    ctx: &HandlerContext,
    scene_id: &str,
    lat: Option<f64>,
    lon: Option<f64>,
    alt: Option<f64>,
    heading: Option<f64>,
) -> Result<MessageBody, ProtocolError> {
    let org = require_perm(ctx, PERM_ADMIN)?;
    if lat.is_some_and(|v| !(-90.0..=90.0).contains(&v))
        || lon.is_some_and(|v| !(-180.0..=180.0).contains(&v))
        || heading.is_some_and(|v| !v.is_finite())
    {
        return Err(ProtocolError::bad_request("the georeference is out of range"));
    }
    if !crate::db::repository::map_scene_set_geo(
        &ctx.state.db,
        &org.org_id,
        scene_id,
        lat,
        lon,
        alt,
        heading,
    )
    .map_err(db_error)?
    {
        return Err(ProtocolError::new(
            ProtocolErrorCode::NotFound,
            "no such scene",
        ));
    }
    let scene = crate::db::repository::map_scene_get(&ctx.state.db, &org.org_id, scene_id)
        .map_err(db_error)?;
    Ok(MessageBody::MapBody(MapPayload::SceneResponse {
        ok: true,
        error: None,
        scene,
    }))
}

fn device_list(ctx: &HandlerContext, scene_id: Option<&str>) -> Result<MessageBody, ProtocolError> {
    let org = require_perm(ctx, PERM_READ)?;
    let devices = crate::db::repository::map_device_list(&ctx.state.db, &org.org_id, scene_id)
        .map_err(db_error)?;
    Ok(MessageBody::MapBody(MapPayload::DeviceListResponse { devices }))
}

fn device_assign(
    ctx: &HandlerContext,
    scene_id: &str,
    node_id: &str,
    device_id: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_perm(ctx, PERM_WRITE)?;
    if node_id.trim().is_empty() || device_id.trim().is_empty() {
        return Err(ProtocolError::bad_request(
            "a device is identified by its node and its id",
        ));
    }
    if !crate::db::repository::map_device_assign(
        &ctx.state.db,
        &org.org_id,
        &org.user_id,
        scene_id,
        node_id,
        device_id,
    )
    .map_err(db_error)?
    {
        return Err(ProtocolError::new(
            ProtocolErrorCode::NotFound,
            "no such scene",
        ));
    }
    let device = crate::db::repository::map_device_list(&ctx.state.db, &org.org_id, Some(scene_id))
        .map_err(db_error)?
        .into_iter()
        .find(|d| d.node_id == node_id && d.device_id == device_id);
    Ok(MessageBody::MapBody(MapPayload::DeviceResponse {
        ok: true,
        error: None,
        device,
    }))
}

fn device_unassign(
    ctx: &HandlerContext,
    node_id: &str,
    device_id: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_perm(ctx, PERM_WRITE)?;
    if !crate::db::repository::map_device_unassign(
        &ctx.state.db,
        &org.org_id,
        node_id,
        device_id,
    )
    .map_err(db_error)?
    {
        return Err(ProtocolError::new(
            ProtocolErrorCode::NotFound,
            "that device is not placed in any scene here",
        ));
    }
    Ok(MessageBody::MapBody(MapPayload::DeviceResponse {
        ok: true,
        error: None,
        device: None,
    }))
}

// `#[handler]` registers the family name, which no frame carries —
// `variant_name_of` reports the concrete request variant, so each one needs its
// own registry entry pointing at the same dispatch wrapper. The tier is
// `UserSession` for all of them; the permission check inside the handler is the
// real gate.
macro_rules! map_handler_entry {
    ($variant:literal, $metric:literal) => {
        ::inventory::submit! {
            crate::dispatch::HandlerMeta {
                variant_name: $variant,
                since_major: 1,
                since_minor: 0,
                required_auth: crate::dispatch::SessionAuthKind::UserSession,
                metric_name: $metric,
                dispatch_fn: __tentaflow_dispatch_map_dispatch,
            }
        }
    };
}

map_handler_entry!("MapSiteListRequest", "tentaflow_ws_handler_map_site_list");
map_handler_entry!("MapSiteUpsertRequest", "tentaflow_ws_handler_map_site_upsert");
map_handler_entry!("MapSiteDeleteRequest", "tentaflow_ws_handler_map_site_delete");
map_handler_entry!("MapSceneListRequest", "tentaflow_ws_handler_map_scene_list");
map_handler_entry!("MapSceneUpsertRequest", "tentaflow_ws_handler_map_scene_upsert");
map_handler_entry!("MapSceneDeleteRequest", "tentaflow_ws_handler_map_scene_delete");
map_handler_entry!("MapSceneSetOwnerRequest", "tentaflow_ws_handler_map_scene_owner");
map_handler_entry!("MapSceneGeoAnchorSetRequest", "tentaflow_ws_handler_map_scene_geo");
map_handler_entry!("MapDeviceListRequest", "tentaflow_ws_handler_map_device_list");
map_handler_entry!("MapDeviceAssignRequest", "tentaflow_ws_handler_map_device_assign");
map_handler_entry!("MapDeviceUnassignRequest", "tentaflow_ws_handler_map_device_unassign");

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch::{state, RequestOrigin};
    use std::collections::HashSet;
    use tentaflow_protocol::SessionAuth;

    /// The organization the fixture database really has — the capture journal
    /// has an FK on `org_id`, so an invented tenant fails every write.
    const ORG: &str = crate::services::org::DEFAULT_ORG_ID;

    /// A context whose org user really exists: the capture journal has an FK on
    /// `actor_user_id`, so a fixture user that was never inserted would fail
    /// every write here for a reason that has nothing to do with the map.
    fn ctx_with(perms: &[&str]) -> HandlerContext {
        ctx_for_user(perms, ORG, "u-1")
    }

    fn ctx_for_user(perms: &[&str], org_id: &str, user_id: &str) -> HandlerContext {
        let state = state::AppState::for_test();
        state
            .db
            .write()
            .expect("test db writer")
            .execute(
                "INSERT OR REPLACE INTO user_accounts \
                 (id, username, password_hash, display_name, is_active, is_admin, \
                  must_change_password, role) \
                 VALUES (?1, ?1, '', ?1, 1, 1, 0, 'admin')",
                rusqlite::params![user_id],
            )
            .expect("seed fixture account");
        HandlerContext {
            session: SessionAuth::UserSession {
                user_id: *uuid::Uuid::new_v4().as_bytes(),
                role: Some("admin".to_string()),
            },
            correlation_id: 1,
            connection_id: 1,
            resume_secret: None,
            state,
            origin: RequestOrigin::Local,
            org_context: Some(OrgContext {
                user_id: user_id.to_string(),
                org_id: org_id.to_string(),
                role_id: "r-1".to_string(),
                permissions: perms.iter().map(|p| p.to_string()).collect::<HashSet<_>>(),
            }),
        }
    }

    /// The same session and database with a different permission set: what an
    /// operator may do is a property of the caller, not of the node.
    fn with_perms(ctx: &HandlerContext, perms: &[&str]) -> HandlerContext {
        let mut next = ctx.clone();
        let org = ctx.org_context.as_ref().expect("fixture has an org");
        next.org_context = Some(OrgContext {
            permissions: perms.iter().map(|p| p.to_string()).collect::<HashSet<_>>(),
            ..org.clone()
        });
        next
    }

    fn site(name: &str) -> MapSite {
        MapSite {
            name: name.to_string(),
            ..Default::default()
        }
    }

    fn created_site(ctx: &HandlerContext, name: &str) -> MapSite {
        match site_upsert(ctx, &site(name)).unwrap() {
            MessageBody::MapBody(MapPayload::SiteResponse { site: Some(s), .. }) => s,
            other => panic!("unexpected reply: {other:?}"),
        }
    }

    fn created_scene(ctx: &HandlerContext, site_id: &str, name: &str) -> MapScene {
        let scene = MapScene {
            site_id: site_id.to_string(),
            name: name.to_string(),
            voxel_res_m: 0.05,
            ..Default::default()
        };
        match scene_upsert(ctx, &scene).unwrap() {
            MessageBody::MapBody(MapPayload::SceneResponse { scene: Some(s), .. }) => s,
            other => panic!("unexpected reply: {other:?}"),
        }
    }

    /// A viewer sees the map and nothing else: the read is served, every write
    /// is `PolicyDenied`. Without this the wire tier (`UserSession`) would be
    /// the only gate, and every logged-in user could delete a building.
    #[test]
    fn a_viewer_reads_the_map_but_changes_nothing() {
        let ctx = ctx_with(&["map.read"]);
        let listed = site_list(&ctx).unwrap();
        match listed {
            MessageBody::MapBody(MapPayload::SiteListResponse {
                sites,
                my_permissions,
            }) => {
                assert!(sites.is_empty());
                assert_eq!(my_permissions, vec!["map.read".to_string()]);
            }
            other => panic!("unexpected reply: {other:?}"),
        }
        for denied in [
            site_upsert(&ctx, &site("Hala")).unwrap_err(),
            site_delete(&ctx, "whatever").unwrap_err(),
            device_assign(&ctx, "scn", "node", "dev").unwrap_err(),
        ] {
            assert_eq!(denied.code, ProtocolErrorCode::PolicyDenied);
        }
    }

    /// An operator places devices but does not create or delete places — that
    /// is an organization-wide decision.
    #[test]
    fn an_operator_places_devices_but_owns_no_places() {
        let admin = ctx_with(&["map.read", "map.write", "map.admin"]);
        let site = created_site(&admin, "Hala");
        let scene = created_scene(&admin, &site.site_id, "Parter");

        // Same node and database as the admin — a fresh fixture would refuse
        // for the trivial reason that it has no scenes.
        let operator = with_perms(&admin, &["map.read", "map.write"]);
        assert_eq!(
            site_upsert(&operator, &site).unwrap_err().code,
            ProtocolErrorCode::PolicyDenied
        );
        assert_eq!(
            scene_set_owner(&operator, &scene.scene_id, "other-node")
                .unwrap_err()
                .code,
            ProtocolErrorCode::PolicyDenied
        );
        let assigned = device_assign(&operator, &scene.scene_id, "node-a", "go2-1").unwrap();
        match assigned {
            MessageBody::MapBody(MapPayload::DeviceResponse { device: Some(d), .. }) => {
                assert_eq!(d.device_id, "go2-1");
                // First device of an empty scene gauges it: identity placement.
                assert_eq!(d.placement.unwrap().method, "identity");
                assert_eq!(d.state, "placed");
            }
            other => panic!("unexpected reply: {other:?}"),
        }
        let unassigned = device_unassign(&operator, "node-a", "go2-1").unwrap();
        assert!(matches!(
            unassigned,
            MessageBody::MapBody(MapPayload::DeviceResponse { ok: true, .. })
        ));
    }

    /// A scene of another organization must read as missing, not as forbidden:
    /// the error code itself would otherwise confirm it exists.
    #[test]
    fn another_organizations_scene_reads_as_missing() {
        let admin = ctx_with(&["map.read", "map.write", "map.admin"]);
        let site = created_site(&admin, "Hala");
        let scene = created_scene(&admin, &site.site_id, "Parter");

        // The SAME node and database, a different tenant — a stranger with his
        // own state would find nothing for the trivial reason that his database
        // is empty, which proves nothing.
        let mut stranger = admin.clone();
        stranger.org_context = Some(OrgContext {
            user_id: "u-2".to_string(),
            org_id: "other-org".to_string(),
            role_id: "r-1".to_string(),
            permissions: ["map.read", "map.write", "map.admin"]
                .iter()
                .map(|p| p.to_string())
                .collect::<HashSet<_>>(),
        });
        assert_eq!(
            scene_delete(&stranger, &scene.scene_id, true).unwrap_err().code,
            ProtocolErrorCode::NotFound
        );
        assert_eq!(
            scene_set_geo(&stranger, &scene.scene_id, Some(52.0), Some(21.0), None, None)
                .unwrap_err()
                .code,
            ProtocolErrorCode::NotFound
        );
        match scene_list(&stranger, None).unwrap() {
            MessageBody::MapBody(MapPayload::SceneListResponse { scenes }) => {
                assert!(scenes.is_empty(), "no scene of another org may be listed");
            }
            other => panic!("unexpected reply: {other:?}"),
        }
    }

    /// Handing a scene over bumps `owner_epoch`, which is what lets a peer
    /// recognize a late write from the previous owner as stale.
    #[test]
    fn handing_a_scene_over_bumps_the_owner_epoch() {
        let ctx = ctx_with(&["map.read", "map.write", "map.admin"]);
        let site = created_site(&ctx, "Hala");
        let scene = created_scene(&ctx, &site.site_id, "Parter");
        assert_eq!(scene.owner_epoch, 1);

        match scene_set_owner(&ctx, &scene.scene_id, "node-b").unwrap() {
            MessageBody::MapBody(MapPayload::SceneResponse { scene: Some(s), .. }) => {
                assert_eq!(s.owner_node_id, "node-b");
                assert_eq!(s.owner_epoch, 2);
            }
            other => panic!("unexpected reply: {other:?}"),
        }
    }

    /// Deleting a mapped scene needs the second answer: the first refusal is
    /// the only thing between a mis-click and a building's geometry.
    #[test]
    fn deleting_a_scene_with_geometry_needs_force() {
        let ctx = ctx_with(&["map.read", "map.write", "map.admin"]);
        let site = created_site(&ctx, "Hala");
        let scene = created_scene(&ctx, &site.site_id, "Parter");
        {
            let conn = ctx.state.db.write().expect("db writer");
            conn.execute(
                "INSERT INTO map_chunks \
                   (scene_id, chunk_key, revision, owner_epoch, sha256, size_bytes, occupied, \
                    updated_hlc) \
                 VALUES (?1, '0_0_0', 1, 1, 'abc', 10, 5, 'hlc')",
                rusqlite::params![scene.scene_id],
            )
            .unwrap();
        }
        assert_eq!(
            scene_delete(&ctx, &scene.scene_id, false).unwrap_err().code,
            ProtocolErrorCode::BadRequest
        );
        assert!(matches!(
            scene_delete(&ctx, &scene.scene_id, true).unwrap(),
            MessageBody::MapBody(MapPayload::SceneResponse { ok: true, .. })
        ));
    }
}
