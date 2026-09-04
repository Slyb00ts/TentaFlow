// =============================================================================
// File: tentaquant.rs
// Purpose: Binary CBOR protocol for TentaQuant — the quantum lab application.
//          One lab is one INSTANCE of the `tentaquant` package, so every
//          request but `LabListRequest` names the instance it means through
//          `instance_id`; the server proves that id is an enabled instance of
//          this package and evaluates THAT instance's permission matrix before
//          anything else happens.
//
//          Lab membership is not modelled here: it is the instance's
//          permission matrix in Addons INTERSECTED with that instance's
//          Visibility (plan §10.1-10.2) — `quant.read` defaults to allow, so
//          the matrix alone would admit the whole organization and Visibility
//          is what scopes a lab to its group. There are deliberately no
//          variants for granting permissions — `AddonPermission*` and the
//          Visibility tab already do that — and `LabPeople*` is a READ of that
//          intersection.
//
//          Project ownership follows ML Studio: the creator owns the project,
//          it is private by default and reachable only through an explicit
//          share (`editor`/`viewer`) or `visibility = "lab"` (read-only).
// Example: MessageBody::TentaQuantBody(TentaQuantPayload::LabListRequest {})
// =============================================================================

use serde::{Deserialize, Serialize};

/// The six permission ids of `app-manifest.toml` (plan §10.2), in the order the
/// UI shows them. Wire responses carry granted subsets of exactly these.
pub const PERMISSION_IDS: [&str; 6] = [
    "quant.read",
    "quant.run",
    "quant.run.gpu",
    "quant.run.qpu",
    "quant.instruct",
    "quant.admin",
];

/// One lab (= one installed instance of the package) the caller may enter.
///
/// There is no `owner` field and there will not be one: a lab is not owned
/// (§18 decision 26). What the tile shows instead is how many people the lab
/// admits — the matrix intersected with the instance's Visibility — and which
/// permissions the caller holds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LabInfo {
    /// The instance `addon_id` (`tentaquant-<8hex>`), which every other request
    /// of the family carries.
    pub instance_id: String,
    pub display_name: String,
    /// A disabled instance is listed for admins only, and refuses every other
    /// request — the tile has to be able to say WHY it is inert.
    pub enabled: bool,
    /// Granted subset of [`PERMISSION_IDS`] for the calling user, resolved
    /// through the permission checker.
    pub my_permissions: Vec<String>,
    /// People this instance admits: granted `quant.read` by the matrix AND
    /// inside the instance's Visibility.
    pub people_count: u32,
    /// Projects visible to the CALLER (own + shared + lab), never the total.
    pub project_count: u32,
    /// Last content change in this lab visible to the caller, RFC3339-ish
    /// SQLite datetime, or absent for an untouched lab.
    pub last_activity_at: Option<String>,
    pub nodes: Vec<LabNodeInfo>,
}

/// A node of the fleet as this instance's reconcile left it. `instance_status`
/// is the platform's `__node_status/<node_id>` row ("ready" | "unsupported" |
/// "init_error" | "unknown"), not a live probe.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LabNodeInfo {
    pub node_id: String,
    pub node_name: String,
    pub is_local: bool,
    pub online: bool,
    pub instance_status: String,
}

/// One person the instance admits — granted `quant.read` by the matrix and
/// inside the instance's Visibility — with the permissions they actually
/// resolve to. Purely derived: there is no membership table to disagree with.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LabPersonInfo {
    pub user_id: String,
    pub display_name: String,
    pub permissions: Vec<String>,
}

/// Instance-wide settings (`settings` table, plan §9.2). Sent and stored whole:
/// the form edits one document, and a partial write would silently reset the
/// fields an older dashboard build does not know about.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LabSettings {
    /// Course ranking, on by default (§18 decision 20); `quant.instruct` may
    /// turn it off.
    pub ranking_enabled: bool,
    /// Qubit ceilings per execution tier (§4.2). Browser is the wasm32 limit,
    /// core the native state-vector limit, python the service, gpu the node.
    pub max_qubits_browser: u32,
    pub max_qubits_core: u32,
    pub max_qubits_python: u32,
    pub max_qubits_gpu: u32,
    /// Tier a new notebook starts on: "browser" | "core" | "python" | "gpu".
    pub default_tier: String,
    /// Kernel session idle TTL and cell time limits (§3.2 state machine).
    pub kernel_idle_ttl_secs: u32,
    pub cell_timeout_secs: u32,
    pub gpu_cell_timeout_secs: u32,
}

