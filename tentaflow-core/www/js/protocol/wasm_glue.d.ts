/* tslint:disable */
/* eslint-disable */

/**
 * Widok zdekodowanego envelope'u wystawiony do JS. Body wyciete jako osobny
 * Uint8Array zeby call-site mogl zdekodowac MessageBody osobno.
 */
export class EnvelopeView {
    private constructor();
    free(): void;
    [Symbol.dispose](): void;
    /**
     * CBOR-zakodowany MessageBody — przekazac do `decodeMessageBody()`.
     */
    readonly body: Uint8Array;
    /**
     * True jesli flaga `IS_ERROR` ustawiona (body = `MessageBody::Error`).
     */
    readonly isError: boolean;
    /**
     * True jesli flaga `IS_STREAM_CHUNK` ustawiona.
     */
    readonly isStreamChunk: boolean;
    /**
     * True jesli flaga `IS_STREAM_END` ustawiona.
     */
    readonly isStreamEnd: boolean;
    /**
     * 32-byte target node id jesli Routing::Forward, inaczej None.
     */
    readonly targetNodeId: Uint8Array | undefined;
    readonly correlation_id: bigint;
    readonly flags: number;
    readonly is_forward: boolean;
    readonly message_kind: number;
    readonly schema_version: number;
    readonly sequence: bigint;
}

/**
 * Wersja schematu protokolu. MUSI byc zgodna ze `tentaflow_protocol::SCHEMA_VERSION`
 * po stronie serwera — handshake sprawdza match, mismatch = reject connection.
 */
export function SCHEMA_VERSION(): number;

/**
 * Zwraca hex Ed25519 public key (64 znaki). Generuje keypair przy pierwszym
 * uzyciu i persistuje w localStorage.
 */
export function browserNodeId(): string;

/**
 * Usuwa keypair z localStorage (wylogowanie/reset tozsamosci browser).
 * Kolejne wywolanie `browserNodeId` wygeneruje nowy keypair.
 */
export function browserResetIdentity(): void;

/**
 * Podpisuje `data` i zwraca raw bajty podpisu (64 B).
 */
export function browserSign(data: Uint8Array): Uint8Array;

/**
 * Podpisuje `data` kluczem prywatnym browser-a. Zwraca signature (64 bajty)
 * jako hex string (128 znakow).
 */
export function browserSignHex(data: Uint8Array): string;

/**
 * Decode a CBOR-encoded Component into a JS object suitable for ComponentRenderer.
 */
export function decodeComponentCbor(cbor_bytes: Uint8Array): any;

/**
 * Decode + bytecheck (NIGDY `access_unchecked`) pelnego envelope'u z WSS input.
 * Zwraca strukturalny widok; body wciaz zakodowany (lazy decode przez
 * `decodeMessageBody`).
 */
export function decodeEnvelope(bytes: Uint8Array): EnvelopeView;

/**
 * Dekoduje CBOR-zakodowany MessageBody na JS object.
 * Dla znanych variantow zwraca obiekt z polem `variant`, a dla nieznanego
 * variantu `{ variant: "Unknown" }`.
 */
export function decodeMessageBody(bytes: Uint8Array): any;

/**
 * Decode CBOR-encoded Vec<PatchOp> into JS array of { path, op, ... }.
 */
export function decodePatchOpsCbor(cbor_bytes: Uint8Array): any;

/**
 * Decode CBOR-encoded Vec<StateEntry> into JS array of { path, value }.
 */
export function decodeStateEntriesCbor(cbor_bytes: Uint8Array): any;

/**
 * Decode UI channel CBOR payload into a JS-friendly object.
 */
export function decodeUiPayload(cbor_bytes: Uint8Array): any;

/**
 * MessageBody::AddonAccessDecisionRequest { addon_id, kind, target, decision }.
 */
export function encodeAddonAccessDecisionRequest(addon_id: string, kind: string, target: string, decision: string): Uint8Array;

/**
 * MessageBody::AddonAccessListRequest { addon_id }.
 */
export function encodeAddonAccessListRequest(addon_id: string): Uint8Array;

/**
 * MessageBody::AddonAdminOnlySetRequest { addon_id, admin_only }.
 */
export function encodeAddonAdminOnlySetRequest(addon_id: string, admin_only: boolean): Uint8Array;

/**
 * MessageBody::AddonUiBody(ReqApplicationsList) — lista aplikacji widocznych
 * w glownym menu launcher. Frontend buduje liste ikon w app menu.
 */
export function encodeAddonApplicationsListRequest(): Uint8Array;

/**
 * MessageBody::AddonInstanceBody(ReqCatalogList) — lista pakietow w katalogu.
 */
export function encodeAddonCatalogListRequest(): Uint8Array;

export function encodeAddonConfigGetRequest(addon_id: string): Uint8Array;

/**
 * `keys` + `values` — rownolegle wektory (len(keys) == len(values)); laczymy po indeksie.
 * wasm-bindgen nie wspiera `Vec<(String,String)>` bezposrednio, a `Vec<String>` dziala.
 */
export function encodeAddonConfigSetRequest(addon_id: string, keys: string[], values: string[]): Uint8Array;

/**
 * MessageBody::AddonDetailRequest { addon_id } — szczegoly addona.
 */
export function encodeAddonDetailRequest(addon_id: string): Uint8Array;

export function encodeAddonInstallRequest(filename: string, content: Uint8Array): Uint8Array;

/**
 * MessageBody::AddonInstanceBody(ReqDuplicate) — duplikacja instancji.
 */
export function encodeAddonInstanceDuplicateRequest(source_addon_id: string, new_display_name: string): Uint8Array;

/**
 * MessageBody::AddonInstanceBody(ReqInstall) — instalacja instancji z katalogu.
 */
export function encodeAddonInstanceInstallRequest(package_id: string, version: string, display_name: string): Uint8Array;

/**
 * MessageBody::AddonInstanceBody(ReqUpdate) — hot-update instancji do wersji.
 */
export function encodeAddonInstanceUpdateRequest(addon_id: string, target_version: string): Uint8Array;

/**
 * MessageBody::AddonInstanceBody(ReqVersions) — wersje dostepne dla instancji.
 */
export function encodeAddonInstanceVersionsRequest(addon_id: string): Uint8Array;

export function encodeAddonLogsRequest(addon_id: string, limit: number, offset: number, level?: string | null, search?: string | null): Uint8Array;

export function encodeAddonNetworkRulesGetRequest(addon_id: string): Uint8Array;

export function encodeAddonNetworkRulesSetRequest(addon_id: string, allowed_hosts: string[], blocked_hosts: string[], mode: string): Uint8Array;

/**
 * MessageBody::AddonOAuthAuthorizeStartRequest — inicjuje flow autoryzacji.
 */
export function encodeAddonOAuthAuthorizeStartRequest(addon_id: string, provider_id: string, mode: string, redirect_after?: string | null): Uint8Array;

/**
 * MessageBody::AddonOAuthConfigClearSecretRequest — usun wylacznie secret.
 */
export function encodeAddonOAuthConfigClearSecretRequest(addon_id: string, provider_id: string): Uint8Array;

/**
 * MessageBody::AddonOAuthConfigListRequest { addon_id } — zero secretow.
 */
export function encodeAddonOAuthConfigListRequest(addon_id: string): Uint8Array;

/**
 * MessageBody::AddonOAuthConfigSetRequest — zapis konfiguracji OAuth.
 * `client_secret` = None (null) => zachowaj obecny, Some(..) => nadpisz.
 */
export function encodeAddonOAuthConfigSetRequest(addon_id: string, provider_id: string, client_id: string, client_secret: string | null | undefined, redirect_uri: string, enabled: boolean, oauth_mode: string): Uint8Array;

/**
 * MessageBody::AddonOAuthLinkedAccountsRequest — lista polaczonych kont.
 * `scope` = "all" (admin) lub "mine" (user).
 */
export function encodeAddonOAuthLinkedAccountsRequest(addon_id: string, scope: string): Uint8Array;

/**
 * MessageBody::AddonOAuthReauthorizeRequest { account_id }.
 */
export function encodeAddonOAuthReauthorizeRequest(account_id: number): Uint8Array;

/**
 * MessageBody::AddonOAuthRevokeRequest { account_id }.
 */
export function encodeAddonOAuthRevokeRequest(account_id: number): Uint8Array;

/**
 * MessageBody::AddonOAuthTestConnectionRequest { addon_id, provider_id }.
 */
export function encodeAddonOAuthTestConnectionRequest(addon_id: string, provider_id: string): Uint8Array;

/**
 * MessageBody::AddonPermissionCatalogRequest { addon_id } — katalog deklaracji.
 */
export function encodeAddonPermissionCatalogRequest(addon_id: string): Uint8Array;

/**
 * MessageBody::AddonPermissionCheckRequest — czy uzytkownik ma uprawnienie.
 * `user_id` = None (pass null z JS) => serwer uzyje id z sesji.
 */
export function encodeAddonPermissionCheckRequest(addon_id: string, permission_id: string, user_id?: string | null): Uint8Array;

/**
 * MessageBody::AddonPermissionDefaultSetRequest — ustawia domyslny grant addona.
 */
export function encodeAddonPermissionDefaultSetRequest(addon_id: string, permission_id: string, grant_mode: string): Uint8Array;

/**
 * MessageBody::AddonPermissionMatrixRequest { addon_id } — aktualna macierz.
 */
export function encodeAddonPermissionMatrixRequest(addon_id: string): Uint8Array;

/**
 * MessageBody::AddonPermissionSetRequest — ustawia grant per (user|group).
 */
export function encodeAddonPermissionSetRequest(addon_id: string, subject_type: string, subject_id: string, permission_id: string, grant_mode: string): Uint8Array;

export function encodeAddonReloadRequest(addon_id: string): Uint8Array;

export function encodeAddonResourcesGetRequest(addon_id: string): Uint8Array;

export function encodeAddonResourcesSetRequest(addon_id: string, max_instances: number, cpu_limit_pct: number, ram_mb: number, storage_mb: number, http_requests_per_min: number, llm_tokens_per_min: number): Uint8Array;

/**
 * MessageBody::AddonShowInCatalogSetRequest { addon_id, show_in_catalog }.
 */
export function encodeAddonShowInCatalogSetRequest(addon_id: string, show_in_catalog: boolean): Uint8Array;

/**
 * MessageBody::AddonStorageBody(StatsRequest) — statystyki storage addona.
 */
export function encodeAddonStorageStatsRequest(addon_id: string): Uint8Array;

export function encodeAddonToggleRequest(addon_id: string, enabled: boolean): Uint8Array;

export function encodeAddonToolsRequest(addon_id: string): Uint8Array;

export function encodeAddonUninstallRequest(addon_id: string): Uint8Array;

/**
 * MessageBody::AddonVectorBody(GetConfigRequest) — config vector backendu addona.
 */
export function encodeAddonVectorGetConfigRequest(addon_id: string): Uint8Array;

/**
 * MessageBody::AddonVectorBody(SetConfigRequest) — zapis config vector backendu.
 * Pola configu jako osobne argumenty (bez serde_json w crate wasm).
 */
export function encodeAddonVectorSetConfigRequest(addon_id: string, backend: string, milvus_source?: string | null, service_node_id?: string | null, service_id?: string | null, manual_uri?: string | null, collection_override?: string | null, milvus_user?: string | null, milvus_password?: string | null): Uint8Array;

