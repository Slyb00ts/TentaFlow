// ===== File: dispatch/app_route.rs — platform node routing (plan §3.1) =====
//
// An app-family dashboard request may be addressed to another fleet node
// (`Routing::Forward` in the envelope): an admin manages ANY node from ANY
// dashboard, and hardware-facing apps (TentaNas) act on the node that owns the
// hardware. The node showing the dashboard is a proxy with NO decision logic:
// it mints a `SessionAssertion` for the person acting, sends the request's
// CBOR bytes verbatim over the trusted mesh, and relays whatever the executing
// node answered.
//
// The executing node verifies the assertion (identity only, never authority),
// rebuilds the actor's context from ITS OWN database — account row, role, org
// membership, permission matrix — and runs the ordinary `dispatch()` pipeline,
// so every gate (`#[policy]` tier, app gate, matrix check) applies exactly as
// if the request had arrived locally. Blind trust in the forwarding node is
// forbidden by design: a compromised peer must not gain more than its own
// local authority.
//
// This module transports `MessageBody` values, one request/response pair per
// call. Streaming subscriptions are NOT forwardable here — they need a relayed
// stream (mesh stream hub), not a oneshot.

use std::sync::Arc;

use tentaflow_protocol::mesh::{MeshCommandResponsePayload, MeshCommandType, SessionAssertion};
use tentaflow_protocol::{MessageBody, ProtocolError, ProtocolErrorCode};

use crate::code_studio::assertion;
use crate::code_studio::remote_proxy;
use crate::dispatch::HandlerContext;
use crate::mesh::iroh_manager::IrohMeshManager;

/// How long the origin waits for the executing node to run one request. App
/// requests can cold-start a native app or a WASM instance, so this mirrors
/// Code Studio's figure rather than the short command timeouts.
const OP_TIMEOUT_SECS: u64 = 45;

/// The assertion's `workspace` field for app routing. Domain separation: an
/// assertion minted for a Code Studio workspace must never authorize an
/// `AppRouteOp` (and vice versa) even if its payload bytes happened to decode
/// both ways — the executing side rejects any other value.
const APP_ROUTE_SCOPE: &str = "app-route";

fn unreachable(node_id: &str, detail: impl std::fmt::Display) -> ProtocolError {
    // Connectivity, not state: nothing about this is persisted.
    ProtocolError::new(
        ProtocolErrorCode::NodeUnreachable,
        format!("node '{node_id}' did not answer: {detail}"),
    )
}

fn decode_body(bytes: &[u8]) -> Result<MessageBody, ProtocolError> {
    tentaflow_protocol::cbor::decode(bytes)
        .map_err(|e| ProtocolError::bad_request(format!("message body decode failed: {e}")))
}

fn encode_body(body: &MessageBody) -> Result<Vec<u8>, ProtocolError> {
    tentaflow_protocol::cbor::encode(body)
        .map_err(|e| ProtocolError::internal(format!("message body encode failed: {e}")))
}

// =============================================================================
// Naming a node
// =============================================================================

/// THE PEER STORE NEVER HOLDS THIS NODE. `seed_discovered_peer` returns early
/// on `node_id == local_node_id`, so asking it for the local hostname always
/// answers `None` and the fallback used to publish the 64-hex node id as the
/// node's NAME — on every install, in every fleet view. The machine's own
/// hostname is what the rest of the core already shows for itself, so it is
/// what this asks first for the local row.
///
/// A peer the store has no hostname for (paired, offline since this node
/// started) falls back to the name the sync registry recorded for it, and
/// then to NOTHING. An empty name is deliberate: the 64-hex node id used to
/// fill that gap and was printed as the node's name on every fleet surface,
/// while the screen has one helper (`nodeLabel`) that says "Węzeł bez nazwy"
/// and keeps the id in a tooltip.
pub(crate) fn node_display_name(ctx: &HandlerContext, node_id: &str) -> String {
    node_display_name_in(&ctx.state, node_id)
}