/// The half of the settings document `quant.admin` owns alone (§10.2): how
/// untrusted code is isolated, who accepted running it natively, and how long
/// run artifacts are kept. Kept apart from [`LabSettings`] because the wire
/// answer must be able to OMIT it — a member reads the qubit ceilings (the
/// `device="auto"` rule of §4.2 needs them) without reading the lab's
/// isolation posture.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LabAdminSettings {
    /// "container" | "trusted_native" (§18 decision 4).
    pub isolation_mode: String,
    /// Run-artifact retention (§9.4).
    pub retention_days: u32,
    /// Who accepted `trusted_native` for a multi-person lab and when
    /// ("<user_id> <timestamp>"), absent while nobody has.
    pub trusted_native_ack: Option<String>,
}

impl Default for LabSettings {
    /// The defaults a freshly installed lab runs with, mirrored by the
    /// `settings` seed of the instance database.
    fn default() -> Self {
        Self {
            ranking_enabled: true,
            max_qubits_browser: 24,
            max_qubits_core: 28,
            max_qubits_python: 28,
            max_qubits_gpu: 30,
            default_tier: "core".to_string(),
            kernel_idle_ttl_secs: 1800,
            cell_timeout_secs: 300,
            gpu_cell_timeout_secs: 900,
        }
    }
}

impl Default for LabAdminSettings {
    /// Untrusted code is containerised until an admin says otherwise, and run
    /// artifacts live half a year (§9.4).
    fn default() -> Self {
        Self {
            isolation_mode: "container".to_string(),
            retention_days: 180,
            trusted_native_ack: None,
        }
    }
}

/// A project row as the caller sees it: `my_role` is resolved server-side from
/// ownership, shares and visibility, so the UI never re-derives access.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectInfo {
    pub project_id: String,
    pub name: String,
    pub description: String,
    pub owner_user_id: String,
    pub owner_name: String,
    /// "private" | "lab".
    pub visibility: String,
    /// "owner" | "editor" | "viewer" | "none". `none` is the answer to a
    /// transfer that left the caller without any access to the project they
    /// just handed over; every other response names a role they hold. It can
    /// only ever reach a client on that one `ProjectResponse` — a project the
    /// caller has no role in is filtered out of every list and answers
    /// `NotFound` on a get — so a UI reads it as "you no longer have this
    /// project", not as a role to render.
    pub my_role: String,
    pub share_count: u32,
    pub file_count: u32,
    pub notebook_count: u32,
    pub run_count: u32,
    /// Project Studio project this one is linked to, when any (§13.7).
    pub linked_project_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub archived_at: Option<String>,
}

/// One share row. `has_lab_access` is false when the person is not in the lab
/// — the matrix withholds `quant.read` or the instance's Visibility does not
/// reach them — and the share is then dormant, which the UI has to say instead
/// of showing an access that does not exist.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectShareInfo {
    pub user_id: String,
    pub display_name: String,
    /// "editor" | "viewer".
    pub role: String,
    pub granted_by: String,
    pub granted_at: String,
    pub has_lab_access: bool,
}

/// A file of a project. The bytes live in the instance CAS (`files/<sha256>`);
/// the row is the name under which the project refers to them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileInfo {
    pub file_id: String,
    pub project_id: String,
    pub path: String,
    /// "notebook" | "py" | "qasm" | "data" | "md".
    pub kind: String,
    pub sha256: String,
    pub size_bytes: u64,
    pub updated_at: String,
}

/// Notebook head. Cells travel separately (`NotebookGetResponse.cells_json`)
/// because the list view needs the head without the content.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NotebookInfo {
    pub notebook_id: String,
    pub project_id: String,
    pub file_id: String,
    pub name: String,
    /// Version the head currently holds; `NotebookSaveRequest.expected_version`
    /// must equal it or the save is a conflict.
    pub current_version: u32,
    pub updated_by: String,
    pub updated_at: String,
}

/// One append-only notebook version. The cells of an old version are fetched
/// with `NotebookGetRequest { version: Some(v) }`, so the list stays cheap.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NotebookVersionInfo {
    pub version: u32,
    pub sha256: String,
    pub author: String,
    pub created_at: String,
}