/**
 * MessageBody::AddonVisibilityListRequest { addon_id } — widocznosc per grupa.
 */
export function encodeAddonVisibilityListRequest(addon_id: string): Uint8Array;

/**
 * MessageBody::AddonVisibilitySetRequest { addon_id, group_id, visible }.
 */
export function encodeAddonVisibilitySetRequest(addon_id: string, group_id: string, visible: boolean): Uint8Array;

/**
 * MessageBody::AddonsListRequest (unit variant).
 */
export function encodeAddonsListRequest(): Uint8Array;

export function encodeAgentPermissionReplyRequest(run_id: string, request_id: string, decision: string): Uint8Array;

export function encodeAgentRunCancelRequest(run_id: string): Uint8Array;

export function encodeAgentRunDetailRequest(run_id: string): Uint8Array;

/**
 * Subscribe to a run-events scope. `scope_kind` is "session" or "run";
 * `scope_id` is the session id or run id respectively.
 */
export function encodeAgentRunEventsSubscribeRequest(scope_kind: string, scope_id: string): Uint8Array;

export function encodeAgentRunReplyRequest(run_id: string, question_id: string, answer: string): Uint8Array;

export function encodeAgentRunsListRequest(agent_id?: string | null, status?: string | null, parent_run_id?: string | null): Uint8Array;

export function encodeAgentsDeleteRequest(agent_id: string): Uint8Array;

export function encodeAgentsDetailRequest(agent_id: string): Uint8Array;

export function encodeAgentsListRequest(enabled?: boolean | null, routable?: boolean | null): Uint8Array;

export function encodeAgentsUpsertRequest(agent_json: string): Uint8Array;

/**
 * MessageBody::AliasConsumerGrantRequest { alias_id, addon_id }.
 */
export function encodeAliasConsumerGrantRequest(alias_id: number, addon_id: string): Uint8Array;

/**
 * MessageBody::AliasConsumerListRequest { alias_id }.
 */
export function encodeAliasConsumerListRequest(alias_id: number): Uint8Array;

/**
 * MessageBody::AliasConsumerRevokeRequest { alias_id, addon_id }.
 */
export function encodeAliasConsumerRevokeRequest(alias_id: number, addon_id: string): Uint8Array;

/**
 * MessageBody::AliasVisibilitySetRequest { alias_id, visibility }.
 */
export function encodeAliasVisibilitySetRequest(alias_id: number, visibility: string): Uint8Array;

/**
 * MessageBody::ApiKeyCreateRequest { name, key_type, subject_id, scope_resources }.
 * `scope_resources` travels as two parallel arrays (types[i] + ids[i]) so the
 * wasm-bindgen boundary stays on simple `Vec<String>` values.
 */
export function encodeApiKeyCreateRequest(name: string, key_type: string, subject_id: string | null | undefined, scope_types: string[], scope_ids: string[]): Uint8Array;

/**
 * MessageBody::ApiKeyListRequest (unit variant).
 */
export function encodeApiKeyListRequest(): Uint8Array;

/**
 * MessageBody::ApiKeyRevokeRequest { key_id }.
 */
export function encodeApiKeyRevokeRequest(key_id: string): Uint8Array;

/**
 * MessageBody::ApiKeyRotateRequest { key_uid }.
 */
export function encodeApiKeyRotateRequest(key_uid: string): Uint8Array;

/**
 * MessageBody::ApiKeyScopeClearRequest { key_uid, resource_type, resource_id }.
 */
export function encodeApiKeyScopeClearRequest(key_uid: string, resource_type: string, resource_id: string): Uint8Array;

/**
 * MessageBody::ApiKeyScopeListRequest { key_uid }.
 */
export function encodeApiKeyScopeListRequest(key_uid: string): Uint8Array;

/**
 * MessageBody::ApiKeyScopeSetRequest { key_uid, resource_type, resource_id, access_level }.
 */
export function encodeApiKeyScopeSetRequest(key_uid: string, resource_type: string, resource_id: string, access_level: string): Uint8Array;

/**
 * MessageBody::AuditLogCleanupRequest — usun wpisy starsze niz N dni.
 */
export function encodeAuditLogCleanupRequest(keep_days: number): Uint8Array;

/**
 * MessageBody::AuditLogExportRequest — eksport CSV z filtrami.
 */
export function encodeAuditLogExportRequest(user_id?: string | null, addon_id?: string | null, action?: string | null, from_date?: string | null, to_date?: string | null, search?: string | null): Uint8Array;

/**
 * MessageBody::AuditLogListRequest — lista logu z filtrami + paginacja.
 */
export function encodeAuditLogListRequest(user_id: string | null | undefined, addon_id: string | null | undefined, action: string | null | undefined, from_date: string | null | undefined, to_date: string | null | undefined, search: string | null | undefined, offset: number, limit: number): Uint8Array;

/**
 * MessageBody::AuthLoginRequest { username, password }.
 */
export function encodeAuthLoginRequest(username: string, password: string): Uint8Array;

/**
 * MessageBody::AuthMeRequest (unit variant).
 */
export function encodeAuthMeRequest(): Uint8Array;

export function encodeBaselineAdoptClearRequest(): Uint8Array;

export function encodeBaselineAdoptStartRequest(donor_node_id: string): Uint8Array;

export function encodeBaselineAdoptStatusRequest(): Uint8Array;

export function encodeBaselineDonorListRequest(): Uint8Array;

/**
 * MessageBody::BrowserCaptureRequest — one-shot capture of the bot's page.
 */
export function encodeBrowserCaptureRequest(session_id: number, kind: string, full_page: boolean): Uint8Array;

/**
 * MessageBody::CameraAdminBody(AddOnvifRequest) — bind a discovered ONVIF
 * device as a managed camera session. Credentials travel over the TLS
 * admin transport and are AES-GCM-sealed server-side before persistence.
 */
export function encodeCameraAddOnvifRequest(display_name: string, device_service_url: string, username: string, password: string, profile_token?: string | null, target_fps?: number | null): Uint8Array;

/**
 * MessageBody::CameraAdminBody(DetectionsSubscribeRequest) — open a per-camera
 * detection overlay stream. The handler validates the `cam_<uuid v4>` id,
 * gates on `camera.read` + org isolation, and replies with a long-lived
 * stream of `CameraDetectionsFrame` chunks until cancel/disconnect.
 */
export function encodeCameraDetectionsSubscribeRequest(camera_id: string): Uint8Array;

/**
 * MessageBody::CameraAdminBody(DiscoverRequest) — kick off ONVIF WS-Discovery
 * against the local network; the response carries the discovered devices.
 */
export function encodeCameraDiscoverRequest(): Uint8Array;

/**
 * MessageBody::CameraAdminBody(FrameUrlRequest) — live-preview tile URL
 * for `<tf-live-camera-tile>`. The handler gates on `camera.read`,
 * enforces UUID v4 camera_id validation, a per-user rate limit, and a
 * 5..=300 s dispatch TTL band before minting against the global frame
 * signed-URL issuer.
 */
export function encodeCameraFrameUrlRequest(camera_id: string, ttl_secs: number): Uint8Array;

export function encodeCatalogListRequest(surface_filter: string | null | undefined, include_blocking_diagnostics: boolean): Uint8Array;

/**
 * MessageBody::ChatStreamRequest — przyjmuje JSON string messages, parsuje
 * jako JsValue. Bootstrap accepts tylko `model_id` + jednoelementowa lista
 * user messages. Pelny messages[] input po integracji serde-wasm-bindgen (#36 ph.2).
 */
export function encodeChatStreamRequestSimple(model_id: string, user_message: string, flow_id?: string | null): Uint8Array;

/**
 * MessageBody::ClusterAddMemberRequest.
 */
export function encodeClusterAddMemberRequest(cluster_id: string, node_id: string, interface_type?: string | null, interface_speed_mbps?: number | null): Uint8Array;

/**
 * MessageBody::ClusterCreateRequest.
 */
export function encodeClusterCreateRequest(name: string, description: string | null | undefined, strategy: string, failover_enabled: boolean, failover_target: string | null | undefined, health_check_interval_ms: number, timeout_ms: number): Uint8Array;

/**
 * MessageBody::ClusterDeleteRequest { cluster_id }.
 */
export function encodeClusterDeleteRequest(cluster_id: string): Uint8Array;

/**
 * MessageBody::ClusterDetailRequest { cluster_id }.
 */
export function encodeClusterDetailRequest(cluster_id: string): Uint8Array;

/**
 * MessageBody::ClusterListRequest (unit variant).
 */
export function encodeClusterListRequest(): Uint8Array;

/**
 * MessageBody::ClusterProbeStreamRequest { node_ids }.
 */
export function encodeClusterProbeStreamRequest(node_ids: string[]): Uint8Array;

/**
 * MessageBody::ClusterRemoveMemberRequest.
 */
export function encodeClusterRemoveMemberRequest(cluster_id: string, node_id: string): Uint8Array;

/**
 * MessageBody::ClusterUpdateRequest. Wszystkie pola opcjonalne — `None`
 * zachowuje obecna wartosc na serwerze.
 */
export function encodeClusterUpdateRequest(cluster_id: string, name?: string | null, description?: string | null, strategy?: string | null, failover_enabled?: boolean | null, failover_target?: string | null, health_check_interval_ms?: number | null, timeout_ms?: number | null): Uint8Array;

export function encodeComplianceAiEventsListRequest(status?: string | null, user_id?: string | null, addon_id?: string | null, limit?: number | null, offset?: number | null): Uint8Array;

export function encodeComplianceDataCategoriesListRequest(): Uint8Array;

export function encodeComplianceRetentionPoliciesListRequest(): Uint8Array;

/**
 * MessageBody::DashboardMetricsRequest (unit variant).
 */
export function encodeDashboardMetricsRequest(): Uint8Array;

/**
 * MessageBody::DeployVllmRecommendRequest. Plynnie przyjmuje JSON
 * (pelne struct DeployVllmRecommendRequest serializowane przez GUI).
 */
export function encodeDeployVllmRecommendRequest(payload_json: string): Uint8Array;

export function encodeDeploymentListRequest(engine_id: string, status: string, only_mine: boolean, limit: number): Uint8Array;

export function encodeDeploymentLogStreamRequest(deploy_id: string, replay_tail: boolean): Uint8Array;

export function encodeDeploymentStatusRequest(deploy_id: string): Uint8Array;

/**
 * Buduje Envelope (routing=Direct) z podanymi polami + body bytes; zwraca
 * CBOR-zakodowany frame jako Uint8Array.
 *
 * `correlation_id` przekazywany jako u64 (BigInt po stronie JS).
 */
export function encodeEnvelopeDirect(correlation_id: bigint, sequence: bigint, message_kind: number, body: Uint8Array): Uint8Array;

/**
 * MessageBody::FastPathListRequest (unit).
 */
export function encodeFastPathListRequest(): Uint8Array;

/**
 * MessageBody::FlowCreateRequest { name, description, graph_json,
 * published_model_name? }. `published_model_name = None` keeps the flow
 * private; passing a value publishes it on `/v1/models` after the
 * catalog rebuild — collisions with aliases / existing flows are
 * rejected by the handler before the row is written.
 */
export function encodeFlowCreateRequest(name: string, description: string | null | undefined, graph_json: string, published_model_name?: string | null): Uint8Array;