/// `node_display_name` for a caller that holds the state rather than the
/// request (a blocking task: the sync registry is a SQLite read).
pub(crate) fn node_display_name_in(state: &super::state::AppState, node_id: &str) -> String {
    if node_id == state.local_node_id.to_string() {
        let local = crate::mesh::node_info_collector::local_hostname();
        if !local.is_empty() && local != "unknown" {
            return local;
        }
    }
    state
        .mesh_peer_store
        .get_hostname(node_id)
        .filter(|n| !n.is_empty())
        .or_else(|| registry_name_in(state, node_id))
        .unwrap_or_default()
}

/// `sync_nodes.display_name` of one node, when the registry has a non-empty
/// one. Asked only after the peer store came up empty, so a healthy fleet
/// never pays for it.
pub(crate) fn registry_name(ctx: &HandlerContext, node_id: &str) -> Option<String> {
    registry_name_in(&ctx.state, node_id)
}

pub(crate) fn registry_name_in(state: &super::state::AppState, node_id: &str) -> Option<String> {
    crate::db::repository::lookup_sync_node_info(&state.db, &[node_id.to_string()])
        .ok()?
        .remove(node_id)
        .map(|(name, _)| name.trim().to_string())
        .filter(|name| !name.is_empty())
}

// =============================================================================
// Origin side — the proxy
// =============================================================================

