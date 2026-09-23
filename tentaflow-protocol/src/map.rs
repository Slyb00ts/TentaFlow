// =============================================================================
// File: map.rs
// Purpose: Binary CBOR protocol for the shared map — sites (physical places),
//          scenes (one metric reconstruction each) and the devices placed in
//          them. The map belongs to the owning node and lives on its disk; the
//          dashboard is a view, so everything here is metadata and placement,
//          never geometry (chunks travel over the `map:` stream and the mesh
//          chunk pull).
//
//          Append-only, and a rename is the one change that breaks every
//          deployed peer while the round-trip tests stay green — ciborium tags
//          by NAME. A field added later MUST carry `#[serde(default)]`.
// Example: MessageBody::MapBody(MapPayload::SiteListRequest {})
// =============================================================================

use serde::{Deserialize, Serialize};

/// A physical place an organization manages. Holds one or more scenes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct MapSite {
    pub site_id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub address: String,
    /// Optional postal coordinates of the site itself — a pin on a world map,
    /// NOT the georeference of a scene (that is `MapScene::geo_*`).
    pub lat: Option<f64>,
    pub lon: Option<f64>,
    pub alt: Option<f64>,
    /// Filled by the responder; ignored on an upsert.
    #[serde(default)]
    pub scene_count: u32,
    #[serde(default)]
    pub devices_online: u32,
    pub last_update_ms: Option<i64>,
    #[serde(default)]
    pub created_by: String,
    #[serde(default)]
    pub created_at_ms: i64,
    #[serde(default)]
    pub updated_at_ms: i64,
}

/// One coherent reconstruction in ONE metric frame (Z-up, right-handed, metres).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct MapScene {
    pub scene_id: String,
    pub site_id: String,
    pub name: String,
    pub voxel_res_m: f32,
    /// The node that owns the geometry: the only writer of chunks and
    /// placements. Chosen when the scene is created, changed only by an admin.
    pub owner_node_id: String,
    #[serde(default)]
    pub owner_epoch: u32,
    pub geo_lat: Option<f64>,
    pub geo_lon: Option<f64>,
    pub geo_alt: Option<f64>,
    pub geo_heading: Option<f64>,
    #[serde(default)]
    pub max_voxels: i64,
    /// Stats, filled by the responder; ignored on an upsert.
    #[serde(default)]
    pub voxels: i64,
    #[serde(default)]
    pub chunks: i64,
    pub last_update_ms: Option<i64>,
    #[serde(default)]
    pub owner_online: bool,
    /// "owner" on the owning node, otherwise how complete this node's replica
    /// is ("none" | "partial" | "full"). Empty when not computed.
    #[serde(default)]
    pub replica_state: String,
    #[serde(default)]
    pub created_by: String,
    #[serde(default)]
    pub created_at_ms: i64,
    #[serde(default)]
    pub updated_at_ms: i64,
}

/// `T_device_odom→scene` for one device session: the ONLY transform that brings
/// a device's frames into the scene.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct MapPlacement {
    pub tx: f64,
    pub ty: f64,
    pub tz: f64,
    pub qx: f64,
    pub qy: f64,
    pub qz: f64,
    pub qw: f64,
    /// "identity" | "manual" | "icp".
    pub method: String,
    /// A locked placement is never rewritten by automatic drift correction; the
    /// correction is reported as `drift_estimate_m` instead.
    #[serde(default)]
    pub locked: bool,
    #[serde(default)]
    pub confidence: f32,
    #[serde(default)]
    pub set_by: String,
    #[serde(default)]
    pub set_at_ms: i64,
}