/**
 * MessageBody::FlowDeleteRequest { flow_id }.
 */
export function encodeFlowDeleteRequest(flow_id: string): Uint8Array;

/**
 * MessageBody::FlowDetailRequest { flow_id }.
 */
export function encodeFlowDetailRequest(flow_id: string): Uint8Array;

/**
 * MessageBody::FlowExecutionsListRequest { flow_id }.
 */
export function encodeFlowExecutionsListRequest(flow_id: string): Uint8Array;

/**
 * MessageBody::FlowInvokeRequest — uniwersalny most do flow engine. Wariant
 * audio-only dla chat audio (jedno wejście Audio). Multi-input dojdzie później.
 */
export function encodeFlowInvokeAudio(flow_id: string | null | undefined, model: string, service_type: string, mime: string, sample_rate: number | null | undefined, audio: Uint8Array, language?: string | null, session_id?: string | null): Uint8Array;

/**
 * MessageBody::FlowListRequest (unit).
 */
export function encodeFlowListRequest(): Uint8Array;

/**
 * MessageBody::FlowNodeTemplatesListRequest (unit).
 */
export function encodeFlowNodeTemplatesListRequest(): Uint8Array;

/**
 * MessageBody::FlowUpdateRequest — partial update flow. Pass
 * `publish_set=true, published_model_name=Some("foo")` to publish or
 * `publish_set=true, published_model_name=None` to un-publish; leave
 * `publish_set=false` to keep whatever the server has.
 */
export function encodeFlowUpdateRequest(flow_id: string, name: string | null | undefined, description: string | null | undefined, flow_json: string | null | undefined, status: string | null | undefined, publish_set: boolean, published_model_name?: string | null): Uint8Array;

/**
 * MessageBody::FlowVersionGetRequest { flow_id, version_id }.
 */
export function encodeFlowVersionGetRequest(flow_id: string, version_id: string): Uint8Array;

/**
 * MessageBody::FlowVersionListRequest { flow_id }.
 */
export function encodeFlowVersionListRequest(flow_id: string): Uint8Array;

/**
 * MessageBody::FlowVersionRestoreRequest { flow_id, version_id }.
 */
export function encodeFlowVersionRestoreRequest(flow_id: string, version_id: string): Uint8Array;

/**
 * MessageBody::HubEngineListRequest (unit).
 */
export function encodeHubEngineListRequest(): Uint8Array;

/**
 * MessageBody::HubModelSearchRequest { query }.
 */
export function encodeHubModelSearchRequest(query: string): Uint8Array;

export function encodeIamClearPermissionRequest(resource_type: string, resource_id: string, subject_type: string, subject_id: string): Uint8Array;

export function encodeIamCreateGroupRequest(name: string, description: string): Uint8Array;

export function encodeIamCreateUserRequest(username: string, password: string, display_name: string, email: string, role: string, group_ids_csv: string): Uint8Array;

export function encodeIamDeleteGroupRequest(group_id: string): Uint8Array;

export function encodeIamDeleteUserRequest(user_id: string): Uint8Array;

export function encodeIamGetUserRequest(user_id: string): Uint8Array;

export function encodeIamGroupMembersRequest(group_id: string): Uint8Array;

export function encodeIamListGroupsRequest(): Uint8Array;

export function encodeIamListPermsForResourceRequest(resource_type: string, resource_id: string): Uint8Array;

export function encodeIamListPermsForSubjectRequest(subject_type: string, subject_id: string): Uint8Array;

export function encodeIamListUsersRequest(): Uint8Array;

export function encodeIamResetUserPasswordRequest(user_id: string, new_password: string): Uint8Array;

export function encodeIamSetPermissionRequest(resource_type: string, resource_id: string, subject_type: string, subject_id: string, access_level: string): Uint8Array;

export function encodeIamSetUserGroupsRequest(user_id: string, group_ids_csv: string): Uint8Array;

export function encodeIamUpdateGroupRequest(group_id: string, name: string, description: string): Uint8Array;

export function encodeIamUpdateUserRequest(user_id: string, display_name: string, email: string, is_active: boolean, role: string): Uint8Array;

/**
 * MessageBody::LegalAdminBody(GenerateRequest) — render and persist a new
 * RODO/GDPR PDF. `variant` must be one of `short` | `standard` | `full`
 * (server-side validation via `RodoVariant::from_str`).
 */
export function encodeLegalDocumentGenerateRequest(variant: string): Uint8Array;

/**
 * MessageBody::LegalAdminBody(RevokeRequest) — soft-delete a previously
 * generated legal document. The PDF stays on disk; the row gets a
 * `revoked_at` stamp and is excluded from default list views.
 */
export function encodeLegalDocumentRevokeRequest(doc_id: string): Uint8Array;

/**
 * MessageBody::LegalAdminBody(ListRequest) — fetch the legal documents
 * catalogue. `include_revoked = false` matches the default dashboard view.
 */
export function encodeLegalDocumentsListRequest(include_revoked: boolean): Uint8Array;

/**
 * MessageBody::MePreferencesGetRequest (unit variant).
 */
export function encodeMePreferencesGetRequest(): Uint8Array;

/**
 * MessageBody::MePreferencesUpdateRequest { language }.
 */
export function encodeMePreferencesUpdateRequest(language?: string | null): Uint8Array;

export function encodeMeetingActionItemStatusUpdateRequest(item_id: number, status: string): Uint8Array;

export function encodeMeetingActionItemsListRequest(meeting_key: string, status_filter?: string | null): Uint8Array;

export function encodeMeetingActiveSessionRequest(): Uint8Array;

export function encodeMeetingSessionDetailRequest(session_id: number, include_transcripts: boolean): Uint8Array;

export function encodeMeetingSessionLeaveRequest(session_id: number): Uint8Array;

export function encodeMeetingSessionListRequest(only_mine: boolean): Uint8Array;

export function encodeMeetingSessionStartRequest(meeting_url: string, title: string, platform: string, bot_name: string, stt_alias: string, tts_alias: string, llm_alias: string): Uint8Array;

export function encodeMeetingSettingsGetRequest(): Uint8Array;

/**
 * `settings` jest JS Array<[key, value]>. Konwertujemy pary do Vec<MeetingSettingKv>.
 */
export function encodeMeetingSettingsUpdateRequest(settings: any): Uint8Array;

export function encodeMeetingSummariesListRequest(meeting_key: string, limit?: number | null): Uint8Array;

export function encodeMeetingTranscriptExportRequest(meeting_key: string): Uint8Array;

export function encodeMeetingTranscriptsListRequest(session_id: number, since_ms: number): Uint8Array;

export function encodeMeshConnectRequest(address: string): Uint8Array;

export function encodeMeshIdentityRequest(): Uint8Array;

export function encodeMeshNodeCommandRequest(node_id: string, command: string, args: string[]): Uint8Array;

export function encodeMeshNodeDetailRequest(node_id: string): Uint8Array;

export function encodeMeshNodeListRequest(): Uint8Array;

export function encodeMeshNodeNetworkConfigRequest(node_id: string, interface_name: string, config_json: string): Uint8Array;

/**
 * MessageBody::MeshPairInitRequest { node_id (32 bytes), pin }.
 */
export function encodeMeshPairInitRequest(node_id: Uint8Array, pin: string): Uint8Array;

export function encodeMeshPairingConfirmRequest(pair_id: string, pin: string): Uint8Array;

export function encodeMeshPairingRejectRequest(pair_id: string): Uint8Array;

export function encodeMeshPairingStartRequest(remote_address: string, pin_hint?: string | null, remote_public_key?: string | null, remote_addresses?: string[] | null, remote_relay_url?: string | null, remote_hostname?: string | null): Uint8Array;

/**
 * MessageBody::MeshPeersListRequest (unit variant).
 */
export function encodeMeshPeersListRequest(): Uint8Array;

export function encodeMeshPendingListRequest(): Uint8Array;

export function encodeMeshServicesListRequest(): Uint8Array;

export function encodeMeshTrustRetrustRequest(node_id: string): Uint8Array;

export function encodeMeshTrustRevokeRequest(node_id: string): Uint8Array;

export function encodeMeshTrustedListRequest(): Uint8Array;

/**
 * MessageBody::MetaCancelStream (unit variant). Correlation_id idzie w envelope.
 */
export function encodeMetaCancelStream(): Uint8Array;

/**
 * MessageBody::MetaHeartbeat { sent_at_epoch }.
 */
export function encodeMetaHeartbeat(sent_at_epoch: bigint): Uint8Array;

/**
 * MessageBody::MetaSchemaVersionCheck { client_version }.
 * Wysylane raz przy handshake — jesli serwer odrzuci, disconnect.
 */
export function encodeMetaSchemaVersionCheck(client_version: number): Uint8Array;

export function encodeMlStudioDatasetProfileRequest(dataset_id: string): Uint8Array;

export function encodeMlStudioDatasetUploadChunkRequest(project_id: string, name: string, filename: string, upload_id: string, seq: number, total_chunks: number, bytes: Uint8Array): Uint8Array;

/**
 * Upload a tabular file for profiling. `bytes` arrives from JS as a Uint8Array
 * and wasm-bindgen materializes it directly into `Vec<u8>` — no base64 or copy
 * step on the JS side.
 */
export function encodeMlStudioDatasetUploadRequest(project_id: string, name: string, filename: string, bytes: Uint8Array): Uint8Array;

export function encodeMlStudioDatasetsListRequest(project_id: string): Uint8Array;

export function encodeMlStudioFtChatRequest(model_id: string, message: string, max_tokens: number): Uint8Array;

export function encodeMlStudioFtDeployRequest(model_id: string, target_node_id: string): Uint8Array;

export function encodeMlStudioFtExportRequest(model_id: string, outtype: string): Uint8Array;

export function encodeMlStudioFtExportStatusRequest(model_id: string): Uint8Array;

export function encodeMlStudioFtTrainStartRequest(project_id: string, dataset_id: string, base_model: string, method: string, objective: string, teacher_model: string | null | undefined, learning_rate: number, batch_size: number, grad_accum_steps: number, epochs: number, lora_r: number, lora_alpha: number, lora_dropout: number, max_seq_len: number, merge_adapter: boolean, target_node_id: string | null | undefined, num_gpus: number, dist_nnodes: number, dist_node_rank: number, dist_master_addr: string, dist_master_port: number): Uint8Array;

export function encodeMlStudioFtTrainStatusRequest(run_id: string): Uint8Array;

export function encodeMlStudioModelsListRequest(project_id: string): Uint8Array;

export function encodeMlStudioProjectCreateRequest(name: string, description: string, project_type: string): Uint8Array;

export function encodeMlStudioProjectDetailRequest(project_id: string): Uint8Array;

export function encodeMlStudioProjectGrantsListRequest(project_id: string): Uint8Array;

export function encodeMlStudioProjectInviteRequest(project_id: string, invitee_user_id: string, role: string): Uint8Array;

export function encodeMlStudioProjectMemberRemoveRequest(project_id: string, user_id: string): Uint8Array;

export function encodeMlStudioProjectMemberRoleSetRequest(project_id: string, user_id: string, role: string): Uint8Array;

export function encodeMlStudioProjectMembersListRequest(project_id: string): Uint8Array;

export function encodeMlStudioProjectResourcesRequest(project_id: string): Uint8Array;

export function encodeMlStudioProjectTypesListRequest(): Uint8Array;