/// The forwarded requests that only read, by their `variant_name_of` name. A
/// dashboard polls these every few seconds, so their audit rows are thinned
/// (`audit_forward`, `execute_audit_due`). Listed by name, not guessed from
/// it: a name that reads like a query can change data
/// (`ProjectStudioCasesBulkStatusRequest` sets the status of many cases), and
/// a request missing here is audited every time. Sorted, for the lookup.
const READ_ONLY_VARIANTS: &[&str] = &[
    "AgentRunDetailRequest",
    "AgentRunsListRequest",
    "AgentsDetailRequest",
    "AgentsListRequest",
    "AliasConsumerListRequest",
    "ApiKeyListRequest",
    "ApiKeyScopeListRequest",
    "AppsListRequest",
    "AuditLogListRequest",
    "BaselineAdoptStatusRequest",
    "BaselineDonorListRequest",
    "BenchmarkGetRequest",
    "BenchmarkListRequest",
    "BusAclListRequest",
    "BusDlqListRequest",
    "BusFieldPolicyListRequest",
    "BusFieldPolicyPreviewRequest",
    "BusGroupDetailRequest",
    "BusGroupListRequest",
    "BusQuotaGetRequest",
    "BusReplicaListRequest",
    "BusSchemaDerivedGetRequest",
    "BusSchemaFieldsGetRequest",
    "BusSchemaForRecordGetRequest",
    "BusSchemaGetRequest",
    "BusSchemaSubjectListRequest",
    "BusSchemaVersionListRequest",
    "BusStatsSnapshotRequest",
    "BusTopicDetailRequest",
    "BusTopicListRequest",
    "CatalogListRequest",
    "ClusterDetailRequest",
    "ClusterListRequest",
    "CodeStudioApprovalsListRequest",
    "CodeStudioCodeSearchRequest",
    "CodeStudioGitStatusRequest",
    "CodeStudioIndexStatusRequest",
    "CodeStudioPatchBlobGetRequest",
    "CodeStudioProjectLinkListRequest",
    "CodeStudioSessionGrantsListRequest",
    "CodeStudioSessionsListRequest",
    "CodeStudioTerminalSnapshotRequest",
    "CodeStudioTerminalsListRequest",
    "CodeStudioWorkspaceGetRequest",
    "CodeStudioWorkspacesListRequest",
    "CodeStudioWorktreesListRequest",
    "ContainerListRequest",
    "DeploymentListRequest",
    "DeploymentStatusRequest",
    "FastPathListRequest",
    "FlowDetailRequest",
    "FlowExecutionsListRequest",
    "FlowListRequest",
    "FlowNodeTemplatesListRequest",
    "FlowVersionGetRequest",
    "FlowVersionListRequest",
    "HubEngineListRequest",
    "HubModelSearchRequest",
    "LegalDocumentsListRequest",
    "MapDeviceListRequest",
    "MapSceneListRequest",
    "MapSiteListRequest",
    "MePreferencesGetRequest",
    "MeetingSessionListRequest",
    "MeshNodeDetailRequest",
    "MeshNodeListRequest",
    "MeshPeersListRequest",
    "MeshPendingListRequest",
    "MeshServicesListRequest",
    "MeshTrustedListRequest",
    "MlStudioSchemaGetRequest",
    "ModelAliasListRequest",
    "ModelConsumerListRequest",
    "ModelDetailRequest",
    "ModelListRequest",
    "ModelVisibilityListRequest",
    "MyOAuthAccountsListRequest",
    "NetworkConfigGetRequest",
    "NetworkInterfacesListRequest",
    "NetworkRelayStatusRequest",
    "NgcStatusRequest",
    "NimCatalogListRequest",
    "OrgStructureHandoverListRequest",
    "OrgStructureHistoryListRequest",
    "OrgStructureMemberListRequest",
    "PiiRuleListRequest",
    "ProcessDefinitionGetRequest",
    "ProcessDefinitionListRequest",
    "ProcessInstanceGetRequest",
    "ProcessInstanceListRequest",
    "ProcessInstanceModelGetRequest",
    "ProcessInstanceTokensGetRequest",
    "ProcessMessageGetRequest",
    "ProcessMessageListRequest",
    "ProcessScopeGetRequest",
    "ProcessUserTaskGetRequest",
    "ProcessVersionGetRequest",
    "ProcessVersionListRequest",
    "ProjectStudioActivityListRequest",
    "ProjectStudioAttachmentGetRequest",
    "ProjectStudioAttachmentPreviewRequest",
    "ProjectStudioAttachmentUploadStatusRequest",
    "ProjectStudioBuildProfileGetRequest",
    "ProjectStudioCaseGetRequest",
    "ProjectStudioCaseVersionGetRequest",
    "ProjectStudioCasesListRequest",
    "ProjectStudioCatalogueGetRequest",
    "ProjectStudioChatsListRequest",
    "ProjectStudioCreatorGrantsListRequest",
    "ProjectStudioEnvApprovalsListRequest",
    "ProjectStudioEnvironmentsListRequest",
    "ProjectStudioGenerationGetRequest",
    "ProjectStudioGenerationsListRequest",
    "ProjectStudioIngestStatusRequest",
    "ProjectStudioKbSearchRequest",
    "ProjectStudioMembersListRequest",
    "ProjectStudioMlLinksListRequest",
    "ProjectStudioNotificationsListRequest",
    "ProjectStudioProjectExportStatusRequest",
    "ProjectStudioProjectGetRequest",
    "ProjectStudioProjectImportStatusRequest",
    "ProjectStudioProjectInheritancePreviewRequest",
    "ProjectStudioProjectLifecyclePreviewRequest",
    "ProjectStudioProjectScopePreviewRequest",
    "ProjectStudioProjectsListRequest",
    "ProjectStudioRunArtifactGetRequest",
    "ProjectStudioRunAutoGetRequest",
    "ProjectStudioRunGetRequest",
    "ProjectStudioRunItemGetRequest",
    "ProjectStudioRunnersListRequest",
    "ProjectStudioRunsListRequest",
    "ProjectStudioScheduleRunsListRequest",
    "ProjectStudioSchedulesListRequest",
    "ProjectStudioSourceFilePreviewRequest",
    "ProjectStudioSourceFilesListRequest",
    "ProjectStudioSourcesListRequest",
    "ProjectStudioSuiteGetRequest",
    "ProjectStudioSuitesListRequest",
    "ProjectStudioTaskGetRequest",
    "ProjectStudioTaskIndexStatusRequest",
    "ProjectStudioTaskLinksListRequest",
    "ProjectStudioTaskTransferPreviewRequest",
    "ProjectStudioTaskTypesListRequest",
    "ProjectStudioTasksListRequest",
    "PromptDetailRequest",
    "PromptListRequest",
    "ProviderAccountAccountGetRequest",
    "ProviderAccountAccountListRequest",
    "ProviderAccountLoginStatusRequest",
    "ProviderAccountMyAccountListRequest",
    "ProviderAccountRuntimeListRequest",
    "ProviderAccountSessionListRequest",
    "RegistryListRequest",
    "RobotGeoAnchorGetRequest",
    "RobotsListRequest",
    "RoleCatalogGetRequest",
    "RoleCatalogListRequest",
    "SchedulerJobsListRequest",
    "SchedulerRunsListRequest",
    "ServiceAccountListRequest",
    "ServiceListRequest",
    "SkillsDetailRequest",
    "SkillsHubSearchRequest",
    "SkillsListRequest",
    "SlaBreachListRequest",
    "SlaPolicyGetRequest",
    "SlaPolicyListRequest",
    "SlaStatusRequest",
    "SsoProvidersListRequest",
    "SyncConflictsListRequest",
    "TentaNasAlertsListRequest",
    "TentaNasApprovalsListRequest",
    "TentaNasDatasetGetRequest",
    "TentaNasDatasetsListRequest",
    "TentaNasDiskGetRequest",
    "TentaNasDisksListRequest",
    "TentaNasElasticArrayGetRequest",
    "TentaNasElasticArraysListRequest",
    "TentaNasFleetMountsListRequest",
    "TentaNasJobGetRequest",
    "TentaNasJobsListRequest",
    "TentaNasNodesListRequest",
    "TentaNasPoolGetRequest",
    "TentaNasPoolsListRequest",
    "TentaNasSchedulesListRequest",
    "TentaNasShareGetRequest",
    "TentaNasShareUsersListRequest",
    "TentaNasSharesListRequest",
    "TentaNasSnapshotSchedulesListRequest",
    "TentaNasSnapshotsListRequest",
    "TentaNasTargetGetRequest",
    "TentaNasTargetsListRequest",
    "TentaQuantExampleGetRequest",
    "TentaQuantExampleListRequest",
    "TentaQuantFileListRequest",
    "TentaQuantKataGetRequest",
    "TentaQuantKataListRequest",
    "TentaQuantLabListRequest",
    "TentaQuantNotebookGetRequest",
    "TentaQuantNotebookListRequest",
    "TentaQuantProjectGetRequest",
    "TentaQuantProjectListRequest",
    "TentaQuantRunGetRequest",
    "TentaQuantRunListRequest",
    "TentaQuantTargetListRequest",
    "TentaVmHostGetRequest",
    "TentaVmHostGrantsListRequest",
    "TentaVmHostsListRequest",
    "TentaVmJobGetRequest",
    "TentaVmJobsListRequest",
    "TlsStatusRequest",
    "TtsRuleListRequest",
    "VoiceProfileListRequest",
];