/// A device (robot or phone) as the map sees it: `(node_id, device_id)` plus the
/// session whose odometry frame the placement belongs to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct MapDevice {
    pub node_id: String,
    pub device_id: String,
    #[serde(default)]
    pub session_epoch: i64,
    pub scene_id: Option<String>,
    /// "unplaced" | "relocalizing" | "placed" | "lost".
    pub state: String,
    pub placement: Option<MapPlacement>,
    pub last_frame_ms: Option<i64>,
    /// Size of the in-memory drift correction the owner measured but did not
    /// write, because the placement is locked. `None` when there is none.
    pub drift_estimate_m: Option<f32>,
    /// Human-readable name for the UI (node display name / robot alias).
    #[serde(default)]
    pub display_name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum MapPayload {
    SiteListRequest {},
    /// `my_permissions` is what the CALLER may do here (`map.read` /
    /// `map.write` / `map.admin`). The dashboard must not derive it from the
    /// session role: the role→permission mapping lives in the roles table an
    /// administrator can edit, so a client-side copy would drift into showing
    /// buttons the server then refuses.
    SiteListResponse {
        sites: Vec<MapSite>,
        #[serde(default)]
        my_permissions: Vec<String>,
    },
    /// Create when `site.site_id` is empty, otherwise update in place.
    SiteUpsertRequest {
        site: MapSite,
    },
    SiteDeleteRequest {
        site_id: String,
    },
    SiteResponse {
        ok: bool,
        error: Option<String>,
        site: Option<MapSite>,
    },

    SceneListRequest {
        site_id: Option<String>,
    },
    SceneListResponse {
        scenes: Vec<MapScene>,
    },
    SceneUpsertRequest {
        scene: MapScene,
    },
    /// `force` also deletes a scene that still has geometry on disk.
    SceneDeleteRequest {
        scene_id: String,
        force: bool,
    },
    SceneResponse {
        ok: bool,
        error: Option<String>,
        scene: Option<MapScene>,
    },
    /// Hand the scene to another node. `accept_loss` is the operator stating
    /// they know the current owner is unreachable and its unindexed chunks go
    /// with it.
    SceneSetOwnerRequest {
        scene_id: String,
        node_id: String,
        accept_loss: bool,
    },
    /// Georeference the scene. A `None` field clears that component.
    SceneGeoAnchorSetRequest {
        scene_id: String,
        lat: Option<f64>,
        lon: Option<f64>,
        alt: Option<f64>,
        heading: Option<f64>,
    },

    DeviceListRequest {
        scene_id: Option<String>,
    },
    DeviceListResponse {
        devices: Vec<MapDevice>,
    },
    /// Give a device the right to write into a scene. Without this row its
    /// frames stay a live preview and never reach the map.
    DeviceAssignRequest {
        scene_id: String,
        node_id: String,
        device_id: String,
    },
    DeviceUnassignRequest {
        node_id: String,
        device_id: String,
    },
    DeviceResponse {
        ok: bool,
        error: Option<String>,
        device: Option<MapDevice>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message_body::MessageBody;

    #[test]
    fn scene_list_response_roundtrips() {
        let body = MessageBody::MapBody(MapPayload::SceneListResponse {
            scenes: vec![MapScene {
                scene_id: "scn-1".into(),
                site_id: "site-1".into(),
                name: "Parter".into(),
                voxel_res_m: 0.05,
                owner_node_id: "helios".into(),
                owner_epoch: 3,
                geo_lat: Some(52.2),
                geo_lon: Some(21.0),
                geo_alt: None,
                geo_heading: Some(13.5),
                max_voxels: 50_000_000,
                voxels: 1_234_567,
                chunks: 42,
                last_update_ms: Some(1_700_000_000_000),
                owner_online: true,
                replica_state: "owner".into(),
                created_by: "admin".into(),
                created_at_ms: 1,
                updated_at_ms: 2,
            }],
        });
        let mut buf = Vec::new();
        ciborium::into_writer(&body, &mut buf).unwrap();
        let decoded: MessageBody = ciborium::from_reader(buf.as_slice()).unwrap();
        assert_eq!(decoded, body);
    }

    #[test]
    fn device_list_response_roundtrips_with_placement() {
        let body = MessageBody::MapBody(MapPayload::DeviceListResponse {
            devices: vec![MapDevice {
                node_id: "helios".into(),
                device_id: "B42D2000XXXXXXXX".into(),
                session_epoch: 7,
                scene_id: Some("scn-1".into()),
                state: "placed".into(),
                placement: Some(MapPlacement {
                    tx: 1.0,
                    ty: -2.5,
                    tz: 0.0,
                    qx: 0.0,
                    qy: 0.0,
                    qz: 0.7071,
                    qw: 0.7071,
                    method: "manual".into(),
                    locked: true,
                    confidence: 0.8,
                    set_by: "admin".into(),
                    set_at_ms: 5,
                }),
                last_frame_ms: Some(1_700_000_000_000),
                drift_estimate_m: Some(0.07),
                display_name: "Go2 Air".into(),
            }],
        });
        let mut buf = Vec::new();
        ciborium::into_writer(&body, &mut buf).unwrap();
        let decoded: MessageBody = ciborium::from_reader(buf.as_slice()).unwrap();
        assert_eq!(decoded, body);
    }
}