export function encodeMlStudioProjectsListRequest(): Uint8Array;

export function encodeMlStudioRecogDatasetRegisterRequest(project_id: string, name: string, path: string): Uint8Array;

export function encodeMlStudioRecogDetectRequest(model_id: string, threshold: number, image_b64: string): Uint8Array;

export function encodeMlStudioRecogImageRequest(dataset_id: string, image_id: string): Uint8Array;

export function encodeMlStudioRecogImagesListRequest(dataset_id: string): Uint8Array;

export function encodeMlStudioRecogSaveAnnotationsRequest(dataset_id: string, image_id: string, annotations_json: string, approve: boolean): Uint8Array;

export function encodeMlStudioRecogTrainStartRequest(project_id: string, dataset_id: string, variant: string, epochs: number, batch_size: number, grad_accum: number, learning_rate: number, resolution: number, early_stopping: boolean, target_node_id?: string | null): Uint8Array;

export function encodeMlStudioRecogTrainStatusRequest(run_id: string): Uint8Array;

export function encodeMlStudioResourceGrantCreateRequest(subject_kind: string, subject_id: string, node_id: string, resource_kind: string, resource_ref: string, quota: string): Uint8Array;

export function encodeMlStudioResourceGrantRevokeRequest(grant_id: string): Uint8Array;

export function encodeMlStudioResourceGrantsListRequest(): Uint8Array;

export function encodeMlStudioTabularTrainRequest(project_id: string, dataset_id: string, target_column: string, task: string, engine?: string | null): Uint8Array;

export function encodeMlStudioTrainingRunsListRequest(project_id: string): Uint8Array;

export function encodeModelAliasCreateRequest(alias: string, target_model: string, strategy?: string | null, fallback_targets?: string | null): Uint8Array;

export function encodeModelAliasDeleteRequest(id: number): Uint8Array;

export function encodeModelAliasListRequest(): Uint8Array;

export function encodeModelAliasUpdateRequest(id: number, alias: string, target_model: string, is_active?: boolean | null, strategy?: string | null, fallback_targets?: string | null): Uint8Array;

/**
 * MessageBody::ModelConsumerGrantRequest { model_id, addon_id }.
 */
export function encodeModelConsumerGrantRequest(model_id: string, addon_id: string): Uint8Array;

/**
 * MessageBody::ModelConsumerListRequest { model_id }.
 */
export function encodeModelConsumerListRequest(model_id: string): Uint8Array;

/**
 * MessageBody::ModelConsumerRevokeRequest { model_id, addon_id }.
 */
export function encodeModelConsumerRevokeRequest(model_id: string, addon_id: string): Uint8Array;

/**
 * MessageBody::ModelDeleteRequest { model_id }.
 */
export function encodeModelDeleteRequest(model_id: string): Uint8Array;

/**
 * MessageBody::ModelDetailRequest { model_id }.
 */
export function encodeModelDetailRequest(model_id: string): Uint8Array;

/**
 * MessageBody::ModelInstallRequest { model_id, source_repo }.
 */
export function encodeModelInstallRequest(model_id: string, source_repo: string): Uint8Array;

/**
 * MessageBody::ModelListRequest (unit variant).
 */
export function encodeModelListRequest(): Uint8Array;

/**
 * MessageBody::ModelVisibilityListRequest (unit variant).
 */
export function encodeModelVisibilityListRequest(): Uint8Array;

/**
 * MessageBody::ModelVisibilitySetRequest { model_id, visibility }.
 */
export function encodeModelVisibilitySetRequest(model_id: string, visibility: string): Uint8Array;

/**
 * MessageBody::MyOAuthAccountsListRequest (unit) — lista kont biezacego usera.
 */
export function encodeMyOAuthAccountsListRequest(): Uint8Array;

/**
 * MessageBody::NetworkBody(NetworkPayload::ReqConfigGet).
 */
export function encodeNetworkConfigGetRequest(): Uint8Array;

/**
 * MessageBody::NetworkBody(NetworkPayload::ReqConfigUpdate(NetworkConfig { .. })).
 * Pola przekazywane jako typed args (no serde-wasm-bindgen); strony JS i WASM
 * zgodne z definicja `NetworkConfig` w `tentaflow-protocol`.
 */
export function encodeNetworkConfigUpdateRequest(bind_mode: string, bind_ipv4: string, hide_docker: boolean, hide_link_local: boolean, hide_loopback: boolean, hide_cgnat: boolean, prefer_same_subnet: boolean, iroh_relay_url: string, excluded_interfaces: string[]): Uint8Array;

/**
 * MessageBody::NetworkBody(NetworkPayload::ReqInterfacesList).
 */
export function encodeNetworkInterfacesListRequest(): Uint8Array;

/**
 * MessageBody::NetworkBody(NetworkPayload::ReqRelayStatus).
 */
export function encodeNetworkRelayStatusRequest(): Uint8Array;

/**
 * MessageBody::NgcStatusRequest (unit variant).
 */
export function encodeNgcStatusRequest(): Uint8Array;

/**
 * MessageBody::NimCatalogListRequest (unit variant).
 */
export function encodeNimCatalogListRequest(): Uint8Array;

/**
 * NotesRequest::Create { title, body }.
 */
export function encodeNoteCreateRequest(title: string, body: string): Uint8Array;

/**
 * NotesRequest::Delete { note_id }.
 */
export function encodeNoteDeleteRequest(note_id: number): Uint8Array;

/**
 * NotesRequest::Detail { note_id }.
 */
export function encodeNoteDetailRequest(note_id: number): Uint8Array;

/**
 * NotesRequest::SetPinned { note_id, pinned }.
 */
export function encodeNoteSetPinnedRequest(note_id: number, pinned: boolean): Uint8Array;

/**
 * NotesRequest::Update { note_id, title, body }.
 */
export function encodeNoteUpdateRequest(note_id: number, title: string, body: string): Uint8Array;

/**
 * NotesRequest::List — empty inner struct.
 */
export function encodeNotesListRequest(): Uint8Array;

/**
 * MessageBody::PiiRuleBody(ListRequest) — wire-compat z dawnym
 * PiiRuleListRequest, JS API niezmienione.
 */
export function encodePiiRuleListRequest(): Uint8Array;

/**
 * MessageBody::ProfilingBody(ProfilingPayload::ActiveInfoRequest(..)).
 */
export function encodeProfilingActiveInfoRequest(node_id: string): Uint8Array;

/**
 * MessageBody::ProfilingBody(ProfilingPayload::CollectorsStatusRequest(..)).
 */
export function encodeProfilingCollectorsStatusRequest(node_id: string): Uint8Array;

/**
 * MessageBody::ProfilingBody(ProfilingPayload::DeleteRequest(..)).
 */
export function encodeProfilingDeleteRequest(node_id: string, session_id: string): Uint8Array;

/**
 * MessageBody::ProfilingBody(ProfilingPayload::DownloadRequest(..)).
 */
export function encodeProfilingDownloadRequest(node_id: string, session_id: string): Uint8Array;

/**
 * MessageBody::ProfilingBody(ProfilingPayload::ReportRequest(..)).
 */
export function encodeProfilingReportRequest(node_id: string, session_id: string): Uint8Array;

/**
 * MessageBody::ProfilingBody(ProfilingPayload::SessionsRequest(..)).
 */
export function encodeProfilingSessionsRequest(node_id: string): Uint8Array;

/**
 * MessageBody::ProfilingBody(ProfilingPayload::StartRequest(..)).
 */
export function encodeProfilingStartRequest(node_id: string, scope: any, label: string, elevation_password?: string | null): Uint8Array;

/**
 * MessageBody::ProfilingBody(ProfilingPayload::StopRequest(..)).
 */
export function encodeProfilingStopRequest(node_id: string, session_id: string): Uint8Array;

/**
 * MessageBody::ProfilingBody(ProfilingPayload::ValidateSudoRequest(..)).
 */
export function encodeProfilingValidateSudoRequest(node_id: string, password: string): Uint8Array;

/**
 * MessageBody::PromptDetailRequest { prompt_id }.
 */
export function encodePromptDetailRequest(prompt_id: string): Uint8Array;

/**
 * MessageBody::PromptListRequest (unit).
 */
export function encodePromptListRequest(): Uint8Array;

/**
 * MessageBody::RegistryListRequest (unit).
 */
export function encodeRegistryListRequest(): Uint8Array;

/**
 * MessageBody::RoleCatalogBody(CreateRequest) — payload jako JSON object
 * odpowiadajacy `RoleCatalogCreateRequest`. Translations sa parami
 * `[code, value]`; brak ikony / color_hint w obiekcie = None.
 */
export function encodeRoleCatalogCreateRequest(payload_json: string): Uint8Array;

/**
 * MessageBody::RoleCatalogBody(DeactivateRequest { id }).
 */
export function encodeRoleCatalogDeactivateRequest(id: string): Uint8Array;

/**
 * MessageBody::RoleCatalogBody(GetBySlugRequest { slug }).
 */
export function encodeRoleCatalogGetBySlugRequest(slug: string): Uint8Array;

/**
 * MessageBody::RoleCatalogBody(GetRequest { id }).
 */
export function encodeRoleCatalogGetRequest(id: string): Uint8Array;

/**
 * MessageBody::RoleCatalogBody(ListLocalesRequest) — unit variant.
 */
export function encodeRoleCatalogListLocalesRequest(): Uint8Array;

/**
 * MessageBody::RoleCatalogBody(ListRequest) — filter jako JSON object.
 * Wszystkie pola filter opcjonalne; pusty `{}` zwraca pelna liste.
 */
export function encodeRoleCatalogListRequest(filter_json: string): Uint8Array;

/**
 * MessageBody::RoleCatalogBody(UpdateRequest) — patch update.
 * `Option<Option<String>>` w JSON: brak pola = nie ruszaj, `null` = wyzeruj,
 * string = ustaw. Vec<(String, String)> jako lista par `[["pl","..."], ...]`.
 * Serde nie potrafi rozroznic "missing" od "null" dla `Option<Option<T>>`,
 * wiec parsujemy ręcznie z `serde_json::Value`.
 */
export function encodeRoleCatalogUpdateRequest(payload_json: string): Uint8Array;

export function encodeSchedulerActionsListRequest(): Uint8Array;

export function encodeSchedulerJobDeleteRequest(job_id: string): Uint8Array;

export function encodeSchedulerJobRunNowRequest(job_id: string): Uint8Array;

export function encodeSchedulerJobUpsertRequest(job_json: string): Uint8Array;

export function encodeSchedulerJobsListRequest(): Uint8Array;

export function encodeSchedulerRunsListRequest(job_id: string, limit: number): Uint8Array;

/**
 * MessageBody::ServiceBody(ServicePayload::ReqUpdate) — edycja serwisu po
 * deploy (Edit modal). 13 pól opcjonalnych; klient sam decyduje co jest
 * `Some(_)`. Payload przyjmujemy jako JSON string żeby nie trzymać 13
 * argumentów wasm-bindgen.
 */
export function encodeServiceConfigUpdateRequest(payload_json: string): Uint8Array;

/**
 * MessageBody::ServiceBody(ServicePayload::ReqDelete) — stop + delete the row
 * (cascades to `model_registry`).
 */
export function encodeServiceDeleteRequest(service_id: number, node_id?: string | null): Uint8Array;