fn is_read_only_variant(variant: &str) -> bool {
    READ_ONLY_VARIANTS.binary_search(&variant).is_ok()
}

/// How long a read-only forward by one user to one node is audited once.
const READ_ONLY_AUDIT_WINDOW: std::time::Duration = std::time::Duration::from_secs(600);

/// Whether a read-only forward `(user, node, variant)` was audited within
/// `READ_ONLY_AUDIT_WINDOW`; records it when it was not.
fn read_only_forward_is_new(
    scope: &'static str,
    org: &str,
    user: &str,
    node: &str,
    variant: &str,
) -> bool {
    static SEEN: std::sync::OnceLock<
        parking_lot::Mutex<
            std::collections::HashMap<
                (&'static str, String, String, String, String),
                std::time::Instant,
            >,
        >,
    > = std::sync::OnceLock::new();
    let mut seen = SEEN.get_or_init(Default::default).lock();
    let now = std::time::Instant::now();
    seen.retain(|_, at| now.duration_since(*at) < READ_ONLY_AUDIT_WINDOW);
    let key = (
        scope,
        org.to_string(),
        user.to_string(),
        node.to_string(),
        variant.to_string(),
    );
    if seen.contains_key(&key) {
        return false;
    }
    seen.insert(key, now);
    true
}

/// Whether the executing node writes its `app.route.execute` row for this
/// request: always for one that changes something, once per window for a
/// read-only one. Its own window, apart from the origin's, so the two rows of
/// one request on a node that is both do not hide each other.
fn execute_audit_due(org: &str, user: &str, from_node: &str, variant: &str) -> bool {
    !is_read_only_variant(variant)
        || read_only_forward_is_new("execute", org, user, from_node, variant)
}

/// Origin-side audit of a forward the executing node answered: WHERE the user
/// sent which request from, and how it ended. The executing node writes its
/// own authoritative entry (`app.route.execute`, with its gate outcome); a
/// request that never got an answer leaves no row here, only the error the
/// caller gets. A read-only request is audited once per user, node and
/// variant per `READ_ONLY_AUDIT_WINDOW`, so a polling page does not fill the
/// table.
fn audit_forward(
    ctx: &HandlerContext,
    org: &crate::services::rbac::OrgContext,
    node_id: &str,
    variant: &str,
    outcome: &str,
) {
    if is_read_only_variant(variant)
        && !read_only_forward_is_new("forward", &org.org_id, &org.user_id, node_id, variant)
    {
        return;
    }
    let _ = crate::db::repository::log_audit_full(
        &ctx.state.db,
        Some(&org.user_id),
        None,
        "app.route.forward",
        Some("mesh_node"),
        Some(node_id),
        Some(&format!("{variant}: {outcome}")),
        "info",
        "B",
        None,
        Some(&org.org_id),
        None,
        Some(&ctx.state.local_node_id),
    );
}

/// Forward one dashboard request (the envelope's raw `MessageBody` CBOR) to
/// `node_id` and return the executing node's response body.
///
/// `body_cbor` travels verbatim — the assertion's `args_digest` binds those
/// exact bytes, and the executing node decodes them with the same decoder the
/// local dispatcher would have used. The caller has already established that
/// the target is not the local node.
pub async fn forward_to_node(
    ctx: &HandlerContext,
    node_id: &str,
    body_cbor: Vec<u8>,
) -> Result<MessageBody, ProtocolError> {
    // Loop guard: a request already executing on behalf of another node must
    // not fan out again — two nodes disagreeing about a target would bounce
    // the request until the deadline.
    if remote_proxy::current_remote_origin_id().is_some() {
        return Err(ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            "a forwarded request cannot be forwarded again",
        ));
    }
    let org = ctx.org_context.as_ref().ok_or_else(|| {
        ProtocolError::new(ProtocolErrorCode::AuthRequired, "org context required")
    })?;
    let iroh = ctx.state.quic_mesh.as_ref().cloned().ok_or_else(|| {
        ProtocolError::new(
            ProtocolErrorCode::NotAvailable,
            "mesh transport is not running on this node",
        )
    })?;
    if !iroh.is_trusted(node_id) {
        return Err(ProtocolError::new(
            ProtocolErrorCode::NotAvailable,
            format!("node '{node_id}' is not a trusted peer"),
        ));
    }

    let caps = assertion::resolve_caps(&ctx.state.db, &org.user_id, APP_ROUTE_SCOPE)
        .map_err(|e| remote_proxy::assertion_denied(&e))?;
    let rbac_rev =
        assertion::rbac_revision(&ctx.state.db, &org.user_id, &org.org_id, APP_ROUTE_SCOPE)
            .map_err(|e| remote_proxy::assertion_denied(&e))?;
    let op = remote_proxy::remote_op_id("", &body_cbor);
    let minted = assertion::issue(&assertion::IssueRequest {
        local_node_id: &ctx.state.local_node_id,
        user_id: &org.user_id,
        owner_node_id: node_id,
        org_id: &org.org_id,
        workspace_id: APP_ROUTE_SCOPE,
        session_id: "",
        caps: &caps,
        rbac_rev: &rbac_rev,
        op_id: &op,
        payload_cbor: &body_cbor,
        lifetime_ms: assertion::DEFAULT_LIFETIME_MS,
    })
    .map_err(|e| remote_proxy::assertion_denied(&e))?;

    let variant = decode_body(&body_cbor)
        .map(|body| crate::dispatch::variant_name_of(&body))
        .unwrap_or("unknown");

    let response = iroh
        .send_command_and_wait(
            node_id,
            MeshCommandType::AppRouteOp {
                assertion: minted,
                payload_cbor: body_cbor,
            },
            OP_TIMEOUT_SECS,
        )
        .await
        .map_err(|e| unreachable(node_id, e))?;

    let (payload, transport_error) = (response.payload, response.error);
    match payload {
        MeshCommandResponsePayload::AppRouteOpResult {
            payload_cbor,
            error,
        } => match error {
            Some(error) => {
                audit_forward(ctx, org, node_id, variant, &format!("{:?}", error.code));
                Err(error)
            }
            None => {
                audit_forward(ctx, org, node_id, variant, "ok");
                decode_body(&payload_cbor)
            }
        },
        _ => Err(unreachable(
            node_id,
            transport_error.unwrap_or_else(|| "unexpected mesh response payload".to_string()),
        )),
    }
}