/// TentaQuant message family (request + response). ciborium encodes variants
/// external-tagged by variant NAME, so never rename a variant or a field
/// without updating the frontend and the golden test (`tentaquant_wire_golden`).
/// The enum is append-only: add at the end, never insert or reorder, and give
/// every new field `#[serde(default)]` so peers that omit it still decode.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum TentaQuantPayload {
    // ---- Lab (the instance itself) ----
    /// The only request WITHOUT an `instance_id`: it is what discovers them.
    LabListRequest {},
    LabListResponse {
        labs: Vec<LabInfo>,
        local_node_id: String,
        /// True when the caller may install another instance from the catalog,
        /// which is what the "+ new lab" tile needs to know.
        can_create: bool,
    },
    LabOverviewRequest {
        instance_id: String,
    },
    LabOverviewResponse {
        instance_id: String,
        /// Projects the caller owns.
        my_projects: u32,
        /// Projects shared with the caller by name.
        shared_with_me: u32,
        /// Projects published to the whole lab (`visibility = "lab"`).
        lab_projects: u32,
        /// Runs of the caller in the last seven days, split by outcome.
        runs_7d_total: u32,
        runs_7d_succeeded: u32,
        runs_7d_failed: u32,
        runs_7d_running: u32,
        /// People the lab admits: `quant.read` from the matrix, inside the
        /// instance's Visibility.
        people_with_access: u32,
        last_activity_at: Option<String>,
    },
    /// Matrix expansion, for `quant.instruct` only.
    LabPeopleRequest {
        instance_id: String,
    },
    LabPeopleResponse {
        instance_id: String,
        people: Vec<LabPersonInfo>,
    },
    SettingsGetRequest {
        instance_id: String,
    },
    SettingsSetRequest {
        instance_id: String,
        settings: LabSettings,
        /// Present only when the caller edits the admin half; a supervisor
        /// sends `None` and never has to echo values it may not read.
        #[serde(default)]
        admin: Option<LabAdminSettings>,
    },
    /// Answer to both `SettingsGetRequest` and `SettingsSetRequest` — the
    /// stored document after the operation, so a rejected field never leaves
    /// the form showing a value the server did not keep. `admin` is filled
    /// only for a caller holding `quant.admin`; for everyone else the admin
    /// half is absent rather than faked with defaults.
    SettingsResponse {
        instance_id: String,
        settings: LabSettings,
        #[serde(default)]
        admin: Option<LabAdminSettings>,
    },

    // ---- Projects ----
    ProjectListRequest {
        instance_id: String,
        #[serde(default)]
        include_archived: bool,
    },
    ProjectListResponse {
        instance_id: String,
        projects: Vec<ProjectInfo>,
    },
    ProjectGetRequest {
        instance_id: String,
        project_id: String,
    },
    ProjectGetResponse {
        instance_id: String,
        project: ProjectInfo,
        /// Empty for anyone but the owner: who a project is shared with is the
        /// owner's business.
        shares: Vec<ProjectShareInfo>,
    },
    ProjectCreateRequest {
        instance_id: String,
        name: String,
        #[serde(default)]
        description: String,
        /// "private" | "lab"; publishing to the lab needs `quant.instruct`.
        visibility: String,
        #[serde(default)]
        linked_project_id: Option<String>,
    },
    ProjectUpdateRequest {
        instance_id: String,
        project_id: String,
        name: String,
        #[serde(default)]
        description: String,
        visibility: String,
        #[serde(default)]
        linked_project_id: Option<String>,
    },
    ProjectArchiveRequest {
        instance_id: String,
        project_id: String,
        archived: bool,
    },
    /// Hands the project to another member; the previous owner keeps no
    /// implicit access (the new owner may share it back).
    ProjectTransferRequest {
        instance_id: String,
        project_id: String,
        new_owner_user_id: String,
    },
    /// Answer to create/update/archive/transfer: the project as it now stands.
    ProjectResponse {
        instance_id: String,
        project: ProjectInfo,
    },
    ProjectDeleteRequest {
        instance_id: String,
        project_id: String,
    },
    ProjectDeleteResponse {
        instance_id: String,
        project_id: String,
    },
    ProjectShareSetRequest {
        instance_id: String,
        project_id: String,
        user_id: String,
        /// "editor" | "viewer".
        role: String,
    },
    ProjectShareRemoveRequest {
        instance_id: String,
        project_id: String,
        user_id: String,
    },
    ProjectSharesResponse {
        instance_id: String,
        project_id: String,
        shares: Vec<ProjectShareInfo>,
    },

    // ---- Files (CAS, 4 MiB chunks) ----
    FileUploadChunkRequest {
        instance_id: String,
        project_id: String,
        /// Client-chosen stream id; chunks must arrive in order and `seq == 0`
        /// restarts the stream.
        upload_id: String,
        path: String,
        kind: String,
        seq: u32,
        total_chunks: u32,
        bytes: Vec<u8>,
    },
    FileUploadChunkResponse {
        instance_id: String,
        project_id: String,
        upload_id: String,
        received_chunks: u32,
        received_bytes: u64,
        complete: bool,
        /// Present only on the chunk that completed the file.
        file: Option<FileInfo>,
    },
    FileListRequest {
        instance_id: String,
        project_id: String,
    },
    FileListResponse {
        instance_id: String,
        project_id: String,
        files: Vec<FileInfo>,
    },
    FileDeleteRequest {
        instance_id: String,
        project_id: String,
        file_id: String,
    },
    FileDeleteResponse {
        instance_id: String,
        project_id: String,
        file_id: String,
    },

    // ---- Notebooks (append-only versions, optimistic locking) ----
    NotebookListRequest {
        instance_id: String,
        project_id: String,
    },
    NotebookListResponse {
        instance_id: String,
        project_id: String,
        notebooks: Vec<NotebookInfo>,
    },
    NotebookCreateRequest {
        instance_id: String,
        project_id: String,
        name: String,
        /// JSON array of cells; an empty string starts an empty notebook.
        #[serde(default)]
        cells_json: String,
    },
    NotebookGetRequest {
        instance_id: String,
        project_id: String,
        notebook_id: String,
        /// Absent = the head version.
        #[serde(default)]
        version: Option<u32>,
    },
    NotebookGetResponse {
        instance_id: String,
        notebook: NotebookInfo,
        /// Cells of the requested version.
        version: u32,
        cells_json: String,
    },
    NotebookSaveRequest {
        instance_id: String,
        project_id: String,
        notebook_id: String,
        cells_json: String,
        /// Version the editor last read. A mismatch is a `Conflict`, never a
        /// silent overwrite.
        expected_version: u32,
    },
    /// Answer to create/save: the notebook head after the write.
    NotebookResponse {
        instance_id: String,
        notebook: NotebookInfo,
    },
    NotebookVersionsRequest {
        instance_id: String,
        project_id: String,
        notebook_id: String,
    },
    NotebookVersionsResponse {
        instance_id: String,
        notebook_id: String,
        versions: Vec<NotebookVersionInfo>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MessageBody;

    fn hex_bytes(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("valid hex"))
            .collect()
    }

    fn round_trip(payload: TentaQuantPayload) {
        let bytes = crate::cbor::encode(&payload).expect("encode");
        let decoded = crate::cbor::decode::<TentaQuantPayload>(&bytes).expect("decode");
        assert_eq!(decoded, payload);
    }

    #[test]
    fn lab_family_round_trips() {
        round_trip(TentaQuantPayload::LabListRequest {});
        round_trip(TentaQuantPayload::LabListResponse {
            labs: vec![LabInfo {
                instance_id: "tentaquant-0a1b2c3d".to_string(),
                display_name: "Kwanty R&D".to_string(),
                enabled: true,
                my_permissions: vec!["quant.read".to_string(), "quant.run".to_string()],
                people_count: 42,
                project_count: 3,
                last_activity_at: Some("2026-09-03 14:02:00".to_string()),
                nodes: vec![LabNodeInfo {
                    node_id: "node-1".to_string(),
                    node_name: "spark-01".to_string(),
                    is_local: true,
                    online: true,
                    instance_status: "ready".to_string(),
                }],
            }],
            local_node_id: "node-1".to_string(),
            can_create: false,
        });
        round_trip(TentaQuantPayload::SettingsResponse {
            instance_id: "tentaquant-0a1b2c3d".to_string(),
            settings: LabSettings::default(),
            admin: Some(LabAdminSettings::default()),
        });
    }

    #[test]
    fn project_and_notebook_families_round_trip() {
        round_trip(TentaQuantPayload::ProjectCreateRequest {
            instance_id: "tentaquant-0a1b2c3d".to_string(),
            name: "Grover 4-kubitowy".to_string(),
            description: String::new(),
            visibility: "private".to_string(),
            linked_project_id: None,
        });
        round_trip(TentaQuantPayload::NotebookSaveRequest {
            instance_id: "tentaquant-0a1b2c3d".to_string(),
            project_id: "p1".to_string(),
            notebook_id: "n1".to_string(),
            cells_json: "[]".to_string(),
            expected_version: 3,
        });
        round_trip(TentaQuantPayload::FileUploadChunkRequest {
            instance_id: "tentaquant-0a1b2c3d".to_string(),
            project_id: "p1".to_string(),
            upload_id: "up-1".to_string(),
            path: "data/bell.json".to_string(),
            kind: "data".to_string(),
            seq: 0,
            total_chunks: 1,
            bytes: vec![0x00, 0xff, 0x10],
        });
    }

    #[test]
    fn message_body_tentaquant_round_trip() {
        let body = MessageBody::TentaQuantBody(TentaQuantPayload::LabOverviewRequest {
            instance_id: "tentaquant-0a1b2c3d".to_string(),
        });
        let bytes = crate::cbor::encode(&body).expect("encode");
        let decoded = crate::cbor::decode::<MessageBody>(&bytes).expect("decode");
        assert_eq!(decoded, body);
    }

    /// Golden wire snapshot: ciborium encodes enum variants as a 1-element map
    /// keyed by the variant NAME (external tagging). Pinning exact bytes turns
    /// any accidental rename of a variant, a field or the
    /// `MessageBody::TentaQuantBody` tag into a test failure instead of a
    /// silent break of every deployed peer.
    #[test]
    fn tentaquant_wire_golden() {
        // TentaQuantPayload::LabListRequest {} — the family's entry point and
        // the one request without an instance id.
        let list = TentaQuantPayload::LabListRequest {};
        let bytes = crate::cbor::encode(&list).expect("encode");
        assert_eq!(
            bytes,
            hex_bytes("a16e4c61624c69737452657175657374a0"),
            "LabListRequest wire drift"
        );

        // MessageBody::TentaQuantBody(LabListRequest) — outer body tag + variant tag.
        let body = MessageBody::TentaQuantBody(list);
        let bytes = crate::cbor::encode(&body).expect("encode");
        assert_eq!(
            bytes,
            hex_bytes("a16e54656e74615175616e74426f6479a16e4c61624c69737452657175657374a0"),
            "MessageBody::TentaQuantBody wire drift"
        );

        // Every other request carries the lab it means — pin that field name.
        let overview = TentaQuantPayload::LabOverviewRequest {
            instance_id: "tentaquant-0a1b2c3d".to_string(),
        };
        let bytes = crate::cbor::encode(&overview).expect("encode");
        assert_eq!(
            bytes,
            hex_bytes(
                "a1724c61624f7665727669657752657175657374a16b696e7374616e63655f69647374656e74617175616e742d3061316232633364"
            ),
            "LabOverviewRequest wire drift"
        );

        // The optimistic-locking save — full field set, `expected_version` last.
        let save = TentaQuantPayload::NotebookSaveRequest {
            instance_id: "tentaquant-0a1b2c3d".to_string(),
            project_id: "p1".to_string(),
            notebook_id: "n1".to_string(),
            cells_json: "[]".to_string(),
            expected_version: 3,
        };
        let bytes = crate::cbor::encode(&save).expect("encode");
        assert_eq!(
            bytes,
            hex_bytes(
                "a1734e6f7465626f6f6b5361766552657175657374a56b696e7374616e63655f69647374656e74617175616e742d30613162326333646a70726f6a6563745f69646270316b6e6f7465626f6f6b5f6964626e316a63656c6c735f6a736f6e625b5d7065787065637465645f76657273696f6e03"
            ),
            "NotebookSaveRequest wire drift"
        );

        // The settings document with its defaults: pins every field name AND
        // the default values a fresh lab starts with, in both halves.
        let settings = TentaQuantPayload::SettingsResponse {
            instance_id: "tentaquant-0a1b2c3d".to_string(),
            settings: LabSettings::default(),
            admin: Some(LabAdminSettings::default()),
        };
        let bytes = crate::cbor::encode(&settings).expect("encode");
        assert_eq!(
            bytes,
            hex_bytes(
                "a17053657474696e6773526573706f6e7365a36b696e7374616e63655f69647374656e74617175616e742d30613162326333646873657474696e6773a96f72616e6b696e675f656e61626c6564f5726d61785f7175626974735f62726f7773657218186f6d61785f7175626974735f636f7265181c716d61785f7175626974735f707974686f6e181c6e6d61785f7175626974735f677075181e6c64656661756c745f7469657264636f7265746b65726e656c5f69646c655f74746c5f736563731907087163656c6c5f74696d656f75745f7365637319012c756770755f63656c6c5f74696d656f75745f736563731903846561646d696ea36e69736f6c6174696f6e5f6d6f646569636f6e7461696e65726e726574656e74696f6e5f6461797318b472747275737465645f6e61746976655f61636bf6"
            ),
            "SettingsResponse wire drift"
        );
    }
}