/**
 * MessageBody::ServiceBody(ServicePayload::ReqEnginePresets) — lista
 * presetów modelu z manifestu silnika (single source of truth z
 * `tentaflow-containers/<cat>/_services/<engine>.toml`).
 */
export function encodeServiceEnginePresetsRequest(engine_id: string): Uint8Array;

/**
 * MessageBody::ServiceBody(ServicePayload::ReqList). Empty filter values are
 * treated as "no filter".
 */
export function encodeServiceListRequest(engine_id_filter?: string | null, category_filter?: string | null): Uint8Array;

/**
 * MessageBody::DeploymentBody(ReqStart) — inicjuje deploy silnika z manifestu.
 * `config_json` przyjmujemy jako stringify JSON z GUI (elastyczna struktura).
 * Nazwa wasm-bindgen `encodeServiceManifestDeployRequest` zachowana dla
 * kompatybilności z frontend codec.js — pod spodem opakowujemy w
 * DeploymentBody::ReqStart (po konsolidacji na inner enum).
 */
export function encodeServiceManifestDeployRequest(engine_id: string, deploy_method: string, node_id: string, config_json: string): Uint8Array;

/**
 * MessageBody::ServiceBody(ServicePayload::ReqModelCatalog) — live model
 * catalog of a deployed external provider service (fetched from provider API).
 */
export function encodeServiceModelCatalogRequest(payload_json: string): Uint8Array;

/**
 * MessageBody::ServiceBody(ServicePayload::ReqModelSelection) — persist the
 * admin's model selection (model_registry upserted to exactly this set).
 */
export function encodeServiceModelSelectionRequest(payload_json: string): Uint8Array;

/**
 * MessageBody::ServiceBody(ServicePayload::ReqOauthPoll) — poll a login
 * flow's status.
 */
export function encodeServiceOauthPollRequest(payload_json: string): Uint8Array;

/**
 * MessageBody::ServiceBody(ServicePayload::ReqOauthStart) — begin a
 * subscription OAuth login (browser PKCE) on the named node.
 */
export function encodeServiceOauthStartRequest(payload_json: string): Uint8Array;

/**
 * MessageBody::ServiceBody(ServicePayload::ReqPause) — supervisor leaves a
 * paused service untouched.
 */
export function encodeServicePauseRequest(service_id: number, paused: boolean, node_id?: string | null): Uint8Array;

/**
 * MessageBody::ServiceBody(ServicePayload::ReqPin) — toggles the pin flag
 * used by the supervisor for auto-respawn.
 */
export function encodeServicePinRequest(service_id: number, pinned: boolean, node_id?: string | null): Uint8Array;

/**
 * MessageBody::ServiceBody(ServicePayload::ReqStart) — unpause + spawn the
 * engine when stopped/failed/paused. Idempotent for already-running services.
 */
export function encodeServiceStartRequest(service_id: number, node_id?: string | null): Uint8Array;

/**
 * MessageBody::ServiceBody(ServicePayload::ReqVramHint) — snapshot VRAM
 * per GPU + lista zewnętrznych procesów (sunshine, chrome itp.).
 */
export function encodeServiceVramHintRequest(gpu_index?: number | null, node_id?: string | null, exclude_service_id?: number | null): Uint8Array;

/**
 * MessageBody::SettingsListRequest (unit variant).
 */
export function encodeSettingsListRequest(): Uint8Array;

/**
 * MessageBody::SettingsUpdateRequest — trzy rownolegle tablice (keys/values/is_secrets).
 * Wszystkie 3 musza miec ten sam dlugosc. Pozwala na batch update z JS bez
 * serde-wasm-bindgen.
 */
export function encodeSettingsUpdateBatch(keys: string[], values: string[], is_secrets: Uint8Array): Uint8Array;

/**
 * MessageBody::SettingsUpdateRequest — simplified: para key/value/is_secret.
 * Pelna lista (N elementow) po integracji serde-wasm-bindgen (#36 phase 2).
 */
export function encodeSettingsUpdateSingle(key: string, value: string, is_secret: boolean): Uint8Array;

export function encodeSkillsCuratorApplyRequest(snapshot_id: string, approved_actions_json: string): Uint8Array;

export function encodeSkillsCuratorRollbackRequest(snapshot_id: string): Uint8Array;

export function encodeSkillsCuratorRunRequest(): Uint8Array;

export function encodeSkillsDeleteRequest(skill_id: string): Uint8Array;

export function encodeSkillsDetailRequest(skill_id: string): Uint8Array;

export function encodeSkillsForkRequest(skill_id: string, new_name: string): Uint8Array;

export function encodeSkillsHubApproveRequest(skill_id: string): Uint8Array;

export function encodeSkillsHubImportRequest(source: string, git_ref?: string | null): Uint8Array;

export function encodeSkillsHubRejectRequest(skill_id: string): Uint8Array;

export function encodeSkillsHubSearchRequest(query: string, source?: string | null): Uint8Array;

export function encodeSkillsListRequest(tag?: string | null, source?: string | null, status?: string | null): Uint8Array;

export function encodeSkillsUpsertRequest(skill_json: string): Uint8Array;

/**
 * MessageBody::SsoProviderCreateRequest — pelne dane providera SSO/OIDC.
 */
export function encodeSsoProviderCreateRequest(name: string, provider_type: string, client_id: string, client_secret: string, discovery_url: string, auto_create_users: boolean, default_group_id?: string | null): Uint8Array;

/**
 * MessageBody::SsoProviderDeleteRequest { id }.
 */
export function encodeSsoProviderDeleteRequest(id: number): Uint8Array;

/**
 * MessageBody::SsoProvidersListRequest (unit variant).
 */
export function encodeSsoProvidersListRequest(): Uint8Array;

/**
 * MessageBody::StreamBody(CloseRequest) — release a live subscription early
 * (e.g. UI tile navigates away). Reuses the original correlation id; the
 * server cancels the streaming task and emits a final Closed frame.
 */
export function encodeStreamCloseRequest(stream_id: string): Uint8Array;

/**
 * MessageBody::StreamBody(SubscribeRequest) — subscribe this connection to a
 * hub-registered stream. The server first answers with a SubscribeResponse
 * (mime + has_init_segment), then pushes a sequence of Frame chunks on the
 * same correlation id, terminating with a single Closed payload.
 */
export function encodeStreamSubscribeRequest(stream_id: string): Uint8Array;

/**
 * MessageBody::SubscribeResumeRequest { resume_token }.
 * Klient po reconnect przekazuje token z poprzedniej SubscribeResumeOffer.
 */
export function encodeSubscribeResumeRequest(resume_token: Uint8Array): Uint8Array;

export function encodeSuggestServicePortRequest(payload_json: string): Uint8Array;

export function encodeSyncConflictResolveRequest(org_id: string, addon_id: string, operation_id: string, resolution: string): Uint8Array;

export function encodeSyncConflictsListRequest(org_id: string, addon_id: string, status: string, limit: number): Uint8Array;

export function encodeSyncStorageReportRequest(): Uint8Array;

/**
 * MessageBody::TlsStatusRequest (unit variant).
 */
export function encodeTlsStatusRequest(): Uint8Array;

export function encodeToolsCatalogRequest(): Uint8Array;

/**
 * MessageBody::TranslateRequest — synchroniczne tlumaczenie przez LLM.
 * `source_lang` = "auto" dla auto-detekcji; `tone` opcjonalny
 * ("formal"/"casual"/"neutral").
 */
export function encodeTranslateRequest(source_text: string, source_lang: string, target_lang: string, tone?: string | null): Uint8Array;

/**
 * MessageBody::TtsPreviewRequest { text, model, voice } — podglad TTS
 * (synteza tekstu po czyszczeniu do audio, odtwarzane w panelu).
 */
export function encodeTtsPreviewRequest(text: string, model: string, voice: string): Uint8Array;

/**
 * MessageBody::TtsRuleCreateRequest(TtsRule).
 */
export function encodeTtsRuleCreateRequest(id: string, pattern: string, voice_id: string, priority: number): Uint8Array;

/**
 * MessageBody::TtsRuleDeleteRequest { rule_id }.
 */
export function encodeTtsRuleDeleteRequest(rule_id: string): Uint8Array;

/**
 * MessageBody::TtsRuleListRequest (unit).
 */
export function encodeTtsRuleListRequest(): Uint8Array;

/**
 * Encode Action into MessageBody::UiChannelCbor frame.
 */
export function encodeUiAction(addon_id: string, panel_id: string, panel_epoch: bigint, action_id: string, params_json: string): Uint8Array;

/**
 * Wraps raw CBOR bytes in `MessageBody::UiChannelCbor` for binary WS transport.
 */
export function encodeUiChannelCbor(cbor_bytes: Uint8Array): Uint8Array;

/**
 * Encode PanelClose into MessageBody::UiChannelCbor frame.
 */
export function encodeUiPanelClose(addon_id: string, panel_id: string, panel_epoch: bigint): Uint8Array;

/**
 * Encode PanelOpen into MessageBody::UiChannelCbor frame.
 */
export function encodeUiPanelOpen(addon_id: string, panel_id: string, locale: string, theme: string, viewport_width: number, viewport_height: number): Uint8Array;

/**
 * LEGACY UsersListRequest — zastapione przez encodeIamListUsersRequest.
 */
export function encodeUsersListRequest(): Uint8Array;

/**
 * MessageBody::VisionBody(InferRequest) — encoder Vision inference.
 */
export function encodeVisionInferRequest(service_name: string, image: Uint8Array, width?: number | null, height?: number | null): Uint8Array;

/**
 * MessageBody::VncTunnelBody(ReqClose) — tear down tunnel explicitly.
 */
export function encodeVncTunnelCloseRequest(tunnel_id: string): Uint8Array;

/**
 * MessageBody::VncTunnelBody(ReqOpen) — start streaming tunnel for session.
 */
export function encodeVncTunnelOpenRequest(session_id: number): Uint8Array;

/**
 * MessageBody::VncTunnelBody(ReqSend) — browser → container RFB bytes.
 */
export function encodeVncTunnelSendRequest(tunnel_id: string, bytes: Uint8Array): Uint8Array;

/**
 * Stale discriminantow message_kind dla dispatchu po stronie JS.
 * Wolac `messageKind()` raz, cachowac result.
 */
export function messageKind(): any;

/**
 * Szybka walidacja ze bajty maja prawidlowy ksztalt (pelny bytecheck envelope)
 * bez zwracania widoku. Uzyte do wczesnego odrzucenia malformed frames przed
 * enqueue do dispatch queue.
 */
export function validateFrame(bytes: Uint8Array): boolean;

/**
 * Inicjalizacja modulu — ustawia panic hook dla lepszych bledow w console.
 * Wolane raz po zaladowaniu .wasm w przegladarce.
 */
export function wasm_main(): void;

export type InitInput = RequestInfo | URL | Response | BufferSource | WebAssembly.Module;