// =============================================================================
// Executing side
// =============================================================================

/// A forwarded request whose assertion did not verify (forged, expired, signed
/// by a key this node does not know) is worth a line in the log: on a healthy
/// mesh it never happens. Once per `(peer, user)` per minute, so a poll that
/// keeps failing does not repeat it.
fn warn_refused_assertion(peer: &str, user: &str, error: &assertion::AssertionError) {
    static LAST: std::sync::OnceLock<
        parking_lot::Mutex<std::collections::HashMap<(String, String), std::time::Instant>>,
    > = std::sync::OnceLock::new();
    let mut last = LAST.get_or_init(Default::default).lock();
    let now = std::time::Instant::now();
    let key = (peer.to_string(), user.to_string());
    if last
        .get(&key)
        .is_some_and(|at| now.duration_since(*at) < std::time::Duration::from_secs(60))
    {
        return;
    }
    last.insert(key, now);
    tracing::warn!(
        peer,
        user,
        error = %error,
        "app route: a forwarded request carried an assertion that did not verify"
    );
}

pub async fn execute_remote_side(
    from_node_id: &str,
    assertion_in: &SessionAssertion,
    payload_cbor: &[u8],
    iroh: &Arc<IrohMeshManager>,
) -> (Vec<u8>, Option<ProtocolError>) {
    match execute_remote_side_inner(from_node_id, assertion_in, payload_cbor, iroh).await {
        Ok(bytes) => (bytes, None),
        Err(error) => (Vec::new(), Some(error)),
    }
}

async fn execute_remote_side_inner(
    from_node_id: &str,
    assertion_in: &SessionAssertion,
    payload_cbor: &[u8],
    iroh: &Arc<IrohMeshManager>,
) -> Result<Vec<u8>, ProtocolError> {
    let state = remote_proxy::node_state().cloned().ok_or_else(|| {
        ProtocolError::new(
            ProtocolErrorCode::NotAvailable,
            "mesh execution context is not initialized on this node",
        )
    })?;

    let verify_input = assertion::VerifyInput {
        channel_peer_id: from_node_id,
        local_node_id: &state.local_node_id,
        payload_cbor,
        now_unix_ms: crate::services::mesh_keys::now_unix_ms(),
    };
    // An unknown key is the ordinary case right after the issuer restarted or
    // rotated — pull its ring once from the SAME authenticated peer, then
    // verify for real.
    if let Err(assertion::AssertionError::UnknownKey { .. }) =
        assertion::verify_claims(assertion_in, &verify_input)
    {
        remote_proxy::fetch_peer_assertion_keys(from_node_id, iroh).await;
    }
    let verified = assertion::verify(&state.db, assertion_in, &verify_input).map_err(|e| {
        warn_refused_assertion(from_node_id, &assertion_in.sub, &e);
        remote_proxy::assertion_denied(&e)
    })?;

    // Domain separation: this assertion must have been minted FOR app routing.
    if verified.workspace_id != APP_ROUTE_SCOPE {
        return Err(ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            "assertion scope does not authorize app routing",
        ));
    }
    let expected_op = remote_proxy::remote_op_id(&verified.session_id, payload_cbor);
    if verified.op_id != expected_op {
        return Err(ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            "assertion operation id does not match its payload",
        ));
    }

    let body = decode_body(payload_cbor)?;
    let variant_name = crate::dispatch::variant_name_of(&body);

    // A streaming subscription has no oneshot response to relay — refuse it
    // clearly instead of letting `dispatch()` report a missing handler.
    if crate::dispatch::subscription::find_stream_handler(variant_name).is_some() {
        return Err(ProtocolError::new(
            ProtocolErrorCode::NotAvailable,
            "streaming subscriptions cannot be forwarded between nodes",
        ));
    }

    // Identity rebuilt from LOCAL data only (account row, role, org RBAC) —
    // the wire supplied the user id and nothing that grants anything.
    let ctx = remote_proxy::handler_context(&state, &verified)?;

    // Executor-side audit with the REAL user, not "mesh peer". A read-only
    // request is audited once per user, node and variant per window, like the
    // origin's row: a dashboard polling another node would fill the table.
    if execute_audit_due(
        &verified.org_id,
        &verified.user_id,
        from_node_id,
        variant_name,
    ) {
        let _ = crate::db::repository::log_audit_full(
            &state.db,
            Some(&verified.user_id),
            None,
            "app.route.execute",
            Some("mesh_node"),
            Some(from_node_id),
            Some(variant_name),
            "info",
            "B",
            None,
            Some(&verified.org_id),
            None,
            Some(&state.local_node_id),
        );
    }

    let (response, _is_error) = remote_proxy::with_remote_origin(
        assertion::digest_hex(payload_cbor),
        crate::dispatch::dispatch(&body, &ctx),
    )
    .await;
    match response {
        // The refusal travels as a typed error so the origin node relays the
        // code (PolicyDenied / AppUnavailable / NotFound...) unchanged.
        MessageBody::Error(error) => Err(error),
        other => encode_body(&other),
    }
}