export interface InitOutput {
    readonly memory: WebAssembly.Memory;
    readonly SCHEMA_VERSION: () => number;
    readonly __wbg_envelopeview_free: (a: number, b: number) => void;
    readonly __wbg_get_envelopeview_correlation_id: (a: number) => bigint;
    readonly __wbg_get_envelopeview_flags: (a: number) => number;
    readonly __wbg_get_envelopeview_is_forward: (a: number) => number;
    readonly __wbg_get_envelopeview_message_kind: (a: number) => number;
    readonly __wbg_get_envelopeview_schema_version: (a: number) => number;
    readonly __wbg_get_envelopeview_sequence: (a: number) => bigint;
    readonly browserNodeId: () => [number, number, number, number];
    readonly browserResetIdentity: () => [number, number];
    readonly browserSign: (a: number, b: number) => [number, number, number, number];
    readonly browserSignHex: (a: number, b: number) => [number, number, number, number];
    readonly decodeComponentCbor: (a: number, b: number) => [number, number, number];
    readonly decodeEnvelope: (a: number, b: number) => [number, number, number];
    readonly decodeMessageBody: (a: number, b: number) => [number, number, number];
    readonly decodePatchOpsCbor: (a: number, b: number) => [number, number, number];
    readonly decodeStateEntriesCbor: (a: number, b: number) => [number, number, number];
    readonly decodeUiPayload: (a: number, b: number) => [number, number, number];
    readonly encodeAddonAccessDecisionRequest: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number) => [number, number, number, number];
    readonly encodeAddonAccessListRequest: (a: number, b: number) => [number, number, number, number];
    readonly encodeAddonAdminOnlySetRequest: (a: number, b: number, c: number) => [number, number, number, number];
    readonly encodeAddonApplicationsListRequest: () => [number, number, number, number];
    readonly encodeAddonCatalogListRequest: () => [number, number, number, number];
    readonly encodeAddonConfigGetRequest: (a: number, b: number) => [number, number, number, number];
    readonly encodeAddonConfigSetRequest: (a: number, b: number, c: number, d: number, e: number, f: number) => [number, number, number, number];
    readonly encodeAddonDetailRequest: (a: number, b: number) => [number, number, number, number];
    readonly encodeAddonInstallRequest: (a: number, b: number, c: number, d: number) => [number, number, number, number];
    readonly encodeAddonInstanceDuplicateRequest: (a: number, b: number, c: number, d: number) => [number, number, number, number];
    readonly encodeAddonInstanceInstallRequest: (a: number, b: number, c: number, d: number, e: number, f: number) => [number, number, number, number];
    readonly encodeAddonInstanceUpdateRequest: (a: number, b: number, c: number, d: number) => [number, number, number, number];
    readonly encodeAddonInstanceVersionsRequest: (a: number, b: number) => [number, number, number, number];
    readonly encodeAddonLogsRequest: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number) => [number, number, number, number];
    readonly encodeAddonNetworkRulesGetRequest: (a: number, b: number) => [number, number, number, number];
    readonly encodeAddonNetworkRulesSetRequest: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number) => [number, number, number, number];
    readonly encodeAddonOAuthAuthorizeStartRequest: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number) => [number, number, number, number];
    readonly encodeAddonOAuthConfigClearSecretRequest: (a: number, b: number, c: number, d: number) => [number, number, number, number];
    readonly encodeAddonOAuthConfigListRequest: (a: number, b: number) => [number, number, number, number];
    readonly encodeAddonOAuthConfigSetRequest: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number, i: number, j: number, k: number, l: number, m: number) => [number, number, number, number];
    readonly encodeAddonOAuthLinkedAccountsRequest: (a: number, b: number, c: number, d: number) => [number, number, number, number];
    readonly encodeAddonOAuthReauthorizeRequest: (a: number) => [number, number, number, number];
    readonly encodeAddonOAuthRevokeRequest: (a: number) => [number, number, number, number];
    readonly encodeAddonOAuthTestConnectionRequest: (a: number, b: number, c: number, d: number) => [number, number, number, number];
    readonly encodeAddonPermissionCatalogRequest: (a: number, b: number) => [number, number, number, number];
    readonly encodeAddonPermissionCheckRequest: (a: number, b: number, c: number, d: number, e: number, f: number) => [number, number, number, number];
    readonly encodeAddonPermissionDefaultSetRequest: (a: number, b: number, c: number, d: number, e: number, f: number) => [number, number, number, number];
    readonly encodeAddonPermissionMatrixRequest: (a: number, b: number) => [number, number, number, number];
    readonly encodeAddonPermissionSetRequest: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number, i: number, j: number) => [number, number, number, number];
    readonly encodeAddonReloadRequest: (a: number, b: number) => [number, number, number, number];
    readonly encodeAddonResourcesGetRequest: (a: number, b: number) => [number, number, number, number];
    readonly encodeAddonResourcesSetRequest: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number) => [number, number, number, number];
    readonly encodeAddonShowInCatalogSetRequest: (a: number, b: number, c: number) => [number, number, number, number];
    readonly encodeAddonStorageStatsRequest: (a: number, b: number) => [number, number, number, number];
    readonly encodeAddonToggleRequest: (a: number, b: number, c: number) => [number, number, number, number];
    readonly encodeAddonToolsRequest: (a: number, b: number) => [number, number, number, number];
    readonly encodeAddonUninstallRequest: (a: number, b: number) => [number, number, number, number];
    readonly encodeAddonVectorGetConfigRequest: (a: number, b: number) => [number, number, number, number];
    readonly encodeAddonVectorSetConfigRequest: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number, i: number, j: number, k: number, l: number, m: number, n: number, o: number, p: number, q: number, r: number) => [number, number, number, number];
    readonly encodeAddonVisibilityListRequest: (a: number, b: number) => [number, number, number, number];
    readonly encodeAddonVisibilitySetRequest: (a: number, b: number, c: number, d: number, e: number) => [number, number, number, number];
    readonly encodeAddonsListRequest: () => [number, number, number, number];
    readonly encodeAgentPermissionReplyRequest: (a: number, b: number, c: number, d: number, e: number, f: number) => [number, number, number, number];
    readonly encodeAgentRunCancelRequest: (a: number, b: number) => [number, number, number, number];
    readonly encodeAgentRunDetailRequest: (a: number, b: number) => [number, number, number, number];
    readonly encodeAgentRunEventsSubscribeRequest: (a: number, b: number, c: number, d: number) => [number, number, number, number];
    readonly encodeAgentRunReplyRequest: (a: number, b: number, c: number, d: number, e: number, f: number) => [number, number, number, number];
    readonly encodeAgentRunsListRequest: (a: number, b: number, c: number, d: number, e: number, f: number) => [number, number, number, number];
    readonly encodeAgentsDeleteRequest: (a: number, b: number) => [number, number, number, number];
    readonly encodeAgentsDetailRequest: (a: number, b: number) => [number, number, number, number];
    readonly encodeAgentsListRequest: (a: number, b: number) => [number, number, number, number];
    readonly encodeAgentsUpsertRequest: (a: number, b: number) => [number, number, number, number];
    readonly encodeAliasConsumerGrantRequest: (a: number, b: number, c: number) => [number, number, number, number];
    readonly encodeAliasConsumerListRequest: (a: number) => [number, number, number, number];
    readonly encodeAliasConsumerRevokeRequest: (a: number, b: number, c: number) => [number, number, number, number];
    readonly encodeAliasVisibilitySetRequest: (a: number, b: number, c: number) => [number, number, number, number];
    readonly encodeApiKeyCreateRequest: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number, i: number, j: number) => [number, number, number, number];
    readonly encodeApiKeyListRequest: () => [number, number, number, number];
    readonly encodeApiKeyRevokeRequest: (a: number, b: number) => [number, number, number, number];
    readonly encodeApiKeyRotateRequest: (a: number, b: number) => [number, number, number, number];
    readonly encodeApiKeyScopeClearRequest: (a: number, b: number, c: number, d: number, e: number, f: number) => [number, number, number, number];
    readonly encodeApiKeyScopeListRequest: (a: number, b: number) => [number, number, number, number];
    readonly encodeApiKeyScopeSetRequest: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number) => [number, number, number, number];
    readonly encodeAuditLogCleanupRequest: (a: number) => [number, number, number, number];
    readonly encodeAuditLogExportRequest: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number, i: number, j: number, k: number, l: number) => [number, number, number, number];
    readonly encodeAuditLogListRequest: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number, i: number, j: number, k: number, l: number, m: number, n: number) => [number, number, number, number];
    readonly encodeAuthLoginRequest: (a: number, b: number, c: number, d: number) => [number, number, number, number];
    readonly encodeAuthMeRequest: () => [number, number, number, number];
    readonly encodeBaselineAdoptClearRequest: () => [number, number, number, number];
    readonly encodeBaselineAdoptStartRequest: (a: number, b: number) => [number, number, number, number];
    readonly encodeBaselineAdoptStatusRequest: () => [number, number, number, number];
    readonly encodeBaselineDonorListRequest: () => [number, number, number, number];
    readonly encodeBrowserCaptureRequest: (a: number, b: number, c: number, d: number) => [number, number, number, number];
    readonly encodeCameraAddOnvifRequest: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number, i: number, j: number, k: number) => [number, number, number, number];
    readonly encodeCameraDetectionsSubscribeRequest: (a: number, b: number) => [number, number, number, number];
    readonly encodeCameraDiscoverRequest: () => [number, number, number, number];
    readonly encodeCameraFrameUrlRequest: (a: number, b: number, c: number) => [number, number, number, number];
    readonly encodeCatalogListRequest: (a: number, b: number, c: number) => [number, number, number, number];
    readonly encodeChatStreamRequestSimple: (a: number, b: number, c: number, d: number, e: number, f: number) => [number, number, number, number];
    readonly encodeClusterAddMemberRequest: (a: number, b: number, c: number, d: number, e: number, f: number, g: number) => [number, number, number, number];
    readonly encodeClusterCreateRequest: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number, i: number, j: number, k: number) => [number, number, number, number];
    readonly encodeClusterDeleteRequest: (a: number, b: number) => [number, number, number, number];
    readonly encodeClusterDetailRequest: (a: number, b: number) => [number, number, number, number];
    readonly encodeClusterListRequest: () => [number, number, number, number];
    readonly encodeClusterProbeStreamRequest: (a: number, b: number) => [number, number, number, number];
    readonly encodeClusterRemoveMemberRequest: (a: number, b: number, c: number, d: number) => [number, number, number, number];
    readonly encodeClusterUpdateRequest: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number, i: number, j: number, k: number, l: number, m: number) => [number, number, number, number];
    readonly encodeComplianceAiEventsListRequest: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number) => [number, number, number, number];
    readonly encodeComplianceDataCategoriesListRequest: () => [number, number, number, number];
    readonly encodeComplianceRetentionPoliciesListRequest: () => [number, number, number, number];
    readonly encodeDashboardMetricsRequest: () => [number, number, number, number];
    readonly encodeDeployVllmRecommendRequest: (a: number, b: number) => [number, number, number, number];
    readonly encodeDeploymentListRequest: (a: number, b: number, c: number, d: number, e: number, f: number) => [number, number, number, number];
    readonly encodeDeploymentLogStreamRequest: (a: number, b: number, c: number) => [number, number, number, number];
    readonly encodeDeploymentStatusRequest: (a: number, b: number) => [number, number, number, number];
    readonly encodeEnvelopeDirect: (a: bigint, b: bigint, c: number, d: number, e: number) => [number, number, number, number];
    readonly encodeFastPathListRequest: () => [number, number, number, number];
    readonly encodeFlowCreateRequest: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number) => [number, number, number, number];
    readonly encodeFlowDeleteRequest: (a: number, b: number) => [number, number, number, number];
    readonly encodeFlowDetailRequest: (a: number, b: number) => [number, number, number, number];
    readonly encodeFlowExecutionsListRequest: (a: number, b: number) => [number, number, number, number];
    readonly encodeFlowInvokeAudio: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number, i: number, j: number, k: number, l: number, m: number, n: number, o: number) => [number, number, number, number];
    readonly encodeFlowListRequest: () => [number, number, number, number];
    readonly encodeFlowNodeTemplatesListRequest: () => [number, number, number, number];
    readonly encodeFlowUpdateRequest: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number, i: number, j: number, k: number, l: number, m: number) => [number, number, number, number];
    readonly encodeFlowVersionGetRequest: (a: number, b: number, c: number, d: number) => [number, number, number, number];
    readonly encodeFlowVersionListRequest: (a: number, b: number) => [number, number, number, number];
    readonly encodeFlowVersionRestoreRequest: (a: number, b: number, c: number, d: number) => [number, number, number, number];
    readonly encodeHubEngineListRequest: () => [number, number, number, number];
    readonly encodeHubModelSearchRequest: (a: number, b: number) => [number, number, number, number];
    readonly encodeIamClearPermissionRequest: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number) => [number, number, number, number];
    readonly encodeIamCreateGroupRequest: (a: number, b: number, c: number, d: number) => [number, number, number, number];
    readonly encodeIamCreateUserRequest: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number, i: number, j: number, k: number, l: number) => [number, number, number, number];
    readonly encodeIamDeleteGroupRequest: (a: number, b: number) => [number, number, number, number];
    readonly encodeIamDeleteUserRequest: (a: number, b: number) => [number, number, number, number];
    readonly encodeIamGetUserRequest: (a: number, b: number) => [number, number, number, number];
    readonly encodeIamGroupMembersRequest: (a: number, b: number) => [number, number, number, number];
    readonly encodeIamListGroupsRequest: () => [number, number, number, number];
    readonly encodeIamListPermsForResourceRequest: (a: number, b: number, c: number, d: number) => [number, number, number, number];
    readonly encodeIamListPermsForSubjectRequest: (a: number, b: number, c: number, d: number) => [number, number, number, number];
    readonly encodeIamListUsersRequest: () => [number, number, number, number];
    readonly encodeIamResetUserPasswordRequest: (a: number, b: number, c: number, d: number) => [number, number, number, number];
    readonly encodeIamSetPermissionRequest: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number, i: number, j: number) => [number, number, number, number];
    readonly encodeIamSetUserGroupsRequest: (a: number, b: number, c: number, d: number) => [number, number, number, number];
    readonly encodeIamUpdateGroupRequest: (a: number, b: number, c: number, d: number, e: number, f: number) => [number, number, number, number];
    readonly encodeIamUpdateUserRequest: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number, i: number) => [number, number, number, number];
    readonly encodeLegalDocumentGenerateRequest: (a: number, b: number) => [number, number, number, number];
    readonly encodeLegalDocumentRevokeRequest: (a: number, b: number) => [number, number, number, number];
    readonly encodeLegalDocumentsListRequest: (a: number) => [number, number, number, number];
    readonly encodeMePreferencesGetRequest: () => [number, number, number, number];
    readonly encodeMePreferencesUpdateRequest: (a: number, b: number) => [number, number, number, number];
    readonly encodeMeetingActionItemStatusUpdateRequest: (a: number, b: number, c: number) => [number, number, number, number];
    readonly encodeMeetingActionItemsListRequest: (a: number, b: number, c: number, d: number) => [number, number, number, number];
    readonly encodeMeetingActiveSessionRequest: () => [number, number, number, number];
    readonly encodeMeetingSessionDetailRequest: (a: number, b: number) => [number, number, number, number];
    readonly encodeMeetingSessionLeaveRequest: (a: number) => [number, number, number, number];
    readonly encodeMeetingSessionListRequest: (a: number) => [number, number, number, number];
    readonly encodeMeetingSessionStartRequest: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number, i: number, j: number, k: number, l: number, m: number, n: number) => [number, number, number, number];
    readonly encodeMeetingSettingsGetRequest: () => [number, number, number, number];
    readonly encodeMeetingSettingsUpdateRequest: (a: any) => [number, number, number, number];
    readonly encodeMeetingSummariesListRequest: (a: number, b: number, c: number) => [number, number, number, number];
    readonly encodeMeetingTranscriptExportRequest: (a: number, b: number) => [number, number, number, number];
    readonly encodeMeetingTranscriptsListRequest: (a: number, b: number) => [number, number, number, number];
    readonly encodeMeshConnectRequest: (a: number, b: number) => [number, number, number, number];
    readonly encodeMeshIdentityRequest: () => [number, number, number, number];
    readonly encodeMeshNodeCommandRequest: (a: number, b: number, c: number, d: number, e: number, f: number) => [number, number, number, number];
    readonly encodeMeshNodeDetailRequest: (a: number, b: number) => [number, number, number, number];
    readonly encodeMeshNodeListRequest: () => [number, number, number, number];
    readonly encodeMeshNodeNetworkConfigRequest: (a: number, b: number, c: number, d: number, e: number, f: number) => [number, number, number, number];
    readonly encodeMeshPairInitRequest: (a: number, b: number, c: number, d: number) => [number, number, number, number];
    readonly encodeMeshPairingConfirmRequest: (a: number, b: number, c: number, d: number) => [number, number, number, number];
    readonly encodeMeshPairingRejectRequest: (a: number, b: number) => [number, number, number, number];
    readonly encodeMeshPairingStartRequest: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number, i: number, j: number, k: number, l: number) => [number, number, number, number];
    readonly encodeMeshPeersListRequest: () => [number, number, number, number];
    readonly encodeMeshPendingListRequest: () => [number, number, number, number];
    readonly encodeMeshServicesListRequest: () => [number, number, number, number];
    readonly encodeMeshTrustRetrustRequest: (a: number, b: number) => [number, number, number, number];
    readonly encodeMeshTrustRevokeRequest: (a: number, b: number) => [number, number, number, number];
    readonly encodeMeshTrustedListRequest: () => [number, number, number, number];
    readonly encodeMetaCancelStream: () => [number, number, number, number];
    readonly encodeMetaHeartbeat: (a: bigint) => [number, number, number, number];
    readonly encodeMetaSchemaVersionCheck: (a: number) => [number, number, number, number];
    readonly encodeMlStudioDatasetProfileRequest: (a: number, b: number) => [number, number, number, number];
    readonly encodeMlStudioDatasetUploadChunkRequest: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number, i: number, j: number, k: number, l: number) => [number, number, number, number];
    readonly encodeMlStudioDatasetUploadRequest: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number) => [number, number, number, number];
    readonly encodeMlStudioDatasetsListRequest: (a: number, b: number) => [number, number, number, number];
    readonly encodeMlStudioFtChatRequest: (a: number, b: number, c: number, d: number, e: number) => [number, number, number, number];
    readonly encodeMlStudioFtDeployRequest: (a: number, b: number, c: number, d: number) => [number, number, number, number];
    readonly encodeMlStudioFtExportRequest: (a: number, b: number, c: number, d: number) => [number, number, number, number];
    readonly encodeMlStudioFtExportStatusRequest: (a: number, b: number) => [number, number, number, number];
    readonly encodeMlStudioFtTrainStartRequest: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number, i: number, j: number, k: number, l: number, m: number, n: number, o: number, p: number, q: number, r: number, s: number, t: number, u: number, v: number, w: number, x: number, y: number, z: number, a1: number, b1: number, c1: number) => [number, number, number, number];
    readonly encodeMlStudioFtTrainStatusRequest: (a: number, b: number) => [number, number, number, number];
    readonly encodeMlStudioModelsListRequest: (a: number, b: number) => [number, number, number, number];
    readonly encodeMlStudioProjectCreateRequest: (a: number, b: number, c: number, d: number, e: number, f: number) => [number, number, number, number];
    readonly encodeMlStudioProjectDetailRequest: (a: number, b: number) => [number, number, number, number];
    readonly encodeMlStudioProjectGrantsListRequest: (a: number, b: number) => [number, number, number, number];
    readonly encodeMlStudioProjectInviteRequest: (a: number, b: number, c: number, d: number, e: number, f: number) => [number, number, number, number];
    readonly encodeMlStudioProjectMemberRemoveRequest: (a: number, b: number, c: number, d: number) => [number, number, number, number];
    readonly encodeMlStudioProjectMemberRoleSetRequest: (a: number, b: number, c: number, d: number, e: number, f: number) => [number, number, number, number];
    readonly encodeMlStudioProjectMembersListRequest: (a: number, b: number) => [number, number, number, number];
    readonly encodeMlStudioProjectResourcesRequest: (a: number, b: number) => [number, number, number, number];
    readonly encodeMlStudioProjectTypesListRequest: () => [number, number, number, number];
    readonly encodeMlStudioProjectsListRequest: () => [number, number, number, number];
    readonly encodeMlStudioRecogDatasetRegisterRequest: (a: number, b: number, c: number, d: number, e: number, f: number) => [number, number, number, number];
    readonly encodeMlStudioRecogDetectRequest: (a: number, b: number, c: number, d: number, e: number) => [number, number, number, number];
    readonly encodeMlStudioRecogImageRequest: (a: number, b: number, c: number, d: number) => [number, number, number, number];
    readonly encodeMlStudioRecogImagesListRequest: (a: number, b: number) => [number, number, number, number];
    readonly encodeMlStudioRecogSaveAnnotationsRequest: (a: number, b: number, c: number, d: number, e: number, f: number, g: number) => [number, number, number, number];
    readonly encodeMlStudioRecogTrainStartRequest: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number, i: number, j: number, k: number, l: number, m: number, n: number) => [number, number, number, number];
    readonly encodeMlStudioRecogTrainStatusRequest: (a: number, b: number) => [number, number, number, number];
    readonly encodeMlStudioResourceGrantCreateRequest: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number, i: number, j: number, k: number, l: number) => [number, number, number, number];
    readonly encodeMlStudioResourceGrantRevokeRequest: (a: number, b: number) => [number, number, number, number];
    readonly encodeMlStudioResourceGrantsListRequest: () => [number, number, number, number];
    readonly encodeMlStudioTabularTrainRequest: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number, i: number, j: number) => [number, number, number, number];
    readonly encodeMlStudioTrainingRunsListRequest: (a: number, b: number) => [number, number, number, number];
    readonly encodeModelAliasCreateRequest: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number) => [number, number, number, number];
    readonly encodeModelAliasDeleteRequest: (a: number) => [number, number, number, number];
    readonly encodeModelAliasListRequest: () => [number, number, number, number];
    readonly encodeModelAliasUpdateRequest: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number, i: number, j: number) => [number, number, number, number];
    readonly encodeModelConsumerGrantRequest: (a: number, b: number, c: number, d: number) => [number, number, number, number];
    readonly encodeModelConsumerListRequest: (a: number, b: number) => [number, number, number, number];
    readonly encodeModelConsumerRevokeRequest: (a: number, b: number, c: number, d: number) => [number, number, number, number];
    readonly encodeModelDeleteRequest: (a: number, b: number) => [number, number, number, number];
    readonly encodeModelDetailRequest: (a: number, b: number) => [number, number, number, number];
    readonly encodeModelInstallRequest: (a: number, b: number, c: number, d: number) => [number, number, number, number];
    readonly encodeModelListRequest: () => [number, number, number, number];
    readonly encodeModelVisibilityListRequest: () => [number, number, number, number];
    readonly encodeModelVisibilitySetRequest: (a: number, b: number, c: number, d: number) => [number, number, number, number];
    readonly encodeMyOAuthAccountsListRequest: () => [number, number, number, number];
    readonly encodeNetworkConfigGetRequest: () => [number, number, number, number];
    readonly encodeNetworkConfigUpdateRequest: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number, i: number, j: number, k: number, l: number, m: number) => [number, number, number, number];
    readonly encodeNetworkInterfacesListRequest: () => [number, number, number, number];
    readonly encodeNetworkRelayStatusRequest: () => [number, number, number, number];
    readonly encodeNgcStatusRequest: () => [number, number, number, number];
    readonly encodeNimCatalogListRequest: () => [number, number, number, number];
    readonly encodeNoteCreateRequest: (a: number, b: number, c: number, d: number) => [number, number, number, number];
    readonly encodeNoteDeleteRequest: (a: number) => [number, number, number, number];
    readonly encodeNoteDetailRequest: (a: number) => [number, number, number, number];
    readonly encodeNoteSetPinnedRequest: (a: number, b: number) => [number, number, number, number];
    readonly encodeNoteUpdateRequest: (a: number, b: number, c: number, d: number, e: number) => [number, number, number, number];
    readonly encodeNotesListRequest: () => [number, number, number, number];
    readonly encodePiiRuleListRequest: () => [number, number, number, number];
    readonly encodeProfilingActiveInfoRequest: (a: number, b: number) => [number, number, number, number];
    readonly encodeProfilingCollectorsStatusRequest: (a: number, b: number) => [number, number, number, number];
    readonly encodeProfilingDeleteRequest: (a: number, b: number, c: number, d: number) => [number, number, number, number];
    readonly encodeProfilingDownloadRequest: (a: number, b: number, c: number, d: number) => [number, number, number, number];
    readonly encodeProfilingReportRequest: (a: number, b: number, c: number, d: number) => [number, number, number, number];
    readonly encodeProfilingSessionsRequest: (a: number, b: number) => [number, number, number, number];
    readonly encodeProfilingStartRequest: (a: number, b: number, c: any, d: number, e: number, f: number, g: number) => [number, number, number, number];
    readonly encodeProfilingStopRequest: (a: number, b: number, c: number, d: number) => [number, number, number, number];
    readonly encodeProfilingValidateSudoRequest: (a: number, b: number, c: number, d: number) => [number, number, number, number];
    readonly encodePromptDetailRequest: (a: number, b: number) => [number, number, number, number];
    readonly encodePromptListRequest: () => [number, number, number, number];
    readonly encodeRegistryListRequest: () => [number, number, number, number];
    readonly encodeRoleCatalogCreateRequest: (a: number, b: number) => [number, number, number, number];
    readonly encodeRoleCatalogDeactivateRequest: (a: number, b: number) => [number, number, number, number];
    readonly encodeRoleCatalogGetBySlugRequest: (a: number, b: number) => [number, number, number, number];
    readonly encodeRoleCatalogGetRequest: (a: number, b: number) => [number, number, number, number];
    readonly encodeRoleCatalogListLocalesRequest: () => [number, number, number, number];
    readonly encodeRoleCatalogListRequest: (a: number, b: number) => [number, number, number, number];
    readonly encodeRoleCatalogUpdateRequest: (a: number, b: number) => [number, number, number, number];
    readonly encodeSchedulerActionsListRequest: () => [number, number, number, number];
    readonly encodeSchedulerJobDeleteRequest: (a: number, b: number) => [number, number, number, number];
    readonly encodeSchedulerJobRunNowRequest: (a: number, b: number) => [number, number, number, number];
    readonly encodeSchedulerJobUpsertRequest: (a: number, b: number) => [number, number, number, number];
    readonly encodeSchedulerJobsListRequest: () => [number, number, number, number];
    readonly encodeSchedulerRunsListRequest: (a: number, b: number, c: number) => [number, number, number, number];
    readonly encodeServiceConfigUpdateRequest: (a: number, b: number) => [number, number, number, number];
    readonly encodeServiceDeleteRequest: (a: number, b: number, c: number) => [number, number, number, number];
    readonly encodeServiceEnginePresetsRequest: (a: number, b: number) => [number, number, number, number];
    readonly encodeServiceListRequest: (a: number, b: number, c: number, d: number) => [number, number, number, number];
    readonly encodeServiceManifestDeployRequest: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number) => [number, number, number, number];
    readonly encodeServiceModelCatalogRequest: (a: number, b: number) => [number, number, number, number];
    readonly encodeServiceModelSelectionRequest: (a: number, b: number) => [number, number, number, number];
    readonly encodeServiceOauthPollRequest: (a: number, b: number) => [number, number, number, number];
    readonly encodeServiceOauthStartRequest: (a: number, b: number) => [number, number, number, number];
    readonly encodeServicePauseRequest: (a: number, b: number, c: number, d: number) => [number, number, number, number];
    readonly encodeServicePinRequest: (a: number, b: number, c: number, d: number) => [number, number, number, number];
    readonly encodeServiceStartRequest: (a: number, b: number, c: number) => [number, number, number, number];
    readonly encodeServiceVramHintRequest: (a: number, b: number, c: number, d: number, e: number) => [number, number, number, number];
    readonly encodeSettingsListRequest: () => [number, number, number, number];
    readonly encodeSettingsUpdateBatch: (a: number, b: number, c: number, d: number, e: number, f: number) => [number, number, number, number];
    readonly encodeSettingsUpdateSingle: (a: number, b: number, c: number, d: number, e: number) => [number, number, number, number];
    readonly encodeSkillsCuratorApplyRequest: (a: number, b: number, c: number, d: number) => [number, number, number, number];
    readonly encodeSkillsCuratorRollbackRequest: (a: number, b: number) => [number, number, number, number];
    readonly encodeSkillsCuratorRunRequest: () => [number, number, number, number];
    readonly encodeSkillsDeleteRequest: (a: number, b: number) => [number, number, number, number];
    readonly encodeSkillsDetailRequest: (a: number, b: number) => [number, number, number, number];
    readonly encodeSkillsForkRequest: (a: number, b: number, c: number, d: number) => [number, number, number, number];
    readonly encodeSkillsHubApproveRequest: (a: number, b: number) => [number, number, number, number];
    readonly encodeSkillsHubImportRequest: (a: number, b: number, c: number, d: number) => [number, number, number, number];
    readonly encodeSkillsHubRejectRequest: (a: number, b: number) => [number, number, number, number];
    readonly encodeSkillsHubSearchRequest: (a: number, b: number, c: number, d: number) => [number, number, number, number];
    readonly encodeSkillsListRequest: (a: number, b: number, c: number, d: number, e: number, f: number) => [number, number, number, number];
    readonly encodeSkillsUpsertRequest: (a: number, b: number) => [number, number, number, number];
    readonly encodeSsoProviderCreateRequest: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number, i: number, j: number, k: number, l: number, m: number) => [number, number, number, number];
    readonly encodeSsoProviderDeleteRequest: (a: number) => [number, number, number, number];
    readonly encodeSsoProvidersListRequest: () => [number, number, number, number];
    readonly encodeStreamCloseRequest: (a: number, b: number) => [number, number, number, number];
    readonly encodeStreamSubscribeRequest: (a: number, b: number) => [number, number, number, number];
    readonly encodeSubscribeResumeRequest: (a: number, b: number) => [number, number, number, number];
    readonly encodeSuggestServicePortRequest: (a: number, b: number) => [number, number, number, number];
    readonly encodeSyncConflictResolveRequest: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number) => [number, number, number, number];
    readonly encodeSyncConflictsListRequest: (a: number, b: number, c: number, d: number, e: number, f: number, g: number) => [number, number, number, number];
    readonly encodeSyncStorageReportRequest: () => [number, number, number, number];
    readonly encodeTlsStatusRequest: () => [number, number, number, number];
    readonly encodeToolsCatalogRequest: () => [number, number, number, number];
    readonly encodeTranslateRequest: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number) => [number, number, number, number];
    readonly encodeTtsPreviewRequest: (a: number, b: number, c: number, d: number, e: number, f: number) => [number, number, number, number];
    readonly encodeTtsRuleCreateRequest: (a: number, b: number, c: number, d: number, e: number, f: number, g: number) => [number, number, number, number];
    readonly encodeTtsRuleDeleteRequest: (a: number, b: number) => [number, number, number, number];
    readonly encodeTtsRuleListRequest: () => [number, number, number, number];
    readonly encodeUiAction: (a: number, b: number, c: number, d: number, e: bigint, f: number, g: number, h: number, i: number) => [number, number, number, number];
    readonly encodeUiChannelCbor: (a: number, b: number) => [number, number, number, number];
    readonly encodeUiPanelClose: (a: number, b: number, c: number, d: number, e: bigint) => [number, number, number, number];
    readonly encodeUiPanelOpen: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number, i: number, j: number) => [number, number, number, number];
    readonly encodeVisionInferRequest: (a: number, b: number, c: number, d: number, e: number, f: number) => [number, number, number, number];
    readonly encodeVncTunnelCloseRequest: (a: number, b: number) => [number, number, number, number];
    readonly encodeVncTunnelOpenRequest: (a: number) => [number, number, number, number];
    readonly encodeVncTunnelSendRequest: (a: number, b: number, c: number, d: number) => [number, number, number, number];
    readonly envelopeview_body: (a: number) => [number, number];
    readonly envelopeview_isError: (a: number) => number;
    readonly envelopeview_isStreamChunk: (a: number) => number;
    readonly envelopeview_isStreamEnd: (a: number) => number;
    readonly envelopeview_targetNodeId: (a: number) => [number, number];
    readonly messageKind: () => any;
    readonly validateFrame: (a: number, b: number) => number;
    readonly wasm_main: () => void;
    readonly encodeUsersListRequest: () => [number, number, number, number];
    readonly __wbindgen_malloc: (a: number, b: number) => number;
    readonly __wbindgen_realloc: (a: number, b: number, c: number, d: number) => number;
    readonly __wbindgen_exn_store: (a: number) => void;
    readonly __externref_table_alloc: () => number;
    readonly __wbindgen_externrefs: WebAssembly.Table;
    readonly __wbindgen_free: (a: number, b: number, c: number) => void;
    readonly __externref_table_dealloc: (a: number) => void;
    readonly __wbindgen_start: () => void;
}

export type SyncInitInput = BufferSource | WebAssembly.Module;

/**
 * Instantiates the given `module`, which can either be bytes or
 * a precompiled `WebAssembly.Module`.
 *
 * @param {{ module: SyncInitInput }} module - Passing `SyncInitInput` directly is deprecated.
 *
 * @returns {InitOutput}
 */
export function initSync(module: { module: SyncInitInput } | SyncInitInput): InitOutput;

/**
 * If `module_or_path` is {RequestInfo} or {URL}, makes a request and
 * for everything else, calls `WebAssembly.instantiate` directly.
 *
 * @param {{ module_or_path: InitInput | Promise<InitInput> }} module_or_path - Passing `InitInput` directly is deprecated.
 *
 * @returns {Promise<InitOutput>}
 */
export default function __wbg_init (module_or_path?: { module_or_path: InitInput | Promise<InitInput> } | InitInput | Promise<InitInput>): Promise<InitOutput>;