#[cfg(test)]
mod audit_thinning_tests {
    use super::*;

    #[test]
    fn a_read_only_request_is_audited_once_per_window_and_a_change_every_time() {
        assert!(execute_audit_due(
            "o",
            "u-thin",
            "n-thin",
            "ProjectStudioCasesListRequest"
        ));
        assert!(!execute_audit_due(
            "o",
            "u-thin",
            "n-thin",
            "ProjectStudioCasesListRequest"
        ));
        assert!(!execute_audit_due(
            "o",
            "u-thin",
            "n-thin",
            "ProjectStudioCasesListRequest"
        ));
        assert!(execute_audit_due(
            "o",
            "u-thin2",
            "n-thin",
            "ProjectStudioCasesListRequest"
        ));
        assert!(execute_audit_due(
            "o",
            "u-thin",
            "n-thin2",
            "ProjectStudioCasesListRequest"
        ));
        assert!(
            execute_audit_due("o2", "u-thin", "n-thin", "ProjectStudioCasesListRequest"),
            "another org is audited on its own"
        );
        assert!(execute_audit_due(
            "o",
            "u-thin",
            "n-thin",
            "BusTopicDetailRequest"
        ));
        for _ in 0..3 {
            assert!(execute_audit_due(
                "o",
                "u-thin",
                "n-thin",
                "ProjectStudioCaseDeleteRequest"
            ));
        }
    }

    #[test]
    fn a_request_that_changes_a_status_is_never_thinned() {
        for name in [
            "ProjectStudioCaseStatusSetRequest",
            "MeetingActionItemStatusUpdateRequest",
            "ProjectStudioCasesBulkStatusRequest",
        ] {
            assert!(!is_read_only_variant(name), "{name}");
            for _ in 0..3 {
                assert!(execute_audit_due("o", "u-status", "n-status", name));
            }
        }
        for name in [
            "ProjectStudioCasesListRequest",
            "BusTopicDetailRequest",
            "TentaNasPoolGetRequest",
            "SlaStatusRequest",
        ] {
            assert!(is_read_only_variant(name), "{name}");
        }
    }

    /// The list is only worth anything if it is sorted (the lookup is a binary
    /// search) and names variants that exist.
    #[test]
    fn the_read_only_list_is_sorted_and_names_real_variants() {
        assert!(READ_ONLY_VARIANTS.windows(2).all(|w| w[0] < w[1]));
        let source = include_str!("mod.rs");
        for name in READ_ONLY_VARIANTS {
            assert!(
                source.contains(&format!("=> \"{name}\"")),
                "{name} is not a variant of `variant_name_of`"
            );
        }
    }

    #[test]
    fn the_origin_and_the_executor_of_one_request_do_not_hide_each_other() {
        assert!(read_only_forward_is_new(
            "forward",
            "o",
            "u-both",
            "n-both",
            "FlowListRequest"
        ));
        assert!(execute_audit_due(
            "o",
            "u-both",
            "n-both",
            "FlowListRequest"
        ));
        assert!(!read_only_forward_is_new(
            "forward",
            "o",
            "u-both",
            "n-both",
            "FlowListRequest"
        ));
        assert!(!execute_audit_due(
            "o",
            "u-both",
            "n-both",
            "FlowListRequest"
        ));
    }
}
